//! Model Router — AI 网关智能模型路由（正则匹配 + 加权选择）
//! AI Gateway intelligent model routing with regex pattern matching and weighted selection
//!
//! 与 API 网关路由不同，这里路由的是「模型请求 → 具体 provider + model」：
//! Unlike API gateway routing (URL → Service), this routes "model request → provider + model":
//!
//! 1. 正则匹配：用户请求 model="gpt-4o" → 匹配 "^gpt-4" 规则
//!    Regex match: user requests "gpt-4o" → matches "^gpt-4" rule
//! 2. 加权选择：同规则下多个 provider 按权重分配（如 80% OpenAI / 20% Azure）
//!    Weighted select: multiple providers under same rule with weight distribution

use regex::Regex;
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

use kong_core::error::{KongError, Result};

use crate::models::{AiModel, AiProviderConfig};

/// 路由规则中的单个模型目标 — a single model target within a routing rule
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelTargetConfig {
    /// Provider 类型（openai / anthropic / gemini / openai_compat）
    pub provider_type: String,
    /// 实际发给 provider 的模型名称 — actual model name sent to provider
    pub model_name: String,
    /// 自定义 endpoint URL（可选，为空时使用 provider 默认）
    #[serde(default)]
    pub endpoint_url: Option<String>,
    /// 认证配置 — authentication config (header_name, header_value, etc.)
    #[serde(default)]
    pub auth_config: serde_json::Value,
    /// 权重（加权轮询）— weight for weighted round-robin
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// 路由优先级(高优先级先选,同优先级内做加权轮询)
    /// Priority for routing (higher first; weighted RR within same priority)
    #[serde(default)]
    pub priority: i32,
    /// 单次请求 prompt token 上限(超过则该 target 在 by-token-size 路由中被过滤)
    /// Per-request prompt token cap; exceeded targets are filtered out under by-token-size routing
    #[serde(default)]
    pub max_input_tokens: Option<i32>,
    /// 语义路由示例 prompt(用于 commit 3 — 启用 enable_semantic_routing 时使用)
    /// Semantic routing example prompts (used by enable_semantic_routing in commit 3)
    #[serde(default)]
    pub semantic_routing_examples: Option<Vec<String>>,
}

fn default_weight() -> u32 {
    1
}

/// 路由规则配置 — a routing rule config: regex pattern → one or more model targets
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelRouteConfig {
    /// 正则模式（匹配请求中的 model 名）— regex pattern to match incoming model name
    pub pattern: String,
    /// 目标模型列表（可多个，加权选择）— target models (multiple = weighted selection)
    pub targets: Vec<ModelTargetConfig>,
}

/// 编译后的路由规则 — compiled routing rule
struct CompiledRoute {
    pattern: Regex,
    pattern_str: String,
    targets: Vec<ModelTargetConfig>,
}

impl std::fmt::Debug for CompiledRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledRoute")
            .field("pattern", &self.pattern_str)
            .field("targets_count", &self.targets.len())
            .finish()
    }
}

/// 路由解析结果 — routing resolution result
#[derive(Debug, Clone)]
pub struct RouteResolution {
    /// 选中的 provider 类型 — selected provider type
    pub provider_type: String,
    /// 选中的 AI Model — selected model
    pub model: AiModel,
    /// 选中的 Provider 配置 — selected provider config
    pub provider_config: AiProviderConfig,
}

/// Model Router — AI 网关级别的智能模型路由器
/// AI Gateway-level intelligent model router
///
/// 工作流程：
/// 1. 用户请求 model="gpt-4o"
/// 2. 按顺序匹配 rules，找到第一条匹配的规则
/// 3. 在规则内的 targets 中按权重选择一个具体的 provider+model
/// 4. 返回 RouteResolution，直接决定上游连接目标
pub struct ModelRouter {
    rules: Vec<CompiledRoute>,
    /// 原子计数器（用于加权轮询）— atomic counter for weighted round-robin
    counter: AtomicU64,
}

impl std::fmt::Debug for ModelRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelRouter")
            .field("rules", &self.rules)
            .field("counter", &self.counter.load(Ordering::Relaxed))
            .finish()
    }
}

impl ModelRouter {
    /// 从配置列表构建 ModelRouter — build from config list
    pub fn from_configs(configs: &[ModelRouteConfig]) -> Result<Self> {
        let mut rules = Vec::with_capacity(configs.len());
        for cfg in configs {
            if cfg.targets.is_empty() {
                return Err(KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!(
                        "model route pattern '{}' has no targets",
                        cfg.pattern
                    ),
                });
            }
            let pattern = Regex::new(&cfg.pattern).map_err(|e| KongError::PluginError {
                plugin_name: "ai-proxy".to_string(),
                message: format!("invalid model route regex '{}': {}", cfg.pattern, e),
            })?;
            rules.push(CompiledRoute {
                pattern,
                pattern_str: cfg.pattern.clone(),
                targets: cfg.targets.clone(),
            });
        }
        Ok(Self {
            rules,
            counter: AtomicU64::new(0),
        })
    }

    /// 解析 model 名称 → 具体的 provider + model(等价于 resolve_for(model_name, None))
    /// Resolve model name → concrete provider + model (alias for resolve_for(model_name, None))
    pub fn resolve(&self, model_name: &str) -> Option<RouteResolution> {
        self.resolve_for(model_name, None)
    }

    /// 按 prompt token 数解析 model 名称 → 具体的 provider + model
    /// Resolve model name with prompt-token-aware filtering → concrete provider + model
    ///
    /// 匹配逻辑 — matching logic:
    /// 1. 按顺序遍历规则,找到第一条 pattern 匹配的规则(first-match wins)
    /// 2. 按 target.priority 降序分组,从最高 priority 开始
    /// 3. 在该 priority 组内过滤:
    ///    - max_input_tokens 未指定 OR prompt_tokens 未指定 OR prompt_tokens <= max_input_tokens
    /// 4. 过滤后非空 → 加权轮询;为空 → 跳到下一档 priority
    /// 5. 全档 priority 都过滤为空 → 返回 None
    ///
    /// `prompt_tokens=None` 时不启用 token 过滤,仅按 priority + 加权选择
    /// When prompt_tokens=None, token filtering is disabled — only priority + weighted RR
    pub fn resolve_for(
        &self,
        model_name: &str,
        prompt_tokens: Option<u64>,
    ) -> Option<RouteResolution> {
        // 第一条匹配的规则 — first matching rule
        let rule = self.rules.iter().find(|r| r.pattern.is_match(model_name))?;

        if rule.targets.is_empty() {
            return None;
        }

        // 收集 distinct priority(降序)— collect distinct priorities (descending)
        let mut priorities: Vec<i32> = rule.targets.iter().map(|t| t.priority).collect();
        priorities.sort_unstable_by(|a, b| b.cmp(a));
        priorities.dedup();

        for priority in &priorities {
            // 同 priority 且通过 token budget 过滤的候选
            // Candidates in this priority tier passing token budget filter
            let candidates: Vec<&ModelTargetConfig> = rule
                .targets
                .iter()
                .filter(|t| {
                    t.priority == *priority && fits_token_budget(t.max_input_tokens, prompt_tokens)
                })
                .collect();

            if candidates.is_empty() {
                continue;
            }

            if candidates.len() == 1 {
                return Some(self.build_resolution(model_name, candidates[0]));
            }

            // 加权轮询 — weighted round-robin within this priority tier
            let total_weight: u64 = candidates.iter().map(|t| t.weight as u64).sum();
            if total_weight == 0 {
                let tick = self.counter.fetch_add(1, Ordering::Relaxed) as usize;
                let pick = candidates[tick % candidates.len()];
                return Some(self.build_resolution(model_name, pick));
            }

            let tick = self.counter.fetch_add(1, Ordering::Relaxed);
            let slot = tick % total_weight;

            let mut cumulative: u64 = 0;
            for target in &candidates {
                cumulative += target.weight as u64;
                if slot < cumulative {
                    return Some(self.build_resolution(model_name, target));
                }
            }

            return Some(self.build_resolution(model_name, candidates[0]));
        }

        // 所有 priority 档全被 token-budget 过滤掉 — every tier exhausted by token budget
        None
    }

    /// 从 target config 构建 RouteResolution — build resolution from target config
    fn build_resolution(&self, model_name: &str, target: &ModelTargetConfig) -> RouteResolution {
        let model = AiModel {
            id: Uuid::new_v4(),
            name: model_name.to_string(),
            model_name: target.model_name.clone(),
            priority: target.priority,
            weight: target.weight as i32,
            max_input_tokens: target.max_input_tokens,
            enabled: true,
            ..Default::default()
        };

        let provider_config = AiProviderConfig {
            id: Uuid::new_v4(),
            name: target.provider_type.clone(),
            provider_type: target.provider_type.clone(),
            endpoint_url: target.endpoint_url.clone(),
            auth_config: target.auth_config.clone(),
            enabled: true,
            ..Default::default()
        };

        RouteResolution {
            provider_type: target.provider_type.clone(),
            model,
            provider_config,
        }
    }
}

/// 判断 target 是否能容纳给定 prompt token 数
/// Whether the target can accommodate the given prompt token count.
///
/// 规则 / Rules:
/// - prompt_tokens=None → 不启用过滤(永远通过)
/// - max_input_tokens=None / Some(<=0) → 视为无限制(永远通过)
/// - 否则 prompt_tokens <= max_input_tokens 才通过
fn fits_token_budget(max_input_tokens: Option<i32>, prompt_tokens: Option<u64>) -> bool {
    match (prompt_tokens, max_input_tokens) {
        (None, _) => true,
        (_, None) => true,
        (_, Some(cap)) if cap <= 0 => true,
        (Some(pt), Some(cap)) => pt <= cap as u64,
    }
}

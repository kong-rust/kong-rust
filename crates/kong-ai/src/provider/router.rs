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

    /// 解析 model 名称 → 具体的 provider + model
    /// Resolve model name → concrete provider + model
    ///
    /// 匹配逻辑 — matching logic:
    /// 1. 按顺序遍历规则，找到第一条 pattern 匹配的规则（first-match wins）
    /// 2. 在该规则的 targets 中按权重加权轮询选择一个目标
    /// 3. 构建 RouteResolution 返回
    /// 4. 无匹配 → 返回 None（fallback 到 inline provider 配置）
    pub fn resolve(&self, model_name: &str) -> Option<RouteResolution> {
        // 第一条匹配的规则 — first matching rule
        let rule = self.rules.iter().find(|r| r.pattern.is_match(model_name))?;

        if rule.targets.len() == 1 {
            return Some(self.build_resolution(model_name, &rule.targets[0]));
        }

        // 加权轮询选择 — weighted round-robin among targets
        let total_weight: u64 = rule.targets.iter().map(|t| t.weight as u64).sum();
        if total_weight == 0 {
            return Some(self.build_resolution(model_name, &rule.targets[0]));
        }

        let tick = self.counter.fetch_add(1, Ordering::Relaxed);
        let slot = tick % total_weight;

        let mut cumulative: u64 = 0;
        for target in &rule.targets {
            cumulative += target.weight as u64;
            if slot < cumulative {
                return Some(self.build_resolution(model_name, target));
            }
        }

        // 理论上不会到这里 — should never reach here
        Some(self.build_resolution(model_name, &rule.targets[0]))
    }

    /// 从 target config 构建 RouteResolution — build resolution from target config
    fn build_resolution(&self, model_name: &str, target: &ModelTargetConfig) -> RouteResolution {
        let model = AiModel {
            id: Uuid::new_v4(),
            name: model_name.to_string(),
            model_name: target.model_name.clone(),
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

//! Model Router — 智能模型路由（正则匹配 + 加权选择）
//! Intelligent model routing with regex pattern matching and weighted selection

use regex::Regex;
use std::sync::atomic::{AtomicU64, Ordering};

use kong_core::error::{KongError, Result};

/// 单条路由规则 — a single routing rule
pub struct ModelRoute {
    /// 正则模式（匹配请求中的 model 名） — regex pattern to match incoming model name
    pattern: Regex,
    /// 原始 pattern 字符串（用于 Debug/序列化） — original pattern string
    pattern_str: String,
    /// 目标 model group 名称 — target model group name (maps to ai_models.name)
    target_group: String,
    /// 权重（同 pattern 多个目标间加权选择）— weight for weighted selection among matching rules
    weight: u32,
}

impl std::fmt::Debug for ModelRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelRoute")
            .field("pattern", &self.pattern_str)
            .field("target_group", &self.target_group)
            .field("weight", &self.weight)
            .finish()
    }
}

/// Model Router — 按顺序匹配请求 model 名到目标 model group
pub struct ModelRouter {
    routes: Vec<ModelRoute>,
    /// 原子计数器（用于加权轮询） — atomic counter for weighted round-robin
    counter: AtomicU64,
}

impl std::fmt::Debug for ModelRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelRouter")
            .field("routes", &self.routes)
            .field("counter", &self.counter.load(Ordering::Relaxed))
            .finish()
    }
}

/// Serde 友好的路由配置结构体 — serde-friendly route config
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelRouteConfig {
    pub pattern: String,
    pub target_group: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
}

fn default_weight() -> u32 {
    1
}

impl ModelRouter {
    /// 从配置列表构建 ModelRouter — build from config list
    pub fn from_configs(configs: &[ModelRouteConfig]) -> Result<Self> {
        let mut routes = Vec::with_capacity(configs.len());
        for cfg in configs {
            let pattern = Regex::new(&cfg.pattern).map_err(|e| KongError::PluginError {
                plugin_name: "ai-proxy".to_string(),
                message: format!("invalid model route regex '{}': {}", cfg.pattern, e),
            })?;
            routes.push(ModelRoute {
                pattern,
                pattern_str: cfg.pattern.clone(),
                target_group: cfg.target_group.clone(),
                weight: cfg.weight,
            });
        }
        Ok(Self {
            routes,
            counter: AtomicU64::new(0),
        })
    }

    /// 解析 model 名称到目标 group — resolve model name to target group
    ///
    /// 匹配逻辑：
    /// 1. 按顺序遍历所有规则，收集所有匹配项
    /// 2. 如果只有一个匹配 → 直接返回
    /// 3. 如果多个匹配 → 加权轮询选择
    /// 4. 无匹配 → 返回 None
    pub fn resolve(&self, model_name: &str) -> Option<String> {
        // 收集所有匹配的规则 — collect all matching rules
        let matched: Vec<&ModelRoute> = self
            .routes
            .iter()
            .filter(|r| r.pattern.is_match(model_name))
            .collect();

        if matched.is_empty() {
            return None;
        }

        if matched.len() == 1 {
            return Some(matched[0].target_group.clone());
        }

        // 加权轮询选择 — weighted round-robin selection
        let total_weight: u64 = matched.iter().map(|r| r.weight as u64).sum();
        if total_weight == 0 {
            // 所有权重为 0，返回第一个匹配 — all weights zero, return first match
            return Some(matched[0].target_group.clone());
        }

        let tick = self.counter.fetch_add(1, Ordering::Relaxed);
        let slot = tick % total_weight;

        let mut cumulative: u64 = 0;
        for route in &matched {
            cumulative += route.weight as u64;
            if slot < cumulative {
                return Some(route.target_group.clone());
            }
        }

        // 理论上不会到这里 — should never reach here
        Some(matched[0].target_group.clone())
    }
}

//! ai-rate-limit 插件 — RPM/TPM 内存限流 + 预扣修正

use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;

use kong_core::error::Result;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

use crate::plugins::context::AiRequestState;
use crate::ratelimit::RateLimiter;
use crate::ratelimit::memory::MemoryRateLimiter;
use crate::token::{global_registry, TokenCounter, TokenizerRegistry};

// ============ 插件配置 ============

/// ai-rate-limit 插件配置
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiRateLimitConfig {
    /// 限流维度："virtual_key" | "consumer" | "route" | "global"
    pub limit_by: String,
    /// Token Per Minute 限制
    pub tpm_limit: Option<u64>,
    /// Request Per Minute 限制
    pub rpm_limit: Option<u64>,
    /// 读取 virtual key 的 header 名称
    pub header_name: String,
    /// 超限错误码
    pub error_code: u16,
    /// 超限错误消息
    pub error_message: String,
}

impl Default for AiRateLimitConfig {
    fn default() -> Self {
        Self {
            limit_by: "consumer".to_string(),
            tpm_limit: None,
            rpm_limit: None,
            header_name: "X-AI-Key".to_string(),
            error_code: 429,
            error_message: "AI rate limit exceeded".to_string(),
        }
    }
}

/// 存储在 ctx.extensions 中的限流上下文（跨阶段共享）
pub struct AiRateLimitContext {
    /// 限流键前缀
    pub rate_key: String,
    /// 预扣的 prompt token 估算值
    pub estimated_prompt_tokens: u64,
}

// ============ 插件结构体 ============

/// AI 速率限制插件
pub struct AiRateLimitPlugin {
    limiter: Arc<dyn RateLimiter>,
}

impl AiRateLimitPlugin {
    /// 创建新的 ai-rate-limit 插件实例（默认 60 秒窗口）
    pub fn new() -> Self {
        Self {
            limiter: Arc::new(MemoryRateLimiter::new(std::time::Duration::from_secs(60))),
        }
    }

    /// 使用自定义限流器创建插件（用于测试）
    pub fn with_limiter(limiter: Arc<dyn RateLimiter>) -> Self {
        Self { limiter }
    }
}

impl Default for AiRateLimitPlugin {
    fn default() -> Self {
        Self::new()
    }
}

// ============ PluginHandler 实现 ============

#[async_trait]
impl PluginHandler for AiRateLimitPlugin {
    fn name(&self) -> &str {
        "ai-rate-limit"
    }

    fn priority(&self) -> i32 {
        // 高于 ai-proxy (770)，先执行限流检查
        771
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let cfg: AiRateLimitConfig = crate::parse_plugin_config(config)?;

        // 1. 根据 limit_by 确定限流键
        let rate_key = match cfg.limit_by.as_str() {
            "global" => "global".to_string(),
            "route" => format!(
                "route:{}",
                ctx.route_id
                    .map(|id| id.to_string())
                    .unwrap_or_default()
            ),
            "consumer" => format!(
                "consumer:{}",
                ctx.consumer_id
                    .map(|id| id.to_string())
                    .unwrap_or_default()
            ),
            // virtual_key 将在 DAO 集成后实现，暂时 fallback 到 global
            _ => "global".to_string(),
        };

        // 2. 检查 RPM（原子 check-and-increment，避免 TOCTOU 竞态）
        if let Some(rpm_limit) = cfg.rpm_limit {
            let rpm_key = format!("{}:rpm", rate_key);
            let (allowed, current) = self.limiter.check_and_increment(&rpm_key, rpm_limit, 1);
            if !allowed {
                ctx.short_circuited = true;
                ctx.exit_status = Some(cfg.error_code);
                ctx.exit_body = Some(
                    serde_json::json!({
                        "message": cfg.error_message,
                        "current_rpm": current,
                        "limit": rpm_limit
                    })
                    .to_string(),
                );
                return Ok(());
            }
        }

        // 3. 检查 TPM（预扣估算值，原子 check-and-increment）
        if let Some(tpm_limit) = cfg.tpm_limit {
            // 预扣：优先用 TokenizerRegistry 精确计算(双轨/HF/远端 API);
            // registry 未注册或 body 缺失时降级到字符估算保持向后兼容
            // Pre-debit: prefer TokenizerRegistry for accurate count;
            // fall back to byte/4 estimation when registry is absent or body missing
            let estimated = compute_estimated_prompt_tokens(ctx).await;

            let tpm_key = format!("{}:tpm", rate_key);
            let (allowed, current) = self.limiter.check_and_increment(&tpm_key, tpm_limit, estimated);
            if !allowed {
                ctx.short_circuited = true;
                ctx.exit_status = Some(cfg.error_code);
                ctx.exit_body = Some(
                    serde_json::json!({
                        "message": cfg.error_message,
                        "current_tpm": current,
                        "limit": tpm_limit
                    })
                    .to_string(),
                );
                return Ok(());
            }

            // 存储预扣信息到 extensions，供 log 阶段修正
            ctx.extensions.insert(AiRateLimitContext {
                rate_key: rate_key.clone(),
                estimated_prompt_tokens: estimated,
            });
        }

        Ok(())
    }

    async fn log(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        // TPM 修正：根据实际 token 消耗量修正预扣值
        let rl_ctx = ctx.extensions.get::<AiRateLimitContext>();
        let ai_state = ctx.extensions.get::<AiRequestState>();

        if let (Some(rl_ctx), Some(ai_state)) = (rl_ctx, ai_state) {
            let cfg: AiRateLimitConfig = crate::parse_plugin_config(config)?;
            if cfg.tpm_limit.is_some() {
                let actual = ai_state.usage.total_tokens.unwrap_or(0);
                let estimated = rl_ctx.estimated_prompt_tokens;
                let tpm_key = format!("{}:tpm", rl_ctx.rate_key);
                if actual > estimated {
                    // 实际消耗 > 预扣：补扣差额
                    self.limiter.increment(&tpm_key, actual - estimated);
                } else if estimated > actual {
                    // 预扣 > 实际消耗：退还多扣的部分
                    self.limiter.decrement(&tpm_key, estimated - actual);
                }
            }
        }
        Ok(())
    }
}

// ============ 辅助函数 / Helpers ============

/// 计算 prompt token 预扣值
/// Compute the prompt-token pre-debit value.
///
/// 优先级 / Priority:
/// 1. 上游已写入的 `AiRequestState.estimated_prompt_tokens`(ai-proxy 在更晚的 priority 执行,
///    本插件 priority=771 早于 ai-proxy 770,通常拿不到 — 仅当 priority 顺序被外部调换时生效)
///    Honor pre-existing AiRequestState (only present if priority order has been swapped externally)
/// 2. 全局 TokenizerRegistry → count_prompt_from_body(自己解析 body,启发式推断 provider)
///    Global TokenizerRegistry path
/// 3. 字符估算(byte_len / 4)— 与历史行为兼容
///    Char estimation fallback — preserves historical behavior
async fn compute_estimated_prompt_tokens(ctx: &kong_core::traits::RequestCtx) -> u64 {
    // 1. 上游可能已经把精确值写入 AiRequestState
    if let Some(state) = ctx.extensions.get::<AiRequestState>() {
        if state.estimated_prompt_tokens > 0 {
            return state.estimated_prompt_tokens;
        }
    }

    let body = match ctx.request_body.as_deref() {
        Some(b) if !b.is_empty() => b,
        _ => return 0,
    };

    // 2. registry 路径
    if let Some(registry) = global_registry() {
        // 从 body 中嗅出 model 名以决定 provider strategy(失败则空字符串)
        let model_name = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| {
                v.get("model")
                    .and_then(|m| m.as_str().map(|s| s.to_string()))
            })
            .unwrap_or_default();
        let provider_type = TokenizerRegistry::infer_provider_type(&model_name);
        return registry
            .count_prompt_from_body(provider_type, &model_name, body)
            .await;
    }

    // 3. 历史 byte/4 估算兜底
    TokenCounter::count_estimate(body)
}

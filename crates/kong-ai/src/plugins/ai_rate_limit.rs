//! ai-rate-limit 插件 — RPM/TPM 内存限流 + 预扣修正

use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;

use kong_core::error::Result;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

use crate::plugins::context::AiRequestState;
use crate::ratelimit::RateLimiter;
use crate::ratelimit::memory::MemoryRateLimiter;

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

        // 2. 检查 RPM
        if let Some(rpm_limit) = cfg.rpm_limit {
            let rpm_key = format!("{}:rpm", rate_key);
            let (allowed, current) = self.limiter.check(&rpm_key, rpm_limit);
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
            self.limiter.increment(&rpm_key, 1);
        }

        // 3. 检查 TPM（预扣估算值）
        if let Some(tpm_limit) = cfg.tpm_limit {
            let tpm_key = format!("{}:tpm", rate_key);
            let (allowed, current) = self.limiter.check(&tpm_key, tpm_limit);
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

            // 预扣：用请求体长度 / 4 估算 prompt tokens
            let estimated = ctx
                .request_body
                .as_ref()
                .map(|b| ((b.len() as u64) + 3) / 4)
                .unwrap_or(0);
            self.limiter.increment(&tpm_key, estimated);

            // 存储预扣信息到 extensions，供 log 阶段修正
            ctx.extensions.insert(AiRateLimitContext {
                rate_key: rate_key.clone(),
                estimated_prompt_tokens: estimated,
            });
        }

        Ok(())
    }

    async fn log(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        // TPM 修正：actual_tokens - estimated
        let rl_ctx = ctx.extensions.get::<AiRateLimitContext>();
        let ai_state = ctx.extensions.get::<AiRequestState>();

        if let (Some(rl_ctx), Some(ai_state)) = (rl_ctx, ai_state) {
            let cfg: AiRateLimitConfig = crate::parse_plugin_config(config)?;
            if cfg.tpm_limit.is_some() {
                let actual = ai_state.usage.total_tokens.unwrap_or(0);
                let estimated = rl_ctx.estimated_prompt_tokens;
                if actual > estimated {
                    let tpm_key = format!("{}:tpm", rl_ctx.rate_key);
                    self.limiter.increment(&tpm_key, actual - estimated);
                }
            }
        }
        Ok(())
    }
}

//! `ai-context-compression` 策略插件。
//!
//! 插件只声明策略并观察结果；Provider 已选定后的 Headroom route 由 `ai-proxy`
//! 应用，从而保持认证、安全检查、缓存键和配额顺序不变。

use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use kong_core::error::Result;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

use crate::context_compression::CompressionBackendDescriptor;

pub const DEFAULT_MIN_INPUT_TOKENS: u64 = 2_000;
pub const DEFAULT_MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

/// Headroom 在派发前不可用时的策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailablePolicy {
    PassThrough,
    Reject,
}

/// 首版流式请求固定旁路，避免把未拦截的 CCR tool call 暴露给客户端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamingPolicy {
    Bypass,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiContextCompressionConfig {
    pub min_input_tokens: u64,
    pub max_input_bytes: usize,
    pub on_unavailable: UnavailablePolicy,
    pub streaming: StreamingPolicy,
    pub expose_metrics_headers: bool,
}

impl Default for AiContextCompressionConfig {
    fn default() -> Self {
        Self {
            min_input_tokens: DEFAULT_MIN_INPUT_TOKENS,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            on_unavailable: UnavailablePolicy::PassThrough,
            streaming: StreamingPolicy::Bypass,
            expose_metrics_headers: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompressionStatus {
    Pending,
    Applied,
    Bypassed,
    Degraded,
    Rejected,
}

impl ContextCompressionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Bypassed => "bypassed",
            Self::Degraded => "degraded",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompressionReason {
    Pending,
    Applied,
    BelowThreshold,
    BodyTooLarge,
    Streaming,
    UnsupportedProvider,
    UnsupportedProtocol,
    ToolChoiceUnsupported,
    UnsupportedPath,
    BackendNotConfigured,
    BackendUnhealthy,
    MetricsInvalid,
    CompressionFailed,
}

impl ContextCompressionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::BelowThreshold => "below_threshold",
            Self::BodyTooLarge => "body_too_large",
            Self::Streaming => "streaming",
            Self::UnsupportedProvider => "unsupported_provider",
            Self::UnsupportedProtocol => "unsupported_protocol",
            Self::ToolChoiceUnsupported => "tool_choice_unsupported",
            Self::UnsupportedPath => "unsupported_path",
            Self::BackendNotConfigured => "backend_not_configured",
            Self::BackendUnhealthy => "backend_unhealthy",
            Self::MetricsInvalid => "metrics_invalid",
            Self::CompressionFailed => "compression_failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContextCompressionOutcome {
    pub status: ContextCompressionStatus,
    pub reason: ContextCompressionReason,
    pub backend: Option<&'static str>,
    pub ccr: bool,
    pub tokens_before: Option<u64>,
    pub tokens_after: Option<u64>,
    pub tokens_saved: Option<u64>,
    pub hop_started_at: Option<Instant>,
    pub hop_latency_ms: Option<u64>,
}

impl Default for ContextCompressionOutcome {
    fn default() -> Self {
        Self {
            status: ContextCompressionStatus::Pending,
            reason: ContextCompressionReason::Pending,
            backend: None,
            ccr: false,
            tokens_before: None,
            tokens_after: None,
            tokens_saved: None,
            hop_started_at: None,
            hop_latency_ms: None,
        }
    }
}

/// 请求级策略和观测状态。
#[derive(Debug, Clone)]
pub struct ContextCompressionContext {
    pub policy: AiContextCompressionConfig,
    pub outcome: ContextCompressionOutcome,
}

impl ContextCompressionContext {
    pub fn new(policy: AiContextCompressionConfig) -> Self {
        Self {
            policy,
            outcome: ContextCompressionOutcome::default(),
        }
    }

    pub fn bypass(&mut self, reason: ContextCompressionReason) {
        self.outcome.status = ContextCompressionStatus::Bypassed;
        self.outcome.reason = reason;
    }

    pub fn reject(&mut self, reason: ContextCompressionReason) {
        self.outcome.status = ContextCompressionStatus::Rejected;
        self.outcome.reason = reason;
    }

    pub fn apply(&mut self, descriptor: CompressionBackendDescriptor) {
        self.outcome.status = ContextCompressionStatus::Applied;
        self.outcome.reason = ContextCompressionReason::Applied;
        self.outcome.backend = Some(descriptor.backend);
        self.outcome.ccr = descriptor.transparent_ccr;
        self.outcome.hop_started_at = Some(Instant::now());
    }
}

pub struct AiContextCompressionPlugin;

impl AiContextCompressionPlugin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AiContextCompressionPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PluginHandler for AiContextCompressionPlugin {
    fn name(&self) -> &str {
        "ai-context-compression"
    }

    fn priority(&self) -> i32 {
        // 在 ai-proxy(769) 之前保存策略；既有 ai-rate-limit(771) 仍先按原文准入。
        770
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let policy: AiContextCompressionConfig = crate::parse_plugin_config(config)?;
        ctx.extensions
            .insert(ContextCompressionContext::new(policy));
        Ok(())
    }

    async fn header_filter(&self, _config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let internal_headers = ctx
            .response_headers
            .keys()
            .filter(|name| name.to_ascii_lowercase().starts_with("x-headroom-"))
            .cloned()
            .collect::<Vec<_>>();
        for name in internal_headers {
            ctx.response_headers_to_remove.push(name);
        }

        let response_headers = ctx.response_headers.clone();
        let Some(compression) = ctx.extensions.get_mut::<ContextCompressionContext>() else {
            return Ok(());
        };

        if let Some(started_at) = compression.outcome.hop_started_at {
            compression.outcome.hop_latency_ms =
                Some(started_at.elapsed().as_millis().min(u64::MAX as u128) as u64);
        }

        if compression.outcome.status == ContextCompressionStatus::Applied {
            observe_headroom_metrics(&response_headers, &mut compression.outcome);
        }

        let expose = compression.policy.expose_metrics_headers;
        let status = compression.outcome.status.as_str().to_string();
        let metrics = (
            compression.outcome.tokens_before,
            compression.outcome.tokens_after,
            compression.outcome.tokens_saved,
        );
        if expose {
            ctx.response_headers_to_set
                .push(("x-kong-ai-context-compression".to_string(), status));
            if let (Some(before), Some(after), Some(saved)) = metrics {
                ctx.response_headers_to_set
                    .push(("x-kong-ai-tokens-before".to_string(), before.to_string()));
                ctx.response_headers_to_set
                    .push(("x-kong-ai-tokens-after".to_string(), after.to_string()));
                ctx.response_headers_to_set
                    .push(("x-kong-ai-tokens-saved".to_string(), saved.to_string()));
            }
        }
        Ok(())
    }

    async fn log(&self, _config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let Some(compression) = ctx.extensions.get::<ContextCompressionContext>() else {
            return Ok(());
        };
        let ratio = match (
            compression.outcome.tokens_before,
            compression.outcome.tokens_saved,
        ) {
            (Some(before), Some(saved)) if before > 0 => Some(saved as f64 / before as f64),
            _ => None,
        };
        let value = serde_json::json!({
            "status": compression.outcome.status,
            "reason": compression.outcome.reason,
            "backend": compression.outcome.backend,
            "ccr": compression.outcome.ccr,
            "tokens_before": compression.outcome.tokens_before,
            "tokens_after": compression.outcome.tokens_after,
            "tokens_saved": compression.outcome.tokens_saved,
            "compression_ratio": ratio,
            "hop_latency_ms": compression.outcome.hop_latency_ms,
        });
        merge_ai_log_field(ctx, "context_compression", value);
        Ok(())
    }
}

fn observe_headroom_metrics(
    headers: &std::collections::HashMap<String, String>,
    outcome: &mut ContextCompressionOutcome,
) {
    let failed = get_header(headers, "x-headroom-compression-failed")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));
    if failed {
        outcome.status = ContextCompressionStatus::Degraded;
        outcome.reason = ContextCompressionReason::CompressionFailed;
    }

    let parsed = (
        parse_u64_header(headers, "x-headroom-tokens-before"),
        parse_u64_header(headers, "x-headroom-tokens-after"),
        parse_u64_header(headers, "x-headroom-tokens-saved"),
    );
    match parsed {
        (Some(before), Some(after), Some(saved))
            if after <= before && saved <= before && before - after == saved =>
        {
            outcome.tokens_before = Some(before);
            outcome.tokens_after = Some(after);
            outcome.tokens_saved = Some(saved);
        }
        _ if !failed => {
            outcome.reason = ContextCompressionReason::MetricsInvalid;
        }
        _ => {}
    }
}

fn parse_u64_header(
    headers: &std::collections::HashMap<String, String>,
    name: &str,
) -> Option<u64> {
    get_header(headers, name)?
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|value| *value <= i64::MAX as u64)
}

fn get_header<'a>(
    headers: &'a std::collections::HashMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn merge_ai_log_field(ctx: &mut RequestCtx, field: &str, value: serde_json::Value) {
    if !ctx
        .log_serialize
        .as_ref()
        .is_some_and(serde_json::Value::is_object)
    {
        ctx.log_serialize = Some(serde_json::json!({}));
    }
    let root = ctx
        .log_serialize
        .as_mut()
        .and_then(serde_json::Value::as_object_mut)
        .expect("log_serialize was normalized to an object");
    if !root.get("ai").is_some_and(serde_json::Value::is_object) {
        root.insert("ai".to_string(), serde_json::json!({}));
    }
    root.get_mut("ai")
        .and_then(serde_json::Value::as_object_mut)
        .expect("ai log field was normalized to an object")
        .insert(field.to_string(), value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use kong_core::traits::PluginHandler;
    use serde_json::json;

    fn plugin_config(value: serde_json::Value) -> PluginConfig {
        PluginConfig {
            name: "ai-context-compression".to_string(),
            config: value,
        }
    }

    #[tokio::test]
    async fn access_applies_defaults() {
        let mut ctx = RequestCtx::new();
        AiContextCompressionPlugin::new()
            .access(&plugin_config(json!({})), &mut ctx)
            .await
            .unwrap();
        let policy = &ctx
            .extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .policy;
        assert_eq!(policy.min_input_tokens, DEFAULT_MIN_INPUT_TOKENS);
        assert_eq!(policy.max_input_bytes, DEFAULT_MAX_INPUT_BYTES);
        assert_eq!(policy.on_unavailable, UnavailablePolicy::PassThrough);
    }

    #[tokio::test]
    async fn header_filter_validates_metrics_and_strips_internal_headers() {
        let plugin = AiContextCompressionPlugin::new();
        let config = plugin_config(json!({"expose_metrics_headers": true}));
        let mut ctx = RequestCtx::new();
        plugin.access(&config, &mut ctx).await.unwrap();
        ctx.extensions
            .get_mut::<ContextCompressionContext>()
            .unwrap()
            .apply(CompressionBackendDescriptor {
                backend: "headroom_proxy",
                transparent_ccr: true,
                streaming: false,
                store_scope: crate::context_compression::CompressionStoreScope::Local,
            });
        ctx.response_headers
            .insert("x-headroom-tokens-before".to_string(), "100".to_string());
        ctx.response_headers
            .insert("x-headroom-tokens-after".to_string(), "40".to_string());
        ctx.response_headers
            .insert("x-headroom-tokens-saved".to_string(), "60".to_string());

        plugin.header_filter(&config, &mut ctx).await.unwrap();
        let outcome = &ctx
            .extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .outcome;
        assert_eq!(outcome.tokens_saved, Some(60));
        assert_eq!(outcome.status, ContextCompressionStatus::Applied);
        assert!(ctx
            .response_headers_to_remove
            .contains(&"x-headroom-tokens-saved".to_string()));
        assert!(ctx
            .response_headers_to_set
            .contains(&("x-kong-ai-tokens-saved".to_string(), "60".to_string())));
    }

    #[tokio::test]
    async fn invalid_metrics_do_not_break_response() {
        let plugin = AiContextCompressionPlugin::new();
        let config = plugin_config(json!({}));
        let mut ctx = RequestCtx::new();
        plugin.access(&config, &mut ctx).await.unwrap();
        ctx.extensions
            .get_mut::<ContextCompressionContext>()
            .unwrap()
            .outcome
            .status = ContextCompressionStatus::Applied;
        ctx.response_headers
            .insert("x-headroom-tokens-before".to_string(), "10".to_string());
        ctx.response_headers
            .insert("x-headroom-tokens-after".to_string(), "20".to_string());
        ctx.response_headers
            .insert("x-headroom-tokens-saved".to_string(), "0".to_string());
        plugin.header_filter(&config, &mut ctx).await.unwrap();
        let outcome = &ctx
            .extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .outcome;
        assert_eq!(outcome.tokens_before, None);
        assert_eq!(outcome.reason, ContextCompressionReason::MetricsInvalid);
    }

    #[tokio::test]
    async fn metrics_outside_the_persistable_range_are_not_trusted() {
        let plugin = AiContextCompressionPlugin::new();
        let config = plugin_config(json!({}));
        let mut ctx = RequestCtx::new();
        plugin.access(&config, &mut ctx).await.unwrap();
        ctx.extensions
            .get_mut::<ContextCompressionContext>()
            .unwrap()
            .outcome
            .status = ContextCompressionStatus::Applied;
        ctx.response_headers
            .insert("x-headroom-tokens-before".to_string(), u64::MAX.to_string());
        ctx.response_headers
            .insert("x-headroom-tokens-after".to_string(), "0".to_string());
        ctx.response_headers
            .insert("x-headroom-tokens-saved".to_string(), u64::MAX.to_string());

        plugin.header_filter(&config, &mut ctx).await.unwrap();
        let outcome = &ctx
            .extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .outcome;
        assert_eq!(outcome.tokens_before, None);
        assert_eq!(outcome.reason, ContextCompressionReason::MetricsInvalid);
    }
}

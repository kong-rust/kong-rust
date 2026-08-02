//! `ai-context-compression` 与 `ai-proxy` 的请求级 contract 测试。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use kong_ai::context_compression::{
    CompressionBackendDescriptor, CompressionBackendError, CompressionBodyTransform,
    CompressionProtocol, CompressionRoute, CompressionStoreScope, ContextCompressionBackend,
    ProviderTarget,
};
use kong_ai::plugins::ai_context_compression::{
    AiContextCompressionPlugin, ContextCompressionContext, ContextCompressionReason,
    ContextCompressionStatus,
};
use kong_ai::plugins::AiProxyPlugin;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};
use serde_json::json;

struct StaticBackend {
    calls: AtomicUsize,
    protocol: CompressionProtocol,
    result: Result<CompressionRoute, CompressionBackendError>,
}

impl StaticBackend {
    fn healthy() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            protocol: CompressionProtocol::OpenAiResponses,
            result: Ok(CompressionRoute {
                scheme: "http".to_string(),
                host: "127.0.0.1".to_string(),
                port: 8787,
                path: "/v1/responses".to_string(),
                control_headers: vec![
                    (
                        "x-headroom-base-url".to_string(),
                        "http://provider.internal:9000".to_string(),
                    ),
                    ("x-headroom-stack".to_string(), "kong-rust".to_string()),
                ],
                body_transform: Some(CompressionBodyTransform::InjectOpenAiResponsesCcrTool),
            }),
        }
    }

    fn healthy_anthropic() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            protocol: CompressionProtocol::AnthropicMessages,
            result: Ok(CompressionRoute {
                scheme: "http".to_string(),
                host: "127.0.0.1".to_string(),
                port: 8787,
                path: "/v1/messages".to_string(),
                control_headers: vec![
                    (
                        "x-headroom-base-url".to_string(),
                        "http://provider.internal:9000/v1".to_string(),
                    ),
                    ("x-headroom-stack".to_string(), "kong-rust".to_string()),
                ],
                body_transform: None,
            }),
        }
    }

    fn failing(error: CompressionBackendError) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            protocol: CompressionProtocol::OpenAiResponses,
            result: Err(error),
        }
    }
}

#[async_trait]
impl ContextCompressionBackend for StaticBackend {
    fn descriptor(&self) -> CompressionBackendDescriptor {
        CompressionBackendDescriptor {
            backend: "headroom_proxy",
            transparent_ccr: true,
            streaming: false,
            store_scope: CompressionStoreScope::Local,
        }
    }

    async fn prepare_route(
        &self,
        protocol: CompressionProtocol,
        _provider: ProviderTarget,
    ) -> Result<CompressionRoute, CompressionBackendError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if protocol != self.protocol {
            return Err(CompressionBackendError::UnsupportedProtocol);
        }
        self.result.clone()
    }
}

fn compression_config(overrides: serde_json::Value) -> PluginConfig {
    let mut config = json!({
        "min_input_tokens": 0,
        "max_input_bytes": 4194304,
        "on_unavailable": "pass_through",
        "streaming": "bypass",
        "expose_metrics_headers": false
    });
    if let (Some(target), Some(source)) = (config.as_object_mut(), overrides.as_object()) {
        for (name, value) in source {
            target.insert(name.clone(), value.clone());
        }
    }
    PluginConfig {
        name: "ai-context-compression".to_string(),
        config,
    }
}

fn proxy_config(streaming: bool, client_protocol: &str) -> PluginConfig {
    PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4o",
            "model_source": "config",
            "route_type": "llm/v1/chat",
            "client_protocol": client_protocol,
            "response_streaming": if streaming { "always" } else { "deny" },
            "provider": {
                "provider_type": "openai",
                "endpoint_url": "http://provider.internal:9000/v1/chat/completions",
                "auth_config": {"header_value": "provider-secret"}
            }
        }),
    }
}

fn responses_proxy_config(streaming: bool) -> PluginConfig {
    PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-5",
            "model_source": "config",
            "route_type": "llm/v1/responses",
            "client_protocol": "openai",
            "response_streaming": if streaming { "always" } else { "deny" },
            "provider": {
                "provider_type": "openai",
                "endpoint_url": "http://provider.internal:9000/v1/responses",
                "auth_config": {"header_value": "provider-secret"}
            }
        }),
    }
}

fn anthropic_proxy_config(streaming: bool) -> PluginConfig {
    PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "claude-contract",
            "model_source": "config",
            "route_type": "llm/v1/chat",
            "client_protocol": "anthropic",
            "response_streaming": if streaming { "always" } else { "deny" },
            "provider": {
                "provider_type": "anthropic",
                "endpoint_url": "http://provider.internal:9000/v1/messages",
                "auth_config": {"header_value": "provider-secret"}
            }
        }),
    }
}

fn gemini_proxy_config() -> PluginConfig {
    PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gemini-2.5-flash",
            "model_source": "config",
            "route_type": "llm/v1/chat",
            "client_protocol": "openai",
            "response_streaming": "deny",
            "provider": {
                "provider_type": "gemini",
                "auth_config": {"header_value": "provider-secret"}
            }
        }),
    }
}

fn responses_request_body(tool_choice: Option<serde_json::Value>) -> String {
    let mut body = json!({
        "model": "gpt-5",
        "input": "hello",
        "tools": [
            {
                "type": "function",
                "name": "existing_tool",
                "description": "existing",
                "parameters": {"type": "object", "properties": {}}
            },
            {
                "type": "function",
                "name": "headroom_retrieve",
                "description": "untrusted client definition",
                "parameters": {"type": "object", "properties": {}}
            }
        ]
    });
    if let Some(tool_choice) = tool_choice {
        body["tool_choice"] = tool_choice;
    }
    body.to_string()
}

fn request_body(client_protocol: &str) -> String {
    if client_protocol == "anthropic" {
        json!({
            "model": "gpt-4o",
            "max_tokens": 128,
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string()
    } else {
        json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string()
    }
}

#[tokio::test]
async fn eligible_request_routes_through_headroom_and_overwrites_client_controls() {
    let backend = Arc::new(StaticBackend::healthy());
    let proxy = AiProxyPlugin::new().with_context_compression_backend(backend.clone());
    let compression = AiContextCompressionPlugin::new();
    let compression_config = compression_config(json!({}));
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(responses_request_body(None));
    ctx.request_headers.insert(
        "x-headroom-base-url".to_string(),
        "http://attacker.invalid".to_string(),
    );
    ctx.request_headers
        .insert("x-headroom-bypass".to_string(), "true".to_string());

    compression
        .access(&compression_config, &mut ctx)
        .await
        .unwrap();
    proxy
        .access(&responses_proxy_config(false), &mut ctx)
        .await
        .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert_eq!(ctx.upstream_target_host.as_deref(), Some("127.0.0.1"));
    assert_eq!(ctx.upstream_target_port, Some(8787));
    assert_eq!(ctx.upstream_path.as_deref(), Some("/v1/responses"));
    assert!(ctx.upstream_headers_to_set.contains(&(
        "x-headroom-base-url".to_string(),
        "http://provider.internal:9000".to_string()
    )));
    assert!(!ctx
        .upstream_headers_to_set
        .iter()
        .any(|(_, value)| value == "http://attacker.invalid"));
    assert!(ctx
        .upstream_headers_to_remove
        .iter()
        .any(|name| name.eq_ignore_ascii_case("x-headroom-bypass")));
    assert!(ctx.upstream_headers_to_set.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("authorization") && value == "Bearer provider-secret"
    }));
    let forwarded: serde_json::Value =
        serde_json::from_str(ctx.upstream_body.as_deref().unwrap()).unwrap();
    let retrieve_tools = forwarded["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tool| tool["name"] == "headroom_retrieve")
        .collect::<Vec<_>>();
    assert_eq!(retrieve_tools.len(), 1);
    assert_eq!(retrieve_tools[0]["strict"], true);
    assert_ne!(
        retrieve_tools[0]["description"],
        "untrusted client definition"
    );
    let outcome = &ctx
        .extensions
        .get::<ContextCompressionContext>()
        .unwrap()
        .outcome;
    assert_eq!(outcome.status, ContextCompressionStatus::Applied);
    assert!(outcome.ccr);
}

#[tokio::test]
async fn anthropic_native_wire_preserves_tools_results_and_extension_fields() {
    let backend = Arc::new(StaticBackend::healthy_anthropic());
    let proxy = AiProxyPlugin::new().with_context_compression_backend(backend.clone());
    let compression = AiContextCompressionPlugin::new();
    let original = json!({
        "model": "client-model",
        "max_tokens": 256,
        "system": [{"type": "text", "text": "system contract"}],
        "messages": [
            {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call_existing_contract",
                    "name": "existing_tool",
                    "input": {"path": "/tmp/contract"}
                }]
            },
            {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call_existing_contract",
                    "content": "CCR_ORIGINAL_SENTINEL"
                }]
            }
        ],
        "tools": [{
            "name": "existing_tool",
            "description": "existing",
            "input_schema": {"type": "object", "properties": {}}
        }],
        "metadata": {"contract_output_schema": {"type": "object"}}
    });
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(original.to_string());
    compression
        .access(&compression_config(json!({})), &mut ctx)
        .await
        .unwrap();

    proxy
        .access(&anthropic_proxy_config(false), &mut ctx)
        .await
        .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert_eq!(ctx.upstream_target_host.as_deref(), Some("127.0.0.1"));
    assert_eq!(ctx.upstream_path.as_deref(), Some("/v1/messages"));
    let forwarded: serde_json::Value =
        serde_json::from_str(ctx.upstream_body.as_deref().unwrap()).unwrap();
    assert_eq!(forwarded["model"], "claude-contract");
    assert_eq!(forwarded["tools"], original["tools"]);
    assert_eq!(forwarded["messages"], original["messages"]);
    assert_eq!(forwarded["system"], original["system"]);
    assert_eq!(forwarded["metadata"], original["metadata"]);
    assert_eq!(forwarded["stream"], false);
    assert!(ctx.upstream_headers_to_set.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("x-api-key") && value == "provider-secret"
    }));
    assert_eq!(
        ctx.extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .outcome
            .status,
        ContextCompressionStatus::Applied
    );
}

#[tokio::test]
async fn responses_tool_choice_none_bypasses_without_mutating_the_body() {
    let backend = Arc::new(StaticBackend::healthy());
    let proxy = AiProxyPlugin::new().with_context_compression_backend(backend.clone());
    let compression = AiContextCompressionPlugin::new();
    let original = responses_request_body(Some(json!("none")));
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(original.clone());
    compression
        .access(&compression_config(json!({})), &mut ctx)
        .await
        .unwrap();

    proxy
        .access(&responses_proxy_config(false), &mut ctx)
        .await
        .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        ctx.upstream_target_host.as_deref(),
        Some("provider.internal")
    );
    assert!(ctx
        .upstream_body
        .as_deref()
        .is_some_and(|body| body.contains("untrusted client definition")));
    let outcome = &ctx
        .extensions
        .get::<ContextCompressionContext>()
        .unwrap()
        .outcome;
    assert_eq!(outcome.status, ContextCompressionStatus::Bypassed);
    assert_eq!(
        outcome.reason,
        ContextCompressionReason::ToolChoiceUnsupported
    );
}

#[tokio::test]
async fn responses_tool_choice_for_an_unrelated_tool_bypasses_without_mutation() {
    let backend = Arc::new(StaticBackend::healthy());
    let proxy = AiProxyPlugin::new().with_context_compression_backend(backend.clone());
    let compression = AiContextCompressionPlugin::new();
    let original = responses_request_body(Some(json!({
        "type": "function",
        "name": "existing_tool"
    })));
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(original);
    compression
        .access(&compression_config(json!({})), &mut ctx)
        .await
        .unwrap();

    proxy
        .access(&responses_proxy_config(false), &mut ctx)
        .await
        .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        ctx.upstream_target_host.as_deref(),
        Some("provider.internal")
    );
    assert!(ctx
        .upstream_body
        .as_deref()
        .is_some_and(|body| body.contains("untrusted client definition")));
    assert_eq!(
        ctx.extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .outcome
            .reason,
        ContextCompressionReason::ToolChoiceUnsupported
    );
}

#[tokio::test]
async fn openai_chat_bypasses_when_backend_cannot_guarantee_ccr_continuation() {
    let backend = Arc::new(StaticBackend::healthy());
    let proxy = AiProxyPlugin::new().with_context_compression_backend(backend.clone());
    let compression = AiContextCompressionPlugin::new();
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(request_body("openai"));
    compression
        .access(&compression_config(json!({})), &mut ctx)
        .await
        .unwrap();

    proxy
        .access(&proxy_config(false, "openai"), &mut ctx)
        .await
        .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        ctx.upstream_target_host.as_deref(),
        Some("provider.internal")
    );
    assert_eq!(
        ctx.extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .outcome
            .reason,
        ContextCompressionReason::UnsupportedProtocol
    );
}

#[tokio::test]
async fn streaming_request_bypasses_without_calling_backend() {
    let backend = Arc::new(StaticBackend::healthy());
    let proxy = AiProxyPlugin::new().with_context_compression_backend(backend.clone());
    let compression = AiContextCompressionPlugin::new();
    let compression_config = compression_config(json!({}));
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(request_body("openai"));
    compression
        .access(&compression_config, &mut ctx)
        .await
        .unwrap();
    proxy
        .access(&proxy_config(true, "openai"), &mut ctx)
        .await
        .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        ctx.upstream_target_host.as_deref(),
        Some("provider.internal")
    );
    let outcome = &ctx
        .extensions
        .get::<ContextCompressionContext>()
        .unwrap()
        .outcome;
    assert_eq!(outcome.status, ContextCompressionStatus::Bypassed);
    assert_eq!(outcome.reason, ContextCompressionReason::Streaming);
}

#[tokio::test]
async fn oversized_request_bypasses_without_calling_backend_or_mutating_payload() {
    let backend = Arc::new(StaticBackend::healthy());
    let proxy = AiProxyPlugin::new().with_context_compression_backend(backend.clone());
    let compression = AiContextCompressionPlugin::new();
    let original = responses_request_body(None);
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(original.clone());
    compression
        .access(&compression_config(json!({"max_input_bytes": 1})), &mut ctx)
        .await
        .unwrap();
    proxy
        .access(&responses_proxy_config(false), &mut ctx)
        .await
        .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    let forwarded: serde_json::Value =
        serde_json::from_str(ctx.upstream_body.as_deref().unwrap()).unwrap();
    let original: serde_json::Value = serde_json::from_str(&original).unwrap();
    assert_eq!(forwarded, original);
    assert_eq!(
        ctx.extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .outcome
            .reason,
        ContextCompressionReason::BodyTooLarge
    );
}

#[tokio::test]
async fn gemini_request_bypasses_without_calling_backend() {
    let backend = Arc::new(StaticBackend::healthy());
    let proxy = AiProxyPlugin::new().with_context_compression_backend(backend.clone());
    let compression = AiContextCompressionPlugin::new();
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(request_body("openai"));
    compression
        .access(&compression_config(json!({})), &mut ctx)
        .await
        .unwrap();
    proxy
        .access(&gemini_proxy_config(), &mut ctx)
        .await
        .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        ctx.extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .outcome
            .reason,
        ContextCompressionReason::UnsupportedProvider
    );
}

#[tokio::test]
async fn missing_backend_honors_pass_through_and_reject_policies() {
    let compression = AiContextCompressionPlugin::new();

    let mut pass_through = RequestCtx::new();
    pass_through.request_body = Some(request_body("openai"));
    compression
        .access(&compression_config(json!({})), &mut pass_through)
        .await
        .unwrap();
    AiProxyPlugin::new()
        .access(&proxy_config(false, "openai"), &mut pass_through)
        .await
        .unwrap();
    assert!(!pass_through.short_circuited);
    assert_eq!(
        pass_through
            .extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .outcome
            .reason,
        ContextCompressionReason::BackendNotConfigured
    );

    let mut rejected = RequestCtx::new();
    rejected.request_body = Some(request_body("anthropic"));
    compression
        .access(
            &compression_config(json!({"on_unavailable": "reject"})),
            &mut rejected,
        )
        .await
        .unwrap();
    AiProxyPlugin::new()
        .access(&proxy_config(false, "anthropic"), &mut rejected)
        .await
        .unwrap();
    assert!(rejected.short_circuited);
    assert_eq!(rejected.exit_status, Some(503));
    let error: serde_json::Value =
        serde_json::from_str(rejected.exit_body.as_deref().unwrap()).unwrap();
    assert_eq!(error["type"], "error");
    assert_eq!(error["error"]["code"], "context_compression_unavailable");
}

#[tokio::test]
async fn unhealthy_backend_honors_pass_through_and_reject_without_partial_route() {
    let compression = AiContextCompressionPlugin::new();

    for (policy, rejected) in [("pass_through", false), ("reject", true)] {
        let backend = Arc::new(StaticBackend::failing(CompressionBackendError::Unavailable));
        let proxy = AiProxyPlugin::new().with_context_compression_backend(backend.clone());
        let mut ctx = RequestCtx::new();
        ctx.request_body = Some(responses_request_body(None));
        compression
            .access(
                &compression_config(json!({"on_unavailable": policy})),
                &mut ctx,
            )
            .await
            .unwrap();
        proxy
            .access(&responses_proxy_config(false), &mut ctx)
            .await
            .unwrap();

        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert_eq!(ctx.short_circuited, rejected);
        assert_ne!(ctx.upstream_target_host.as_deref(), Some("127.0.0.1"));
        assert!(!ctx
            .upstream_headers_to_set
            .iter()
            .any(|(name, _)| { name.eq_ignore_ascii_case("x-headroom-base-url") }));
        let outcome = &ctx
            .extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .outcome;
        assert_eq!(outcome.reason, ContextCompressionReason::BackendUnhealthy);
        assert_eq!(
            outcome.status,
            if rejected {
                ContextCompressionStatus::Rejected
            } else {
                ContextCompressionStatus::Bypassed
            }
        );
    }
}

#[tokio::test]
async fn unsupported_target_bypasses_without_partial_route() {
    let backend = Arc::new(StaticBackend::failing(
        CompressionBackendError::UnsupportedTarget,
    ));
    let proxy = AiProxyPlugin::new().with_context_compression_backend(backend.clone());
    let compression = AiContextCompressionPlugin::new();
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(responses_request_body(None));
    compression
        .access(&compression_config(json!({})), &mut ctx)
        .await
        .unwrap();
    proxy
        .access(&responses_proxy_config(false), &mut ctx)
        .await
        .unwrap();

    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        ctx.upstream_target_host.as_deref(),
        Some("provider.internal")
    );
    assert!(!ctx
        .upstream_headers_to_set
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("x-headroom-base-url")));
    assert_eq!(
        ctx.extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .outcome
            .reason,
        ContextCompressionReason::UnsupportedPath
    );
}

#[tokio::test]
async fn malformed_request_keeps_the_existing_ai_proxy_error_contract() {
    let config = responses_proxy_config(false);
    let mut baseline = RequestCtx::new();
    baseline.request_body = Some("{not-json".to_string());
    let baseline_error = AiProxyPlugin::new()
        .access(&config, &mut baseline)
        .await
        .unwrap_err()
        .to_string();

    let mut compressed = RequestCtx::new();
    compressed.request_body = Some("{not-json".to_string());
    AiContextCompressionPlugin::new()
        .access(&compression_config(json!({})), &mut compressed)
        .await
        .unwrap();
    let compressed_error = AiProxyPlugin::new()
        .with_context_compression_backend(Arc::new(StaticBackend::healthy()))
        .access(&config, &mut compressed)
        .await
        .unwrap_err()
        .to_string();

    assert_eq!(compressed_error, baseline_error);
    assert!(!compressed.short_circuited);
}

#[tokio::test]
async fn threshold_bypass_keeps_original_provider_target() {
    let backend = Arc::new(StaticBackend::healthy());
    let proxy = AiProxyPlugin::new().with_context_compression_backend(backend.clone());
    let compression = AiContextCompressionPlugin::new();
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(request_body("openai"));
    compression
        .access(
            &compression_config(json!({"min_input_tokens": 1000000})),
            &mut ctx,
        )
        .await
        .unwrap();
    proxy
        .access(&proxy_config(false, "openai"), &mut ctx)
        .await
        .unwrap();
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        ctx.upstream_target_host.as_deref(),
        Some("provider.internal")
    );
    assert_eq!(
        ctx.extensions
            .get::<ContextCompressionContext>()
            .unwrap()
            .outcome
            .reason,
        ContextCompressionReason::BelowThreshold
    );
}

#[test]
fn plugin_priority_is_between_rate_limit_and_proxy() {
    assert_eq!(AiContextCompressionPlugin::new().priority(), 770);
    assert_eq!(AiProxyPlugin::new().priority(), 769);
}

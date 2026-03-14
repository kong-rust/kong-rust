use std::path::PathBuf;

use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};
use kong_lua_bridge::loader;

fn plugin_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("kong/plugins")
}

#[test]
fn test_ai_proxy_plugin_is_loadable_from_transplanted_copy() {
    let handlers = loader::load_lua_plugins(&[plugin_root()], &["ai-proxy".to_string()]).unwrap();

    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].name(), "ai-proxy");
    assert_eq!(handlers[0].priority(), 770);
    assert!(!handlers[0].version().is_empty());
}

#[test]
fn test_ai_proxy_schema_is_loadable_from_transplanted_copy() {
    let schema = loader::load_plugin_schema(&[plugin_root()], "ai-proxy").unwrap();

    assert_eq!(schema["name"], "ai-proxy");
    assert_eq!(schema["fields"][0]["protocols"]["type"], "set");
    assert_eq!(schema["fields"][1]["config"]["type"], "record");
}

#[tokio::test]
async fn test_ai_proxy_access_phase_runs_with_minimal_openai_config() {
    let mut handlers =
        loader::load_lua_plugins(&[plugin_root()], &["ai-proxy".to_string()]).unwrap();
    let handler = handlers.pop().expect("ai-proxy handler should exist");

    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: serde_json::json!({
            "route_type": "llm/v1/chat",
            "auth": {
                "header_name": "authorization",
                "header_value": "Bearer test-token",
                "allow_override": false
            },
            "model": {
                "provider": "openai",
                "name": "gpt-4o-mini"
            },
            "logging": {
                "log_statistics": false,
                "log_payloads": false
            },
            "llm_format": "openai",
            "max_request_body_size": 8192
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_method = "POST".to_string();
    ctx.request_path = "/v1/chat/completions".to_string();
    ctx.request_scheme = "https".to_string();
    ctx.request_host = "gateway.test".to_string();
    ctx.request_port = 443;
    ctx.request_headers
        .insert("content-type".to_string(), "application/json".to_string());
    ctx.request_body = Some(
        serde_json::json!({
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ]
        })
        .to_string(),
    );

    handler.access(&config, &mut ctx).await.unwrap();

    assert!(!ctx.short_circuited);
    assert!(ctx
        .upstream_headers_to_set
        .iter()
        .any(|(name, value)| { name == "Accept-Encoding" && value == "gzip, identity" }));
    assert!(ctx
        .upstream_headers_to_set
        .iter()
        .any(|(name, value)| { name == "authorization" && value == "Bearer test-token" }));
    assert_eq!(ctx.upstream_scheme.as_deref(), Some("https"));
    assert_eq!(ctx.upstream_target_host.as_deref(), Some("api.openai.com"));
    assert_eq!(ctx.upstream_target_port, Some(443));
    assert_eq!(ctx.upstream_path.as_deref(), Some("/v1/chat/completions"));
    assert_eq!(
        ctx.upstream_body.as_deref(),
        Some(
            r#"{"messages":[{"content":"hello","role":"user"}],"model":"gpt-4o-mini","stream":false}"#
        )
    );
}

#[tokio::test]
async fn test_ai_proxy_access_phase_rewrites_gemini_chat_requests() {
    let mut handlers =
        loader::load_lua_plugins(&[plugin_root()], &["ai-proxy".to_string()]).unwrap();
    let handler = handlers.pop().expect("ai-proxy handler should exist");

    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: serde_json::json!({
            "route_type": "llm/v1/chat",
            "auth": {
                "header_name": "X-goog-api-key",
                "header_value": "test-key",
                "allow_override": false
            },
            "model": {
                "provider": "gemini",
                "name": "gemini-flash-latest",
                "options": {
                    "anthropic_version": "2023-06-01",
                    "azure_api_version": "2023-05-15"
                }
            },
            "logging": {
                "log_statistics": false,
                "log_payloads": false
            },
            "llm_format": "openai",
            "max_request_body_size": 8192
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_method = "POST".to_string();
    ctx.request_path = "/v1/chat/completions".to_string();
    ctx.request_scheme = "https".to_string();
    ctx.request_host = "gateway.test".to_string();
    ctx.request_port = 443;
    ctx.request_headers
        .insert("content-type".to_string(), "application/json".to_string());
    ctx.request_body = Some(
        serde_json::json!({
            "model": "gemini-flash-latest",
            "messages": [
                {
                    "role": "user",
                    "content": "Hello"
                }
            ]
        })
        .to_string(),
    );

    handler.access(&config, &mut ctx).await.unwrap();

    assert!(!ctx.short_circuited);
    assert_eq!(
        ctx.upstream_target_host.as_deref(),
        Some("generativelanguage.googleapis.com")
    );
    assert_eq!(ctx.upstream_target_port, Some(443));
    assert_eq!(
        ctx.upstream_path.as_deref(),
        Some("/v1beta/models/gemini-flash-latest:generateContent")
    );
    assert!(ctx
        .upstream_headers_to_set
        .iter()
        .any(|(name, value)| { name == "X-goog-api-key" && value == "test-key" }));
    assert_eq!(
        ctx.upstream_body.as_deref(),
        Some(
            r#"{"contents":[{"parts":[{"text":"Hello"}],"role":"user"}],"generationConfig":{}}"#
        )
    );
}

//! 配置验证测试 — ai-proxy 各种配置组合的解析和行为

mod helpers;

use kong_ai::plugins::ai_proxy::AiProxyPlugin;
use kong_ai::plugins::context::{AiRequestState, ClientProtocol};
use kong_ai::provider::DriverRegistry;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};
use serde_json::json;

use helpers::MockLlmServer;

fn make_chat_body() -> String {
    json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "Hello!"}]
    })
    .to_string()
}

// ============ 全字段配置 ============

#[tokio::test]
async fn test_ai_proxy_config_all_fields() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4-turbo",
            "model_source": "config",
            "route_type": "llm/v1/chat",
            "client_protocol": "openai",
            "response_streaming": "allow",
            "max_request_body_size": 256,
            "model_name_header": true,
            "timeout": 30000,
            "retries": 3,
            "log_payloads": true,
            "log_statistics": true,
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "Bearer test-key" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();
    assert!(!ctx.short_circuited);

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.model.model_name, "gpt-4-turbo");

    mock.shutdown().await;
}

// ============ model_source=request ============

#[tokio::test]
async fn test_ai_proxy_config_model_source_request() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "",
            "model_source": "request",
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "Bearer test-key" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(
        json!({
            "model": "gpt-4-turbo",
            "messages": [{"role": "user", "content": "Hi"}]
        })
        .to_string(),
    );

    plugin.access(&config, &mut ctx).await.unwrap();
    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.model.model_name, "gpt-4-turbo");

    mock.shutdown().await;
}

// ============ model_source=config fallback ============

#[tokio::test]
async fn test_ai_proxy_config_model_source_config_fallback() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "",
            "model_source": "config",
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "Bearer test-key" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(
        state.model.model_name, "gpt-4",
        "配置 model 为空时应 fallback 到请求体 model"
    );

    mock.shutdown().await;
}

// ============ response_streaming 三个值 ============

#[tokio::test]
async fn test_config_streaming_allow() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "response_streaming": "allow",
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "test" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert!(!state.stream_mode, "allow + 非流式请求 -> false");

    mock.shutdown().await;
}

#[tokio::test]
async fn test_config_streaming_always() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "response_streaming": "always",
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "test" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert!(state.stream_mode, "always 应强制开启流式");

    mock.shutdown().await;
}

#[tokio::test]
async fn test_config_streaming_deny() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "response_streaming": "deny",
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "test" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(
        json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true
        })
        .to_string(),
    );
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert!(!state.stream_mode, "deny 应强制关闭流式");

    mock.shutdown().await;
}

// ============ 不支持的 provider_type ============

#[tokio::test]
async fn test_unknown_provider_type() {
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "provider": {
                "provider_type": "unknown_provider",
                "auth_config": { "header_value": "test" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());

    let result = plugin.access(&config, &mut ctx).await;
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("unsupported provider"), "错误: {}", err);
}

// ============ DriverRegistry 内置 driver ============

#[test]
fn test_driver_registry_builtin_providers() {
    let registry = DriverRegistry::new();

    assert!(registry.get("openai").is_some());
    assert!(registry.get("anthropic").is_some());
    assert!(registry.get("gemini").is_some());
    assert!(registry.get("openai_compat").is_some());
    assert!(registry.get("nonexistent").is_none());
}

// ============ 缺少 provider ============

#[tokio::test]
async fn test_missing_provider_config() {
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({"model": "gpt-4"}),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());

    let result = plugin.access(&config, &mut ctx).await;
    assert!(result.is_err(), "缺少 provider 应返回错误");
}

// ============ 默认配置值 ============

#[tokio::test]
async fn test_default_config_values() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "test-key" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.model.model_name, "gpt-4");
    assert_eq!(state.client_protocol, ClientProtocol::OpenAi);
    assert!(!state.stream_mode);

    mock.shutdown().await;
}

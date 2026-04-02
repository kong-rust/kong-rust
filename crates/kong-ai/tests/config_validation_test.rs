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

// ============ Kong 官方 ai-proxy 配置格式兼容 ============

/// Regression: Kong 官方 config.model 是 JSON 对象，不是字符串
/// 之前 model 定义为 String，导致 "invalid type: map, expected a string"
#[tokio::test]
async fn test_kong_official_model_object_format() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    // 完整还原 Kong 官方 ai-proxy 存储在数据库中的配置格式
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "auth": {
                "allow_override": false,
                "gcp_use_service_account": false,
                "header_name": "Authorization",
                "header_value": "Bearer test-key"
            },
            "llm_format": "openai",
            "logging": {
                "log_payloads": false,
                "log_statistics": false
            },
            "max_request_body_size": 8192,
            "model": {
                "provider": "openai",
                "name": "gpt-4",
                "options": {
                    "anthropic_version": "2023-06-01",
                    "azure_api_version": "2023-05-15",
                    "upstream_url": format!("http://127.0.0.1:{}/v1/chat/completions", mock.port)
                }
            },
            "model_name_header": true,
            "response_streaming": "allow",
            "route_type": "llm/v1/chat"
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    // model.name 应被正确提取
    assert_eq!(state.model.model_name, "gpt-4");
    // model.provider 应被用作 provider_type
    assert_eq!(state.provider_config.provider_type, "openai");
    // auth.header_value 应被传递到 provider_config.auth_config
    let auth = &state.provider_config.auth_config;
    assert_eq!(
        auth.get("header_value").and_then(|v| v.as_str()),
        Some("Bearer test-key"),
        "auth.header_value 应从 Kong 官方 config.auth 传入"
    );

    mock.shutdown().await;
}

/// Kong 官方格式 config.model 没有 name 字段时，应 fallback 到请求体中的 model
#[tokio::test]
async fn test_kong_official_model_no_name_fallback_to_request() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "auth": {
                "header_name": "Authorization",
                "header_value": "Bearer test-key"
            },
            "model": {
                "provider": "openai",
                "options": {
                    "upstream_url": format!("http://127.0.0.1:{}/v1/chat/completions", mock.port)
                }
            },
            "route_type": "llm/v1/chat"
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(
        json!({
            "model": "gpt-4-turbo",
            "messages": [{"role": "user", "content": "Hello"}]
        })
        .to_string(),
    );
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    // model.name 缺失时应用请求体中的 model
    assert_eq!(state.model.model_name, "gpt-4-turbo");
    assert_eq!(state.provider_config.provider_type, "openai");

    mock.shutdown().await;
}

/// Kong 官方格式和 kong-rust 格式同时存在时，kong-rust 的 provider 字段优先
#[tokio::test]
async fn test_kong_rust_provider_takes_precedence() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": {
                "provider": "gemini",
                "name": "gemini-pro"
            },
            "auth": {
                "header_value": "key-from-auth"
            },
            // kong-rust 格式的 provider 字段同时存在
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "key-from-provider" }
            },
            "route_type": "llm/v1/chat"
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    // kong-rust provider 字段优先于 model.provider
    assert_eq!(
        state.provider_config.provider_type, "openai",
        "provider 字段应优先于 model.provider"
    );

    mock.shutdown().await;
}

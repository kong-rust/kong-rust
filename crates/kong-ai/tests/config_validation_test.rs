//! 配置验证测试 — ai-proxy 各种配置组合的解析和行为

mod helpers;

use kong_ai::plugins::ai_proxy::{AiProxyConfig, AiProxyPlugin, ModelField};
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

// ============ ModelField 反序列化单元测试 ============

/// ModelField：字符串类型反序列化
#[test]
fn test_model_field_deserialize_string() {
    let field: ModelField = serde_json::from_value(json!("gpt-4")).unwrap();
    assert_eq!(field.model_name(), "gpt-4");
    assert!(field.provider_type().is_none(), "Simple 格式不含 provider");
}

/// ModelField：对象类型反序列化（含全部字段）
#[test]
fn test_model_field_deserialize_object_full() {
    let field: ModelField = serde_json::from_value(json!({
        "provider": "anthropic",
        "name": "claude-3-opus",
        "options": { "anthropic_version": "2023-06-01" }
    }))
    .unwrap();
    assert_eq!(field.model_name(), "claude-3-opus");
    assert_eq!(field.provider_type(), Some("anthropic"));
}

/// ModelField：对象类型，name 缺失 → model_name 返回空字符串
#[test]
fn test_model_field_deserialize_object_no_name() {
    let field: ModelField = serde_json::from_value(json!({
        "provider": "gemini"
    }))
    .unwrap();
    assert_eq!(field.model_name(), "", "缺少 name 字段应返回空字符串");
    assert_eq!(field.provider_type(), Some("gemini"));
}

/// ModelField：null 值 → 等同于空字符串
#[test]
fn test_model_field_deserialize_null() {
    let field: ModelField = serde_json::from_value(json!(null)).unwrap();
    assert_eq!(field.model_name(), "");
    assert!(field.provider_type().is_none());
}

/// ModelField：非法类型（数字）→ 反序列化失败
#[test]
fn test_model_field_deserialize_invalid_number() {
    let result = serde_json::from_value::<ModelField>(json!(42));
    assert!(result.is_err(), "数字类型应反序列化失败");
}

/// ModelField：非法类型（数组）→ 反序列化失败
#[test]
fn test_model_field_deserialize_invalid_array() {
    let result = serde_json::from_value::<ModelField>(json!(["openai"]));
    assert!(result.is_err(), "数组类型应反序列化失败");
}

/// ModelField：对象缺少必需的 provider 字段 → 反序列化失败
#[test]
fn test_model_field_deserialize_object_missing_provider() {
    let result = serde_json::from_value::<ModelField>(json!({
        "name": "gpt-4"
    }));
    assert!(result.is_err(), "缺少 provider 的对象应反序列化失败");
}

// ============ AiProxyConfig 反序列化测试 ============

/// Kong 官方完整配置反序列化（不经过 access，仅验证 parse）
#[test]
fn test_kong_official_config_deserialization() {
    let config: AiProxyConfig = serde_json::from_value(json!({
        "auth": {
            "allow_override": true,
            "gcp_use_service_account": true,
            "header_name": "x-api-key",
            "header_value": "sk-test-123",
            "param_name": "key",
            "param_value": "pk-test",
            "param_location": "query"
        },
        "llm_format": "anthropic",
        "logging": {
            "log_payloads": true,
            "log_statistics": false
        },
        "model": {
            "provider": "anthropic",
            "name": "claude-3-haiku",
            "options": {
                "anthropic_version": "2023-06-01",
                "upstream_url": "https://api.anthropic.com"
            }
        },
        "max_request_body_size": 4096,
        "response_streaming": "deny",
        "route_type": "llm/v1/chat"
    }))
    .unwrap();

    // model 字段
    assert_eq!(config.effective_model_name(), "claude-3-haiku");
    assert_eq!(config.model.provider_type(), Some("anthropic"));

    // auth 字段
    let auth = config.auth.as_ref().unwrap();
    assert_eq!(auth.header_name.as_deref(), Some("x-api-key"));
    assert_eq!(auth.header_value.as_deref(), Some("sk-test-123"));
    assert_eq!(auth.param_name.as_deref(), Some("key"));
    assert_eq!(auth.param_value.as_deref(), Some("pk-test"));
    assert_eq!(auth.param_location.as_deref(), Some("query"));
    assert_eq!(auth.allow_override, Some(true));
    assert_eq!(auth.gcp_use_service_account, Some(true));

    // logging 字段
    let logging = config.logging.as_ref().unwrap();
    assert_eq!(logging.log_payloads, Some(true));
    assert_eq!(logging.log_statistics, Some(false));

    // llm_format
    assert_eq!(config.llm_format.as_deref(), Some("anthropic"));

    // 其他字段
    assert_eq!(config.max_request_body_size, 4096);
    assert_eq!(config.response_streaming, "deny");
}

/// 空 JSON → 全部使用默认值
#[test]
fn test_kong_config_empty_json_defaults() {
    let config: AiProxyConfig = serde_json::from_value(json!({})).unwrap();
    assert_eq!(config.effective_model_name(), "");
    assert_eq!(config.model_source, "config");
    assert_eq!(config.route_type, "llm/v1/chat");
    assert_eq!(config.client_protocol, "openai");
    assert!(config.llm_format.is_none());
    assert_eq!(config.response_streaming, "allow");
    assert_eq!(config.max_request_body_size, 128);
    assert!(config.auth.is_none());
    assert!(config.logging.is_none());
    assert!(config.provider.is_none());
}

// ============ effective_client_protocol 逻辑测试 ============

/// client_protocol 显式设为 "anthropic" → 忽略 llm_format
#[test]
fn test_effective_protocol_explicit_anthropic_ignores_llm_format() {
    let config: AiProxyConfig = serde_json::from_value(json!({
        "client_protocol": "anthropic",
        "llm_format": "openai"
    }))
    .unwrap();
    assert_eq!(
        config.effective_client_protocol(),
        "anthropic",
        "显式 client_protocol 应优先于 llm_format"
    );
}

/// client_protocol 默认 "openai" + llm_format="anthropic" → 使用 llm_format
#[test]
fn test_effective_protocol_default_with_llm_format_anthropic() {
    let config: AiProxyConfig = serde_json::from_value(json!({
        "llm_format": "anthropic"
    }))
    .unwrap();
    assert_eq!(
        config.effective_client_protocol(),
        "anthropic",
        "默认 client_protocol 时 llm_format 应覆盖"
    );
}

/// 两者都不设置 → 默认 "openai"
#[test]
fn test_effective_protocol_both_default() {
    let config: AiProxyConfig = serde_json::from_value(json!({})).unwrap();
    assert_eq!(config.effective_client_protocol(), "openai");
}

// ============ effective_provider 逻辑测试 ============

/// Kong 格式 auth 含 param 系列字段 → 应全部传入 auth_config
#[test]
fn test_effective_provider_param_based_auth() {
    let config: AiProxyConfig = serde_json::from_value(json!({
        "model": {
            "provider": "gemini",
            "name": "gemini-pro"
        },
        "auth": {
            "param_name": "key",
            "param_value": "AIza-test-key",
            "param_location": "query"
        }
    }))
    .unwrap();

    let provider = config.effective_provider().unwrap();
    assert_eq!(provider.provider_type, "gemini");
    let auth = &provider.auth_config;
    assert_eq!(auth.get("param_name").and_then(|v| v.as_str()), Some("key"));
    assert_eq!(
        auth.get("param_value").and_then(|v| v.as_str()),
        Some("AIza-test-key")
    );
    assert_eq!(
        auth.get("param_location").and_then(|v| v.as_str()),
        Some("query")
    );
    // header 字段未设置，不应出现在 auth_config 中
    assert!(auth.get("header_name").is_none());
    assert!(auth.get("header_value").is_none());
}

/// Kong 格式有 model.options.upstream_url → 提取为 endpoint_url
#[test]
fn test_effective_provider_upstream_url_extraction() {
    let config: AiProxyConfig = serde_json::from_value(json!({
        "model": {
            "provider": "openai",
            "name": "gpt-4",
            "options": {
                "upstream_url": "https://custom-openai.example.com/v1"
            }
        }
    }))
    .unwrap();

    let provider = config.effective_provider().unwrap();
    assert_eq!(
        provider.endpoint_url.as_deref(),
        Some("https://custom-openai.example.com/v1")
    );
}

/// Kong 格式无 model.options → endpoint_url 为 None
#[test]
fn test_effective_provider_no_options_no_endpoint() {
    let config: AiProxyConfig = serde_json::from_value(json!({
        "model": {
            "provider": "openai",
            "name": "gpt-4"
        }
    }))
    .unwrap();

    let provider = config.effective_provider().unwrap();
    assert!(
        provider.endpoint_url.is_none(),
        "无 options 时 endpoint_url 应为 None"
    );
}

/// Kong 格式无 auth → auth_config 为空对象
#[test]
fn test_effective_provider_no_auth_empty_object() {
    let config: AiProxyConfig = serde_json::from_value(json!({
        "model": {
            "provider": "openai",
            "name": "gpt-4"
        }
    }))
    .unwrap();

    let provider = config.effective_provider().unwrap();
    assert!(
        provider.auth_config.as_object().unwrap().is_empty(),
        "无 auth 时 auth_config 应为空对象"
    );
}

/// model 为 Simple 字符串且无 provider → effective_provider 返回 None
#[test]
fn test_effective_provider_simple_model_no_provider_returns_none() {
    let config: AiProxyConfig = serde_json::from_value(json!({
        "model": "gpt-4"
    }))
    .unwrap();

    assert!(
        config.effective_provider().is_none(),
        "Simple model 无 provider 字段时应返回 None"
    );
}

// ============ Kong 官方格式 E2E access 测试 ============

/// Kong 官方格式 Anthropic provider — 端到端 access 验证
#[tokio::test]
async fn test_kong_official_anthropic_provider() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": {
                "provider": "anthropic",
                "name": "claude-3-haiku",
                "options": {
                    "upstream_url": format!("http://127.0.0.1:{}/v1/messages", mock.port)
                }
            },
            "auth": {
                "header_name": "x-api-key",
                "header_value": "sk-ant-test"
            },
            "route_type": "llm/v1/chat"
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.model.model_name, "claude-3-haiku");
    assert_eq!(state.provider_config.provider_type, "anthropic");
    assert_eq!(
        state
            .provider_config
            .auth_config
            .get("header_value")
            .and_then(|v| v.as_str()),
        Some("sk-ant-test")
    );

    mock.shutdown().await;
}

/// Kong 官方格式 + llm_format="anthropic" → 客户端协议应为 Anthropic
#[tokio::test]
async fn test_kong_official_llm_format_anthropic_protocol() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": {
                "provider": "openai",
                "name": "gpt-4",
                "options": {
                    "upstream_url": format!("http://127.0.0.1:{}/v1/chat/completions", mock.port)
                }
            },
            "auth": {
                "header_value": "Bearer test"
            },
            "llm_format": "anthropic",
            "route_type": "llm/v1/chat"
        }),
    };

    let mut ctx = RequestCtx::new();
    // Anthropic 格式请求体（需要 max_tokens 字段）
    ctx.request_body = Some(
        json!({
            "model": "gpt-4",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hello"}]
        })
        .to_string(),
    );
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(
        state.client_protocol,
        ClientProtocol::Anthropic,
        "llm_format=anthropic 应使客户端协议为 Anthropic"
    );

    mock.shutdown().await;
}

/// Kong 官方格式 + model_source=request → 使用请求体中的模型名
#[tokio::test]
async fn test_kong_official_model_source_request() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": {
                "provider": "openai",
                "name": "gpt-4",
                "options": {
                    "upstream_url": format!("http://127.0.0.1:{}/v1/chat/completions", mock.port)
                }
            },
            "auth": { "header_value": "Bearer test" },
            "model_source": "request",
            "route_type": "llm/v1/chat"
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(
        json!({
            "model": "gpt-4-turbo-from-request",
            "messages": [{"role": "user", "content": "Hi"}]
        })
        .to_string(),
    );
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(
        state.model.model_name, "gpt-4-turbo-from-request",
        "model_source=request 应使用请求体中的 model"
    );

    mock.shutdown().await;
}

/// Kong 官方格式 + response_streaming="always" → 强制流式
#[tokio::test]
async fn test_kong_official_streaming_always() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": {
                "provider": "openai",
                "name": "gpt-4",
                "options": {
                    "upstream_url": format!("http://127.0.0.1:{}/v1/chat/completions", mock.port)
                }
            },
            "auth": { "header_value": "Bearer test" },
            "response_streaming": "always",
            "route_type": "llm/v1/chat"
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body()); // 非流式请求
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert!(
        state.stream_mode,
        "response_streaming=always 应强制开启流式"
    );

    mock.shutdown().await;
}

/// Kong 官方格式 + response_streaming="deny" → 强制关闭流式
#[tokio::test]
async fn test_kong_official_streaming_deny() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": {
                "provider": "openai",
                "name": "gpt-4",
                "options": {
                    "upstream_url": format!("http://127.0.0.1:{}/v1/chat/completions", mock.port)
                }
            },
            "auth": { "header_value": "Bearer test" },
            "response_streaming": "deny",
            "route_type": "llm/v1/chat"
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
    assert!(
        !state.stream_mode,
        "response_streaming=deny 应强制关闭流式"
    );

    mock.shutdown().await;
}

/// Kong 官方格式：未知 provider 类型 → access 应报错
#[tokio::test]
async fn test_kong_official_unknown_provider() {
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": {
                "provider": "nonexistent_provider",
                "name": "some-model"
            },
            "auth": { "header_value": "test" },
            "route_type": "llm/v1/chat"
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());

    let result = plugin.access(&config, &mut ctx).await;
    assert!(result.is_err(), "未知 provider 应返回错误");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("unsupported provider"),
        "错误信息应包含 unsupported provider: {}",
        err
    );
}

/// Kong 官方格式：model 为对象但无 provider 和 inline provider → 应报缺少 provider 错误
#[tokio::test]
async fn test_kong_official_missing_both_providers() {
    let plugin = AiProxyPlugin::new();
    // model 为 Simple 字符串，无 provider 和 auth
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "route_type": "llm/v1/chat"
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());

    let result = plugin.access(&config, &mut ctx).await;
    assert!(result.is_err(), "无任何 provider 配置应报错");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("missing provider"),
        "错误信息应包含 missing provider: {}",
        err
    );
}

/// Kong 官方格式：upstream_url 覆盖验证 — 应体现在上游连接参数中
#[tokio::test]
async fn test_kong_official_upstream_url_used_in_connection() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": {
                "provider": "openai",
                "name": "gpt-4",
                "options": {
                    "upstream_url": format!("http://127.0.0.1:{}/v1/chat/completions", mock.port)
                }
            },
            "auth": {
                "header_name": "Authorization",
                "header_value": "Bearer test-key"
            },
            "route_type": "llm/v1/chat"
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    // upstream_url 中的 host:port 应被解析为上游目标
    assert_eq!(
        ctx.upstream_target_host.as_deref(),
        Some("127.0.0.1"),
        "upstream_url 的 host 应被设为上游目标"
    );
    assert_eq!(
        ctx.upstream_target_port,
        Some(mock.port),
        "upstream_url 的 port 应被设为上游目标端口"
    );

    mock.shutdown().await;
}

/// Kong 官方格式：max_request_body_size 生效 → 超大请求体应返回 413
#[tokio::test]
async fn test_kong_official_max_request_body_size() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": {
                "provider": "openai",
                "name": "gpt-4",
                "options": {
                    "upstream_url": format!("http://127.0.0.1:{}/v1/chat/completions", mock.port)
                }
            },
            "auth": { "header_value": "Bearer test" },
            // 设置非常小的 body 上限（1KB）
            "max_request_body_size": 1,
            "route_type": "llm/v1/chat"
        }),
    };

    let mut ctx = RequestCtx::new();
    // 构造超过 1KB 的请求体
    let large_content = "x".repeat(2048);
    ctx.request_body = Some(
        json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": large_content}]
        })
        .to_string(),
    );
    plugin.access(&config, &mut ctx).await.unwrap();

    assert!(ctx.short_circuited, "超大请求体应被短路");
    assert_eq!(ctx.exit_status, Some(413), "应返回 413 状态码");

    mock.shutdown().await;
}

/// Kong 官方格式：model_name_header 应将模型名写入响应头
#[tokio::test]
async fn test_kong_official_model_name_in_state() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": {
                "provider": "openai",
                "name": "gpt-4-0125-preview",
                "options": {
                    "upstream_url": format!("http://127.0.0.1:{}/v1/chat/completions", mock.port)
                }
            },
            "auth": { "header_value": "Bearer test" },
            "model_name_header": true,
            "route_type": "llm/v1/chat"
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(
        state.model.model_name, "gpt-4-0125-preview",
        "AiRequestState 中的 model_name 应来自 Kong 配置 model.name"
    );

    mock.shutdown().await;
}

/// Kong 官方格式：混合 auth 字段（header + param）同时存在
#[test]
fn test_effective_provider_mixed_auth_fields() {
    let config: AiProxyConfig = serde_json::from_value(json!({
        "model": {
            "provider": "openai",
            "name": "gpt-4"
        },
        "auth": {
            "header_name": "Authorization",
            "header_value": "Bearer sk-test",
            "param_name": "api_key",
            "param_value": "pk-test",
            "param_location": "body"
        }
    }))
    .unwrap();

    let provider = config.effective_provider().unwrap();
    let auth = &provider.auth_config;
    // 全部 5 个字段都应存在
    assert_eq!(auth.get("header_name").and_then(|v| v.as_str()), Some("Authorization"));
    assert_eq!(auth.get("header_value").and_then(|v| v.as_str()), Some("Bearer sk-test"));
    assert_eq!(auth.get("param_name").and_then(|v| v.as_str()), Some("api_key"));
    assert_eq!(auth.get("param_value").and_then(|v| v.as_str()), Some("pk-test"));
    assert_eq!(auth.get("param_location").and_then(|v| v.as_str()), Some("body"));
}

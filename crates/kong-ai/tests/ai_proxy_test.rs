//! ai-proxy 插件单元测试

use bytes::Bytes;
use kong_ai::plugins::ai_proxy::{AiProxyConfig, AiProxyPlugin};
use kong_ai::plugins::context::AiRequestState;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};
use serde_json::json;

/// 构建测试用插件配置
fn make_plugin_config(config_json: serde_json::Value) -> PluginConfig {
    PluginConfig {
        name: "ai-proxy".to_string(),
        config: config_json,
    }
}

/// 构建包含 OpenAI chat 请求体的 RequestCtx
fn make_ctx_with_body(body: &str) -> RequestCtx {
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(body.to_string());
    ctx
}

/// 标准的 OpenAI chat 请求体
fn openai_chat_body(model: &str) -> String {
    json!({
        "model": model,
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    })
    .to_string()
}

/// 标准的内联 provider 配置 JSON
fn inline_provider_config(model: &str) -> serde_json::Value {
    json!({
        "model": model,
        "model_source": "config",
        "route_type": "llm/v1/chat",
        "provider": {
            "provider_type": "openai",
            "auth_config": {
                "header_value": "Bearer sk-test-key"
            }
        }
    })
}

// ============ 配置解析测试 ============

#[test]
fn test_ai_proxy_config_parse() {
    // 完整配置解析
    let config_json = json!({
        "model": "gpt-4",
        "model_source": "config",
        "route_type": "llm/v1/chat",
        "client_protocol": "openai",
        "response_streaming": "deny",
        "max_request_body_size": 256,
        "model_name_header": false,
        "timeout": 30000,
        "retries": 3,
        "log_payloads": true,
        "log_statistics": false,
        "provider": {
            "provider_type": "openai",
            "auth_config": {"header_value": "Bearer sk-xxx"}
        }
    });

    let cfg: AiProxyConfig = serde_json::from_value(config_json).unwrap();
    assert_eq!(cfg.effective_model_name(), "gpt-4");
    assert_eq!(cfg.model_source, "config");
    assert_eq!(cfg.route_type, "llm/v1/chat");
    assert_eq!(cfg.client_protocol, "openai");
    assert_eq!(cfg.response_streaming, "deny");
    assert_eq!(cfg.max_request_body_size, 256);
    assert!(!cfg.model_name_header);
    assert_eq!(cfg.timeout, 30000);
    assert_eq!(cfg.retries, 3);
    assert!(cfg.log_payloads);
    assert!(!cfg.log_statistics);
    assert!(cfg.provider.is_some());
    assert_eq!(cfg.provider.unwrap().provider_type, "openai");
}

#[test]
fn test_ai_proxy_config_defaults() {
    // 最小配置应使用默认值
    let config_json = json!({});
    let cfg: AiProxyConfig = serde_json::from_value(config_json).unwrap();

    assert_eq!(cfg.effective_model_name(), "");
    assert_eq!(cfg.model_source, "config");
    assert_eq!(cfg.route_type, "llm/v1/chat");
    assert_eq!(cfg.client_protocol, "openai");
    assert_eq!(cfg.response_streaming, "allow");
    assert_eq!(cfg.max_request_body_size, 128);
    assert!(cfg.model_name_header);
    assert_eq!(cfg.timeout, 60_000);
    assert_eq!(cfg.retries, 1);
    assert!(!cfg.log_payloads);
    assert!(cfg.log_statistics);
    assert!(cfg.provider.is_none());
}

// ============ access 阶段测试 ============

#[tokio::test]
async fn test_ai_proxy_access_sets_upstream() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));
    let mut ctx = make_ctx_with_body(&openai_chat_body("gpt-4"));

    plugin.access(&config, &mut ctx).await.unwrap();

    // 验证上游连接参数
    assert_eq!(ctx.upstream_target_host, Some("api.openai.com".to_string()));
    assert_eq!(ctx.upstream_target_port, Some(443));
    assert_eq!(ctx.upstream_scheme, Some("https".to_string()));
    assert_eq!(
        ctx.upstream_path,
        Some("/v1/chat/completions".to_string())
    );

    // 验证上游请求体已设置
    assert!(ctx.upstream_body.is_some());
    let body: serde_json::Value = serde_json::from_str(ctx.upstream_body.as_ref().unwrap()).unwrap();
    assert_eq!(body["model"], "gpt-4");

    // 验证 AiRequestState 已存储
    let state = ctx.extensions.get::<AiRequestState>();
    assert!(state.is_some());
    let state = state.unwrap();
    assert_eq!(state.model.model_name, "gpt-4");
    assert!(!state.stream_mode);

    // 验证上游请求头包含 Authorization
    let has_auth = ctx
        .upstream_headers_to_set
        .iter()
        .any(|(k, v)| k == "Authorization" && v.contains("sk-test-key"));
    assert!(has_auth, "应包含 Authorization header");

    // 验证 Content-Type header
    let has_ct = ctx
        .upstream_headers_to_set
        .iter()
        .any(|(k, v)| k == "Content-Type" && v == "application/json");
    assert!(has_ct, "应包含 Content-Type header");
}

#[tokio::test]
async fn test_ai_proxy_access_model_source_request() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(json!({
        "model_source": "request",
        "provider": {
            "provider_type": "openai",
            "auth_config": {"header_value": "Bearer sk-test"}
        }
    }));
    let mut ctx = make_ctx_with_body(&openai_chat_body("gpt-3.5-turbo"));

    plugin.access(&config, &mut ctx).await.unwrap();

    // 模型应来自请求体
    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.model.model_name, "gpt-3.5-turbo");

    // 上游请求体中的 model 应是 gpt-3.5-turbo
    let body: serde_json::Value = serde_json::from_str(ctx.upstream_body.as_ref().unwrap()).unwrap();
    assert_eq!(body["model"], "gpt-3.5-turbo");
}

#[tokio::test]
async fn test_ai_proxy_access_model_source_config_overrides_request() {
    let plugin = AiProxyPlugin::new();
    // 配置中指定 model=gpt-4，请求体中 model=gpt-3.5-turbo
    let config = make_plugin_config(inline_provider_config("gpt-4"));
    let mut ctx = make_ctx_with_body(&openai_chat_body("gpt-3.5-turbo"));

    plugin.access(&config, &mut ctx).await.unwrap();

    // model_source=config 时，配置中的模型应覆盖请求体中的
    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.model.model_name, "gpt-4");

    let body: serde_json::Value = serde_json::from_str(ctx.upstream_body.as_ref().unwrap()).unwrap();
    assert_eq!(body["model"], "gpt-4");
}

#[tokio::test]
async fn test_ai_proxy_access_missing_body_returns_error() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));
    let mut ctx = RequestCtx::new();
    // request_body 为 None

    let result = plugin.access(&config, &mut ctx).await;
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("request body is empty"),
        "错误信息应包含 'request body is empty'，实际：{}",
        err_msg
    );
}

#[tokio::test]
async fn test_ai_proxy_access_missing_provider_returns_error() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(json!({
        "model": "gpt-4",
    }));
    let mut ctx = make_ctx_with_body(&openai_chat_body("gpt-4"));

    let result = plugin.access(&config, &mut ctx).await;
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("missing provider") || err_msg.contains("model_routes"),
        "错误信息应包含 provider 缺失提示，实际：{}",
        err_msg
    );
}

#[tokio::test]
async fn test_ai_proxy_access_body_too_large_short_circuits() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(json!({
        "model": "gpt-4",
        "max_request_body_size": 1, // 1 KB
        "provider": {
            "provider_type": "openai",
            "auth_config": {"header_value": "Bearer sk-test"}
        }
    }));

    // 构造超过 1KB 的请求体
    let large_content = "x".repeat(2048);
    let body = json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": large_content}]
    })
    .to_string();

    let mut ctx = make_ctx_with_body(&body);
    plugin.access(&config, &mut ctx).await.unwrap();

    assert!(ctx.short_circuited);
    assert_eq!(ctx.exit_status, Some(413));
}

// ============ body_filter 阶段测试 ============

#[tokio::test]
async fn test_ai_proxy_body_filter_nonstreaming() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));

    // 先运行 access 设置状态
    let mut ctx = make_ctx_with_body(&openai_chat_body("gpt-4"));
    plugin.access(&config, &mut ctx).await.unwrap();

    // 模拟上游响应
    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "application/json".to_string());

    let response_body = json!({
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "created": 1677652288,
        "model": "gpt-4",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Hello!"},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    })
    .to_string();

    // 模拟分 2 个 chunk 到达
    let chunk1 = &response_body[..response_body.len() / 2];
    let chunk2 = &response_body[response_body.len() / 2..];

    // 第一个 chunk：非 end_of_stream
    let mut body1: Option<Bytes> = Some(Bytes::from(chunk1.to_string()));
    plugin
        .body_filter(&config, &mut ctx, &mut body1, false)
        .await
        .unwrap();
    // 缓冲中，body 应被清空
    assert!(body1.is_none(), "非 end_of_stream 时 body 应被清空（缓冲中）");

    // 第二个 chunk：end_of_stream
    let mut body2: Option<Bytes> = Some(Bytes::from(chunk2.to_string()));
    plugin
        .body_filter(&config, &mut ctx, &mut body2, true)
        .await
        .unwrap();

    // 验证响应体已被替换为 ChatResponse JSON
    assert!(body2.is_some(), "end_of_stream 时应返回完整响应体");
    let result_str = String::from_utf8_lossy(body2.as_ref().unwrap());
    let result: serde_json::Value = serde_json::from_str(&result_str).unwrap();
    assert_eq!(result["id"], "chatcmpl-123");
    assert_eq!(result["model"], "gpt-4");

    // 验证 usage 已提取
    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.usage.prompt_tokens, Some(10));
    assert_eq!(state.usage.completion_tokens, Some(5));
    assert_eq!(state.usage.total_tokens, Some(15));
}

// ============ log 阶段测试 ============

#[tokio::test]
async fn test_ai_proxy_log_writes_analytics() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));

    let mut ctx = make_ctx_with_body(&openai_chat_body("gpt-4"));
    plugin.access(&config, &mut ctx).await.unwrap();

    // 模拟 usage 数据
    {
        let state = ctx.extensions.get_mut::<AiRequestState>().unwrap();
        state.usage.prompt_tokens = Some(100);
        state.usage.completion_tokens = Some(50);
        state.usage.total_tokens = Some(150);
    }

    plugin.log(&config, &mut ctx).await.unwrap();

    // 验证 log_serialize 已设置
    assert!(ctx.log_serialize.is_some());
    let log = ctx.log_serialize.as_ref().unwrap();
    assert_eq!(log["ai"]["proxy"]["provider"], "openai");
    assert_eq!(log["ai"]["proxy"]["model"], "gpt-4");
    assert_eq!(log["ai"]["proxy"]["stream"], false);
    assert_eq!(log["ai"]["usage"]["prompt_tokens"], 100);
    assert_eq!(log["ai"]["usage"]["completion_tokens"], 50);
    assert_eq!(log["ai"]["usage"]["total_tokens"], 150);
    assert!(log["ai"]["latency"]["e2e_ms"].as_u64().is_some());
}

// ============ parse_plugin_config 测试 ============

#[test]
fn test_parse_plugin_config_helper() {
    let pc = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({"model": "gpt-4"}),
    };

    let cfg: AiProxyConfig = kong_ai::parse_plugin_config(&pc).unwrap();
    assert_eq!(cfg.effective_model_name(), "gpt-4");
}

#[test]
fn test_parse_plugin_config_invalid_returns_error() {
    let pc = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!("not an object"),
    };

    let result: Result<AiProxyConfig, _> = kong_ai::parse_plugin_config(&pc);
    assert!(result.is_err());
}

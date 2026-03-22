//! 错误处理测试 — 验证 ai-proxy 插件在异常场景下的行为

mod helpers;

use bytes::Bytes;
use kong_ai::plugins::ai_proxy::AiProxyPlugin;
use kong_ai::plugins::context::AiRequestState;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};
use serde_json::json;

use helpers::MockLlmServer;

fn make_proxy_config(mock_port: u16) -> PluginConfig {
    PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "model_source": "config",
            "route_type": "llm/v1/chat",
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock_port),
                "auth_config": { "header_value": "Bearer test-key" }
            }
        }),
    }
}

fn make_chat_body() -> String {
    json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "Hello!"}]
    })
    .to_string()
}

// ============ 上游 401 错误传播 ============

#[tokio::test]
async fn test_proxy_upstream_401_unauthorized() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = make_proxy_config(mock.port);

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    ctx.response_status = Some(401);
    ctx.response_headers
        .insert("content-type".to_string(), "application/json".to_string());
    let error_body = json!({
        "error": { "message": "Incorrect API key", "type": "invalid_request_error" }
    })
    .to_string();
    let mut body = Some(Bytes::from(error_body.clone()));

    plugin.header_filter(&config, &mut ctx).await.unwrap();
    plugin
        .body_filter(&config, &mut ctx, &mut body, true)
        .await
        .unwrap();

    // 401 错误应被保留，不 panic
    assert!(body.is_some());
    let resp = std::str::from_utf8(body.as_ref().unwrap()).unwrap();
    assert!(
        resp.contains("invalid_request_error") || resp.contains("Incorrect API key"),
        "401 错误信息应保留"
    );

    mock.shutdown().await;
}

// ============ 上游 200 但 JSON 无效 ============

#[tokio::test]
async fn test_proxy_upstream_bad_response_format() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = make_proxy_config(mock.port);

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "application/json".to_string());
    let mut body = Some(Bytes::from("this is not json"));

    plugin.header_filter(&config, &mut ctx).await.unwrap();
    // 不应 panic
    let result = plugin
        .body_filter(&config, &mut ctx, &mut body, true)
        .await;
    assert!(result.is_ok(), "无效 JSON 响应不应 panic");
    assert!(body.is_some(), "应保留某种响应内容");

    mock.shutdown().await;
}

// ============ 上游响应无 usage 字段 ============

#[tokio::test]
async fn test_proxy_upstream_no_usage_in_response() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = make_proxy_config(mock.port);

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "application/json".to_string());
    let resp_no_usage = json!({
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "created": 1700000000u64,
        "model": "gpt-4",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hi!"}, "finish_reason": "stop"}]
    })
    .to_string();
    let mut body = Some(Bytes::from(resp_no_usage));

    plugin.header_filter(&config, &mut ctx).await.unwrap();
    plugin
        .body_filter(&config, &mut ctx, &mut body, true)
        .await
        .unwrap();
    assert!(body.is_some());

    // log 阶段不应出错
    plugin.log(&config, &mut ctx).await.unwrap();
    let log_data = ctx.log_serialize.as_ref().unwrap();
    // usage 字段应为 null
    assert!(
        log_data["ai"]["usage"]["prompt_tokens"].is_null(),
        "无 usage 时 prompt_tokens 应为 null"
    );

    mock.shutdown().await;
}

// ============ response_streaming=deny 强制关闭流式 ============

#[tokio::test]
async fn test_proxy_response_streaming_deny() {
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
                "auth_config": { "header_value": "Bearer test-key" }
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
    assert!(!ctx.short_circuited, "deny 不应拒绝请求，而是强制非流式");

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert!(!state.stream_mode, "deny 应将 stream_mode 设为 false");

    mock.shutdown().await;
}

// ============ 无效 JSON 请求体 ============

#[tokio::test]
async fn test_proxy_invalid_request_body_json() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = make_proxy_config(mock.port);

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some("not valid json".to_string());

    let result = plugin.access(&config, &mut ctx).await;
    assert!(result.is_err(), "无效 JSON 请求体应返回错误");

    mock.shutdown().await;
}

// ============ 空 messages 数组 ============

#[tokio::test]
async fn test_proxy_empty_messages_array() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = make_proxy_config(mock.port);

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(
        json!({"model": "gpt-4", "messages": []}).to_string(),
    );

    // 空 messages 数组合法，不应 panic
    let result = plugin.access(&config, &mut ctx).await;
    assert!(result.is_ok(), "空 messages 不应 panic");
    assert!(!ctx.short_circuited);

    mock.shutdown().await;
}

// ============ model_source=request 但请求无 model ============

#[tokio::test]
async fn test_proxy_model_not_found_in_request() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "",
            "model_source": "request",
            "route_type": "llm/v1/chat",
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "Bearer test-key" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(
        json!({"model": "", "messages": [{"role": "user", "content": "Hello"}]}).to_string(),
    );

    let result = plugin.access(&config, &mut ctx).await;
    assert!(result.is_err(), "model_source=request 但无 model 应报错");

    mock.shutdown().await;
}

// ============ request_body 为 None ============

#[tokio::test]
async fn test_proxy_empty_request_body() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = make_proxy_config(mock.port);

    let mut ctx = RequestCtx::new();
    ctx.request_body = None;

    let result = plugin.access(&config, &mut ctx).await;
    assert!(result.is_err(), "空请求体应返回错误");

    mock.shutdown().await;
}

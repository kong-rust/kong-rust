//! 日志/可观测性测试 — log_serialize 输出验证

mod helpers;

use bytes::Bytes;
use kong_ai::plugins::ai_proxy::AiProxyPlugin;
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
            "log_statistics": true,
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

fn mock_response_with_usage(prompt: u64, completion: u64) -> String {
    json!({
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "created": 1700000000u64,
        "model": "gpt-4",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hi!"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion}
    })
    .to_string()
}

// ============ log_statistics=true 时输出 usage ============

#[tokio::test]
async fn test_log_statistics_enabled() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = make_proxy_config(mock.port);

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());

    // 完整流程: access → header_filter → body_filter → log
    plugin.access(&config, &mut ctx).await.unwrap();

    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "application/json".to_string());
    let mut body = Some(Bytes::from(mock_response_with_usage(20, 10)));

    plugin.header_filter(&config, &mut ctx).await.unwrap();
    plugin
        .body_filter(&config, &mut ctx, &mut body, true)
        .await
        .unwrap();
    plugin.log(&config, &mut ctx).await.unwrap();

    assert!(ctx.log_serialize.is_some(), "log 阶段应输出分析数据");
    let log = ctx.log_serialize.as_ref().unwrap();

    // 验证 ai.usage 字段
    assert_eq!(log["ai"]["usage"]["prompt_tokens"], 20);
    assert_eq!(log["ai"]["usage"]["completion_tokens"], 10);
    assert_eq!(log["ai"]["usage"]["total_tokens"], 30);

    // 验证 ai.proxy 字段
    assert_eq!(log["ai"]["proxy"]["provider"], "openai");
    assert_eq!(log["ai"]["proxy"]["model"], "gpt-4");
    assert_eq!(log["ai"]["proxy"]["route_type"], "llm/v1/chat");
    assert_eq!(log["ai"]["proxy"]["stream"], false);

    mock.shutdown().await;
}

// ============ log 包含延迟数据 ============

#[tokio::test]
async fn test_log_latency_tracking() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = make_proxy_config(mock.port);

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "application/json".to_string());
    let mut body = Some(Bytes::from(mock_response_with_usage(10, 5)));

    plugin.header_filter(&config, &mut ctx).await.unwrap();
    plugin
        .body_filter(&config, &mut ctx, &mut body, true)
        .await
        .unwrap();
    plugin.log(&config, &mut ctx).await.unwrap();

    let log = ctx.log_serialize.as_ref().unwrap();
    let e2e_ms = log["ai"]["latency"]["e2e_ms"].as_u64().unwrap();
    // e2e_ms 应为正数（至少 0ms）
    assert!(
        e2e_ms < 10000,
        "e2e_ms 应为合理值（<10s），实际: {}",
        e2e_ms
    );
}

// ============ 流式模式 log 标记 stream=true ============

#[tokio::test]
async fn test_log_streaming_mode_flag() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "model_source": "config",
            "route_type": "llm/v1/chat",
            "response_streaming": "always",
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

    // 模拟流式响应
    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "text/event-stream".to_string());
    plugin.header_filter(&config, &mut ctx).await.unwrap();

    // 发送一个 chunk + [DONE]
    let chunk = json!({"id":"s","object":"chat.completion.chunk","created":1700000000u64,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Hi"},"finish_reason":null}],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}});
    let sse = format!("data: {}\n\ndata: [DONE]\n\n", chunk);
    let mut body = Some(Bytes::from(sse));
    plugin
        .body_filter(&config, &mut ctx, &mut body, true)
        .await
        .unwrap();

    plugin.log(&config, &mut ctx).await.unwrap();

    let log = ctx.log_serialize.as_ref().unwrap();
    assert_eq!(log["ai"]["proxy"]["stream"], true, "流式模式应标记 stream=true");

    mock.shutdown().await;
}

// ============ log 合并到已有 log_serialize ============

#[tokio::test]
async fn test_log_merges_with_existing() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = make_proxy_config(mock.port);

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    // 预设 log_serialize（模拟其他插件已写入数据）
    ctx.log_serialize = Some(json!({"existing_key": "existing_value"}));

    plugin.access(&config, &mut ctx).await.unwrap();

    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "application/json".to_string());
    let mut body = Some(Bytes::from(mock_response_with_usage(10, 5)));

    plugin.header_filter(&config, &mut ctx).await.unwrap();
    plugin
        .body_filter(&config, &mut ctx, &mut body, true)
        .await
        .unwrap();
    plugin.log(&config, &mut ctx).await.unwrap();

    let log = ctx.log_serialize.as_ref().unwrap();
    // 原有数据应保留
    assert_eq!(
        log["existing_key"], "existing_value",
        "已有 log 数据不应被覆盖"
    );
    // ai 数据应添加
    assert!(log["ai"].is_object(), "应合并 ai 日志数据");
    assert_eq!(log["ai"]["usage"]["prompt_tokens"], 10);
}

// ============ 无 AiRequestState 时 log 不报错 ============

#[tokio::test]
async fn test_log_without_ai_state() {
    // 如果 access 未执行（如被其他插件短路），log 不应出错
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "provider": {
                "provider_type": "openai",
                "auth_config": { "header_value": "test" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    // 不执行 access，直接调用 log
    let result = plugin.log(&config, &mut ctx).await;
    assert!(result.is_ok(), "无 AiRequestState 时 log 不应报错");
    // log_serialize 应为 None（未写入数据）
    assert!(ctx.log_serialize.is_none());
}

// ============ 不同 provider 和 model 在 log 中正确记录 ============

#[tokio::test]
async fn test_log_records_correct_provider_and_model() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    // 使用 Anthropic provider
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "claude-3-sonnet",
            "model_source": "config",
            "route_type": "llm/v1/chat",
            "provider": {
                "provider_type": "anthropic",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "sk-ant-test" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    // 模拟 Anthropic 响应
    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "application/json".to_string());
    let anthropic_resp = json!({
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "Hello!"}],
        "model": "claude-3-sonnet",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 15, "output_tokens": 8}
    })
    .to_string();
    let mut body = Some(Bytes::from(anthropic_resp));

    plugin.header_filter(&config, &mut ctx).await.unwrap();
    plugin
        .body_filter(&config, &mut ctx, &mut body, true)
        .await
        .unwrap();
    plugin.log(&config, &mut ctx).await.unwrap();

    let log = ctx.log_serialize.as_ref().unwrap();
    assert_eq!(
        log["ai"]["proxy"]["provider"], "anthropic",
        "日志应记录正确的 provider"
    );
    assert_eq!(
        log["ai"]["proxy"]["model"], "claude-3-sonnet",
        "日志应记录正确的 model"
    );

    mock.shutdown().await;
}

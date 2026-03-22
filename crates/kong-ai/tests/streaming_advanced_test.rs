//! 高级流式测试 — SSE 解析边界、多事件序列、usage 累积

mod helpers;

use bytes::Bytes;
use kong_ai::codec::{SseEvent, SseFormat, SseParser};
use kong_ai::plugins::ai_proxy::AiProxyPlugin;
use kong_ai::plugins::context::AiRequestState;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};
use serde_json::json;

use helpers::MockLlmServer;

fn make_streaming_config(mock_port: u16) -> PluginConfig {
    PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "model_source": "config",
            "route_type": "llm/v1/chat",
            "client_protocol": "openai",
            "response_streaming": "always",
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

// ============ SSE: JSON 跨 chunk 分割 ============

#[test]
fn test_sse_parser_partial_json_split() {
    let mut parser = SseParser::new(SseFormat::Standard);

    // 第一块: 不完整（缺少 \n\n）
    let chunk1 = "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"del";
    let events1 = parser.feed(chunk1);
    assert!(events1.is_empty(), "不完整事件不应产出");

    // 第二块: 补全
    let chunk2 = "ta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n";
    let events2 = parser.feed(chunk2);
    assert_eq!(events2.len(), 1, "补全后应产出 1 个事件");

    let data: serde_json::Value = serde_json::from_str(&events2[0].data).unwrap();
    assert_eq!(data["choices"][0]["delta"]["content"], "hi");
}

// ============ SSE: 空白 chunk ============

#[test]
fn test_sse_parser_empty_chunks() {
    let mut parser = SseParser::new(SseFormat::Standard);

    let events1 = parser.feed("data: {\"id\":\"1\"}\n\n");
    assert_eq!(events1.len(), 1);

    // 空白不应产出
    assert!(parser.feed("   \n").is_empty());
    assert!(parser.feed("").is_empty());

    let events2 = parser.feed("data: {\"id\":\"2\"}\n\n");
    assert_eq!(events2.len(), 1);
}

// ============ SSE: 多事件同一 chunk ============

#[test]
fn test_sse_parser_multiple_events_in_one_chunk() {
    let mut parser = SseParser::new(SseFormat::Standard);

    let multi = "data: {\"id\":\"1\"}\n\ndata: {\"id\":\"2\"}\n\ndata: [DONE]\n\n";
    let events = parser.feed(multi);

    assert_eq!(events.len(), 3);
    assert!(!events[0].is_done());
    assert!(!events[1].is_done());
    assert!(events[2].is_done());
}

// ============ [DONE] 事件识别 ============

#[test]
fn test_sse_done_event_detection() {
    let done = SseEvent {
        event_type: "message".to_string(),
        data: "[DONE]".to_string(),
        id: None,
    };
    assert!(done.is_done());

    let done_padded = SseEvent {
        event_type: "message".to_string(),
        data: " [DONE] ".to_string(),
        id: None,
    };
    assert!(done_padded.is_done());

    let normal = SseEvent {
        event_type: "message".to_string(),
        data: "{\"id\":\"123\"}".to_string(),
        id: None,
    };
    assert!(!normal.is_done());
}

// ============ Anthropic 完整流式序列 ============

#[tokio::test]
async fn test_streaming_anthropic_event_types_sequence() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "claude-3-sonnet",
            "model_source": "config",
            "route_type": "llm/v1/chat",
            "client_protocol": "openai",
            "response_streaming": "always",
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

    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "text/event-stream".to_string());
    plugin.header_filter(&config, &mut ctx).await.unwrap();

    // message_start
    let mut b1 = Some(Bytes::from("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_001\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-3-sonnet\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n"));
    plugin.body_filter(&config, &mut ctx, &mut b1, false).await.unwrap();

    // content_block_start（跳过）
    let mut b2 = Some(Bytes::from("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n"));
    plugin.body_filter(&config, &mut ctx, &mut b2, false).await.unwrap();

    // content_block_delta x2
    let mut b3 = Some(Bytes::from("event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n"));
    plugin.body_filter(&config, &mut ctx, &mut b3, false).await.unwrap();

    let mut b4 = Some(Bytes::from("event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" World\"}}\n\n"));
    plugin.body_filter(&config, &mut ctx, &mut b4, false).await.unwrap();

    // message_delta + message_stop
    let mut b5 = Some(Bytes::from("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":15}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
    plugin.body_filter(&config, &mut ctx, &mut b5, true).await.unwrap();

    // log 阶段
    plugin.log(&config, &mut ctx).await.unwrap();
    assert!(ctx.log_serialize.is_some());
    let log = ctx.log_serialize.as_ref().unwrap();
    assert_eq!(log["ai"]["proxy"]["stream"], true);

    mock.shutdown().await;
}

// ============ OpenAI 流式 usage 从最终 chunk ============

#[tokio::test]
async fn test_streaming_usage_from_final_chunk() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = make_streaming_config(mock.port);

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "text/event-stream".to_string());
    plugin.header_filter(&config, &mut ctx).await.unwrap();

    // chunk 1（无 usage）
    let c1 = json!({"id":"s","object":"chat.completion.chunk","created":1700000000u64,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]});
    let mut b1 = Some(Bytes::from(format!("data: {}\n\n", c1)));
    plugin.body_filter(&config, &mut ctx, &mut b1, false).await.unwrap();

    // chunk 2（带 usage）+ [DONE]
    let c2 = json!({"id":"s","object":"chat.completion.chunk","created":1700000000u64,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"!"},"finish_reason":"stop"}],"usage":{"prompt_tokens":20,"completion_tokens":8,"total_tokens":28}});
    let mut b2 = Some(Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n", c2)));
    plugin.body_filter(&config, &mut ctx, &mut b2, true).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.usage.prompt_tokens, Some(20));
    assert_eq!(state.usage.completion_tokens, Some(8));
    assert_eq!(state.usage.total_tokens, Some(28));

    mock.shutdown().await;
}

// ============ 流式 response_buffer 累积 ============

#[tokio::test]
async fn test_streaming_response_buffer_accumulates() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = make_streaming_config(mock.port);

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(make_chat_body());
    plugin.access(&config, &mut ctx).await.unwrap();

    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "text/event-stream".to_string());
    plugin.header_filter(&config, &mut ctx).await.unwrap();

    // 多个 content chunk
    for text in &["Hello", ", ", "World", "!"] {
        let chunk = json!({"id":"s","object":"chat.completion.chunk","created":1700000000u64,"model":"gpt-4","choices":[{"index":0,"delta":{"content":text},"finish_reason":null}]});
        let mut body = Some(Bytes::from(format!("data: {}\n\n", chunk)));
        plugin.body_filter(&config, &mut ctx, &mut body, false).await.unwrap();
    }

    // [DONE]
    let mut done = Some(Bytes::from("data: [DONE]\n\n"));
    plugin.body_filter(&config, &mut ctx, &mut done, true).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    let buffer = state.response_buffer.as_ref().unwrap();
    assert!(!buffer.is_empty(), "response_buffer 应有累积内容");

    mock.shutdown().await;
}

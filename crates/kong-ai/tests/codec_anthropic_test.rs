//! Anthropic Codec 测试 — 客户端侧 Anthropic Messages 协议编解码

use kong_ai::codec::anthropic_format::AnthropicCodec;
use kong_ai::codec::{ChatResponse, Choice, Message, SseEvent, Usage};

#[test]
fn test_decode_request_basic() {
    let body = r#"{
        "model": "claude-3-sonnet",
        "max_tokens": 1024,
        "messages": [
            {"role": "user", "content": "Hello, Claude!"}
        ]
    }"#;

    let req = AnthropicCodec::decode_request(body).unwrap();
    assert_eq!(req.model, "claude-3-sonnet");
    assert_eq!(req.max_tokens, Some(1024));
    assert_eq!(req.messages.len(), 1);
    assert_eq!(req.messages[0].role, "user");
    assert_eq!(
        req.messages[0].content,
        Some(serde_json::Value::String("Hello, Claude!".to_string()))
    );
}

#[test]
fn test_decode_request_with_system() {
    // system 字段应转换为 role=system 的消息插入到 messages 首部
    let body = r#"{
        "model": "claude-3-sonnet",
        "max_tokens": 1024,
        "system": "You are a helpful assistant.",
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    }"#;

    let req = AnthropicCodec::decode_request(body).unwrap();
    assert_eq!(req.messages.len(), 2);
    assert_eq!(req.messages[0].role, "system");
    assert_eq!(
        req.messages[0].content,
        Some(serde_json::Value::String(
            "You are a helpful assistant.".to_string()
        ))
    );
    assert_eq!(req.messages[1].role, "user");
}

#[test]
fn test_decode_request_with_content_blocks() {
    // Anthropic content blocks 数组应提取文本
    let body = r#"{
        "model": "claude-3-sonnet",
        "max_tokens": 1024,
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "Part 1"}, {"type": "text", "text": "Part 2"}]}
        ]
    }"#;

    let req = AnthropicCodec::decode_request(body).unwrap();
    assert_eq!(req.messages.len(), 1);
    // content blocks 拼接为纯文本
    assert_eq!(
        req.messages[0].content,
        Some(serde_json::Value::String("Part 1Part 2".to_string()))
    );
}

#[test]
fn test_decode_request_with_temperature() {
    let body = r#"{
        "model": "claude-3-sonnet",
        "max_tokens": 2048,
        "temperature": 0.5,
        "top_p": 0.9,
        "messages": [
            {"role": "user", "content": "Hi"}
        ]
    }"#;

    let req = AnthropicCodec::decode_request(body).unwrap();
    assert_eq!(req.temperature, Some(0.5));
    assert_eq!(req.top_p, Some(0.9));
    assert_eq!(req.max_tokens, Some(2048));
}

#[test]
fn test_decode_request_with_stream() {
    let body = r#"{
        "model": "claude-3-sonnet",
        "max_tokens": 1024,
        "stream": true,
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    }"#;

    let req = AnthropicCodec::decode_request(body).unwrap();
    assert_eq!(req.stream, Some(true));
}

#[test]
fn test_encode_response_basic() {
    let response = ChatResponse {
        id: "msg_abc123".to_string(),
        object: "chat.completion".to_string(),
        created: Some(1234567890),
        model: "claude-3-sonnet".to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message {
                role: "assistant".to_string(),
                content: Some(serde_json::Value::String("Hello! How can I help?".to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            finish_reason: Some("stop".to_string()),
        }],
        usage: Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 8,
            total_tokens: 18,
        }),
    };

    let encoded = AnthropicCodec::encode_response(&response).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&encoded).unwrap();

    assert_eq!(parsed["type"], "message");
    assert_eq!(parsed["role"], "assistant");
    assert_eq!(parsed["model"], "claude-3-sonnet");
    assert_eq!(parsed["id"], "msg_abc123");

    // content 是 blocks 数组
    assert_eq!(parsed["content"][0]["type"], "text");
    assert_eq!(parsed["content"][0]["text"], "Hello! How can I help?");

    // stop_reason 映射：stop → end_turn
    assert_eq!(parsed["stop_reason"], "end_turn");

    // usage 映射：prompt_tokens → input_tokens
    assert_eq!(parsed["usage"]["input_tokens"], 10);
    assert_eq!(parsed["usage"]["output_tokens"], 8);
}

#[test]
fn test_encode_response_length_stop_reason() {
    let response = ChatResponse {
        id: "msg_test".to_string(),
        object: "chat.completion".to_string(),
        created: None,
        model: "claude-3-sonnet".to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message {
                role: "assistant".to_string(),
                content: Some(serde_json::Value::String("truncated".to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            finish_reason: Some("length".to_string()),
        }],
        usage: None,
    };

    let encoded = AnthropicCodec::encode_response(&response).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(parsed["stop_reason"], "max_tokens");
}

#[test]
fn test_encode_stream_event_first() {
    // 第一个事件应生成 message_start + content_block_start + content_block_delta
    let event = SseEvent {
        event_type: "message".to_string(),
        data: r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}"#.to_string(),
        id: None,
    };

    let events = AnthropicCodec::encode_stream_event(&event, true).unwrap();

    // 至少应有 message_start, content_block_start, content_block_delta
    assert!(events.len() >= 3, "expected at least 3 events, got {}", events.len());
    assert_eq!(events[0].event_type, "message_start");
    assert_eq!(events[1].event_type, "content_block_start");
    assert_eq!(events[2].event_type, "content_block_delta");

    // 验证 content_block_delta 包含文本
    let delta: serde_json::Value = serde_json::from_str(&events[2].data).unwrap();
    assert_eq!(delta["delta"]["text"], "Hello");
}

#[test]
fn test_encode_stream_event_middle() {
    // 中间事件只生成 content_block_delta
    let event = SseEvent {
        event_type: "message".to_string(),
        data: r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","model":"gpt-4","choices":[{"index":0,"delta":{"content":"world"},"finish_reason":null}]}"#.to_string(),
        id: None,
    };

    let events = AnthropicCodec::encode_stream_event(&event, false).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "content_block_delta");

    let delta: serde_json::Value = serde_json::from_str(&events[0].data).unwrap();
    assert_eq!(delta["delta"]["text"], "world");
}

#[test]
fn test_encode_stream_event_done() {
    // [DONE] 事件 → message_delta + message_stop
    let event = SseEvent {
        event_type: "message".to_string(),
        data: "[DONE]".to_string(),
        id: None,
    };

    let events = AnthropicCodec::encode_stream_event(&event, false).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, "message_delta");
    assert_eq!(events[1].event_type, "message_stop");
}

#[test]
fn test_encode_stream_event_empty_content() {
    // 空 content 的事件（如 role-only delta）不应生成 content_block_delta
    let event = SseEvent {
        event_type: "message".to_string(),
        data: r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#.to_string(),
        id: None,
    };

    let events = AnthropicCodec::encode_stream_event(&event, false).unwrap();
    assert!(events.is_empty(), "empty content should produce no events");
}

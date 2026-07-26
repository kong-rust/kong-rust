//! Anthropic Driver 测试 — provider 侧格式转换

use kong_ai::codec::{ChatRequest, Message, SseEvent};
use kong_ai::models::{AiModel, AiProviderConfig, AuthConfig};
use kong_ai::provider::anthropic::AnthropicDriver;
use kong_ai::provider::AiDriver;
use kong_ai::usage::normalizer::{UsageAccumulator, UsageObservation};
use std::collections::HashMap;

fn make_chat_request(stream: bool) -> ChatRequest {
    ChatRequest {
        model: "claude-3-sonnet-20240229".to_string(),
        messages: vec![
            Message {
                role: "system".to_string(),
                content: Some(serde_json::Value::String(
                    "You are a helpful assistant.".to_string(),
                )),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: "user".to_string(),
                content: Some(serde_json::Value::String("Hello".to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ],
        temperature: Some(0.7),
        max_tokens: Some(1024),
        top_p: None,
        top_k: None,
        stream: if stream { Some(true) } else { None },
        stream_options: None,
        tools: None,
        tool_choice: None,
        extra: HashMap::new(),
    }
}

fn make_model() -> AiModel {
    AiModel {
        model_name: "claude-3-sonnet-20240229".to_string(),
        ..Default::default()
    }
}

fn make_provider_config(api_key: &str) -> AiProviderConfig {
    let auth = AuthConfig {
        header_value: Some(api_key.to_string()),
        ..Default::default()
    };
    AiProviderConfig {
        provider_type: "anthropic".to_string(),
        auth_config: serde_json::to_value(auth).unwrap(),
        ..Default::default()
    }
}

#[test]
fn test_anthropic_provider_type() {
    let driver = AnthropicDriver;
    assert_eq!(driver.provider_type(), "anthropic");
}

#[test]
fn test_anthropic_transform_request_system_extraction() {
    // system 消息应提取为 top-level system 字段
    let driver = AnthropicDriver;
    let request = make_chat_request(false);
    let model = make_model();
    let config = make_provider_config("sk-ant-test");

    let result = driver.transform_request(&request, &model, &config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result.body).unwrap();

    // system 字段应存在
    assert_eq!(parsed["system"], "You are a helpful assistant.");
    // messages 中不应包含 system 消息
    assert_eq!(parsed["messages"].as_array().unwrap().len(), 1);
    assert_eq!(parsed["messages"][0]["role"], "user");
    assert_eq!(parsed["messages"][0]["content"], "Hello");
    // max_tokens 必填
    assert_eq!(parsed["max_tokens"], 1024);
    assert_eq!(parsed["model"], "claude-3-sonnet-20240229");
}

#[test]
fn test_anthropic_transform_request_default_max_tokens() {
    // 未设置 max_tokens 时应默认 4096
    let driver = AnthropicDriver;
    let mut request = make_chat_request(false);
    request.max_tokens = None;
    let model = make_model();
    let config = make_provider_config("sk-ant-test");

    let result = driver.transform_request(&request, &model, &config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result.body).unwrap();
    assert_eq!(parsed["max_tokens"], 4096);
}

#[test]
fn test_anthropic_transform_request_stream() {
    let driver = AnthropicDriver;
    let request = make_chat_request(true);
    let model = make_model();
    let config = make_provider_config("sk-ant-test");

    let result = driver.transform_request(&request, &model, &config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result.body).unwrap();
    assert_eq!(parsed["stream"], true);
}

#[test]
fn test_anthropic_transform_response() {
    let driver = AnthropicDriver;
    let model = make_model();
    let headers = HashMap::new();

    let body = r#"{
        "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "Hello! How can I help you today?"}],
        "model": "claude-3-sonnet-20240229",
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 12,
            "output_tokens": 10
        }
    }"#;

    let response = driver
        .transform_response(200, &headers, body, &model)
        .unwrap();

    assert_eq!(response.id, "msg_01XFDUDYJgAACzvnptvVoYEL");
    assert_eq!(response.object, "chat.completion");
    assert_eq!(response.model, "claude-3-sonnet-20240229");
    assert_eq!(response.choices.len(), 1);
    assert_eq!(response.choices[0].message.role, "assistant");
    assert_eq!(
        response.choices[0].message.content,
        Some(serde_json::Value::String(
            "Hello! How can I help you today?".to_string()
        ))
    );
    // stop_reason 映射：end_turn → stop
    assert_eq!(response.choices[0].finish_reason, Some("stop".to_string()));

    let usage = response.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 12);
    assert_eq!(usage.completion_tokens, 10);
    assert_eq!(usage.total_tokens, 22);
}

#[test]
fn test_anthropic_transform_response_error_status() {
    let driver = AnthropicDriver;
    let model = make_model();
    let headers = HashMap::new();

    let result = driver.transform_response(
        429,
        &headers,
        r#"{"error":{"type":"rate_limit_error","message":"Rate limited"}}"#,
        &model,
    );
    assert!(result.is_err());
}

#[test]
fn test_anthropic_transform_stream_event_message_start() {
    let driver = AnthropicDriver;
    let model = make_model();

    let event = SseEvent {
        event_type: "message_start".to_string(),
        data: r#"{"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","content":[],"model":"claude-3-sonnet-20240229","stop_reason":null,"usage":{"input_tokens":25,"output_tokens":1}}}"#.to_string(),
        id: None,
    };

    let result = driver.transform_stream_event(&event, &model).unwrap();
    assert!(result.is_some());

    let transformed = result.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&transformed.data).unwrap();
    assert_eq!(parsed["object"], "chat.completion.chunk");
    assert_eq!(parsed["choices"][0]["delta"]["role"], "assistant");
}

#[test]
fn test_anthropic_transform_stream_event_content_block_delta() {
    let driver = AnthropicDriver;
    let model = make_model();

    let event = SseEvent {
        event_type: "content_block_delta".to_string(),
        data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#.to_string(),
        id: None,
    };

    let result = driver.transform_stream_event(&event, &model).unwrap();
    assert!(result.is_some());

    let transformed = result.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&transformed.data).unwrap();
    assert_eq!(parsed["choices"][0]["delta"]["content"], "Hello");
}

#[test]
fn test_anthropic_transform_stream_event_message_delta() {
    let driver = AnthropicDriver;
    let model = make_model();

    let event = SseEvent {
        event_type: "message_delta".to_string(),
        data: r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}"#.to_string(),
        id: None,
    };

    let result = driver.transform_stream_event(&event, &model).unwrap();
    assert!(result.is_some());

    let transformed = result.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&transformed.data).unwrap();
    assert_eq!(parsed["choices"][0]["finish_reason"], "stop");
}

#[test]
fn test_anthropic_transform_stream_event_message_stop() {
    // message_stop 事件应返回 None（触发 [DONE]）
    let driver = AnthropicDriver;
    let model = make_model();

    let event = SseEvent {
        event_type: "message_stop".to_string(),
        data: r#"{"type":"message_stop"}"#.to_string(),
        id: None,
    };

    let result = driver.transform_stream_event(&event, &model).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_anthropic_transform_stream_event_content_block_start() {
    // content_block_start 应被跳过
    let driver = AnthropicDriver;
    let model = make_model();

    let event = SseEvent {
        event_type: "content_block_start".to_string(),
        data:
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#
                .to_string(),
        id: None,
    };

    let result = driver.transform_stream_event(&event, &model).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_anthropic_configure_upstream() {
    let driver = AnthropicDriver;
    let model = make_model();
    let config = make_provider_config("sk-ant-api-key-123");

    let upstream = driver.configure_upstream(&model, &config, false).unwrap();

    assert_eq!(upstream.scheme, "https");
    assert_eq!(upstream.host, "api.anthropic.com");
    assert_eq!(upstream.port, 443);
    assert_eq!(upstream.path, "/v1/messages");

    // 应有 anthropic-version header 和 x-api-key header
    let version_header = upstream
        .headers
        .iter()
        .find(|(k, _)| k == "anthropic-version");
    assert!(version_header.is_some());
    assert_eq!(version_header.unwrap().1, "2023-06-01");

    let api_key_header = upstream.headers.iter().find(|(k, _)| k == "x-api-key");
    assert!(api_key_header.is_some());
    assert_eq!(api_key_header.unwrap().1, "sk-ant-api-key-123");
}

#[test]
fn test_anthropic_extract_usage() {
    let driver = AnthropicDriver;

    let body = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "Hi"}],
        "model": "claude-3-sonnet-20240229",
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 20, "output_tokens": 15}
    }"#;

    let usage = driver.extract_usage(body).unwrap();
    assert_eq!(usage.prompt_tokens, Some(20));
    assert_eq!(usage.completion_tokens, Some(15));
    assert_eq!(
        usage.total_tokens, None,
        "Anthropic 未返回官方 total，派生总量由统一累加器负责"
    );
}

#[test]
fn test_anthropic_extract_usage_includes_cache_tokens_in_prompt() {
    let driver = AnthropicDriver;
    let body = serde_json::json!({
        "usage": {
            "input_tokens": 20,
            "cache_creation_input_tokens": 7,
            "cache_read_input_tokens": 5,
            "output_tokens": 15
        }
    })
    .to_string();

    let usage = driver.extract_usage(&body).unwrap();
    assert_eq!(usage.prompt_tokens, Some(32));
    assert_eq!(usage.completion_tokens, Some(15));
    assert_eq!(usage.cache_write_input_tokens, Some(7));
    assert_eq!(usage.cache_read_input_tokens, Some(5));
    assert!(!usage.invalid);
}

#[test]
fn test_anthropic_cache_token_sum_overflow_invalidates_whole_usage() {
    let driver = AnthropicDriver;
    let body = serde_json::json!({
        "usage": {
            "input_tokens": i64::MAX,
            "cache_creation_input_tokens": 1,
            "cache_read_input_tokens": 0,
            "output_tokens": 1
        }
    })
    .to_string();

    let usage = driver.extract_usage(&body).unwrap();
    assert!(usage.invalid);
    assert_eq!(usage.prompt_tokens, None);
    assert_eq!(usage.completion_tokens, None);
    assert_eq!(usage.total_tokens, None);
    assert_eq!(usage.cache_write_input_tokens, None);
    assert_eq!(usage.cache_read_input_tokens, None);
}

#[test]
fn test_anthropic_extract_stream_usage_message_start() {
    let driver = AnthropicDriver;

    let event = SseEvent {
        event_type: "message_start".to_string(),
        data: r#"{"type":"message_start","message":{"id":"msg_123","type":"message","role":"assistant","content":[],"model":"claude-3-sonnet","stop_reason":null,"usage":{"input_tokens":25,"output_tokens":1}}}"#.to_string(),
        id: None,
    };

    let usage = driver.extract_stream_usage(&event).unwrap();
    assert_eq!(usage.prompt_tokens, Some(25));
    assert_eq!(
        usage.completion_tokens, None,
        "message_start 的初始 output_tokens 不是最终 completion"
    );
    assert_eq!(usage.total_tokens, None);
}

#[test]
fn test_anthropic_extract_stream_usage_message_delta() {
    let driver = AnthropicDriver;

    let event = SseEvent {
        event_type: "message_delta".to_string(),
        data: r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":42}}"#.to_string(),
        id: None,
    };

    let usage = driver.extract_stream_usage(&event).unwrap();
    assert_eq!(usage.completion_tokens, Some(42));
}

#[test]
fn test_anthropic_stream_latest_message_delta_replaces_completion_snapshot() {
    let driver = AnthropicDriver;
    let start = SseEvent {
        event_type: "message_start".to_string(),
        data: r#"{"type":"message_start","message":{"usage":{"input_tokens":25,"cache_creation_input_tokens":2,"cache_read_input_tokens":3,"output_tokens":1}}}"#.to_string(),
        id: None,
    };
    let first_delta = SseEvent {
        event_type: "message_delta".to_string(),
        data: r#"{"type":"message_delta","usage":{"output_tokens":15}}"#.to_string(),
        id: None,
    };
    let final_delta = SseEvent {
        event_type: "message_delta".to_string(),
        data: r#"{"type":"message_delta","usage":{"output_tokens":42}}"#.to_string(),
        id: None,
    };

    let mut accumulator = UsageAccumulator::default();
    for event in [&start, &first_delta, &final_delta] {
        let usage = driver.extract_stream_usage(event).unwrap();
        assert!(!usage.invalid);
        accumulator.observe_provider(UsageObservation {
            prompt_tokens: usage.prompt_tokens.map(|value| value as i64),
            completion_tokens: usage.completion_tokens.map(|value| value as i64),
            total_tokens: usage.total_tokens.map(|value| value as i64),
            reasoning_tokens: usage.reasoning_tokens.map(|value| value as i64),
            cache_read_input_tokens: usage.cache_read_input_tokens.map(|value| value as i64),
            cache_write_input_tokens: usage.cache_write_input_tokens.map(|value| value as i64),
            ..Default::default()
        });
    }

    let normalized = accumulator.finish(true, true);
    assert_eq!(normalized.prompt_tokens.unwrap().value, 30);
    assert_eq!(
        normalized.completion_tokens.unwrap().value,
        42,
        "最后一个 message_delta 必须替换此前 completion 快照"
    );
    assert_eq!(normalized.total_tokens.unwrap().value, 72);
    assert_eq!(normalized.cache_write_input_tokens, Some(2));
    assert_eq!(normalized.cache_read_input_tokens, Some(3));
}

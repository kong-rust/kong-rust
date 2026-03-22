//! Tool Call 格式转换测试 — tools 字段保留、Anthropic 编解码

use kong_ai::codec::anthropic_format::AnthropicCodec;
use kong_ai::codec::{
    ChatRequest, ChatResponse, Choice, FunctionCall, FunctionDef, Message, SseEvent, Tool,
    ToolCall, Usage,
};
use kong_ai::models::{AiModel, AiProviderConfig, AuthConfig};
use kong_ai::provider::openai::OpenAiDriver;
use kong_ai::provider::AiDriver;
use std::collections::HashMap;

fn make_model() -> AiModel {
    AiModel {
        model_name: "gpt-4".to_string(),
        ..Default::default()
    }
}

fn make_provider_config() -> AiProviderConfig {
    AiProviderConfig {
        provider_type: "openai".to_string(),
        auth_config: serde_json::to_value(AuthConfig {
            header_value: Some("sk-test".to_string()),
            ..Default::default()
        })
        .unwrap(),
        ..Default::default()
    }
}

// ============ OpenAI: tools 字段保留 ============

#[test]
fn test_openai_request_with_tools() {
    let driver = OpenAiDriver;
    let model = make_model();
    let config = make_provider_config();

    let request = ChatRequest {
        model: "gpt-4".to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: Some(serde_json::Value::String(
                "What's the weather in SF?".to_string(),
            )),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        temperature: None,
        max_tokens: None,
        top_p: None,
        top_k: None,
        stream: None,
        stream_options: None,
        tools: Some(vec![Tool {
            tool_type: "function".to_string(),
            function: FunctionDef {
                name: "get_weather".to_string(),
                description: Some("Get weather".to_string()),
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"location": {"type": "string"}},
                    "required": ["location"]
                })),
            },
        }]),
        tool_choice: Some(serde_json::json!("auto")),
        extra: HashMap::new(),
    };

    let result = driver
        .transform_request(&request, &model, &config)
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result.body).unwrap();

    assert!(parsed["tools"].is_array(), "tools 字段应保留");
    assert_eq!(parsed["tools"][0]["type"], "function");
    assert_eq!(parsed["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(parsed["tool_choice"], "auto");
}

// ============ OpenAI: tool_calls 在消息中保留 ============

#[test]
fn test_openai_request_with_tool_calls_in_message() {
    let driver = OpenAiDriver;
    let model = make_model();
    let config = make_provider_config();

    let request = ChatRequest {
        model: "gpt-4".to_string(),
        messages: vec![
            Message {
                role: "user".to_string(),
                content: Some(serde_json::Value::String("Weather?".to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_123".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "get_weather".to_string(),
                        arguments: r#"{"location":"SF"}"#.to_string(),
                    },
                }]),
                tool_call_id: None,
                name: None,
            },
            Message {
                role: "tool".to_string(),
                content: Some(serde_json::Value::String("72F, sunny".to_string())),
                tool_calls: None,
                tool_call_id: Some("call_123".to_string()),
                name: None,
            },
        ],
        temperature: None,
        max_tokens: None,
        top_p: None,
        top_k: None,
        stream: None,
        stream_options: None,
        tools: None,
        tool_choice: None,
        extra: HashMap::new(),
    };

    let result = driver
        .transform_request(&request, &model, &config)
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result.body).unwrap();

    assert!(parsed["messages"][1]["tool_calls"].is_array());
    assert_eq!(
        parsed["messages"][1]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(parsed["messages"][2]["tool_call_id"], "call_123");
}

// ============ Anthropic: finish_reason 映射 ============

#[test]
fn test_anthropic_codec_encode_tool_call_response() {
    let response = ChatResponse {
        id: "chatcmpl-abc".to_string(),
        object: "chat.completion".to_string(),
        created: None,
        model: "gpt-4".to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message {
                role: "assistant".to_string(),
                content: Some(serde_json::Value::String("Checking.".to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            finish_reason: Some("tool_calls".to_string()),
        }],
        usage: Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
        }),
    };

    let encoded = AnthropicCodec::encode_response(&response).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&encoded).unwrap();

    // tool_calls → tool_use
    assert_eq!(parsed["stop_reason"], "tool_use");
    assert_eq!(parsed["role"], "assistant");
    assert!(parsed["content"].is_array());
}

// ============ Anthropic: decode 字段保留 ============

#[test]
fn test_anthropic_codec_decode_preserves_fields() {
    let body = serde_json::json!({
        "model": "claude-3-opus",
        "max_tokens": 2048,
        "messages": [{"role": "user", "content": "Hello"}],
        "system": "You are helpful.",
        "temperature": 0.5,
        "top_p": 0.9,
        "top_k": 40
    })
    .to_string();

    let req = AnthropicCodec::decode_request(&body).unwrap();
    assert_eq!(req.model, "claude-3-opus");
    assert_eq!(req.max_tokens, Some(2048));
    assert_eq!(req.temperature, Some(0.5));
    assert_eq!(req.top_p, Some(0.9));
    assert_eq!(req.top_k, Some(40));
    assert_eq!(req.messages[0].role, "system");
}

// ============ Anthropic: content blocks 拼接 ============

#[test]
fn test_anthropic_codec_decode_content_blocks() {
    let body = serde_json::json!({
        "model": "claude-3-opus",
        "max_tokens": 1024,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "Hello "},
                {"type": "text", "text": "World"}
            ]
        }]
    })
    .to_string();

    let req = AnthropicCodec::decode_request(&body).unwrap();
    let content = req.messages[0]
        .content
        .as_ref()
        .unwrap()
        .as_str()
        .unwrap();
    assert_eq!(content, "Hello World");
}

// ============ Anthropic: encode usage 映射 ============

#[test]
fn test_anthropic_codec_encode_response_usage_mapping() {
    let response = ChatResponse {
        id: "msg-123".to_string(),
        object: "chat.completion".to_string(),
        created: None,
        model: "claude-3".to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message {
                role: "assistant".to_string(),
                content: Some(serde_json::Value::String("Hello!".to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            finish_reason: Some("stop".to_string()),
        }],
        usage: Some(Usage {
            prompt_tokens: 25,
            completion_tokens: 10,
            total_tokens: 35,
        }),
    };

    let encoded = AnthropicCodec::encode_response(&response).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&encoded).unwrap();

    assert_eq!(parsed["usage"]["input_tokens"], 25);
    assert_eq!(parsed["usage"]["output_tokens"], 10);
    assert_eq!(parsed["stop_reason"], "end_turn");
}

// ============ Anthropic: encode [DONE] stream event ============

#[test]
fn test_anthropic_codec_encode_stream_done_event() {
    let done = SseEvent {
        event_type: "message".to_string(),
        data: "[DONE]".to_string(),
        id: None,
    };
    let events = AnthropicCodec::encode_stream_event(&done, false).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, "message_delta");
    assert_eq!(events[1].event_type, "message_stop");
}

// ============ Anthropic: encode first stream event ============

#[test]
fn test_anthropic_codec_encode_stream_first_event() {
    let first = SseEvent {
        event_type: "message".to_string(),
        data: serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion.chunk",
            "model": "gpt-4",
            "choices": [{"index": 0, "delta": {"content": "Hi"}, "finish_reason": null}]
        })
        .to_string(),
        id: None,
    };
    let events = AnthropicCodec::encode_stream_event(&first, true).unwrap();
    // 第一个事件: message_start + content_block_start + content_block_delta
    assert!(events.len() >= 3, "第一个事件应产出至少 3 个 Anthropic 事件");
    assert_eq!(events[0].event_type, "message_start");
    assert_eq!(events[1].event_type, "content_block_start");
    assert_eq!(events[2].event_type, "content_block_delta");
}

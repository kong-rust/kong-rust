//! OpenAI Driver 测试

use kong_ai::codec::{ChatRequest, Message, SseEvent};
use kong_ai::models::{AiModel, AiProviderConfig, AuthConfig};
use kong_ai::provider::openai::OpenAiDriver;
use kong_ai::provider::AiDriver;
use std::collections::HashMap;

/// 构造测试用 ChatRequest
fn make_chat_request(stream: bool) -> ChatRequest {
    ChatRequest {
        model: "gpt-4".to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: Some(serde_json::Value::String("Hello".to_string())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        temperature: Some(0.7),
        max_tokens: Some(100),
        top_p: None,
        top_k: None,
        stream: if stream { Some(true) } else { None },
        stream_options: None,
        tools: None,
        tool_choice: None,
        extra: HashMap::new(),
    }
}

/// 构造测试用 AiModel
fn make_model() -> AiModel {
    AiModel {
        model_name: "gpt-4".to_string(),
        ..Default::default()
    }
}

/// 构造测试用 AiProviderConfig
fn make_provider_config(api_key: &str) -> AiProviderConfig {
    let auth = AuthConfig {
        header_value: Some(api_key.to_string()),
        ..Default::default()
    };
    AiProviderConfig {
        provider_type: "openai".to_string(),
        auth_config: serde_json::to_value(auth).unwrap(),
        ..Default::default()
    }
}

#[test]
fn test_openai_transform_request_chat() {
    let driver = OpenAiDriver;
    let request = make_chat_request(false);
    let model = make_model();
    let config = make_provider_config("sk-test-key");

    let result = driver.transform_request(&request, &model, &config).unwrap();

    assert_eq!(result.content_type, "application/json");

    // 验证 JSON 序列化结果
    let parsed: serde_json::Value = serde_json::from_str(&result.body).unwrap();
    assert_eq!(parsed["model"], "gpt-4");
    assert_eq!(parsed["messages"][0]["role"], "user");
    assert_eq!(parsed["messages"][0]["content"], "Hello");
    assert_eq!(parsed["temperature"], 0.7);
    assert_eq!(parsed["max_tokens"], 100);
    // 非流式请求不应该有 stream_options
    assert!(parsed.get("stream_options").is_none() || parsed["stream_options"].is_null());
}

#[test]
fn test_openai_transform_request_stream_options_injection() {
    // 流式请求应自动注入 stream_options.include_usage = true
    let driver = OpenAiDriver;
    let request = make_chat_request(true);
    let model = make_model();
    let config = make_provider_config("sk-test-key");

    let result = driver.transform_request(&request, &model, &config).unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&result.body).unwrap();
    assert_eq!(parsed["stream"], true);
    assert_eq!(parsed["stream_options"]["include_usage"], true);
}

#[test]
fn test_openai_transform_response() {
    let driver = OpenAiDriver;
    let model = make_model();
    let headers = HashMap::new();

    let body = r#"{
        "id": "chatcmpl-abc123",
        "object": "chat.completion",
        "created": 1234567890,
        "model": "gpt-4",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "Hello! How can I help?"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 8,
            "total_tokens": 18
        }
    }"#;

    let response = driver.transform_response(200, &headers, body, &model).unwrap();

    assert_eq!(response.id, "chatcmpl-abc123");
    assert_eq!(response.object, "chat.completion");
    assert_eq!(response.model, "gpt-4");
    assert_eq!(response.choices.len(), 1);
    assert_eq!(response.choices[0].message.role, "assistant");
    assert_eq!(
        response.choices[0].message.content,
        Some(serde_json::Value::String("Hello! How can I help?".to_string()))
    );
    assert_eq!(response.choices[0].finish_reason, Some("stop".to_string()));

    let usage = response.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 8);
    assert_eq!(usage.total_tokens, 18);
}

#[test]
fn test_openai_transform_response_error_status() {
    // 非 200 状态应返回错误
    let driver = OpenAiDriver;
    let model = make_model();
    let headers = HashMap::new();

    let result = driver.transform_response(
        429,
        &headers,
        r#"{"error":{"message":"Rate limit exceeded"}}"#,
        &model,
    );
    assert!(result.is_err());
}

#[test]
fn test_openai_configure_upstream() {
    let driver = OpenAiDriver;
    let model = make_model();
    let config = make_provider_config("sk-test-key-123");

    let upstream = driver.configure_upstream(&model, &config).unwrap();

    assert_eq!(upstream.scheme, "https");
    assert_eq!(upstream.host, "api.openai.com");
    assert_eq!(upstream.port, 443);
    assert_eq!(upstream.path, "/v1/chat/completions");

    // 验证 auth header
    assert_eq!(upstream.headers.len(), 1);
    assert_eq!(upstream.headers[0].0, "Authorization");
    assert_eq!(upstream.headers[0].1, "Bearer sk-test-key-123");
}

#[test]
fn test_openai_configure_upstream_custom_endpoint() {
    // 自定义 endpoint URL
    let driver = OpenAiDriver;
    let model = make_model();
    let mut config = make_provider_config("sk-test");
    config.endpoint_url = Some("https://custom-openai.example.com:8443/v2/chat".to_string());

    let upstream = driver.configure_upstream(&model, &config).unwrap();

    assert_eq!(upstream.scheme, "https");
    assert_eq!(upstream.host, "custom-openai.example.com");
    assert_eq!(upstream.port, 8443);
    assert_eq!(upstream.path, "/v2/chat");
}

#[test]
fn test_openai_configure_upstream_bearer_prefix() {
    // 如果 header_value 已经带 "Bearer " 前缀，不应重复添加
    let driver = OpenAiDriver;
    let model = make_model();
    let auth = AuthConfig {
        header_value: Some("Bearer sk-already-prefixed".to_string()),
        ..Default::default()
    };
    let config = AiProviderConfig {
        auth_config: serde_json::to_value(auth).unwrap(),
        ..Default::default()
    };

    let upstream = driver.configure_upstream(&model, &config).unwrap();
    assert_eq!(upstream.headers[0].1, "Bearer sk-already-prefixed");
}

#[test]
fn test_openai_extract_usage() {
    let driver = OpenAiDriver;

    let body = r#"{
        "id": "chatcmpl-abc",
        "object": "chat.completion",
        "model": "gpt-4",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hi"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 15, "completion_tokens": 20, "total_tokens": 35}
    }"#;

    let usage = driver.extract_usage(body).unwrap();
    assert_eq!(usage.prompt_tokens, Some(15));
    assert_eq!(usage.completion_tokens, Some(20));
    assert_eq!(usage.total_tokens, Some(35));
}

#[test]
fn test_openai_extract_usage_no_usage() {
    // 没有 usage 字段
    let driver = OpenAiDriver;
    let body = r#"{
        "id": "chatcmpl-abc",
        "object": "chat.completion",
        "model": "gpt-4",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hi"}, "finish_reason": "stop"}]
    }"#;

    let usage = driver.extract_usage(body);
    assert!(usage.is_none());
}

#[test]
fn test_openai_transform_stream_event() {
    let driver = OpenAiDriver;
    let model = make_model();

    // 正常流式 chunk
    let event = SseEvent {
        event_type: "message".to_string(),
        data: r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#.to_string(),
        id: None,
    };

    let result = driver.transform_stream_event(&event, &model).unwrap();
    assert!(result.is_some());
    let output = result.unwrap();
    assert_eq!(output.data, event.data);
}

#[test]
fn test_openai_transform_stream_event_done() {
    // [DONE] 事件应返回 None
    let driver = OpenAiDriver;
    let model = make_model();

    let event = SseEvent {
        event_type: "message".to_string(),
        data: "[DONE]".to_string(),
        id: None,
    };

    let result = driver.transform_stream_event(&event, &model).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_openai_extract_stream_usage() {
    let driver = OpenAiDriver;

    // 带 usage 的最终 chunk
    let event = SseEvent {
        event_type: "message".to_string(),
        data: r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"gpt-4","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#.to_string(),
        id: None,
    };

    let usage = driver.extract_stream_usage(&event).unwrap();
    assert_eq!(usage.prompt_tokens, Some(10));
    assert_eq!(usage.completion_tokens, Some(5));
    assert_eq!(usage.total_tokens, Some(15));
}

#[test]
fn test_openai_extract_stream_usage_done() {
    // [DONE] 事件不应有 usage
    let driver = OpenAiDriver;

    let event = SseEvent {
        event_type: "message".to_string(),
        data: "[DONE]".to_string(),
        id: None,
    };

    let usage = driver.extract_stream_usage(&event);
    assert!(usage.is_none());
}

#[test]
fn test_openai_provider_type() {
    let driver = OpenAiDriver;
    assert_eq!(driver.provider_type(), "openai");
}

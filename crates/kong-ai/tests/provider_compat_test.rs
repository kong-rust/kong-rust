//! OpenAI Compat Driver 测试 — 兼容模式 provider

use kong_ai::codec::{ChatRequest, Message, SseEvent};
use kong_ai::models::{AiModel, AiProviderConfig, AuthConfig};
use kong_ai::provider::openai_compat::OpenAiCompatDriver;
use kong_ai::provider::AiDriver;
use std::collections::HashMap;

fn make_chat_request() -> ChatRequest {
    ChatRequest {
        model: "qwen-turbo".to_string(),
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
        stream: None,
        stream_options: None,
        tools: None,
        tool_choice: None,
        extra: HashMap::new(),
    }
}

fn make_model() -> AiModel {
    AiModel {
        model_name: "qwen-turbo".to_string(),
        ..Default::default()
    }
}

fn make_provider_config_with_endpoint(api_key: &str, endpoint: &str) -> AiProviderConfig {
    let auth = AuthConfig {
        header_value: Some(api_key.to_string()),
        ..Default::default()
    };
    AiProviderConfig {
        provider_type: "openai_compat".to_string(),
        auth_config: serde_json::to_value(auth).unwrap(),
        endpoint_url: Some(endpoint.to_string()),
        ..Default::default()
    }
}

#[test]
fn test_compat_provider_type() {
    let driver = OpenAiCompatDriver::new();
    assert_eq!(driver.provider_type(), "openai_compat");
}

#[test]
fn test_compat_configure_upstream_uses_custom_endpoint() {
    // 核心测试：兼容模式应使用自定义 endpoint
    let driver = OpenAiCompatDriver::new();
    let model = make_model();
    let config = make_provider_config_with_endpoint(
        "sk-qwen-key",
        "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
    );

    let upstream = driver.configure_upstream(&model, &config).unwrap();

    assert_eq!(upstream.scheme, "https");
    assert_eq!(upstream.host, "dashscope.aliyuncs.com");
    assert_eq!(upstream.port, 443);
    assert_eq!(
        upstream.path,
        "/compatible-mode/v1/chat/completions"
    );
}

#[test]
fn test_compat_configure_upstream_requires_endpoint() {
    // 未设置 endpoint_url 应报错
    let driver = OpenAiCompatDriver::new();
    let model = make_model();
    let auth = AuthConfig {
        header_value: Some("sk-test".to_string()),
        ..Default::default()
    };
    let config = AiProviderConfig {
        provider_type: "openai_compat".to_string(),
        auth_config: serde_json::to_value(auth).unwrap(),
        endpoint_url: None, // 无 endpoint
        ..Default::default()
    };

    let result = driver.configure_upstream(&model, &config);
    assert!(result.is_err());
}

#[test]
fn test_compat_transform_request_delegates_to_openai() {
    // 格式转换应与 OpenAI 一致
    let driver = OpenAiCompatDriver::new();
    let request = make_chat_request();
    let model = make_model();
    let config = make_provider_config_with_endpoint(
        "sk-test",
        "https://example.com/v1/chat/completions",
    );

    let result = driver
        .transform_request(&request, &model, &config)
        .unwrap();

    assert_eq!(result.content_type, "application/json");
    let parsed: serde_json::Value = serde_json::from_str(&result.body).unwrap();
    assert_eq!(parsed["model"], "qwen-turbo");
    assert_eq!(parsed["messages"][0]["role"], "user");
}

#[test]
fn test_compat_transform_response_delegates_to_openai() {
    let driver = OpenAiCompatDriver::new();
    let model = make_model();
    let headers = HashMap::new();

    let body = r#"{
        "id": "chatcmpl-qwen-123",
        "object": "chat.completion",
        "model": "qwen-turbo",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Hello from Qwen!"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 4, "total_tokens": 9}
    }"#;

    let response = driver.transform_response(200, &headers, body, &model).unwrap();
    assert_eq!(response.model, "qwen-turbo");
    assert_eq!(response.choices[0].finish_reason, Some("stop".to_string()));
}

#[test]
fn test_compat_extract_usage() {
    let driver = OpenAiCompatDriver::new();

    let body = r#"{
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "model": "qwen-turbo",
        "choices": [{"index":0,"message":{"role":"assistant","content":"Hi"},"finish_reason":"stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    }"#;

    let usage = driver.extract_usage(body).unwrap();
    assert_eq!(usage.prompt_tokens, Some(10));
    assert_eq!(usage.completion_tokens, Some(5));
    assert_eq!(usage.total_tokens, Some(15));
}

#[test]
fn test_compat_transform_stream_event() {
    let driver = OpenAiCompatDriver::new();
    let model = make_model();

    let event = SseEvent {
        event_type: "message".to_string(),
        data: r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"qwen-turbo","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#.to_string(),
        id: None,
    };

    let result = driver.transform_stream_event(&event, &model).unwrap();
    assert!(result.is_some());
}

#[test]
fn test_compat_hunyuan_endpoint() {
    // 混元 endpoint 测试
    let driver = OpenAiCompatDriver::new();
    let model = AiModel {
        model_name: "hunyuan-lite".to_string(),
        ..Default::default()
    };
    let config = make_provider_config_with_endpoint(
        "sk-hunyuan-key",
        "https://hunyuan.tencentcloudapi.com/v1/chat/completions",
    );

    let upstream = driver.configure_upstream(&model, &config).unwrap();
    assert_eq!(upstream.host, "hunyuan.tencentcloudapi.com");
    assert_eq!(upstream.path, "/v1/chat/completions");
}

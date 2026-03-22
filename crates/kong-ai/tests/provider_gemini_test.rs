//! Gemini Driver 测试 — Google Generative AI API 格式转换

use kong_ai::codec::{ChatRequest, Message, SseEvent};
use kong_ai::models::{AiModel, AiProviderConfig, AuthConfig};
use kong_ai::provider::gemini::GeminiDriver;
use kong_ai::provider::AiDriver;
use std::collections::HashMap;

fn make_chat_request() -> ChatRequest {
    ChatRequest {
        model: "gemini-1.5-pro".to_string(),
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
        stream: None,
        stream_options: None,
        tools: None,
        tool_choice: None,
        extra: HashMap::new(),
    }
}

fn make_model() -> AiModel {
    AiModel {
        model_name: "gemini-1.5-pro".to_string(),
        ..Default::default()
    }
}

fn make_provider_config(api_key: &str) -> AiProviderConfig {
    let auth = AuthConfig {
        header_value: Some(api_key.to_string()),
        ..Default::default()
    };
    AiProviderConfig {
        provider_type: "gemini".to_string(),
        auth_config: serde_json::to_value(auth).unwrap(),
        ..Default::default()
    }
}

#[test]
fn test_gemini_provider_type() {
    let driver = GeminiDriver;
    assert_eq!(driver.provider_type(), "gemini");
}

#[test]
fn test_gemini_transform_request_contents_format() {
    let driver = GeminiDriver;
    let request = make_chat_request();
    let model = make_model();
    let config = make_provider_config("AIza-test-key");

    let result = driver.transform_request(&request, &model, &config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result.body).unwrap();

    // system → systemInstruction
    assert!(parsed["systemInstruction"].is_object());
    assert_eq!(
        parsed["systemInstruction"]["parts"][0]["text"],
        "You are a helpful assistant."
    );

    // contents 中不应包含 system 消息，assistant → model
    let contents = parsed["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0]["role"], "user");
    assert_eq!(contents[0]["parts"][0]["text"], "Hello");

    // generationConfig
    assert_eq!(parsed["generationConfig"]["temperature"], 0.7);
    assert_eq!(parsed["generationConfig"]["maxOutputTokens"], 1024);
}

#[test]
fn test_gemini_transform_request_assistant_to_model() {
    // assistant 角色应映射为 model
    let driver = GeminiDriver;
    let request = ChatRequest {
        model: "gemini-1.5-pro".to_string(),
        messages: vec![
            Message {
                role: "user".to_string(),
                content: Some(serde_json::Value::String("Hi".to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: "assistant".to_string(),
                content: Some(serde_json::Value::String("Hello!".to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: "user".to_string(),
                content: Some(serde_json::Value::String("How are you?".to_string())),
                tool_calls: None,
                tool_call_id: None,
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
    let model = make_model();
    let config = make_provider_config("key");

    let result = driver.transform_request(&request, &model, &config).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result.body).unwrap();

    let contents = parsed["contents"].as_array().unwrap();
    assert_eq!(contents[1]["role"], "model");
}

#[test]
fn test_gemini_transform_response_candidates() {
    let driver = GeminiDriver;
    let model = make_model();
    let headers = HashMap::new();

    let body = r#"{
        "candidates": [{
            "content": {
                "parts": [{"text": "Hello! How can I help you?"}],
                "role": "model"
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 8,
            "totalTokenCount": 18
        },
        "modelVersion": "gemini-1.5-pro-001"
    }"#;

    let response = driver.transform_response(200, &headers, body, &model).unwrap();

    assert_eq!(response.object, "chat.completion");
    assert_eq!(response.model, "gemini-1.5-pro-001");
    assert_eq!(response.choices.len(), 1);
    assert_eq!(
        response.choices[0].message.content,
        Some(serde_json::Value::String(
            "Hello! How can I help you?".to_string()
        ))
    );
    // STOP → stop
    assert_eq!(response.choices[0].finish_reason, Some("stop".to_string()));

    let usage = response.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 8);
    assert_eq!(usage.total_tokens, 18);
}

#[test]
fn test_gemini_transform_response_error_status() {
    let driver = GeminiDriver;
    let model = make_model();
    let headers = HashMap::new();

    let result = driver.transform_response(
        400,
        &headers,
        r#"{"error":{"message":"Invalid request"}}"#,
        &model,
    );
    assert!(result.is_err());
}

#[test]
fn test_gemini_configure_upstream_path_with_model() {
    let driver = GeminiDriver;
    let model = make_model();
    let config = make_provider_config("AIza-test-key");

    // 非流式请求应使用 :generateContent 端点
    let upstream = driver.configure_upstream(&model, &config, false).unwrap();

    assert_eq!(upstream.scheme, "https");
    assert_eq!(upstream.host, "generativelanguage.googleapis.com");
    assert_eq!(upstream.port, 443);
    assert!(upstream.path.contains("gemini-1.5-pro"));
    assert!(upstream.path.contains("generateContent"));
    assert!(!upstream.path.contains("stream"), "非流式请求不应使用 streamGenerateContent");

    // 流式请求应使用 :streamGenerateContent?alt=sse 端点
    let upstream_stream = driver.configure_upstream(&model, &config, true).unwrap();
    assert!(upstream_stream.path.contains("streamGenerateContent"));
    assert!(upstream_stream.path.contains("alt=sse"));

    // 应有 Bearer auth header
    let auth_header = upstream.headers.iter().find(|(k, _)| k == "Authorization");
    assert!(auth_header.is_some());
    assert!(auth_header.unwrap().1.starts_with("Bearer "));
}

#[test]
fn test_gemini_configure_upstream_custom_endpoint() {
    let driver = GeminiDriver;
    let model = make_model();
    let mut config = make_provider_config("AIza-test-key");
    config.endpoint_url = Some("https://custom-gemini.example.com/v1/generate".to_string());

    let upstream = driver.configure_upstream(&model, &config, false).unwrap();
    assert_eq!(upstream.host, "custom-gemini.example.com");
    assert_eq!(upstream.path, "/v1/generate");
}

#[test]
fn test_gemini_extract_usage() {
    let driver = GeminiDriver;

    let body = r#"{
        "candidates": [{"content": {"parts": [{"text": "Hi"}], "role": "model"}, "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 15, "candidatesTokenCount": 5, "totalTokenCount": 20}
    }"#;

    let usage = driver.extract_usage(body).unwrap();
    assert_eq!(usage.prompt_tokens, Some(15));
    assert_eq!(usage.completion_tokens, Some(5));
    assert_eq!(usage.total_tokens, Some(20));
}

#[test]
fn test_gemini_transform_stream_event() {
    let driver = GeminiDriver;
    let model = make_model();

    let event = SseEvent {
        event_type: "message".to_string(),
        data: r#"{
            "candidates": [{"content": {"parts": [{"text": "Hello"}], "role": "model"}, "finishReason": null}],
            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 1, "totalTokenCount": 11}
        }"#.to_string(),
        id: None,
    };

    let result = driver.transform_stream_event(&event, &model).unwrap();
    assert!(result.is_some());

    let transformed = result.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&transformed.data).unwrap();
    assert_eq!(parsed["object"], "chat.completion.chunk");
    assert_eq!(parsed["choices"][0]["delta"]["content"], "Hello");
}

#[test]
fn test_gemini_transform_stream_event_done() {
    let driver = GeminiDriver;
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
fn test_gemini_extract_stream_usage() {
    let driver = GeminiDriver;

    let event = SseEvent {
        event_type: "message".to_string(),
        data: r#"{
            "candidates": [{"content": {"parts": [{"text": "Hi"}], "role": "model"}}],
            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 5, "totalTokenCount": 15}
        }"#.to_string(),
        id: None,
    };

    let usage = driver.extract_stream_usage(&event).unwrap();
    assert_eq!(usage.prompt_tokens, Some(10));
    assert_eq!(usage.completion_tokens, Some(5));
    assert_eq!(usage.total_tokens, Some(15));
}

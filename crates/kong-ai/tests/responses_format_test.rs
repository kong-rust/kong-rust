//! v1/responses 格式转换 + provider function calling 测试
//! 覆盖请求降级、响应升级、流式事件状态机、provider tool_calls

use kong_ai::codec::responses_format::{
    self, ResponsesEventState, ResponsesRequest, StrippedTools,
};
use kong_ai::codec::{
    ChatResponse, Choice, FunctionCall, Message, SseEvent, ToolCall, Usage,
};
use kong_ai::models::{AiModel, AiProviderConfig, AuthConfig};
use kong_ai::provider::anthropic::AnthropicDriver;
use kong_ai::provider::gemini::GeminiDriver;
use kong_ai::provider::AiDriver;
use std::collections::HashMap;

// ============ 请求降级测试 ============

#[test]
fn test_responses_to_chat_string_input() {
    let req: ResponsesRequest = serde_json::from_value(serde_json::json!({
        "model": "gpt-4o",
        "input": "Hello, world!"
    }))
    .unwrap();

    let (chat, stripped) = responses_format::responses_to_chat(&req).unwrap();
    assert_eq!(chat.model, "gpt-4o");
    assert_eq!(chat.messages.len(), 1);
    assert_eq!(chat.messages[0].role, "user");
    assert_eq!(
        chat.messages[0].content,
        Some(serde_json::Value::String("Hello, world!".to_string()))
    );
    assert!(stripped.unsupported.is_empty());
}

#[test]
fn test_responses_to_chat_with_instructions() {
    let req: ResponsesRequest = serde_json::from_value(serde_json::json!({
        "model": "gpt-4o",
        "input": "Hi",
        "instructions": "You are a helpful assistant."
    }))
    .unwrap();

    let (chat, _) = responses_format::responses_to_chat(&req).unwrap();
    assert_eq!(chat.messages.len(), 2);
    assert_eq!(chat.messages[0].role, "system");
    assert_eq!(
        chat.messages[0].content,
        Some(serde_json::Value::String(
            "You are a helpful assistant.".to_string()
        ))
    );
    assert_eq!(chat.messages[1].role, "user");
}

#[test]
fn test_responses_to_chat_array_input() {
    let req: ResponsesRequest = serde_json::from_value(serde_json::json!({
        "model": "gpt-4o",
        "input": [
            {"role": "user", "content": "What is 2+2?"},
            {"role": "assistant", "content": "4"},
            {"role": "user", "content": "And 3+3?"}
        ]
    }))
    .unwrap();

    let (chat, _) = responses_format::responses_to_chat(&req).unwrap();
    assert_eq!(chat.messages.len(), 3);
    assert_eq!(chat.messages[0].role, "user");
    assert_eq!(chat.messages[1].role, "assistant");
    assert_eq!(chat.messages[2].role, "user");
}

#[test]
fn test_responses_to_chat_function_call_output() {
    let req: ResponsesRequest = serde_json::from_value(serde_json::json!({
        "model": "gpt-4o",
        "input": [
            {"role": "user", "content": "What's the weather?"},
            {"type": "function_call_output", "call_id": "call_123", "output": "{\"temp\": 72}"}
        ]
    }))
    .unwrap();

    let (chat, _) = responses_format::responses_to_chat(&req).unwrap();
    assert_eq!(chat.messages.len(), 2);
    assert_eq!(chat.messages[1].role, "tool");
    assert_eq!(
        chat.messages[1].tool_call_id,
        Some("call_123".to_string())
    );
}

#[test]
fn test_responses_to_chat_tools_filtering() {
    let req: ResponsesRequest = serde_json::from_value(serde_json::json!({
        "model": "gpt-4o",
        "input": "Hi",
        "tools": [
            {"type": "function", "function": {"name": "get_weather", "parameters": {}}},
            {"type": "web_search"},
            {"type": "file_search"}
        ]
    }))
    .unwrap();

    let (chat, stripped) = responses_format::responses_to_chat(&req).unwrap();
    assert_eq!(chat.tools.as_ref().unwrap().len(), 1);
    assert_eq!(chat.tools.as_ref().unwrap()[0].function.name, "get_weather");
    assert_eq!(stripped.unsupported, vec!["web_search", "file_search"]);
}

#[test]
fn test_responses_to_chat_background_rejected() {
    let req: ResponsesRequest = serde_json::from_value(serde_json::json!({
        "model": "gpt-4o",
        "input": "Hi",
        "background": true
    }))
    .unwrap();

    let result = responses_format::responses_to_chat(&req);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("background"));
}

#[test]
fn test_responses_to_chat_params_mapping() {
    let req: ResponsesRequest = serde_json::from_value(serde_json::json!({
        "model": "gpt-4o",
        "input": "Hi",
        "temperature": 0.7,
        "top_p": 0.9,
        "max_output_tokens": 1024,
        "stream": true
    }))
    .unwrap();

    let (chat, _) = responses_format::responses_to_chat(&req).unwrap();
    assert_eq!(chat.temperature, Some(0.7));
    assert_eq!(chat.top_p, Some(0.9));
    assert_eq!(chat.max_tokens, Some(1024));
    assert_eq!(chat.stream, Some(true));
}

// ============ 响应升级测试 ============

#[test]
fn test_chat_to_responses_text() {
    let resp = ChatResponse {
        id: "chatcmpl-123".to_string(),
        object: "chat.completion".to_string(),
        created: None,
        model: "gpt-4o".to_string(),
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
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        }),
    };

    let result = responses_format::chat_to_responses(&resp, &StrippedTools::default());
    assert!(result.id.starts_with("resp_"));
    assert_eq!(result.object, "response");
    assert_eq!(result.status, "completed");
    assert_eq!(result.model, "gpt-4o");
    assert_eq!(result.output.len(), 1);
    assert_eq!(result.output[0]["type"], "message");
    assert_eq!(result.output[0]["content"][0]["text"], "Hello!");
    assert_eq!(result.usage.input_tokens, 10);
    assert_eq!(result.usage.output_tokens, 5);
    assert_eq!(result.usage.total_tokens, 15);
}

#[test]
fn test_chat_to_responses_tool_calls() {
    let resp = ChatResponse {
        id: "chatcmpl-456".to_string(),
        object: "chat.completion".to_string(),
        created: None,
        model: "gpt-4o".to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_abc".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "get_weather".to_string(),
                        arguments: r#"{"city":"SF"}"#.to_string(),
                    },
                }]),
                tool_call_id: None,
                name: None,
            },
            finish_reason: Some("tool_calls".to_string()),
        }],
        usage: Some(Usage {
            prompt_tokens: 20,
            completion_tokens: 10,
            total_tokens: 30,
        }),
    };

    let result = responses_format::chat_to_responses(&resp, &StrippedTools::default());
    assert_eq!(result.output.len(), 1);
    assert_eq!(result.output[0]["type"], "function_call");
    assert_eq!(result.output[0]["name"], "get_weather");
    assert_eq!(result.output[0]["call_id"], "call_abc");
}

#[test]
fn test_chat_to_responses_with_stripped_tools() {
    let resp = ChatResponse {
        id: "chatcmpl-789".to_string(),
        object: "chat.completion".to_string(),
        created: None,
        model: "gpt-4o".to_string(),
        choices: vec![Choice {
            index: 0,
            message: Message {
                role: "assistant".to_string(),
                content: Some(serde_json::Value::String("Done".to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            finish_reason: Some("stop".to_string()),
        }],
        usage: None,
    };

    let stripped = StrippedTools {
        unsupported: vec!["web_search".to_string()],
    };
    let result = responses_format::chat_to_responses(&resp, &stripped);
    assert!(result.metadata.is_some());
    let meta = result.metadata.unwrap();
    assert_eq!(meta["warnings"]["unsupported_tools"][0], "web_search");
}

// ============ 流式事件状态机测试 ============

#[test]
fn test_streaming_text_content() {
    let mut state = ResponsesEventState::new();

    // 首个 chunk（携带 role）
    let events = state.process_chat_chunk(
        r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
    );
    // 应该输出 response.created + response.in_progress
    assert!(events.len() >= 2);
    assert!(events[0].contains("response.created"));
    assert!(events[1].contains("response.in_progress"));

    // 内容 chunk
    let events = state.process_chat_chunk(
        r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#,
    );
    // 应该输出 output_item.added + content_part.added + output_text.delta
    assert!(events.iter().any(|e| e.contains("response.output_item.added")));
    assert!(events.iter().any(|e| e.contains("response.output_text.delta")));
    assert!(events.iter().any(|e| e.contains("Hello")));

    // 结束 chunk（finish_reason 不再触发 close，由 process_done 处理）
    let _events = state.process_chat_chunk(
        r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
    );

    // 模拟 [DONE] 事件：注入 usage 后调用 process_done
    state.usage = responses_format::ResponsesUsage {
        input_tokens: 10,
        output_tokens: 5,
        total_tokens: 15,
    };
    let events = state.process_done();
    // 应该输出 content_part.done + output_item.done + response.completed（含正确 usage）
    assert!(events.iter().any(|e| e.contains("response.completed")));
    assert!(events.iter().any(|e| e.contains("\"input_tokens\":10")));
    assert!(events.iter().any(|e| e.contains("\"total_tokens\":15")));
}

#[test]
fn test_streaming_tool_calls() {
    let mut state = ResponsesEventState::new();

    // role delta
    let _ = state.process_chat_chunk(
        r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
    );

    // tool call 开始
    let events = state.process_chat_chunk(
        r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#,
    );
    assert!(events.iter().any(|e| e.contains("response.output_item.added")));
    assert!(events.iter().any(|e| e.contains("function_call")));
    assert!(events.iter().any(|e| e.contains("get_weather")));

    // tool call 参数 delta
    let events = state.process_chat_chunk(
        r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]},"finish_reason":null}]}"#,
    );
    assert!(events.iter().any(|e| e.contains("function_call_arguments.delta")));

    // 结束（finish_reason 不再触发 close，由 process_done 处理）
    let _events = state.process_chat_chunk(
        r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
    );

    // 模拟 [DONE]
    let events = state.process_done();
    assert!(events.iter().any(|e| e.contains("response.completed")));
}

#[test]
fn test_streaming_done() {
    let mut state = ResponsesEventState::new();
    let events = state.process_done();
    // Init 阶段直接 done → 应该输出 response.completed
    assert!(events.iter().any(|e| e.contains("response.completed")));
}

// ============ Provider function calling 测试 ============

fn make_model() -> AiModel {
    AiModel {
        model_name: "test-model".to_string(),
        ..Default::default()
    }
}

// Anthropic: 非流式 tool_use 提取
#[test]
fn test_anthropic_non_streaming_tool_use() {
    let driver = AnthropicDriver;
    let model = make_model();
    let headers = HashMap::new();

    let body = r#"{
        "id": "msg_123",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "I'll check the weather."},
            {"type": "tool_use", "id": "toolu_abc", "name": "get_weather", "input": {"city": "SF"}}
        ],
        "model": "claude-3-opus",
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 100, "output_tokens": 50}
    }"#;

    let result = driver.transform_response(200, &headers, body, &model).unwrap();
    assert_eq!(result.choices[0].finish_reason.as_deref(), Some("tool_calls"));

    let tc = result.choices[0].message.tool_calls.as_ref().unwrap();
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0].id, "toolu_abc");
    assert_eq!(tc[0].function.name, "get_weather");
    assert!(tc[0].function.arguments.contains("SF"));

    // 文本内容也应该被提取
    let content = result.choices[0].message.content.as_ref().unwrap();
    assert!(content.as_str().unwrap().contains("weather"));
}

// Anthropic: 流式 tool_use
#[test]
fn test_anthropic_streaming_tool_use() {
    let driver = AnthropicDriver;
    let model = make_model();

    // content_block_start (tool_use)
    let event = SseEvent {
        event_type: "content_block_start".to_string(),
        data: r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_abc","name":"get_weather","input":{}}}"#.to_string(),
        id: None,
    };
    let result = driver.transform_stream_event(&event, &model).unwrap();
    assert!(result.is_some());
    let data: serde_json::Value = serde_json::from_str(&result.unwrap().data).unwrap();
    let tc = &data["choices"][0]["delta"]["tool_calls"][0];
    assert_eq!(tc["id"], "toolu_abc");
    assert_eq!(tc["function"]["name"], "get_weather");

    // content_block_delta (input_json_delta)
    let event = SseEvent {
        event_type: "content_block_delta".to_string(),
        data: r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"SF\"}"}}"#.to_string(),
        id: None,
    };
    let result = driver.transform_stream_event(&event, &model).unwrap();
    assert!(result.is_some());
    let data: serde_json::Value = serde_json::from_str(&result.unwrap().data).unwrap();
    let tc = &data["choices"][0]["delta"]["tool_calls"][0];
    assert_eq!(tc["function"]["arguments"], "{\"city\":\"SF\"}");
}

// Gemini: 非流式 functionCall 提取
#[test]
fn test_gemini_non_streaming_function_call() {
    let driver = GeminiDriver;
    let model = make_model();
    let headers = HashMap::new();

    let body = r#"{
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    {"functionCall": {"name": "get_weather", "args": {"city": "SF"}}}
                ]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {"promptTokenCount": 50, "candidatesTokenCount": 20, "totalTokenCount": 70}
    }"#;

    let result = driver.transform_response(200, &headers, body, &model).unwrap();
    let tc = result.choices[0].message.tool_calls.as_ref().unwrap();
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0].function.name, "get_weather");
    assert!(tc[0].function.arguments.contains("SF"));
}

// Gemini: 流式 functionCall
#[test]
fn test_gemini_streaming_function_call() {
    let driver = GeminiDriver;
    let model = make_model();

    let event = SseEvent {
        event_type: "message".to_string(),
        data: r#"{
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"functionCall": {"name": "get_weather", "args": {"city": "SF"}}}]
                },
                "finishReason": "STOP"
            }]
        }"#.to_string(),
        id: None,
    };

    let result = driver.transform_stream_event(&event, &model).unwrap();
    assert!(result.is_some());
    let data: serde_json::Value = serde_json::from_str(&result.unwrap().data).unwrap();
    let tc = &data["choices"][0]["delta"]["tool_calls"];
    assert!(tc.is_array());
    assert_eq!(tc[0]["function"]["name"], "get_weather");
}

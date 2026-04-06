//! Anthropic Driver — provider 侧格式转换（上游为 Anthropic API）
//! 将内部 OpenAI 规范化格式与 Anthropic Messages API 互转

use crate::codec::{
    ChatRequest, ChatResponse, Choice, FunctionCall, Message, SseEvent, ToolCall, Usage,
};
use crate::models::{AiModel, AiProviderConfig, AuthConfig};
use crate::provider::{AiDriver, ProviderRequest, TokenUsage, UpstreamConfig};
use kong_core::error::{KongError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Anthropic Driver 实现
pub struct AnthropicDriver;

// ============ Anthropic 原生 API 类型 ============

/// Anthropic Messages 请求
#[derive(Debug, Serialize)]
struct AnthropicApiRequest {
    model: String,
    messages: Vec<AnthropicApiMessage>,
    max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize)]
struct AnthropicApiMessage {
    role: String,
    content: serde_json::Value,
}

/// Anthropic Messages 响应
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AnthropicApiResponse {
    id: String,
    #[serde(rename = "type")]
    msg_type: String,
    role: String,
    content: Vec<AnthropicContentBlock>,
    model: String,
    stop_reason: Option<String>,
    usage: AnthropicApiUsage,
}

#[derive(Debug, Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
    // tool_use 类型的字段
    id: Option<String>,
    name: Option<String>,
    input: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct AnthropicApiUsage {
    input_tokens: u64,
    output_tokens: u64,
}

impl AiDriver for AnthropicDriver {
    fn provider_type(&self) -> &str {
        "anthropic"
    }

    fn transform_request(
        &self,
        request: &ChatRequest,
        _model: &AiModel,
        _provider_config: &AiProviderConfig,
    ) -> Result<ProviderRequest> {
        // 提取 system 消息 → top-level system 字段
        let mut system_text: Option<String> = None;
        let mut messages = Vec::new();

        for msg in &request.messages {
            if msg.role == "system" {
                let text = msg
                    .content
                    .as_ref()
                    .map(|v| match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default();
                // 多个 system 消息拼接
                match system_text.as_mut() {
                    Some(existing) => {
                        existing.push('\n');
                        existing.push_str(&text);
                    }
                    None => system_text = Some(text),
                }
            } else {
                let content = msg
                    .content
                    .clone()
                    .unwrap_or(serde_json::Value::String(String::new()));
                messages.push(AnthropicApiMessage {
                    role: msg.role.clone(),
                    content,
                });
            }
        }

        // max_tokens 必填，默认 4096
        let max_tokens = request.max_tokens.unwrap_or(4096);

        let api_req = AnthropicApiRequest {
            model: request.model.clone(),
            messages,
            max_tokens,
            system: system_text,
            temperature: request.temperature,
            top_p: request.top_p,
            top_k: request.top_k,
            stream: request.stream,
        };

        let body = serde_json::to_string(&api_req)?;

        Ok(ProviderRequest {
            body,
            content_type: "application/json".to_string(),
            extra_headers: Vec::new(),
        })
    }

    fn transform_response(
        &self,
        status: u16,
        _headers: &HashMap<String, String>,
        body: &str,
        _model: &AiModel,
    ) -> Result<ChatResponse> {
        if status != 200 {
            return Err(KongError::UpstreamError(format!(
                "Anthropic API returned status {}: {}",
                status,
                body.chars().take(500).collect::<String>()
            )));
        }

        let resp: AnthropicApiResponse =
            serde_json::from_str(body).map_err(|e| {
                KongError::SerializationError(format!("invalid Anthropic response: {}", e))
            })?;

        // 提取文本内容
        let text = resp
            .content
            .iter()
            .filter(|b| b.block_type == "text")
            .filter_map(|b| b.text.as_deref())
            .collect::<Vec<_>>()
            .join("");

        // 提取 tool_use content blocks → OpenAI tool_calls
        let tool_calls: Vec<ToolCall> = resp
            .content
            .iter()
            .filter(|b| b.block_type == "tool_use")
            .filter_map(|b| {
                let id = b.id.as_ref()?;
                let name = b.name.as_ref()?;
                let args = b
                    .input
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                Some(ToolCall {
                    id: id.clone(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: args,
                    },
                })
            })
            .collect();

        // stop_reason 映射
        let finish_reason = resp.stop_reason.map(|r| match r.as_str() {
            "end_turn" => "stop".to_string(),
            "max_tokens" => "length".to_string(),
            "tool_use" => "tool_calls".to_string(),
            other => other.to_string(),
        });

        let total = resp.usage.input_tokens + resp.usage.output_tokens;

        Ok(ChatResponse {
            id: resp.id,
            object: "chat.completion".to_string(),
            created: None,
            model: resp.model,
            choices: vec![Choice {
                index: 0,
                message: Message {
                    role: "assistant".to_string(),
                    content: if text.is_empty() { None } else { Some(serde_json::Value::String(text)) },
                    tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                    tool_call_id: None,
                    name: None,
                },
                finish_reason,
            }],
            usage: Some(Usage {
                prompt_tokens: resp.usage.input_tokens,
                completion_tokens: resp.usage.output_tokens,
                total_tokens: total,
            }),
        })
    }

    fn transform_stream_event(
        &self,
        event: &SseEvent,
        _model: &AiModel,
    ) -> Result<Option<SseEvent>> {
        // 根据 event_type 分发处理 Anthropic 5 种事件类型
        let data: serde_json::Value = serde_json::from_str(&event.data).map_err(|e| {
            KongError::SerializationError(format!("invalid Anthropic SSE data: {}", e))
        })?;

        let event_type = data
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or(&event.event_type);

        match event_type {
            "message_start" => {
                // 提取 usage，不需要转换为 OpenAI chunk（不含内容）
                // 返回一个空的 OpenAI chunk 仅携带 model 信息
                let model = data
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                let chunk = serde_json::json!({
                    "id": data.get("message").and_then(|m| m.get("id")).and_then(|v| v.as_str()).unwrap_or("msg_unknown"),
                    "object": "chat.completion.chunk",
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "delta": { "role": "assistant" },
                        "finish_reason": null
                    }]
                });

                Ok(Some(SseEvent {
                    event_type: "message".to_string(),
                    data: chunk.to_string(),
                    id: None,
                }))
            }
            "content_block_start" => {
                // 检查是否为 tool_use 类型的 content block
                let block_type = data
                    .get("content_block")
                    .and_then(|b| b.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("text");

                if block_type == "tool_use" {
                    // tool_use block 开始，提取 id 和 name
                    let block_index = data.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
                    let tool_id = data
                        .get("content_block")
                        .and_then(|b| b.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("call_unknown");
                    let tool_name = data
                        .get("content_block")
                        .and_then(|b| b.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    // 输出 tool_call 的初始 delta（包含 id、type、function.name）
                    let chunk = serde_json::json!({
                        "id": "msg_stream",
                        "object": "chat.completion.chunk",
                        "model": "unknown",
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "tool_calls": [{
                                    "index": block_index,
                                    "id": tool_id,
                                    "type": "function",
                                    "function": {
                                        "name": tool_name,
                                        "arguments": ""
                                    }
                                }]
                            },
                            "finish_reason": null
                        }]
                    });

                    return Ok(Some(SseEvent {
                        event_type: "message".to_string(),
                        data: chunk.to_string(),
                        id: None,
                    }));
                }
                // text block 开始，跳过
                Ok(None)
            }
            "ping" => Ok(None),
            "content_block_delta" => {
                let delta_type = data
                    .get("delta")
                    .and_then(|d| d.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("text_delta");

                match delta_type {
                    "text_delta" => {
                        let text = data
                            .get("delta")
                            .and_then(|d| d.get("text"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("");

                        if text.is_empty() {
                            return Ok(None);
                        }

                        let chunk = serde_json::json!({
                            "id": "msg_stream",
                            "object": "chat.completion.chunk",
                            "model": "unknown",
                            "choices": [{
                                "index": 0,
                                "delta": { "content": text },
                                "finish_reason": null
                            }]
                        });

                        Ok(Some(SseEvent {
                            event_type: "message".to_string(),
                            data: chunk.to_string(),
                            id: None,
                        }))
                    }
                    "input_json_delta" => {
                        // tool_use 的参数增量
                        let partial_json = data
                            .get("delta")
                            .and_then(|d| d.get("partial_json"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        if partial_json.is_empty() {
                            return Ok(None);
                        }

                        let block_index = data.get("index").and_then(|i| i.as_u64()).unwrap_or(0);

                        let chunk = serde_json::json!({
                            "id": "msg_stream",
                            "object": "chat.completion.chunk",
                            "model": "unknown",
                            "choices": [{
                                "index": 0,
                                "delta": {
                                    "tool_calls": [{
                                        "index": block_index,
                                        "function": {
                                            "arguments": partial_json
                                        }
                                    }]
                                },
                                "finish_reason": null
                            }]
                        });

                        Ok(Some(SseEvent {
                            event_type: "message".to_string(),
                            data: chunk.to_string(),
                            id: None,
                        }))
                    }
                    _ => Ok(None),
                }
            }
            "content_block_stop" => {
                // 跳过
                Ok(None)
            }
            "message_delta" => {
                // 消息结束事件，携带 stop_reason 和 final usage
                let stop_reason = data
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|v| v.as_str())
                    .map(|r| match r {
                        "end_turn" => "stop",
                        "max_tokens" => "length",
                        "tool_use" => "tool_calls",
                        other => other,
                    })
                    .unwrap_or("stop");

                let chunk = serde_json::json!({
                    "id": "msg_stream",
                    "object": "chat.completion.chunk",
                    "model": "unknown",
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": stop_reason
                    }]
                });

                Ok(Some(SseEvent {
                    event_type: "message".to_string(),
                    data: chunk.to_string(),
                    id: None,
                }))
            }
            "message_stop" => {
                // 流结束，返回 None 触发 [DONE]
                Ok(None)
            }
            _ => {
                // 未知事件类型，跳过
                Ok(None)
            }
        }
    }

    fn configure_upstream(
        &self,
        _model: &AiModel,
        provider_config: &AiProviderConfig,
        _stream: bool,
    ) -> Result<UpstreamConfig> {
        let (scheme, host, port, path) =
            if let Some(ref endpoint_url) = provider_config.endpoint_url {
                crate::provider::openai::parse_endpoint_url(endpoint_url)?
            } else {
                (
                    "https".to_string(),
                    "api.anthropic.com".to_string(),
                    443,
                    "/v1/messages".to_string(),
                )
            };

        let mut headers = vec![
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ];

        // 认证：Anthropic 使用 x-api-key header
        let auth: AuthConfig =
            serde_json::from_value(provider_config.auth_config.clone()).unwrap_or_default();

        if let Some(ref header_value) = auth.header_value {
            let header_name = auth
                .header_name
                .clone()
                .unwrap_or_else(|| "x-api-key".to_string());
            headers.push((header_name, header_value.clone()));
        }

        Ok(UpstreamConfig {
            scheme,
            host,
            port,
            path,
            headers,
        })
    }

    fn extract_usage(&self, body: &str) -> Option<TokenUsage> {
        let resp: AnthropicApiResponse = serde_json::from_str(body).ok()?;
        Some(TokenUsage {
            prompt_tokens: Some(resp.usage.input_tokens),
            completion_tokens: Some(resp.usage.output_tokens),
            total_tokens: Some(resp.usage.input_tokens + resp.usage.output_tokens),
        })
    }

    fn extract_stream_usage(&self, event: &SseEvent) -> Option<TokenUsage> {
        let data: serde_json::Value = serde_json::from_str(&event.data).ok()?;
        let event_type = data.get("type").and_then(|t| t.as_str())?;

        match event_type {
            "message_start" => {
                // message_start 携带初始 usage（input_tokens）
                let usage = data.get("message")?.get("usage")?;
                let input = usage.get("input_tokens")?.as_u64()?;
                Some(TokenUsage {
                    prompt_tokens: Some(input),
                    completion_tokens: None,
                    total_tokens: None,
                })
            }
            "message_delta" => {
                // message_delta 携带最终 output_tokens
                let usage = data.get("usage")?;
                let output = usage.get("output_tokens")?.as_u64()?;
                Some(TokenUsage {
                    prompt_tokens: None,
                    completion_tokens: Some(output),
                    total_tokens: None,
                })
            }
            _ => None,
        }
    }
}

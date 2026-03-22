//! Anthropic Messages 协议编解码器 — 客户端侧 /v1/messages 协议转换
//! 当 client_protocol=anthropic 时，网关向客户端暴露 Anthropic 原生格式

use crate::codec::{ChatRequest, ChatResponse, Message, SseEvent};
use kong_core::error::{KongError, Result};
use serde::{Deserialize, Serialize};

/// Anthropic 客户端协议编解码器
pub struct AnthropicCodec;

// ============ Anthropic 原生请求/响应类型（仅用于解析客户端输入、构造客户端输出） ============

/// Anthropic Messages 请求格式
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AnthropicRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    #[serde(default)]
    system: Option<serde_json::Value>, // String 或 content blocks 数组
    max_tokens: u64,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    top_p: Option<f64>,
    #[serde(default)]
    top_k: Option<u64>,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

/// Anthropic 消息格式
#[derive(Debug, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: serde_json::Value, // String 或 content blocks 数组
}

/// Anthropic Messages 响应格式
#[derive(Debug, Serialize)]
struct AnthropicResponse {
    id: String,
    #[serde(rename = "type")]
    msg_type: String,
    role: String,
    content: Vec<ContentBlock>,
    model: String,
    stop_reason: Option<String>,
    stop_sequence: Option<String>,
    usage: AnthropicUsage,
}

/// Anthropic content block
#[derive(Debug, Serialize, Deserialize, Clone)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

/// Anthropic usage 格式
#[derive(Debug, Serialize, Deserialize, Clone)]
struct AnthropicUsage {
    input_tokens: u64,
    output_tokens: u64,
}

impl AnthropicCodec {
    /// Anthropic Messages 请求 → OpenAI ChatRequest（客户端入口解码）
    pub fn decode_request(body: &str) -> Result<ChatRequest> {
        let req: AnthropicRequest = serde_json::from_str(body).map_err(|e| {
            KongError::SerializationError(format!("invalid Anthropic request: {}", e))
        })?;

        let mut messages = Vec::new();

        // system 字段 → messages[0] with role="system"
        if let Some(system) = req.system {
            let system_content = match system {
                serde_json::Value::String(s) => serde_json::Value::String(s),
                // content blocks 数组 → 提取文本拼接
                serde_json::Value::Array(blocks) => {
                    let text: Vec<String> = blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()))
                        .collect();
                    serde_json::Value::String(text.join("\n"))
                }
                other => other,
            };
            messages.push(Message {
                role: "system".to_string(),
                content: Some(system_content),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }

        // 转换消息列表
        for msg in req.messages {
            let content = convert_anthropic_content_to_openai(&msg.content);
            messages.push(Message {
                role: msg.role,
                content: Some(content),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }

        Ok(ChatRequest {
            model: req.model,
            messages,
            temperature: req.temperature,
            max_tokens: Some(req.max_tokens),
            top_p: req.top_p,
            top_k: req.top_k,
            stream: req.stream,
            stream_options: None,
            tools: None,
            tool_choice: None,
            extra: std::collections::HashMap::new(),
        })
    }

    /// OpenAI ChatResponse → Anthropic Messages 响应（客户端出口编码）
    pub fn encode_response(response: &ChatResponse) -> Result<String> {
        let choice = response.choices.first();

        // 提取文本内容
        let content_text = choice
            .and_then(|c| c.message.content.as_ref())
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default();

        let content = vec![ContentBlock {
            block_type: "text".to_string(),
            text: Some(content_text),
        }];

        // 转换 finish_reason → stop_reason
        let stop_reason = choice.and_then(|c| c.finish_reason.as_ref()).map(|r| {
            match r.as_str() {
                "stop" => "end_turn".to_string(),
                "length" => "max_tokens".to_string(),
                "tool_calls" => "tool_use".to_string(),
                other => other.to_string(),
            }
        });

        // 转换 usage
        let usage = response.usage.as_ref().map(|u| AnthropicUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        }).unwrap_or(AnthropicUsage {
            input_tokens: 0,
            output_tokens: 0,
        });

        let anthropic_resp = AnthropicResponse {
            id: response.id.clone(),
            msg_type: "message".to_string(),
            role: "assistant".to_string(),
            content,
            model: response.model.clone(),
            stop_reason,
            stop_sequence: None,
            usage,
        };

        serde_json::to_string(&anthropic_resp).map_err(|e| {
            KongError::SerializationError(format!("failed to encode Anthropic response: {}", e))
        })
    }

    /// OpenAI SSE event → Anthropic SSE events（流式编码）
    /// 返回一个或多个 Anthropic SSE 事件
    pub fn encode_stream_event(event: &SseEvent, is_first: bool) -> Result<Vec<SseEvent>> {
        // [DONE] 终止事件 → message_delta + message_stop
        if event.is_done() {
            let delta_event = SseEvent {
                event_type: "message_delta".to_string(),
                data: serde_json::json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": "end_turn" },
                    "usage": { "output_tokens": 0 }
                }).to_string(),
                id: None,
            };
            let stop_event = SseEvent {
                event_type: "message_stop".to_string(),
                data: serde_json::json!({ "type": "message_stop" }).to_string(),
                id: None,
            };
            return Ok(vec![delta_event, stop_event]);
        }

        // 解析 OpenAI stream chunk
        let chunk: serde_json::Value = serde_json::from_str(&event.data).map_err(|e| {
            KongError::SerializationError(format!("invalid OpenAI stream chunk: {}", e))
        })?;

        let mut events = Vec::new();

        // 第一个事件：先发 message_start + content_block_start
        if is_first {
            let model = chunk.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
            let message_start = SseEvent {
                event_type: "message_start".to_string(),
                data: serde_json::json!({
                    "type": "message_start",
                    "message": {
                        "id": chunk.get("id").and_then(|v| v.as_str()).unwrap_or("msg_unknown"),
                        "type": "message",
                        "role": "assistant",
                        "content": [],
                        "model": model,
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": { "input_tokens": 0, "output_tokens": 0 }
                    }
                }).to_string(),
                id: None,
            };
            events.push(message_start);

            let block_start = SseEvent {
                event_type: "content_block_start".to_string(),
                data: serde_json::json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": { "type": "text", "text": "" }
                }).to_string(),
                id: None,
            };
            events.push(block_start);
        }

        // 提取 delta content
        let content = chunk
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        if !content.is_empty() {
            let block_delta = SseEvent {
                event_type: "content_block_delta".to_string(),
                data: serde_json::json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": { "type": "text_delta", "text": content }
                }).to_string(),
                id: None,
            };
            events.push(block_delta);
        }

        Ok(events)
    }
}

/// 将 Anthropic content（String 或 content blocks 数组）转换为 OpenAI content 格式
fn convert_anthropic_content_to_openai(content: &serde_json::Value) -> serde_json::Value {
    match content {
        // 纯文本字符串直接使用
        serde_json::Value::String(_) => content.clone(),
        // content blocks 数组 → 提取文本拼接
        serde_json::Value::Array(blocks) => {
            let text: Vec<String> = blocks
                .iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        b.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            serde_json::Value::String(text.join(""))
        }
        other => other.clone(),
    }
}

//! AI 代理插件跨阶段共享状态 — cross-phase shared state for ai-proxy plugin

use crate::codec::responses_format::{ResponsesEventState, StrippedTools};
use crate::codec::SseEvent;
use crate::models::{AiModel, AiProviderConfig};
use crate::provider::{AiDriver, TokenUsage};
use crate::usage::StreamTerminalState;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

/// 客户端协议类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClientProtocol {
    OpenAi,
    Anthropic,
}

/// AI 代理插件跨阶段共享状态 — 存储在 ctx.extensions 中
pub struct AiRequestState {
    /// 当前请求使用的 driver 实例
    pub driver: Arc<dyn AiDriver>,
    /// 当前请求使用的模型
    pub model: AiModel,
    /// 当前请求使用的 provider 配置
    pub provider_config: AiProviderConfig,
    /// 是否为流式模式
    pub stream_mode: bool,
    /// 客户端协议类型
    pub client_protocol: ClientProtocol,
    /// SSE 解析器（流式模式下使用，Task 6 启用）
    pub sse_parser: Option<crate::codec::SseParser>,
    /// 跨 HTTP chunk 暂存的不完整 UTF-8 尾字节。
    pub stream_utf8_buffer: Vec<u8>,
    /// Token 使用量统计
    pub usage: TokenUsage,
    /// 仅在完整、安全观察到最终响应后生成的 completion 估值。
    pub estimated_completion_tokens: Option<u64>,
    /// 流式转换过程中安全观察到的 completion 文本。
    pub completion_text: String,
    /// Chat Completions 流按 choice index 分桶，避免跨 choice 拼接后低估 token。
    pub completion_text_by_choice: BTreeMap<u64, String>,
    /// 任一原始流事件无法完整解析/转换时，禁止用残片估算 completion。
    pub completion_observation_invalid: bool,
    /// 非流式响应缓冲区
    pub response_buffer: Option<String>,
    /// 请求开始时间
    pub request_start: Instant,
    /// 首 token 时间（流式模式使用）
    pub ttft: Option<Instant>,
    /// 是否已经观察到有效的 provider 流事件。
    pub valid_stream_event_seen: bool,
    /// provider 原始协议的流终态。
    pub stream_terminal: StreamTerminalState,
    /// 首个有效 provider 流事件时间。
    pub first_stream_event_at: Option<Instant>,
    /// 路由类型（如 "llm/v1/chat"、"llm/v1/responses"）
    pub route_type: String,
    /// 是否为第一个流式事件（用于 Anthropic 客户端协议编码）
    pub is_first_stream_event: bool,
    /// v1/responses 翻译模式（非 OpenAI provider 时启用）
    pub responses_mode: bool,
    /// v1/responses pass-through 模式（OpenAI provider 时启用）
    pub responses_pass_through: bool,
    /// v1/responses 流式事件状态机
    pub responses_event_state: Option<ResponsesEventState>,
    /// 被剥离的不支持的工具类型（用于非流式响应中回填 metadata.warnings）
    pub stripped_tools: Option<StrippedTools>,
    /// 流式 tool_call 本地计数器（用于重映射 Anthropic 全局 block index → 0-based tool_call index）
    pub stream_tool_call_count: u32,
    /// access 阶段 TokenizerRegistry 计算的 prompt token 估值（供 balancer by_token_size、ai-rate-limit log 修正等下游消费）
    /// Prompt-token estimate produced by TokenizerRegistry during access; consumed by
    /// balancer by_token_size routing, ai-rate-limit log-stage reconciliation, etc.
    pub estimated_prompt_tokens: u64,
}

impl AiRequestState {
    /// 只依据 provider 原始 SSE 事件推进 usage 流终态。
    pub fn observe_stream_event(&mut self, event: &SseEvent) {
        if event.is_done() {
            self.stream_terminal = StreamTerminalState::Complete;
            return;
        }
        let parsed = serde_json::from_str::<serde_json::Value>(&event.data).ok();
        if parsed.is_none() && event.event_type == "message" {
            return;
        }
        let event_name = if event.event_type != "message" {
            event.event_type.as_str()
        } else {
            parsed
                .as_ref()
                .and_then(|value| value.get("type"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        };
        if event_name == "error"
            || event_name == "response.failed"
            || parsed
                .as_ref()
                .is_some_and(|value| value.get("error").is_some())
        {
            self.stream_terminal = StreamTerminalState::ProviderFailed;
            return;
        }
        let gemini_finished = parsed.as_ref().is_some_and(has_finish_reason);
        if matches!(
            event_name,
            "response.completed" | "response.incomplete" | "message_stop"
        ) {
            self.stream_terminal = StreamTerminalState::Complete;
            return;
        }
        if !is_valid_provider_stream_event(
            &self.provider_config.provider_type,
            self.responses_mode || self.responses_pass_through,
            event_name,
            parsed.as_ref(),
        ) {
            return;
        }
        self.valid_stream_event_seen = true;
        let now = Instant::now();
        self.first_stream_event_at.get_or_insert(now);
        self.ttft.get_or_insert(now);
        if gemini_finished {
            self.stream_terminal = StreamTerminalState::Complete;
        }
    }
}

fn is_valid_provider_stream_event(
    provider_type: &str,
    responses_surface: bool,
    event_name: &str,
    parsed: Option<&serde_json::Value>,
) -> bool {
    if matches!(
        event_name,
        "ping" | "heartbeat" | "keepalive" | "response.keepalive"
    ) {
        return false;
    }
    let Some(value) = parsed else {
        return false;
    };
    match provider_type {
        "anthropic" => matches!(
            event_name,
            "message_start"
                | "content_block_start"
                | "content_block_delta"
                | "content_block_stop"
                | "message_delta"
        ),
        "gemini" => {
            value
                .get("candidates")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|candidates| !candidates.is_empty())
                || value.get("usageMetadata").is_some()
        }
        "openai" if responses_surface => event_name.starts_with("response."),
        "openai" | "openai_compat" => value.is_object(),
        _ => value.is_object(),
    }
}

fn has_finish_reason(value: &serde_json::Value) -> bool {
    value
        .get("candidates")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|candidates| {
            candidates.iter().any(|candidate| {
                candidate
                    .get("finishReason")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|reason| !reason.is_empty())
            })
        })
}

#[cfg(test)]
mod tests {
    use super::is_valid_provider_stream_event;

    #[test]
    fn provider_stream_event_schema_excludes_heartbeats() {
        let ping = serde_json::json!({"type": "ping"});
        assert!(!is_valid_provider_stream_event(
            "anthropic",
            false,
            "ping",
            Some(&ping),
        ));

        let delta = serde_json::json!({
            "type": "content_block_delta",
            "delta": {"type": "text_delta", "text": "hello"}
        });
        assert!(is_valid_provider_stream_event(
            "anthropic",
            false,
            "content_block_delta",
            Some(&delta),
        ));

        let response_delta =
            serde_json::json!({"type": "response.output_text.delta", "delta": "hello"});
        assert!(is_valid_provider_stream_event(
            "openai",
            true,
            "response.output_text.delta",
            Some(&response_delta),
        ));
        assert!(!is_valid_provider_stream_event(
            "openai",
            true,
            "",
            Some(&serde_json::json!({"choices": []})),
        ));
    }
}

//! AI 代理插件跨阶段共享状态 — cross-phase shared state for ai-proxy plugin

use crate::codec::responses_format::{ResponsesEventState, StrippedTools};
use crate::models::{AiModel, AiProviderConfig};
use crate::provider::{AiDriver, TokenUsage};
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
    /// Token 使用量统计
    pub usage: TokenUsage,
    /// 非流式响应缓冲区
    pub response_buffer: Option<String>,
    /// 请求开始时间
    pub request_start: Instant,
    /// 首 token 时间（流式模式使用）
    pub ttft: Option<Instant>,
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
}

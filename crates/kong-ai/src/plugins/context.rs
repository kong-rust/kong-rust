//! AI 代理插件跨阶段共享状态 — cross-phase shared state for ai-proxy plugin

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
    /// 路由类型（如 "llm/v1/chat"）
    pub route_type: String,
}

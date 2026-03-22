//! 协议编解码器 — SSE 解析、请求/响应格式转换

pub mod openai_format;
pub mod sse;

pub use openai_format::*;
pub use sse::{SseEvent, SseFormat, SseParser};

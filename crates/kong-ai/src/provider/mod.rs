//! AI Provider 抽象层 — 统一的 AiDriver trait 和 DriverRegistry
//! 每个 provider（OpenAI、Anthropic、Gemini 等）实现 AiDriver trait

pub mod anthropic;
pub mod balancer;
pub mod gemini;
pub mod openai;
pub mod openai_compat;

pub use balancer::ModelGroupBalancer;

use crate::codec::{ChatRequest, ChatResponse, SseEvent};
use crate::models::{AiModel, AiProviderConfig};
use kong_core::error::Result;
use std::collections::HashMap;
use std::sync::Arc;

/// 上游连接配置 — 由 driver 根据 model/provider 生成
#[derive(Debug, Clone)]
pub struct UpstreamConfig {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

/// 转换后的 provider 请求
#[derive(Debug)]
pub struct ProviderRequest {
    /// JSON 文本体（非 Bytes，与 RequestCtx.upstream_body 保持一致）
    pub body: String,
    pub content_type: String,
    pub extra_headers: Vec<(String, String)>,
}

/// Token 使用量统计
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

/// AI Driver trait — 每个 provider 必须实现
/// 负责请求/响应格式转换、上游配置、token 使用量提取
pub trait AiDriver: Send + Sync {
    /// 返回 provider 类型标识（如 "openai"、"anthropic"）
    fn provider_type(&self) -> &str;

    /// 将内部规范化请求转换为 provider 特定格式
    fn transform_request(
        &self,
        request: &ChatRequest,
        model: &AiModel,
        provider_config: &AiProviderConfig,
    ) -> Result<ProviderRequest>;

    /// 将 provider 响应转换为内部规范化格式
    fn transform_response(
        &self,
        status: u16,
        headers: &HashMap<String, String>,
        body: &str,
        model: &AiModel,
    ) -> Result<ChatResponse>;

    /// 转换流式 SSE 事件（返回 None 表示终止事件如 [DONE]）
    fn transform_stream_event(
        &self,
        event: &SseEvent,
        model: &AiModel,
    ) -> Result<Option<SseEvent>>;

    /// 生成上游连接配置（scheme、host、port、path、auth headers）
    fn configure_upstream(
        &self,
        model: &AiModel,
        provider_config: &AiProviderConfig,
    ) -> Result<UpstreamConfig>;

    /// 从非流式响应 body 中提取 token 使用量
    fn extract_usage(&self, body: &str) -> Option<TokenUsage>;

    /// 从流式事件中提取 token 使用量
    fn extract_stream_usage(&self, event: &SseEvent) -> Option<TokenUsage>;
}

/// Driver 注册表 — 按 provider 类型名称查找 driver 实例
pub struct DriverRegistry {
    drivers: HashMap<String, Arc<dyn AiDriver>>,
}

impl DriverRegistry {
    /// 创建注册表并注册内置 driver
    pub fn new() -> Self {
        let mut r = Self {
            drivers: HashMap::new(),
        };
        r.register("openai", Arc::new(openai::OpenAiDriver));
        r.register("anthropic", Arc::new(anthropic::AnthropicDriver));
        r.register("gemini", Arc::new(gemini::GeminiDriver));
        r.register("openai_compat", Arc::new(openai_compat::OpenAiCompatDriver::new()));
        r
    }

    /// 注册自定义 driver
    pub fn register(&mut self, name: &str, driver: Arc<dyn AiDriver>) {
        self.drivers.insert(name.to_string(), driver);
    }

    /// 按 provider 类型查找 driver
    pub fn get(&self, provider_type: &str) -> Option<&Arc<dyn AiDriver>> {
        self.drivers.get(provider_type)
    }
}

impl Default for DriverRegistry {
    fn default() -> Self {
        Self::new()
    }
}

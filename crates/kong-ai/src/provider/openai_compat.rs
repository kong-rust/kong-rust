//! OpenAI Compatible Driver — 兼容 OpenAI 格式的第三方 provider
//! 适用于通义千问 (Qwen)、混元等实现了 OpenAI 兼容接口的 provider
//! 所有格式转换委托给 OpenAiDriver，仅 configure_upstream 使用自定义 endpoint

use crate::codec::{ChatRequest, ChatResponse, SseEvent};
use crate::models::{AiModel, AiProviderConfig};
use crate::provider::openai::OpenAiDriver;
use crate::provider::{AiDriver, ProviderRequest, TokenUsage, UpstreamConfig};
use kong_core::error::{KongError, Result};
use std::collections::HashMap;

/// OpenAI Compatible Driver（委托模式）
pub struct OpenAiCompatDriver {
    inner: OpenAiDriver,
}

impl OpenAiCompatDriver {
    pub fn new() -> Self {
        Self {
            inner: OpenAiDriver,
        }
    }
}

impl Default for OpenAiCompatDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl AiDriver for OpenAiCompatDriver {
    fn provider_type(&self) -> &str {
        "openai_compat"
    }

    fn transform_request(
        &self,
        request: &ChatRequest,
        model: &AiModel,
        provider_config: &AiProviderConfig,
    ) -> Result<ProviderRequest> {
        // 完全委托给 OpenAI driver
        self.inner.transform_request(request, model, provider_config)
    }

    fn transform_response(
        &self,
        status: u16,
        headers: &HashMap<String, String>,
        body: &str,
        model: &AiModel,
    ) -> Result<ChatResponse> {
        self.inner.transform_response(status, headers, body, model)
    }

    fn transform_stream_event(
        &self,
        event: &SseEvent,
        model: &AiModel,
    ) -> Result<Option<SseEvent>> {
        self.inner.transform_stream_event(event, model)
    }

    fn configure_upstream(
        &self,
        model: &AiModel,
        provider_config: &AiProviderConfig,
    ) -> Result<UpstreamConfig> {
        // 必须提供 endpoint_url（兼容模式的核心配置）
        if provider_config.endpoint_url.is_none() {
            return Err(KongError::ConfigError(
                "openai_compat provider requires endpoint_url".to_string(),
            ));
        }
        // 委托给 OpenAI driver（它已支持 endpoint_url 解析）
        self.inner.configure_upstream(model, provider_config)
    }

    fn extract_usage(&self, body: &str) -> Option<TokenUsage> {
        self.inner.extract_usage(body)
    }

    fn extract_stream_usage(&self, event: &SseEvent) -> Option<TokenUsage> {
        self.inner.extract_stream_usage(event)
    }
}

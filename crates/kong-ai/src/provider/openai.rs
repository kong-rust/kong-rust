//! OpenAI Driver — 最简单的 driver，内部格式即 OpenAI 格式
//! transform 几乎是直通，仅需注入 stream_options

use crate::codec::{ChatRequest, ChatResponse, ChatStreamChunk, SseEvent, StreamOptions};
use crate::models::{AiModel, AiProviderConfig, AuthConfig};
use crate::provider::{AiDriver, ProviderRequest, TokenUsage, UpstreamConfig};
use kong_core::error::{KongError, Result};
use std::collections::HashMap;

/// OpenAI Driver 实现
pub struct OpenAiDriver;

impl AiDriver for OpenAiDriver {
    fn provider_type(&self) -> &str {
        "openai"
    }

    fn transform_request(
        &self,
        request: &ChatRequest,
        _model: &AiModel,
        _provider_config: &AiProviderConfig,
    ) -> Result<ProviderRequest> {
        // 内部格式即 OpenAI 格式，近乎直通
        let mut req = request.clone();

        // 流式请求时注入 stream_options.include_usage = true，确保拿到 usage
        if req.stream == Some(true) {
            req.stream_options = Some(StreamOptions {
                include_usage: Some(true),
            });
        }

        let body = serde_json::to_string(&req)?;

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
                "OpenAI API returned status {}: {}",
                status,
                body.chars().take(500).collect::<String>()
            )));
        }

        let response: ChatResponse = serde_json::from_str(body)?;
        Ok(response)
    }

    fn transform_stream_event(
        &self,
        event: &SseEvent,
        _model: &AiModel,
    ) -> Result<Option<SseEvent>> {
        // [DONE] 终止事件
        if event.is_done() {
            return Ok(None);
        }

        // 验证数据可解析为 ChatStreamChunk（确保格式正确）
        let _chunk: ChatStreamChunk = serde_json::from_str(&event.data).map_err(|e| {
            KongError::SerializationError(format!("invalid OpenAI stream chunk: {}", e))
        })?;

        // OpenAI 格式直通，无需转换
        Ok(Some(event.clone()))
    }

    fn configure_upstream(
        &self,
        _model: &AiModel,
        provider_config: &AiProviderConfig,
        _stream: bool,
    ) -> Result<UpstreamConfig> {
        // 如果 provider 配置了自定义 endpoint，使用它；否则使用 OpenAI 默认
        let (scheme, host, port, path) =
            if let Some(ref endpoint_url) = provider_config.endpoint_url {
                parse_endpoint_url(endpoint_url)?
            } else {
                (
                    "https".to_string(),
                    "api.openai.com".to_string(),
                    443,
                    "/v1/chat/completions".to_string(),
                )
            };

        // 从 auth_config 提取认证 header
        let mut headers = Vec::new();
        let auth: AuthConfig =
            serde_json::from_value(provider_config.auth_config.clone()).unwrap_or_default();

        if let Some(ref header_value) = auth.header_value {
            let header_name = auth
                .header_name
                .clone()
                .unwrap_or_else(|| "Authorization".to_string());
            // OpenAI 使用 Bearer token 格式
            let value = if header_name == "Authorization" && !header_value.starts_with("Bearer ") {
                format!("Bearer {}", header_value)
            } else {
                header_value.clone()
            };
            headers.push((header_name, value));
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
        let response: ChatResponse = serde_json::from_str(body).ok()?;
        response.usage.map(|u| TokenUsage {
            prompt_tokens: Some(u.prompt_tokens),
            completion_tokens: Some(u.completion_tokens),
            total_tokens: Some(u.total_tokens),
        })
    }

    fn extract_stream_usage(&self, event: &SseEvent) -> Option<TokenUsage> {
        if event.is_done() {
            return None;
        }
        let chunk: ChatStreamChunk = serde_json::from_str(&event.data).ok()?;
        chunk.usage.map(|u| TokenUsage {
            prompt_tokens: Some(u.prompt_tokens),
            completion_tokens: Some(u.completion_tokens),
            total_tokens: Some(u.total_tokens),
        })
    }
}

/// 解析 endpoint URL 为 (scheme, host, port, path)
pub fn parse_endpoint_url(url: &str) -> Result<(String, String, u16, String)> {
    // 简单解析，不引入 url crate
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        ("https".to_string(), rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        ("http".to_string(), rest)
    } else {
        return Err(KongError::ConfigError(format!(
            "invalid endpoint URL: {}",
            url
        )));
    };

    let default_port: u16 = if scheme == "https" { 443 } else { 80 };

    // 分离 path
    let (host_port, path) = match rest.find('/') {
        Some(pos) => (&rest[..pos], rest[pos..].to_string()),
        None => (rest, "/".to_string()),
    };

    // 分离 host 和 port
    let (host, port) = match host_port.rfind(':') {
        Some(pos) => {
            let port_str = &host_port[pos + 1..];
            match port_str.parse::<u16>() {
                Ok(p) => (host_port[..pos].to_string(), p),
                Err(_) => (host_port.to_string(), default_port),
            }
        }
        None => (host_port.to_string(), default_port),
    };

    Ok((scheme, host, port, path))
}

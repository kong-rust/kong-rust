//! OpenAI 兼容 embedding client — 调用 /v1/embeddings 端点
//! OpenAI-compatible embedding client targeting POST /v1/embeddings.
//!
//! 兼容性 / Compatibility:
//! - OpenAI 官方 (text-embedding-3-small / large, ada-002)
//! - Azure OpenAI (设置 endpoint_url)
//! - 任何 OpenAI 兼容服务(vLLM、Ollama、自建网关)

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use kong_core::error::{KongError, Result};

use super::EmbeddingClient;

/// OpenAI 兼容 embedding client
pub struct OpenAiEmbeddingClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    timeout: Duration,
    /// 自定义 auth header(默认 Authorization: Bearer)— optional auth header override
    auth_header_name: Option<String>,
    auth_header_value: Option<String>,
}

impl OpenAiEmbeddingClient {
    /// 构造 client(通常 reqwest::Client 由调用方提供以共享连接池)
    pub fn new(
        http: reqwest::Client,
        base_url: Option<String>,
        api_key: Option<String>,
        model: String,
        timeout: Duration,
    ) -> Self {
        let base_url = base_url
            .unwrap_or_else(|| "https://api.openai.com".to_string())
            .trim_end_matches('/')
            .to_string();
        Self {
            http,
            base_url,
            api_key,
            model,
            timeout,
            auth_header_name: None,
            auth_header_value: None,
        }
    }

    /// 自定义认证头 — 用于非 Bearer 认证(如 Azure 的 api-key 头)
    pub fn with_auth_header(mut self, name: String, value: String) -> Self {
        self.auth_header_name = Some(name);
        self.auth_header_value = Some(value);
        self
    }
}

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    input: &'a str,
    model: &'a str,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingDatum>,
}

#[derive(Deserialize)]
struct EmbeddingDatum {
    embedding: Vec<f32>,
}

#[async_trait]
impl EmbeddingClient for OpenAiEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let url = format!("{}/v1/embeddings", self.base_url);
        let body = EmbeddingRequest {
            input: text,
            model: &self.model,
        };

        let mut builder = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body);

        // 优先使用自定义 auth header,fallback 到 Bearer + api_key
        if let (Some(name), Some(value)) = (&self.auth_header_name, &self.auth_header_value) {
            builder = builder.header(name, value);
        } else if let Some(key) = &self.api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }

        let resp = tokio::time::timeout(self.timeout, builder.send())
            .await
            .map_err(|_| {
                KongError::UpstreamError(format!(
                    "embedding request to {} timed out after {:?}",
                    url, self.timeout
                ))
            })?
            .map_err(|e| KongError::UpstreamError(format!("embedding request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(KongError::UpstreamError(format!(
                "embedding API returned {}: {}",
                status,
                body.chars().take(500).collect::<String>()
            )));
        }

        let parsed: EmbeddingResponse = resp
            .json()
            .await
            .map_err(|e| KongError::SerializationError(format!("invalid embedding response: {}", e)))?;

        parsed
            .data
            .into_iter()
            .next()
            .map(|d| d.embedding)
            .ok_or_else(|| {
                KongError::UpstreamError("embedding API returned empty data array".to_string())
            })
    }

    fn identifier(&self) -> &str {
        &self.model
    }
}

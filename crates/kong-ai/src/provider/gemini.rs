//! Gemini Driver — Google Generative AI API 格式转换
//! 将内部 OpenAI 规范化格式与 Gemini generateContent API 互转

use crate::codec::{
    ChatRequest, ChatResponse, Choice, Message, SseEvent, Usage,
};
use crate::models::{AiModel, AiProviderConfig, AuthConfig};
use crate::provider::{AiDriver, ProviderRequest, TokenUsage, UpstreamConfig};
use kong_core::error::{KongError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Gemini Driver 实现
pub struct GeminiDriver;

// ============ Gemini API 原生类型 ============

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiApiRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u64>,
}

/// Gemini API 响应
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiApiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    usage_metadata: Option<GeminiUsageMetadata>,
    #[serde(default)]
    model_version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    content: Option<GeminiContent>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    prompt_token_count: Option<u64>,
    candidates_token_count: Option<u64>,
    total_token_count: Option<u64>,
}

impl AiDriver for GeminiDriver {
    fn provider_type(&self) -> &str {
        "gemini"
    }

    fn transform_request(
        &self,
        request: &ChatRequest,
        _model: &AiModel,
        _provider_config: &AiProviderConfig,
    ) -> Result<ProviderRequest> {
        let mut system_instruction: Option<GeminiContent> = None;
        let mut contents = Vec::new();

        for msg in &request.messages {
            let text = msg
                .content
                .as_ref()
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();

            if msg.role == "system" {
                // system → systemInstruction
                system_instruction = Some(GeminiContent {
                    role: None,
                    parts: vec![GeminiPart { text: Some(text) }],
                });
            } else {
                // Gemini role 映射：assistant → model
                let role = match msg.role.as_str() {
                    "assistant" => "model".to_string(),
                    other => other.to_string(),
                };
                contents.push(GeminiContent {
                    role: Some(role),
                    parts: vec![GeminiPart { text: Some(text) }],
                });
            }
        }

        // generation_config（有任何参数设置时才包含）
        let gen_config = if request.temperature.is_some()
            || request.top_p.is_some()
            || request.top_k.is_some()
            || request.max_tokens.is_some()
        {
            Some(GeminiGenerationConfig {
                temperature: request.temperature,
                top_p: request.top_p,
                top_k: request.top_k,
                max_output_tokens: request.max_tokens,
            })
        } else {
            None
        };

        let api_req = GeminiApiRequest {
            contents,
            system_instruction: system_instruction,
            generation_config: gen_config,
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
                "Gemini API returned status {}: {}",
                status,
                body.chars().take(500).collect::<String>()
            )));
        }

        let resp: GeminiApiResponse = serde_json::from_str(body).map_err(|e| {
            KongError::SerializationError(format!("invalid Gemini response: {}", e))
        })?;

        // 提取文本
        let text = resp
            .candidates
            .as_ref()
            .and_then(|c| c.first())
            .and_then(|c| c.content.as_ref())
            .and_then(|c| c.parts.first())
            .and_then(|p| p.text.as_deref())
            .unwrap_or("")
            .to_string();

        // finish_reason 映射
        let finish_reason = resp
            .candidates
            .as_ref()
            .and_then(|c| c.first())
            .and_then(|c| c.finish_reason.as_ref())
            .map(|r| match r.as_str() {
                "STOP" => "stop".to_string(),
                "MAX_TOKENS" => "length".to_string(),
                other => other.to_lowercase(),
            });

        // usage
        let usage = resp.usage_metadata.as_ref().map(|u| {
            let pt = u.prompt_token_count.unwrap_or(0);
            let ct = u.candidates_token_count.unwrap_or(0);
            let tt = u.total_token_count.unwrap_or(pt + ct);
            Usage {
                prompt_tokens: pt,
                completion_tokens: ct,
                total_tokens: tt,
            }
        });

        Ok(ChatResponse {
            id: format!("gemini-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: None,
            model: resp.model_version.unwrap_or_else(|| "gemini".to_string()),
            choices: vec![Choice {
                index: 0,
                message: Message {
                    role: "assistant".to_string(),
                    content: Some(serde_json::Value::String(text)),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                finish_reason,
            }],
            usage,
        })
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

        // Gemini SSE 流式返回的是 GeminiApiResponse JSON
        let resp: GeminiApiResponse = serde_json::from_str(&event.data).map_err(|e| {
            KongError::SerializationError(format!("invalid Gemini stream chunk: {}", e))
        })?;

        let text = resp
            .candidates
            .as_ref()
            .and_then(|c| c.first())
            .and_then(|c| c.content.as_ref())
            .and_then(|c| c.parts.first())
            .and_then(|p| p.text.as_deref())
            .unwrap_or("");

        let finish_reason = resp
            .candidates
            .as_ref()
            .and_then(|c| c.first())
            .and_then(|c| c.finish_reason.as_ref())
            .map(|r| match r.as_str() {
                "STOP" => "stop".to_string(),
                "MAX_TOKENS" => "length".to_string(),
                other => other.to_lowercase(),
            });

        // 转换为 OpenAI stream chunk 格式
        let chunk = serde_json::json!({
            "id": "gemini-stream",
            "object": "chat.completion.chunk",
            "model": resp.model_version.unwrap_or_else(|| "gemini".to_string()),
            "choices": [{
                "index": 0,
                "delta": {
                    "content": text
                },
                "finish_reason": finish_reason
            }]
        });

        Ok(Some(SseEvent {
            event_type: "message".to_string(),
            data: chunk.to_string(),
            id: None,
        }))
    }

    fn configure_upstream(
        &self,
        model: &AiModel,
        provider_config: &AiProviderConfig,
        stream: bool,
    ) -> Result<UpstreamConfig> {
        if let Some(ref endpoint_url) = provider_config.endpoint_url {
            let (scheme, host, port, path) =
                crate::provider::openai::parse_endpoint_url(endpoint_url)?;
            let mut headers = Vec::new();
            add_auth_headers(provider_config, &mut headers);
            return Ok(UpstreamConfig {
                scheme,
                host,
                port,
                path,
                headers,
            });
        }

        // 默认 Gemini API 路径（含模型名）
        // 非流式: :generateContent，流式: :streamGenerateContent?alt=sse
        let model_name = &model.model_name;
        let path = if stream {
            format!(
                "/v1beta/models/{}:streamGenerateContent?alt=sse",
                model_name
            )
        } else {
            format!("/v1beta/models/{}:generateContent", model_name)
        };

        let mut headers = Vec::new();

        // Gemini 认证：API key 作为查询参数或 Bearer token
        let auth: AuthConfig =
            serde_json::from_value(provider_config.auth_config.clone()).unwrap_or_default();

        // 如果有 param_value，追加到 path 查询参数 — append API key to query string
        // 如果有 header_value，作为 Bearer token — use as Bearer token
        let path = if let Some(ref param_value) = auth.param_value {
            let param_name = auth.param_name.as_deref().unwrap_or("key");
            if path.contains('?') {
                format!("{}&{}={}", path, param_name, param_value)
            } else {
                format!("{}?{}={}", path, param_name, param_value)
            }
        } else {
            path
        };

        if let Some(ref header_value) = auth.header_value {
            let header_name = auth
                .header_name
                .clone()
                .unwrap_or_else(|| "Authorization".to_string());
            let value = if header_name == "Authorization" && !header_value.starts_with("Bearer ") {
                format!("Bearer {}", header_value)
            } else {
                header_value.clone()
            };
            headers.push((header_name, value));
        }

        Ok(UpstreamConfig {
            scheme: "https".to_string(),
            host: "generativelanguage.googleapis.com".to_string(),
            port: 443,
            path,
            headers,
        })
    }

    fn extract_usage(&self, body: &str) -> Option<TokenUsage> {
        let resp: GeminiApiResponse = serde_json::from_str(body).ok()?;
        let u = resp.usage_metadata?;
        let pt = u.prompt_token_count.unwrap_or(0);
        let ct = u.candidates_token_count.unwrap_or(0);
        Some(TokenUsage {
            prompt_tokens: Some(pt),
            completion_tokens: Some(ct),
            total_tokens: Some(u.total_token_count.unwrap_or(pt + ct)),
        })
    }

    fn extract_stream_usage(&self, event: &SseEvent) -> Option<TokenUsage> {
        if event.is_done() {
            return None;
        }
        let resp: GeminiApiResponse = serde_json::from_str(&event.data).ok()?;
        let u = resp.usage_metadata?;
        let pt = u.prompt_token_count.unwrap_or(0);
        let ct = u.candidates_token_count.unwrap_or(0);
        Some(TokenUsage {
            prompt_tokens: Some(pt),
            completion_tokens: Some(ct),
            total_tokens: Some(u.total_token_count.unwrap_or(pt + ct)),
        })
    }
}

/// 从 auth_config 提取认证 headers
fn add_auth_headers(provider_config: &AiProviderConfig, headers: &mut Vec<(String, String)>) {
    let auth: AuthConfig =
        serde_json::from_value(provider_config.auth_config.clone()).unwrap_or_default();

    if let Some(ref header_value) = auth.header_value {
        let header_name = auth
            .header_name
            .clone()
            .unwrap_or_else(|| "Authorization".to_string());
        let value = if header_name == "Authorization" && !header_value.starts_with("Bearer ") {
            format!("Bearer {}", header_value)
        } else {
            header_value.clone()
        };
        headers.push((header_name, value));
    }
}

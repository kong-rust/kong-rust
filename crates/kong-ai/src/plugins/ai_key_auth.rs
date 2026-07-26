//! ai-key-auth plugin — virtual key authentication and model allow list
//! ai-key-auth 插件 — 虚拟密钥认证与模型白名单
//!
//! Runs ahead of every other AI plugin (priority 774) so that downstream policy
//! plugins observe an authenticated identity.
//! 先于所有其他 AI 插件执行（priority 774），使下游策略插件能看到已认证的身份。

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use kong_core::error::Result;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

use crate::auth::{model_allowed, AiAuthContext, AuthError, VirtualKeyAuthenticator};

/// Standard bearer credential header — 标准 bearer 凭证头
const HEADER_AUTHORIZATION: &str = "authorization";
/// Anthropic SDK credential header — Anthropic SDK 凭证头
const HEADER_X_API_KEY: &str = "x-api-key";

// ============ Plugin config — 插件配置 ============

/// ai-key-auth plugin config — ai-key-auth 插件配置
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiKeyAuthConfig {
    /// Fallback credential header, tried after Authorization and x-api-key
    /// 兜底凭证头，在 Authorization 与 x-api-key 之后尝试
    pub key_header: String,
    /// Error body format: "auto" | "openai" | "anthropic"
    /// 错误体格式："auto" | "openai" | "anthropic"
    pub error_format: String,
}

impl Default for AiKeyAuthConfig {
    fn default() -> Self {
        Self {
            // Matches ai-rate-limit's header_name default — 与 ai-rate-limit 的 header_name 默认值一致
            key_header: "X-AI-Key".to_string(),
            error_format: "auto".to_string(),
        }
    }
}

/// Where the credential came from — drives "auto" error format detection
/// 凭证来源 — 用于 "auto" 错误格式判定
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialSource {
    Bearer,
    XApiKey,
    Custom,
}

/// Error body dialect — 错误体风格
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorFormat {
    OpenAi,
    Anthropic,
}

// ============ Plugin — 插件结构体 ============

/// Virtual key authentication plugin — 虚拟密钥认证插件
pub struct AiKeyAuthPlugin {
    authenticator: Arc<VirtualKeyAuthenticator>,
}

impl AiKeyAuthPlugin {
    pub fn new(authenticator: Arc<VirtualKeyAuthenticator>) -> Self {
        Self { authenticator }
    }

    /// Extract a credential, in order: Authorization: Bearer → x-api-key → custom header
    /// 按序提取凭证：Authorization: Bearer → x-api-key → 自定义头
    fn extract_credential(
        ctx: &RequestCtx,
        cfg: &AiKeyAuthConfig,
    ) -> Option<(String, CredentialSource)> {
        if let Some(value) = ctx.request_headers.get(HEADER_AUTHORIZATION) {
            // Scheme is case-insensitive per RFC 7235 — 按 RFC 7235，scheme 大小写不敏感
            if let Some(token) = value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
            {
                let token = token.trim();
                if !token.is_empty() {
                    return Some((token.to_string(), CredentialSource::Bearer));
                }
            }
        }

        if let Some(value) = ctx.request_headers.get(HEADER_X_API_KEY) {
            let value = value.trim();
            if !value.is_empty() {
                return Some((value.to_string(), CredentialSource::XApiKey));
            }
        }

        let custom = cfg.key_header.to_lowercase();
        // Skip headers already handled above — 跳过上面已处理的头
        if custom != HEADER_AUTHORIZATION && custom != HEADER_X_API_KEY {
            if let Some(value) = ctx.request_headers.get(&custom) {
                let value = value.trim();
                if !value.is_empty() {
                    return Some((value.to_string(), CredentialSource::Custom));
                }
            }
        }

        None
    }

    /// Resolve the error dialect — 判定错误体风格
    ///
    /// "auto" infers Anthropic from the x-api-key credential header or an
    /// Anthropic-shaped request path, and falls back to OpenAI.
    /// "auto" 依据 x-api-key 凭证头或 Anthropic 形态的请求路径推断为 Anthropic，否则回退 OpenAI。
    fn resolve_error_format(
        cfg: &AiKeyAuthConfig,
        ctx: &RequestCtx,
        source: Option<CredentialSource>,
    ) -> ErrorFormat {
        match cfg.error_format.as_str() {
            "anthropic" => ErrorFormat::Anthropic,
            "openai" => ErrorFormat::OpenAi,
            _ => {
                if source == Some(CredentialSource::XApiKey)
                    || ctx.request_path.contains("/v1/messages")
                {
                    ErrorFormat::Anthropic
                } else {
                    ErrorFormat::OpenAi
                }
            }
        }
    }

    /// Short-circuit with a client-protocol-appropriate error body
    /// 以符合客户端协议的错误体短路
    fn reject(
        ctx: &mut RequestCtx,
        status: u16,
        format: ErrorFormat,
        message: &str,
        openai_type: &str,
        openai_code: &str,
        anthropic_type: &str,
    ) {
        let body = match format {
            ErrorFormat::OpenAi => serde_json::json!({
                "error": {
                    "message": message,
                    "type": openai_type,
                    "code": openai_code,
                }
            }),
            ErrorFormat::Anthropic => serde_json::json!({
                "type": "error",
                "error": {
                    "type": anthropic_type,
                    "message": message,
                }
            }),
        };

        ctx.short_circuited = true;
        ctx.exit_status = Some(status);
        ctx.exit_body = Some(body.to_string());
        // Set explicitly rather than relying on the proxy's default content type
        // 显式设置，不依赖代理层的默认 content type
        let mut headers = std::collections::HashMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        ctx.exit_headers = Some(headers);
    }

    /// Reject a missing or invalid credential — 拒绝缺失或无效的凭证
    fn reject_auth(ctx: &mut RequestCtx, format: ErrorFormat, error: AuthError) {
        let (message, code) = match error {
            AuthError::MissingKey => ("missing API key", "missing_api_key"),
            AuthError::InvalidKey => ("invalid API key", "invalid_api_key"),
        };
        Self::reject(
            ctx,
            401,
            format,
            message,
            "invalid_request_error",
            code,
            "authentication_error",
        );
    }

    /// Extract the requested model name from the request body
    /// 从请求体提取请求的模型名
    ///
    /// Returns None when the body is absent, is not JSON, or carries no `model`
    /// string — under `model_source=config` clients legitimately omit it.
    /// 请求体缺失、非 JSON 或无 `model` 字符串时返回 None — `model_source=config` 部署下客户端合法地不传该字段。
    fn requested_model(ctx: &RequestCtx) -> Option<String> {
        let body = ctx.request_body.as_ref()?;
        let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
        parsed
            .get("model")
            .and_then(|m| m.as_str())
            .map(|m| m.to_string())
    }
}

// ============ PluginHandler — 插件处理器实现 ============

#[async_trait]
impl PluginHandler for AiKeyAuthPlugin {
    fn name(&self) -> &str {
        "ai-key-auth"
    }

    fn priority(&self) -> i32 {
        // Ahead of ai-prompt-guard (773) — identity must resolve before any policy runs
        // 高于 ai-prompt-guard (773) — 身份必须在任何策略执行前解析
        774
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let cfg: AiKeyAuthConfig = crate::parse_plugin_config(config)?;

        // 1. Extract credential — 提取凭证
        let credential = Self::extract_credential(ctx, &cfg);
        let format = Self::resolve_error_format(&cfg, ctx, credential.as_ref().map(|(_, s)| *s));

        let (raw_key, _source) = match credential {
            Some(credential) => credential,
            None => {
                Self::reject_auth(ctx, format, AuthError::MissingKey);
                return Ok(());
            }
        };

        // 2-3. Look up and validate (enabled / expires_at) — 查找并校验（enabled / expires_at）
        let key = match self.authenticator.authenticate(&raw_key).await {
            Ok(key) => key,
            Err(error) => {
                Self::reject_auth(ctx, format, error);
                return Ok(());
            }
        };

        // 4. 先注入已认证身份，使后续 model allow-list 拒绝也能进入 usage 事实。
        ctx.extensions.insert(AiAuthContext {
            virtual_key_id: key.id,
            key_name: key.name.clone(),
            key_prefix: key.key_prefix.clone(),
            consumer_id: key.consumer_id,
        });
        if let Some(consumer_id) = key.consumer_id {
            ctx.consumer_id = Some(consumer_id);
            ctx.authenticated_consumer = Some(serde_json::json!({ "id": consumer_id }));
        }
        ctx.authenticated_credential = Some(serde_json::json!({
            "id": key.id,
            "name": key.name.clone(),
        }));

        // 5. Model allow list — 模型白名单
        if let Some(allowed) = key.allowed_models.as_ref() {
            if let Some(model) = Self::requested_model(ctx) {
                if !model_allowed(allowed, &model) {
                    tracing::info!(
                        "ai-key-auth rejected model '{}' for key '{}'",
                        model,
                        key.name
                    );
                    Self::reject(
                        ctx,
                        403,
                        format,
                        &format!("model `{}` is not allowed for this API key", model),
                        "invalid_request_error",
                        "model_not_allowed",
                        "permission_error",
                    );
                    return Ok(());
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_headers(headers: &[(&str, &str)]) -> RequestCtx {
        let mut ctx = RequestCtx::new();
        for (name, value) in headers {
            // The proxy lowercases header names before plugins run
            // 代理层在插件执行前已将头名转为小写
            ctx.request_headers
                .insert(name.to_lowercase(), value.to_string());
        }
        ctx
    }

    #[test]
    fn extracts_bearer_credential() {
        let ctx = ctx_with_headers(&[("Authorization", "Bearer sk-kr-abc")]);
        let (key, source) =
            AiKeyAuthPlugin::extract_credential(&ctx, &AiKeyAuthConfig::default()).unwrap();
        assert_eq!(key, "sk-kr-abc");
        assert_eq!(source, CredentialSource::Bearer);
    }

    #[test]
    fn bearer_scheme_is_case_insensitive() {
        let ctx = ctx_with_headers(&[("Authorization", "bearer sk-kr-abc")]);
        let (key, _) =
            AiKeyAuthPlugin::extract_credential(&ctx, &AiKeyAuthConfig::default()).unwrap();
        assert_eq!(key, "sk-kr-abc");
    }

    #[test]
    fn extracts_x_api_key_credential() {
        let ctx = ctx_with_headers(&[("x-api-key", "sk-kr-abc")]);
        let (key, source) =
            AiKeyAuthPlugin::extract_credential(&ctx, &AiKeyAuthConfig::default()).unwrap();
        assert_eq!(key, "sk-kr-abc");
        assert_eq!(source, CredentialSource::XApiKey);
    }

    #[test]
    fn extracts_custom_header_credential() {
        let ctx = ctx_with_headers(&[("X-AI-Key", "sk-kr-abc")]);
        let (key, source) =
            AiKeyAuthPlugin::extract_credential(&ctx, &AiKeyAuthConfig::default()).unwrap();
        assert_eq!(key, "sk-kr-abc");
        assert_eq!(source, CredentialSource::Custom);
    }

    #[test]
    fn bearer_wins_over_other_headers() {
        let ctx = ctx_with_headers(&[
            ("Authorization", "Bearer from-bearer"),
            ("x-api-key", "from-x-api-key"),
            ("X-AI-Key", "from-custom"),
        ]);
        let (key, source) =
            AiKeyAuthPlugin::extract_credential(&ctx, &AiKeyAuthConfig::default()).unwrap();
        assert_eq!(key, "from-bearer");
        assert_eq!(source, CredentialSource::Bearer);
    }

    #[test]
    fn non_bearer_authorization_falls_through() {
        // Basic auth must not be mistaken for a virtual key — Basic 认证不得被当作虚拟密钥
        let ctx = ctx_with_headers(&[
            ("Authorization", "Basic dXNlcjpwYXNz"),
            ("x-api-key", "sk-kr-abc"),
        ]);
        let (key, source) =
            AiKeyAuthPlugin::extract_credential(&ctx, &AiKeyAuthConfig::default()).unwrap();
        assert_eq!(key, "sk-kr-abc");
        assert_eq!(source, CredentialSource::XApiKey);
    }

    #[test]
    fn blank_credentials_are_ignored() {
        let ctx = ctx_with_headers(&[("Authorization", "Bearer   "), ("x-api-key", "  ")]);
        assert!(AiKeyAuthPlugin::extract_credential(&ctx, &AiKeyAuthConfig::default()).is_none());
    }

    #[test]
    fn no_credential_returns_none() {
        let ctx = ctx_with_headers(&[]);
        assert!(AiKeyAuthPlugin::extract_credential(&ctx, &AiKeyAuthConfig::default()).is_none());
    }

    #[test]
    fn auto_format_infers_anthropic_from_x_api_key() {
        let ctx = ctx_with_headers(&[]);
        let format = AiKeyAuthPlugin::resolve_error_format(
            &AiKeyAuthConfig::default(),
            &ctx,
            Some(CredentialSource::XApiKey),
        );
        assert_eq!(format, ErrorFormat::Anthropic);
    }

    #[test]
    fn auto_format_infers_anthropic_from_path() {
        // Covers the missing-credential case, where no header signal exists
        // 覆盖无凭证场景 — 此时没有任何头部信号
        let mut ctx = ctx_with_headers(&[]);
        ctx.request_path = "/ai/demo/v1/messages".to_string();
        let format = AiKeyAuthPlugin::resolve_error_format(&AiKeyAuthConfig::default(), &ctx, None);
        assert_eq!(format, ErrorFormat::Anthropic);
    }

    #[test]
    fn auto_format_defaults_to_openai() {
        let mut ctx = ctx_with_headers(&[]);
        ctx.request_path = "/ai/demo/v1/chat/completions".to_string();
        let format = AiKeyAuthPlugin::resolve_error_format(
            &AiKeyAuthConfig::default(),
            &ctx,
            Some(CredentialSource::Bearer),
        );
        assert_eq!(format, ErrorFormat::OpenAi);
    }

    #[test]
    fn explicit_format_overrides_inference() {
        let ctx = ctx_with_headers(&[]);
        let cfg = AiKeyAuthConfig {
            error_format: "openai".to_string(),
            ..Default::default()
        };
        let format =
            AiKeyAuthPlugin::resolve_error_format(&cfg, &ctx, Some(CredentialSource::XApiKey));
        assert_eq!(format, ErrorFormat::OpenAi);

        let cfg = AiKeyAuthConfig {
            error_format: "anthropic".to_string(),
            ..Default::default()
        };
        let format =
            AiKeyAuthPlugin::resolve_error_format(&cfg, &ctx, Some(CredentialSource::Bearer));
        assert_eq!(format, ErrorFormat::Anthropic);
    }

    #[test]
    fn openai_error_body_shape() {
        let mut ctx = ctx_with_headers(&[]);
        AiKeyAuthPlugin::reject_auth(&mut ctx, ErrorFormat::OpenAi, AuthError::MissingKey);

        assert!(ctx.short_circuited);
        assert_eq!(ctx.exit_status, Some(401));
        let body: serde_json::Value =
            serde_json::from_str(ctx.exit_body.as_ref().unwrap()).unwrap();
        assert_eq!(body["error"]["message"], "missing API key");
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["code"], "missing_api_key");
        assert_eq!(
            ctx.exit_headers.as_ref().unwrap().get("Content-Type"),
            Some(&"application/json".to_string())
        );
    }

    #[test]
    fn anthropic_error_body_shape() {
        let mut ctx = ctx_with_headers(&[]);
        AiKeyAuthPlugin::reject_auth(&mut ctx, ErrorFormat::Anthropic, AuthError::InvalidKey);

        assert_eq!(ctx.exit_status, Some(401));
        let body: serde_json::Value =
            serde_json::from_str(ctx.exit_body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "authentication_error");
        assert_eq!(body["error"]["message"], "invalid API key");
    }

    #[test]
    fn invalid_and_missing_keys_are_indistinguishable_in_status() {
        // Status must not reveal which failure occurred — 状态码不得泄露具体失败原因
        let mut missing = ctx_with_headers(&[]);
        AiKeyAuthPlugin::reject_auth(&mut missing, ErrorFormat::OpenAi, AuthError::MissingKey);
        let mut invalid = ctx_with_headers(&[]);
        AiKeyAuthPlugin::reject_auth(&mut invalid, ErrorFormat::OpenAi, AuthError::InvalidKey);
        assert_eq!(missing.exit_status, invalid.exit_status);
    }

    #[test]
    fn requested_model_reads_body() {
        let mut ctx = ctx_with_headers(&[]);
        ctx.request_body = Some(r#"{"model":"gpt-4o","messages":[]}"#.to_string());
        assert_eq!(
            AiKeyAuthPlugin::requested_model(&ctx),
            Some("gpt-4o".to_string())
        );
    }

    #[test]
    fn requested_model_absent_when_body_lacks_model() {
        let mut ctx = ctx_with_headers(&[]);
        ctx.request_body = Some(r#"{"messages":[]}"#.to_string());
        assert_eq!(AiKeyAuthPlugin::requested_model(&ctx), None);
    }

    #[test]
    fn requested_model_absent_for_missing_or_invalid_body() {
        let ctx = ctx_with_headers(&[]);
        assert_eq!(AiKeyAuthPlugin::requested_model(&ctx), None);

        let mut ctx = ctx_with_headers(&[]);
        ctx.request_body = Some("not json".to_string());
        assert_eq!(AiKeyAuthPlugin::requested_model(&ctx), None);
    }
}

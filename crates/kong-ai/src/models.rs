//! AI Gateway 数据模型 — AI Provider、AI Model、AI Virtual Key

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use kong_core::traits::Entity;

/// 单个 AI 模型允许配置的最大权重。
pub const MAX_MODEL_WEIGHT: i32 = 10_000;

/// AI Provider 配置（对应 ai_providers 表）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AiProviderConfig {
    pub id: Uuid,
    pub name: String,
    pub provider_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    pub auth_config: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    pub config: serde_json::Value,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// AI Model（同 name 组成 model group 用于 LB — models with the same name form a load-balancing group）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AiModel {
    pub id: Uuid,
    pub name: String,
    pub provider_id: Uuid,
    pub model_name: String,
    pub priority: i32,
    pub weight: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    /// 单次请求 prompt token 上限（balancer by_token_size 路由用）
    /// 配合 TokenizerRegistry 计算的 prompt_tokens,在 ModelGroupBalancer.select_for 里做候选过滤:
    /// `prompt_tokens <= max_input_tokens` 才能命中,超过则 fallback 到下一 priority
    /// Per-request prompt token cap used by ModelGroupBalancer.select_for (by_token_size routing).
    /// Candidates only match when `prompt_tokens <= max_input_tokens`; otherwise fallback to next priority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<i32>,
    pub config: serde_json::Value,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// AI Virtual Key（虚拟 API Key — virtual API key for rate limiting and budget control）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AiVirtualKey {
    pub id: Uuid,
    pub name: String,
    pub key_hash: String,
    pub key_prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumer_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tpm_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_limit: Option<f64>,
    pub budget_used: f64,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// 认证配置（嵌入在 AiProviderConfig.auth_config JSONB 中 — embedded in auth_config JSONB column）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aws_access_key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aws_secret_access_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aws_region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gcp_service_account_json: Option<String>,
}

// ============ Entity trait 实现 — Entity trait implementations ============

impl Entity for AiProviderConfig {
    fn table_name() -> &'static str { "ai_providers" }
    fn id(&self) -> Uuid { self.id }
    fn endpoint_key() -> Option<&'static str> { Some("name") }
    fn endpoint_key_value(&self) -> Option<String> { Some(self.name.clone()) }
    fn tags(&self) -> Option<&Vec<String>> { self.tags.as_ref() }
}

impl Entity for AiModel {
    fn table_name() -> &'static str { "ai_models" }
    fn id(&self) -> Uuid { self.id }
    fn tags(&self) -> Option<&Vec<String>> { self.tags.as_ref() }
}

impl Entity for AiVirtualKey {
    fn table_name() -> &'static str { "ai_virtual_keys" }
    fn id(&self) -> Uuid { self.id }
    fn endpoint_key() -> Option<&'static str> { Some("name") }
    fn endpoint_key_value(&self) -> Option<String> { Some(self.name.clone()) }
    fn tags(&self) -> Option<&Vec<String>> { self.tags.as_ref() }
}

#[cfg(test)]
mod tests {
    use super::{AiModel, AiProviderConfig, AiVirtualKey};

    fn assert_integer_timestamps(value: serde_json::Value) {
        assert!(value["created_at"].as_i64().is_some());
        assert!(value["updated_at"].as_i64().is_some());
    }

    #[test]
    fn ai_entity_timestamps_serialize_as_integer_epoch_seconds() {
        let created_at = 1_700_000_000;
        let updated_at = 1_700_000_001;

        assert_integer_timestamps(serde_json::to_value(AiProviderConfig {
            created_at: Some(created_at),
            updated_at: Some(updated_at),
            ..Default::default()
        }).unwrap());
        assert_integer_timestamps(serde_json::to_value(AiModel {
            created_at: Some(created_at),
            updated_at: Some(updated_at),
            ..Default::default()
        }).unwrap());
        let virtual_key = serde_json::to_value(AiVirtualKey {
            expires_at: Some(updated_at),
            created_at: Some(created_at),
            updated_at: Some(updated_at),
            ..Default::default()
        }).unwrap();
        assert_integer_timestamps(virtual_key.clone());
        assert!(virtual_key["expires_at"].as_i64().is_some());
    }
}

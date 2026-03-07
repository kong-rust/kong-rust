use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::traits::Entity;

/// Vault 实体 — 与 Kong sm_vaults 表完全一致
/// 注意：Kong 中 table_name 为 "sm_vaults"，但 admin_api_name 为 "vaults"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vault {
    pub id: Uuid,
    /// Vault 前缀标识符，唯一，格式要求 [a-z][a-z0-9-]-[a-z0-9]+
    pub prefix: String,
    /// Vault 类型名称，必填
    pub name: String,
    /// Vault 描述
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Vault 配置（动态 JSON 结构，由各 vault 类型的 schema 定义）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl Default for Vault {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            prefix: String::new(),
            name: String::new(),
            description: None,
            config: None,
            created_at: 0,
            updated_at: 0,
            tags: None,
        }
    }
}

impl Entity for Vault {
    fn table_name() -> &'static str {
        "sm_vaults"
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn endpoint_key() -> Option<&'static str> {
        Some("prefix")
    }

    fn endpoint_key_value(&self) -> Option<String> {
        Some(self.prefix.clone())
    }

    fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
}

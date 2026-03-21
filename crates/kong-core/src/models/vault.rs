use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::traits::Entity;

/// Vault entity — fully consistent with Kong sm_vaults table — Vault 实体 — 与 Kong sm_vaults 表完全一致
/// Note: Kong uses table_name "sm_vaults", but admin_api_name is "vaults" — 注意：Kong 中 table_name 为 "sm_vaults"，但 admin_api_name 为 "vaults"
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Vault {
    pub id: Uuid,
    /// Vault prefix identifier, unique, format: [a-z][a-z0-9-]-[a-z0-9]+ — Vault 前缀标识符，唯一，格式要求 [a-z][a-z0-9-]-[a-z0-9]+
    pub prefix: String,
    /// Vault type name, required — Vault 类型名称，必填
    pub name: String,
    /// Vault description — Vault 描述
    pub description: Option<String>,
    /// Vault configuration (dynamic JSON structure, defined by each vault type's schema) — Vault 配置（动态 JSON 结构，由各 vault 类型的 schema 定义）
    pub config: Option<serde_json::Value>,
    pub created_at: i64,
    pub updated_at: i64,
    pub tags: Option<Vec<String>>,
    /// Workspace ID (foreign key to workspaces) — 工作空间 ID（外键引用 workspaces）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_id: Option<Uuid>,
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
            ws_id: None,
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

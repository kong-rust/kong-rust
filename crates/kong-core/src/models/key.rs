use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::ForeignKey;
use crate::traits::Entity;

/// Key entity — fully consistent with Kong keys table — Key 实体 — 与 Kong keys 表完全一致
///
/// Kong stores jwk/pem in either plaintext or as vault references; the DB column `pem` is JSONB
/// containing optional `private_key` and `public_key` strings.
/// Kong 中 jwk/pem 可能存明文或为 vault 引用；数据库 `pem` 列为 JSONB，含可选 `private_key`、`public_key` 字符串。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Key {
    pub id: Uuid,
    /// Foreign key to key_sets (JSON `set`, DB column `set_id`) — 外键指向 key_sets（JSON 中 `set`，DB 列 `set_id`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<ForeignKey>,
    /// Unique name — 唯一名称
    pub name: Option<String>,
    /// Key ID — required — 密钥 ID，必填
    pub kid: String,
    /// JSON Web Key string — JWK 字符串
    pub jwk: Option<String>,
    /// PEM formatted key pair: { "private_key": "...", "public_key": "..." } — PEM 格式密钥对
    pub pem: Option<serde_json::Value>,
    /// Auto-generated cache key (format: `<kid>:<set_id>`) — 自动生成的缓存键（格式：`<kid>:<set_id>`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_key: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub tags: Option<Vec<String>>,
    /// Workspace ID (foreign key to workspaces) — 工作空间 ID（外键引用 workspaces）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_id: Option<Uuid>,
}

impl Default for Key {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            set: None,
            name: None,
            kid: String::new(),
            jwk: None,
            pem: None,
            cache_key: None,
            created_at: 0,
            updated_at: 0,
            tags: None,
            ws_id: None,
        }
    }
}

impl Entity for Key {
    fn table_name() -> &'static str {
        "keys"
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn endpoint_key() -> Option<&'static str> {
        Some("name")
    }

    fn endpoint_key_value(&self) -> Option<String> {
        self.name.clone()
    }

    fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
}

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::traits::Entity;

/// Consumer entity — fully consistent with Kong consumers table — Consumer 实体 — 与 Kong consumers 表完全一致
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Consumer {
    pub id: Uuid,
    pub created_at: i64,
    pub updated_at: i64,
    /// Unique username (at least one of username or custom_id must be provided) — 唯一用户名（至少提供 username 或 custom_id 之一）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// External unique ID (at least one of username or custom_id must be provided) — 外部唯一 ID（至少提供 username 或 custom_id 之一）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl Default for Consumer {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: 0,
            updated_at: 0,
            username: None,
            custom_id: None,
            tags: None,
        }
    }
}

impl Entity for Consumer {
    fn table_name() -> &'static str {
        "consumers"
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn endpoint_key() -> Option<&'static str> {
        Some("username")
    }

    fn endpoint_key_value(&self) -> Option<String> {
        self.username.clone()
    }

    fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
}

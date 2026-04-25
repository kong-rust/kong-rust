use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::traits::Entity;

/// KeySet entity — fully consistent with Kong key_sets table — KeySet 实体 — 与 Kong key_sets 表完全一致
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KeySet {
    pub id: Uuid,
    /// Optional unique name — 可选的唯一名称
    pub name: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub tags: Option<Vec<String>>,
    /// Workspace ID (foreign key to workspaces) — 工作空间 ID（外键引用 workspaces）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_id: Option<Uuid>,
}

impl Default for KeySet {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: None,
            created_at: 0,
            updated_at: 0,
            tags: None,
            ws_id: None,
        }
    }
}

impl Entity for KeySet {
    fn table_name() -> &'static str {
        "key_sets"
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

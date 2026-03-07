use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::ForeignKey;
use crate::traits::Entity;

/// SNI 实体 — 与 Kong snis 表完全一致
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sni {
    pub id: Uuid,
    /// SNI 名称（支持通配符如 *.example.com），唯一
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// 关联的证书（外键引用 certificates），必填
    pub certificate: ForeignKey,
}

impl Default for Sni {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            created_at: 0,
            updated_at: 0,
            tags: None,
            certificate: ForeignKey::new(Uuid::nil()),
        }
    }
}

impl Entity for Sni {
    fn table_name() -> &'static str {
        "snis"
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn endpoint_key() -> Option<&'static str> {
        Some("name")
    }

    fn endpoint_key_value(&self) -> Option<String> {
        Some(self.name.clone())
    }

    fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
}

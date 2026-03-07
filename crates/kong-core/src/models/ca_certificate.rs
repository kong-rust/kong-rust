use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::traits::Entity;

/// CA Certificate 实体 — 与 Kong ca_certificates 表完全一致
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaCertificate {
    pub id: Uuid,
    pub created_at: i64,
    pub updated_at: i64,
    /// CA 证书内容（PEM 格式），必填
    pub cert: String,
    /// 证书摘要（SHA256 hex），自动计算，唯一
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl Default for CaCertificate {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: 0,
            updated_at: 0,
            cert: String::new(),
            cert_digest: None,
            tags: None,
        }
    }
}

impl Entity for CaCertificate {
    fn table_name() -> &'static str {
        "ca_certificates"
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
}

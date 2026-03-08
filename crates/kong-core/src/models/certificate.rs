use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::traits::Entity;

/// Certificate entity — fully consistent with Kong certificates table — Certificate 实体 — 与 Kong certificates 表完全一致
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Certificate {
    pub id: Uuid,
    pub created_at: i64,
    pub updated_at: i64,
    /// Certificate content (PEM format), required — 证书内容（PEM 格式），必填
    pub cert: String,
    /// Private key (PEM format), required — 私钥（PEM 格式），必填
    pub key: String,
    /// Alternative certificate (PEM format, for different key types e.g. RSA + ECDSA) — 备选证书（PEM 格式，用于不同密钥类型如 RSA + ECDSA）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_alt: Option<String>,
    /// Alternative private key (PEM format) — 备选私钥（PEM 格式）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_alt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl Default for Certificate {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: 0,
            updated_at: 0,
            cert: String::new(),
            key: String::new(),
            cert_alt: None,
            key_alt: None,
            tags: None,
        }
    }
}

impl Entity for Certificate {
    fn table_name() -> &'static str {
        "certificates"
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
}

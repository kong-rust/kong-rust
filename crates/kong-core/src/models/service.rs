use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{ForeignKey, Protocol};
use crate::traits::Entity;

/// Service 实体 — 与 Kong services 表完全一致
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Service {
    pub id: Uuid,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 重试次数，默认 5，范围 0-32767
    pub retries: i32,
    /// 协议，默认 http
    pub protocol: Protocol,
    /// 上游主机名，必填
    pub host: String,
    /// 上游端口，默认 80
    pub port: u16,
    /// 上游路径
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 连接超时（毫秒），默认 60000
    pub connect_timeout: i32,
    /// 写超时（毫秒），默认 60000
    pub write_timeout: i32,
    /// 读超时（毫秒），默认 60000
    pub read_timeout: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// 客户端证书（外键引用 certificates）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_certificate: Option<ForeignKey>,
    /// 是否验证上游 TLS 证书
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_verify: Option<bool>,
    /// TLS 验证深度，范围 0-64
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_verify_depth: Option<i32>,
    /// CA 证书 UUID 列表
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_certificates: Option<Vec<Uuid>>,
    /// 是否启用，默认 true
    pub enabled: bool,
}

impl Default for Service {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: 0,
            updated_at: 0,
            name: None,
            retries: 5,
            protocol: Protocol::Http,
            host: String::new(),
            port: 80,
            path: None,
            connect_timeout: 60000,
            write_timeout: 60000,
            read_timeout: 60000,
            tags: None,
            client_certificate: None,
            tls_verify: None,
            tls_verify_depth: None,
            ca_certificates: None,
            enabled: true,
        }
    }
}

impl Entity for Service {
    fn table_name() -> &'static str {
        "services"
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

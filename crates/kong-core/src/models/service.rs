use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{ForeignKey, Protocol};
use crate::traits::Entity;

/// Service entity — fully consistent with Kong services table — Service 实体 — 与 Kong services 表完全一致
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Service {
    pub id: Uuid,
    pub created_at: i64,
    pub updated_at: i64,
    pub name: Option<String>,
    /// Retry count, default 5, range 0-32767 — 重试次数，默认 5，范围 0-32767
    pub retries: i32,
    /// Protocol, default http — 协议，默认 http
    pub protocol: Protocol,
    /// Upstream hostname, required — 上游主机名，必填
    pub host: String,
    /// Upstream port, default 80 — 上游端口，默认 80
    pub port: u16,
    /// Upstream path — 上游路径
    pub path: Option<String>,
    /// Connection timeout in milliseconds, default 60000 — 连接超时（毫秒），默认 60000
    pub connect_timeout: i32,
    /// Write timeout in milliseconds, default 60000 — 写超时（毫秒），默认 60000
    pub write_timeout: i32,
    /// Read timeout in milliseconds, default 60000 — 读超时（毫秒），默认 60000
    pub read_timeout: i32,
    pub tags: Option<Vec<String>>,
    /// Client certificate (foreign key to certificates) — 客户端证书（外键引用 certificates）
    pub client_certificate: Option<ForeignKey>,
    /// Whether to verify upstream TLS certificate — 是否验证上游 TLS 证书
    pub tls_verify: Option<bool>,
    /// TLS verification depth, range 0-64 — TLS 验证深度，范围 0-64
    pub tls_verify_depth: Option<i32>,
    /// CA certificate UUID list — CA 证书 UUID 列表
    pub ca_certificates: Option<Vec<Uuid>>,
    /// Whether enabled, default true — 是否启用，默认 true
    pub enabled: bool,
    /// Workspace ID (foreign key to workspaces) — 工作空间 ID（外键引用 workspaces）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_id: Option<Uuid>,
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
            ws_id: None,
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

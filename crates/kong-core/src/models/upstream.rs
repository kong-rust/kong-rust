use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{ForeignKey, HashOn, LbAlgorithm};
use crate::traits::Entity;

/// Active healthcheck — healthy threshold configuration — 主动健康检查 — 健康阈值配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HealthyConfig {
    /// Check interval in seconds, 0 means disabled — 检查间隔（秒），0 表示禁用
    #[serde(default)]
    pub interval: f64,
    /// HTTP status codes considered healthy — 被认为健康的 HTTP 状态码列表
    #[serde(default)]
    pub http_statuses: Vec<i32>,
    /// Mark as healthy after this many consecutive successes, 0 means disabled — 连续成功次数达到此值后标记为健康，0 表示禁用
    #[serde(default)]
    pub successes: i32,
}

/// Active healthcheck — unhealthy threshold configuration — 主动健康检查 — 不健康阈值配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnhealthyConfig {
    /// Check interval in seconds, 0 means disabled — 检查间隔（秒），0 表示禁用
    #[serde(default)]
    pub interval: f64,
    /// HTTP status codes considered unhealthy — 被认为不健康的 HTTP 状态码列表
    #[serde(default)]
    pub http_statuses: Vec<i32>,
    /// TCP failure count threshold, 0 means disabled — TCP 失败次数阈值，0 表示禁用
    #[serde(default)]
    pub tcp_failures: i32,
    /// Timeout count threshold, 0 means disabled — 超时次数阈值，0 表示禁用
    #[serde(default)]
    pub timeouts: i32,
    /// HTTP failure count threshold, 0 means disabled — HTTP 失败次数阈值，0 表示禁用
    #[serde(default)]
    pub http_failures: i32,
}

/// Active healthcheck configuration — 主动健康检查配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveHealthcheck {
    /// Check type: tcp, http, https, grpc, grpcs — 检查类型：tcp, http, https, grpc, grpcs
    #[serde(rename = "type", default = "default_check_type")]
    pub check_type: String,
    /// Timeout in seconds, default 1 — 超时时间（秒），默认 1
    #[serde(default = "default_timeout")]
    pub timeout: f64,
    /// Concurrency, default 10 — 并发数，默认 10
    #[serde(default = "default_concurrency")]
    pub concurrency: i32,
    /// HTTP check path, default "/" — HTTP 检查路径，默认 "/"
    #[serde(default = "default_http_path")]
    pub http_path: String,
    /// HTTPS SNI — HTTPS SNI
    #[serde(skip_serializing_if = "Option::is_none")]
    pub https_sni: Option<String>,
    /// Whether to verify HTTPS certificate, default true — 是否验证 HTTPS 证书，默认 true
    #[serde(default = "default_true")]
    pub https_verify_certificate: bool,
    /// Custom request headers — 自定义请求头
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::HashMap<String, Vec<String>>>,
    /// Healthy threshold configuration — 健康阈值配置
    #[serde(default)]
    pub healthy: HealthyConfig,
    /// Unhealthy threshold configuration — 不健康阈值配置
    #[serde(default)]
    pub unhealthy: UnhealthyConfig,
}

impl Default for ActiveHealthcheck {
    fn default() -> Self {
        Self {
            check_type: "http".to_string(),
            timeout: 1.0,
            concurrency: 10,
            http_path: "/".to_string(),
            https_sni: None,
            https_verify_certificate: true,
            headers: None,
            healthy: HealthyConfig::default(),
            unhealthy: UnhealthyConfig::default(),
        }
    }
}

/// Passive healthcheck configuration — 被动健康检查配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassiveHealthcheck {
    /// Check type — 检查类型
    #[serde(rename = "type", default = "default_check_type")]
    pub check_type: String,
    /// Healthy threshold configuration — 健康阈值配置
    #[serde(default)]
    pub healthy: HealthyConfig,
    /// Unhealthy threshold configuration — 不健康阈值配置
    #[serde(default)]
    pub unhealthy: UnhealthyConfig,
}

impl Default for PassiveHealthcheck {
    fn default() -> Self {
        Self {
            check_type: "http".to_string(),
            healthy: HealthyConfig::default(),
            unhealthy: UnhealthyConfig::default(),
        }
    }
}

/// Healthcheck configuration — 健康检查配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthcheckConfig {
    /// Active healthcheck — 主动健康检查
    #[serde(default)]
    pub active: ActiveHealthcheck,
    /// Passive healthcheck — 被动健康检查
    #[serde(default)]
    pub passive: PassiveHealthcheck,
    /// Health threshold percentage (0-100), default 0 — 健康阈值百分比（0-100），默认 0
    #[serde(default)]
    pub threshold: f64,
}

fn default_check_type() -> String {
    "http".to_string()
}

fn default_timeout() -> f64 {
    1.0
}

fn default_concurrency() -> i32 {
    10
}

fn default_http_path() -> String {
    "/".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for HealthcheckConfig {
    fn default() -> Self {
        Self {
            active: ActiveHealthcheck {
                check_type: "http".to_string(),
                timeout: 1.0,
                concurrency: 10,
                http_path: "/".to_string(),
                https_sni: None,
                https_verify_certificate: true,
                headers: None,
                healthy: HealthyConfig {
                    interval: 0.0,
                    http_statuses: vec![200, 302],
                    successes: 0,
                },
                unhealthy: UnhealthyConfig {
                    interval: 0.0,
                    http_statuses: vec![429, 404, 500, 501, 502, 503, 504, 505],
                    tcp_failures: 0,
                    timeouts: 0,
                    http_failures: 0,
                },
            },
            passive: PassiveHealthcheck {
                check_type: "http".to_string(),
                healthy: HealthyConfig {
                    interval: 0.0,
                    http_statuses: vec![
                        200, 201, 202, 203, 204, 205, 206, 207, 208, 226, 300, 301, 302, 303, 304,
                        305, 306, 307, 308,
                    ],
                    successes: 0,
                },
                unhealthy: UnhealthyConfig {
                    interval: 0.0,
                    http_statuses: vec![429, 500, 503],
                    tcp_failures: 0,
                    timeouts: 0,
                    http_failures: 0,
                },
            },
            threshold: 0.0,
        }
    }
}

/// Upstream entity — fully consistent with Kong upstreams table — Upstream 实体 — 与 Kong upstreams 表完全一致
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Upstream {
    pub id: Uuid,
    pub created_at: i64,
    pub updated_at: i64,
    /// Upstream name (must be a valid hostname, not an IP), unique — 上游名称（必须是有效的主机名，不能是 IP），唯一
    pub name: String,
    /// Load balancing algorithm, default round-robin — 负载均衡算法，默认 round-robin
    #[serde(default)]
    pub algorithm: LbAlgorithm,
    /// Hash method, default none — 哈希方式，默认 none
    #[serde(default)]
    pub hash_on: HashOn,
    /// Hash fallback method, default none — 哈希回退方式，默认 none
    #[serde(default)]
    pub hash_fallback: HashOn,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_on_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_fallback_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_on_cookie: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_on_cookie_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_on_query_arg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_fallback_query_arg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_on_uri_capture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash_fallback_uri_capture: Option<String>,
    /// Consistent hashing slot count, default 10000, range 10-65536 — 一致性哈希槽位数，默认 10000，范围 10-65536
    pub slots: i32,
    /// Healthcheck configuration — 健康检查配置
    #[serde(default)]
    pub healthchecks: HealthcheckConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Custom Host header (used when sending to upstream) — 自定义 Host 头（发送到上游时使用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_header: Option<String>,
    /// Client certificate (foreign key to certificates) — 客户端证书（外键引用 certificates）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_certificate: Option<ForeignKey>,
    /// Whether to use SRV hostname, default false — 是否使用 SRV 主机名，默认 false
    #[serde(default)]
    pub use_srv_name: bool,
    /// Workspace ID (foreign key to workspaces) — 工作空间 ID（外键引用 workspaces）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_id: Option<Uuid>,
}

impl Default for Upstream {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: 0,
            updated_at: 0,
            name: String::new(),
            algorithm: LbAlgorithm::RoundRobin,
            hash_on: HashOn::None,
            hash_fallback: HashOn::None,
            hash_on_header: None,
            hash_fallback_header: None,
            hash_on_cookie: None,
            hash_on_cookie_path: None,
            hash_on_query_arg: None,
            hash_fallback_query_arg: None,
            hash_on_uri_capture: None,
            hash_fallback_uri_capture: None,
            slots: 10000,
            healthchecks: HealthcheckConfig::default(),
            tags: None,
            host_header: None,
            client_certificate: None,
            use_srv_name: false,
            ws_id: None,
        }
    }
}

impl Entity for Upstream {
    fn table_name() -> &'static str {
        "upstreams"
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

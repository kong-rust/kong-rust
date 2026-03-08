use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Protocol type — fully consistent with Kong's protocol enum — 协议类型 — 与 Kong 的 protocol 枚举完全一致
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Http,
    Https,
    Tcp,
    Tls,
    Udp,
    Grpc,
    Grpcs,
    #[serde(rename = "tls_passthrough")]
    TlsPassthrough,
    #[serde(rename = "ws")]
    Ws,
    #[serde(rename = "wss")]
    Wss,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Http => write!(f, "http"),
            Protocol::Https => write!(f, "https"),
            Protocol::Tcp => write!(f, "tcp"),
            Protocol::Tls => write!(f, "tls"),
            Protocol::Udp => write!(f, "udp"),
            Protocol::Grpc => write!(f, "grpc"),
            Protocol::Grpcs => write!(f, "grpcs"),
            Protocol::TlsPassthrough => write!(f, "tls_passthrough"),
            Protocol::Ws => write!(f, "ws"),
            Protocol::Wss => write!(f, "wss"),
        }
    }
}

/// Load balancing algorithm — consistent with Kong's algorithm field — 负载均衡算法 — 与 Kong 的 algorithm 字段一致
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LbAlgorithm {
    #[serde(rename = "round-robin")]
    RoundRobin,
    #[serde(rename = "consistent-hashing")]
    ConsistentHashing,
    #[serde(rename = "least-connections")]
    LeastConnections,
    #[serde(rename = "latency")]
    Latency,
}

impl Default for LbAlgorithm {
    fn default() -> Self {
        LbAlgorithm::RoundRobin
    }
}

/// Hash method — consistent with Kong's hash_on / hash_fallback — 哈希方式 — 与 Kong 的 hash_on / hash_fallback 一致
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HashOn {
    None,
    Consumer,
    Ip,
    Header,
    Cookie,
    Path,
    #[serde(rename = "query_arg")]
    QueryArg,
    #[serde(rename = "uri_capture")]
    UriCapture,
}

impl Default for HashOn {
    fn default() -> Self {
        HashOn::None
    }
}

/// Path handling mode — consistent with Kong's path_handling — 路径处理方式 — 与 Kong 的 path_handling 一致
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathHandling {
    #[serde(rename = "v0")]
    V0,
    #[serde(rename = "v1")]
    V1,
}

impl Default for PathHandling {
    fn default() -> Self {
        PathHandling::V0
    }
}

/// Foreign key reference — used to represent associations in JSON serialization — 外键引用 — 用于 JSON 序列化时表示关联关系
/// Kong's JSON format is { "id": "uuid-string" } — Kong 的 JSON 格式为 { "id": "uuid-string" }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKey {
    pub id: Uuid,
}

impl ForeignKey {
    pub fn new(id: Uuid) -> Self {
        Self { id }
    }
}

impl From<Uuid> for ForeignKey {
    fn from(id: Uuid) -> Self {
        Self { id }
    }
}

/// IP + port pair — used for Route sources/destinations — IP + 端口对 — 用于 Route 的 sources/destinations
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CidrPort {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

/// Plugin ordering configuration — 插件排序配置
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginOrdering {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<std::collections::HashMap<String, Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<std::collections::HashMap<String, Vec<String>>>,
}

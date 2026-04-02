//! Kong clustering — CP/DP hybrid mode implementation
//! Kong 集群 — CP/DP 混合模式实现

pub mod cache;
pub mod cp;
pub mod dp;
pub mod protocol;
pub mod tls;

use std::collections::HashMap;
use uuid::Uuid;
use chrono::{DateTime, Utc};

/// Sync status between CP and DP — CP 与 DP 之间的同步状态
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    Unknown,
    Normal,
    KongVersionIncompatible,
    PluginSetIncompatible,
    PluginVersionIncompatible,
    FilterSetIncompatible,
}

impl Default for SyncStatus {
    fn default() -> Self {
        Self::Unknown
    }
}

/// DP node info tracked by CP — CP 跟踪的 DP 节点信息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DataPlaneInfo {
    pub id: Uuid,
    pub ip: String,
    pub hostname: String,
    pub version: String,
    pub sync_status: SyncStatus,
    pub config_hash: String,
    pub last_seen: DateTime<Utc>,
    pub labels: HashMap<String, String>,
}

/// Multi-level config hashes — 多级配置哈希
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConfigHashes {
    pub config: String,
    pub routes: String,
    pub services: String,
    pub plugins: String,
    pub upstreams: String,
    pub targets: String,
}

/// Empty config hash constant (32 zeros) — 空配置哈希常量（32 个零）
pub const EMPTY_CONFIG_HASH: &str = "00000000000000000000000000000000";

/// Clustering error types — 集群错误类型
#[derive(Debug, thiserror::Error)]
pub enum ClusterError {
    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Timeout")]
    Timeout,
}

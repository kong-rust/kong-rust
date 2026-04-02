pub mod error;
pub mod models;
pub mod traits;

use serde::{Serialize, Deserialize};

/// Cluster deployment role — 集群部署角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClusterRole {
    /// Standalone mode, both proxy and admin — 单体模式
    #[serde(rename = "traditional")]
    Traditional,
    /// Control plane, manages config and pushes to DPs — 控制平面
    #[serde(rename = "control_plane")]
    ControlPlane,
    /// Data plane, receives config from CP — 数据平面
    #[serde(rename = "data_plane")]
    DataPlane,
}

impl ClusterRole {
    pub fn is_control_plane(&self) -> bool {
        matches!(self, Self::ControlPlane)
    }
    pub fn is_data_plane(&self) -> bool {
        matches!(self, Self::DataPlane)
    }
    pub fn is_traditional(&self) -> bool {
        matches!(self, Self::Traditional)
    }
}

impl Default for ClusterRole {
    fn default() -> Self {
        Self::Traditional
    }
}

impl std::fmt::Display for ClusterRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Traditional => write!(f, "traditional"),
            Self::ControlPlane => write!(f, "control_plane"),
            Self::DataPlane => write!(f, "data_plane"),
        }
    }
}

impl std::str::FromStr for ClusterRole {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "traditional" => Ok(Self::Traditional),
            "control_plane" => Ok(Self::ControlPlane),
            "data_plane" => Ok(Self::DataPlane),
            _ => Err(format!("invalid role: '{}', valid values: traditional, control_plane, data_plane", s)),
        }
    }
}

/// Clustering sync status — 集群同步状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClusterSyncStatus {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "normal")]
    Normal,
    #[serde(rename = "kong_version_incompatible")]
    KongVersionIncompatible,
    #[serde(rename = "plugin_set_incompatible")]
    PluginSetIncompatible,
    #[serde(rename = "plugin_version_incompatible")]
    PluginVersionIncompatible,
}

impl Default for ClusterSyncStatus {
    fn default() -> Self {
        Self::Unknown
    }
}

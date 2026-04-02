//! mTLS configuration for cluster communication — 集群通信 mTLS 配置
//!
//! Supports two modes: — 支持两种模式:
//! - shared: both sides use the same certificate, digest verification — 共享模式: 双方使用相同证书，摘要验证
//! - pki: CA-signed certificates with full chain verification — PKI 模式: CA 签发证书，完整链验证

use std::path::Path;
use crate::ClusterError;

/// TLS mode for cluster communication — 集群通信 TLS 模式
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClusterTlsMode {
    Shared,
    Pki,
}

impl ClusterTlsMode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "pki" => Self::Pki,
            _ => Self::Shared,
        }
    }
}

/// Cluster TLS configuration — 集群 TLS 配置
#[derive(Debug, Clone)]
pub struct ClusterTlsConfig {
    pub mode: ClusterTlsMode,
    pub cert_path: String,
    pub key_path: String,
    pub ca_cert_path: Option<String>,
    pub server_name: Option<String>,
}

impl ClusterTlsConfig {
    /// Create from KongConfig — 从 KongConfig 创建
    pub fn from_kong_config(config: &kong_config::KongConfig) -> Result<Self, ClusterError> {
        let cert_path = config.cluster_cert.clone()
            .ok_or_else(|| ClusterError::Tls("cluster_cert not configured".to_string()))?;
        let key_path = config.cluster_cert_key.clone()
            .ok_or_else(|| ClusterError::Tls("cluster_cert_key not configured".to_string()))?;

        // Verify files exist — 验证文件存在
        if !Path::new(&cert_path).exists() {
            return Err(ClusterError::Tls(format!("cluster_cert file not found: {}", cert_path)));
        }
        if !Path::new(&key_path).exists() {
            return Err(ClusterError::Tls(format!("cluster_cert_key file not found: {}", key_path)));
        }

        let mode = ClusterTlsMode::from_str(&config.cluster_mtls);

        if mode == ClusterTlsMode::Pki {
            if let Some(ref ca) = config.cluster_ca_cert {
                if !Path::new(ca).exists() {
                    return Err(ClusterError::Tls(format!("cluster_ca_cert file not found: {}", ca)));
                }
            }
        }

        Ok(Self {
            mode,
            cert_path,
            key_path,
            ca_cert_path: config.cluster_ca_cert.clone(),
            server_name: config.cluster_server_name.clone(),
        })
    }

    /// Get the SNI server name for verification — 获取用于验证的 SNI 服务器名
    pub fn effective_server_name(&self) -> &str {
        match &self.server_name {
            Some(name) if !name.is_empty() => name,
            _ => "kong_clustering",
        }
    }
}

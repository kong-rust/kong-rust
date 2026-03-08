//! 异步 DNS 解析器 — 基于 hickory-resolver
//!
//! 特性：
//! - 原生异步，不阻塞 tokio 工作线程
//! - 内置 DNS 缓存（遵循 TTL）
//! - IP 地址直连优化（跳过 DNS 查询）
//! - 支持自定义 nameserver（dns_resolver 配置项）

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::proto::xfer::Protocol;
use hickory_resolver::TokioResolver;

/// 共享异步 DNS 解析器
pub struct DnsResolver {
    resolver: TokioResolver,
}

impl DnsResolver {
    /// 根据 KongConfig 的 dns_* 配置构建解析器
    pub fn new(config: &kong_config::KongConfig) -> Self {
        let resolver_config = if config.dns_resolver.is_empty() {
            ResolverConfig::default()
        } else {
            // 使用自定义 nameserver
            let mut rc = ResolverConfig::new();
            for ns in &config.dns_resolver {
                // 格式：IP 或 IP:PORT
                let addr: SocketAddr = if ns.contains(':') {
                    ns.parse().unwrap_or_else(|_| {
                        tracing::warn!("DNS nameserver 地址解析失败: {}, 使用默认端口", ns);
                        SocketAddr::new(ns.parse().unwrap_or(IpAddr::from([8, 8, 8, 8])), 53)
                    })
                } else {
                    let ip: IpAddr = ns.parse().unwrap_or_else(|_| {
                        tracing::warn!("DNS nameserver IP 解析失败: {}, 使用 8.8.8.8", ns);
                        IpAddr::from([8, 8, 8, 8])
                    });
                    SocketAddr::new(ip, 53)
                };
                rc.add_name_server(NameServerConfig::new(addr, Protocol::Udp));
                rc.add_name_server(NameServerConfig::new(addr, Protocol::Tcp));
            }
            rc
        };

        let mut opts = ResolverOpts::default();
        opts.cache_size = config.dns_cache_size as usize;
        if let Some(ttl) = config.dns_valid_ttl {
            opts.positive_min_ttl = Some(std::time::Duration::from_secs(ttl));
            opts.positive_max_ttl = Some(std::time::Duration::from_secs(ttl));
        }

        let provider = hickory_resolver::name_server::TokioConnectionProvider::default();
        let resolver = TokioResolver::builder_with_config(resolver_config, provider)
            .with_options(opts)
            .build();

        tracing::info!(
            "异步 DNS 解析器已初始化 (cache_size={}, nameservers={})",
            config.dns_cache_size,
            if config.dns_resolver.is_empty() {
                "系统默认".to_string()
            } else {
                config.dns_resolver.join(", ")
            }
        );

        Self { resolver }
    }

    /// 解析 host 为 SocketAddr
    ///
    /// - 如果 host 是 IP 地址，直接返回（跳过 DNS）
    /// - 如果 host 是域名，异步 DNS 查询
    pub async fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Result<SocketAddr, Box<dyn std::error::Error + Send + Sync>> {
        // IP 直连优化
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(SocketAddr::new(ip, port));
        }

        // 异步 DNS 查询
        let response = self.resolver.lookup_ip(host).await.map_err(|e| {
            Box::new(e) as Box<dyn std::error::Error + Send + Sync>
        })?;

        response
            .iter()
            .next()
            .map(|ip| SocketAddr::new(ip, port))
            .ok_or_else(|| {
                Box::from(format!("DNS 解析无结果: {}", host))
                    as Box<dyn std::error::Error + Send + Sync>
            })
    }
}

/// 便于共享的 Arc 包装
pub type SharedDnsResolver = Arc<DnsResolver>;

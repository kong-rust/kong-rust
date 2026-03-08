//! Stream (L4) proxy engine — built on Pingora ServerApp trait — Stream (L4) 代理引擎 — 基于 Pingora ServerApp trait
//!
//! Supports three proxy modes: — 支持三种代理模式：
//! 1. TCP — plaintext TCP forwarding, routed by source/dest IP:port — 明文 TCP 转发，按 source/dest IP:port 路由
//! 2. TLS Passthrough — peek ClientHello to extract SNI, no TLS termination, forward as-is — peek ClientHello 提取 SNI，不终止 TLS，原样转发
//! 3. TLS Termination — terminate TLS, route by SNI, forward plaintext to upstream — 终止 TLS，获取 SNI 路由，转发明文到上游
//!
//! All stream_listen ports are registered as plain TCP (add_tcp); the application layer decides TLS handling. — 所有 stream_listen 端口统一注册为纯 TCP（add_tcp），由应用层决定 TLS 处理方式。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use pingora_core::apps::ServerApp;
use pingora_core::connectors::TransportConnector;
use pingora_core::protocols::Stream;
use pingora_core::server::ShutdownWatch;
use pingora_core::upstreams::peer::BasicPeer;
use uuid::Uuid;

use kong_core::models::{Protocol, Service};
use kong_router::stream::{StreamRequestContext, StreamRouter};

use crate::access_log::AccessLogWriter;
use crate::balancer::LoadBalancer;
use crate::dns::SharedDnsResolver;
use crate::stream_tls::{is_tls_handshake, parse_sni_from_client_hello};
use crate::tls::CertificateManager;

/// Stream proxy service — implements Pingora ServerApp trait — Stream 代理服务 — 实现 Pingora ServerApp trait
pub struct KongStreamProxy {
    /// Stream router (hot-reloadable) — Stream 路由器（可热更新）
    pub stream_router: Arc<RwLock<StreamRouter>>,
    /// Load balancers (shared with HTTP proxy) — 负载均衡器（与 HTTP 代理共享）
    pub balancers: Arc<RwLock<HashMap<String, LoadBalancer>>>,
    /// Service cache (shared with HTTP proxy) — Service 缓存（与 HTTP 代理共享）
    pub services: Arc<RwLock<HashMap<Uuid, Service>>>,
    /// TLS certificate manager (shared with HTTP proxy) — TLS 证书管理器（与 HTTP 代理共享）
    pub cert_manager: Arc<CertificateManager>,
    /// Upstream connector — 上游连接器
    pub connector: TransportConnector,
    /// Async access log writer — Access log 异步写入器
    pub access_log_writer: Option<AccessLogWriter>,
    /// Async DNS resolver — 异步 DNS 解析器
    pub dns_resolver: SharedDnsResolver,
}

/// Proxy pipeline result for access log — 代理流水线结果，用于 access log
struct StreamProxyResult {
    status: &'static str,
    mode: &'static str,
    route_label: Option<String>,
    upstream: Option<String>,
}

impl KongStreamProxy {
    /// Create Stream proxy — 创建 Stream 代理
    pub fn new(
        routes: &[kong_core::models::Route],
        balancers: Arc<RwLock<HashMap<String, LoadBalancer>>>,
        services: Arc<RwLock<HashMap<Uuid, Service>>>,
        cert_manager: Arc<CertificateManager>,
        dns_resolver: SharedDnsResolver,
    ) -> Self {
        Self {
            stream_router: Arc::new(RwLock::new(StreamRouter::new(routes))),
            balancers,
            services,
            cert_manager,
            connector: TransportConnector::new(None),
            access_log_writer: None,
            dns_resolver,
        }
    }

    /// Update Stream routing table — 更新 Stream 路由表
    pub fn update_routes(&self, routes: &[kong_core::models::Route]) {
        if let Ok(mut router) = self.stream_router.write() {
            router.rebuild(routes);
        }
    }

    /// Resolve upstream address (reuses HTTP proxy load balancing logic) — 解析上游地址（复用 HTTP 代理的负载均衡逻辑）
    fn resolve_upstream(&self, service: &Service) -> Option<(String, bool)> {
        let use_tls = matches!(
            service.protocol,
            Protocol::Https | Protocol::Tls | Protocol::Grpcs
        );

        // Try resolving via load balancer — 尝试通过负载均衡器解析
        if let Ok(balancers) = self.balancers.read() {
            if let Some(lb) = balancers.get(&service.host) {
                if let Some(addr) = lb.select() {
                    return Some((addr, use_tls));
                }
            }
        }

        // Use Service's host:port directly — 直接使用 Service 的 host:port
        Some((format!("{}:{}", service.host, service.port), use_tls))
    }

    /// Handle a single Stream connection — 处理单个 Stream 连接
    async fn handle_connection(&self, mut downstream: Stream) {
        let start = std::time::Instant::now();

        // Get client and local addresses — 获取客户端和本地地址
        let (source_ip, source_port, dest_ip, dest_port) = extract_addrs(&*downstream);
        let remote_addr = source_ip
            .map(|ip| format!("{}:{}", ip, source_port.unwrap_or(0)))
            .unwrap_or_else(|| "-".to_string());

        // 1. Peek TLS Record Header (5 bytes) to determine if TLS and get record length — Peek TLS Record Header（5 字节）判断是否 TLS 并获取记录长度
        let mut header_buf = [0u8; 5];
        let is_tls = match downstream.try_peek(&mut header_buf).await {
            Ok(true) => is_tls_handshake(header_buf[0]),
            _ => false,
        };

        // 2. If TLS, peek full ClientHello using exact record length — 如果是 TLS，用精确的记录长度 peek 完整 ClientHello
        let sni = if is_tls {
            // TLS Record Header: ContentType(1) + Version(2) + Length(2) — TLS Record Header：ContentType(1) + Version(2) + Length(2)
            let record_len = ((header_buf[3] as usize) << 8) | (header_buf[4] as usize);
            // Total = 5 (header) + record payload, cap to reasonable max — 总长 = 5（header）+ 记录负载，限制合理上限
            let total = (5 + record_len).min(16384);
            let mut hello_buf = vec![0u8; total];
            match downstream.try_peek(&mut hello_buf).await {
                Ok(true) => parse_sni_from_client_hello(&hello_buf),
                _ => None,
            }
        } else {
            None
        };

        // 3. Build routing context — 构建路由上下文
        let ctx = StreamRequestContext {
            source_ip,
            source_port,
            dest_ip,
            dest_port,
            sni: sni.clone(),
        };

        // Execute proxy pipeline, collect result for unified access log — 执行代理流水线，收集结果用于统一 access log
        let proxy_result = self.do_proxy(downstream, &ctx, is_tls).await;

        let elapsed = start.elapsed();
        let mode = if is_tls { proxy_result.mode } else { "tcp" };
        let status = proxy_result.status;
        let route_label = proxy_result.route_label.as_deref().unwrap_or("-");
        let upstream = proxy_result.upstream.as_deref().unwrap_or("-");

        // Access Log — 写入 access log
        let now = chrono::Utc::now().format("%d/%b/%Y:%H:%M:%S %z");
        let log_line = format!(
            "{} [{}] {} {} -> {} sni={} elapsed={}ms status={}\n",
            remote_addr,
            now,
            mode,
            route_label,
            upstream,
            sni.as_deref().unwrap_or("-"),
            elapsed.as_millis(),
            status,
        );

        if let Some(ref writer) = self.access_log_writer {
            writer.write(log_line);
        }

        tracing::debug!(
            "stream: {} {} -> {} sni={} {}ms {}",
            mode,
            remote_addr,
            upstream,
            sni.as_deref().unwrap_or("-"),
            elapsed.as_millis(),
            status,
        );
    }

    /// Execute the proxy pipeline (route → service → upstream → forward) — 执行代理流水线（路由 → 服务 → 上游 → 转发）
    async fn do_proxy(
        &self,
        downstream: Stream,
        ctx: &StreamRequestContext,
        is_tls: bool,
    ) -> StreamProxyResult {
        // 1. Route matching — 路由匹配
        let route_match = {
            let router = match self.stream_router.read() {
                Ok(r) => r,
                Err(_) => {
                    tracing::error!("Stream 路由器读取失败");
                    return StreamProxyResult {
                        status: "ROUTER_ERR", mode: if is_tls { "tls" } else { "tcp" },
                        route_label: None, upstream: None,
                    };
                }
            };
            router.find_route(ctx)
        };

        let route_match = match route_match {
            Some(rm) => rm,
            None => {
                tracing::debug!("Stream 连接无匹配路由: sni={:?}", ctx.sni);
                return StreamProxyResult {
                    status: "NO_ROUTE", mode: if is_tls { "tls" } else { "tcp" },
                    route_label: None, upstream: None,
                };
            }
        };

        let route_label = Some(
            route_match.route_name.clone()
                .unwrap_or_else(|| route_match.route_id.to_string()),
        );

        // 2. Find Service — 查找 Service
        let service = match route_match.service_id {
            Some(service_id) => {
                let services = match self.services.read() {
                    Ok(s) => s,
                    Err(_) => {
                        return StreamProxyResult {
                            status: "SVC_ERR", mode: if is_tls { "tls" } else { "tcp" },
                            route_label, upstream: None,
                        };
                    }
                };
                services.get(&service_id).cloned()
            }
            None => None,
        };

        let service = match service {
            Some(s) if s.enabled => s,
            _ => {
                tracing::debug!("Stream 路由 {} 无有效 Service", route_match.route_id);
                return StreamProxyResult {
                    status: "NO_SVC", mode: if is_tls { "tls" } else { "tcp" },
                    route_label, upstream: None,
                };
            }
        };

        // 3. Resolve upstream address — 解析上游地址
        let (upstream_addr, upstream_tls) = match self.resolve_upstream(&service) {
            Some(r) => r,
            None => {
                tracing::error!("Stream 上游解析失败: {}", service.host);
                return StreamProxyResult {
                    status: "UPSTREAM_ERR", mode: if is_tls { "tls" } else { "tcp" },
                    route_label, upstream: Some(service.host.clone()),
                };
            }
        };

        // 4. Determine proxy mode and execute — 判断代理模式并执行
        let is_passthrough = route_match
            .protocols
            .iter()
            .any(|p| matches!(p, Protocol::TlsPassthrough));

        let mode = if is_passthrough {
            "tls_passthrough"
        } else if is_tls {
            "tls"
        } else {
            "tcp"
        };

        let result = if is_passthrough {
            self.proxy_passthrough(downstream, &upstream_addr).await
        } else if is_tls && route_match.protocols.iter().any(|p| matches!(p, Protocol::Tls)) {
            // TLS Termination: simplified as TCP passthrough for now — TLS Termination 暂作为 TCP 透传
            // TODO: 实现完整 TLS termination（SslAcceptor + CertificateManager）
            tracing::warn!(
                "TLS Termination 模式暂作为 TCP 透传处理 (route={})",
                route_match.route_id
            );
            self.proxy_passthrough(downstream, &upstream_addr).await
        } else {
            self.proxy_tcp(downstream, &upstream_addr, upstream_tls).await
        };

        let status = if result.is_ok() { "OK" } else { "ERR" };
        if let Err(ref e) = result {
            tracing::debug!("Stream 代理错误: {} : {}", upstream_addr, e);
        }

        StreamProxyResult {
            status, mode, route_label, upstream: Some(upstream_addr),
        }
    }

    /// TCP plaintext proxy: bidirectional forwarding after establishing upstream connection — TCP 明文代理：建立上游连接后双向转发
    async fn proxy_tcp(
        &self,
        mut downstream: Stream,
        upstream_addr: &str,
        upstream_tls: bool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut upstream = self.connect_upstream(upstream_addr, upstream_tls).await?;
        bidirectional_copy(&mut downstream, &mut upstream).await?;
        Ok(())
    }

    /// TLS Passthrough proxy: no TLS termination, forward as-is — TLS Passthrough 代理：不终止 TLS，原样转发
    async fn proxy_passthrough(
        &self,
        mut downstream: Stream,
        upstream_addr: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Passthrough mode: no TLS to upstream (data is already encrypted) — Passthrough 模式上游不做 TLS（数据已加密）
        let mut upstream = self.connect_upstream(upstream_addr, false).await?;
        bidirectional_copy(&mut downstream, &mut upstream).await?;
        Ok(())
    }

    /// Connect to upstream — 连接上游
    async fn connect_upstream(
        &self,
        addr: &str,
        _tls: bool,
    ) -> Result<Stream, Box<dyn std::error::Error + Send + Sync>> {
        // Async DNS resolution — 异步 DNS 解析
        let (host, port) = if let Some(colon_pos) = addr.rfind(':') {
            let h = &addr[..colon_pos];
            let p: u16 = addr[colon_pos + 1..].parse().unwrap_or(80);
            (h, p)
        } else {
            (addr, 80u16)
        };

        let socket_addr = self.dns_resolver.resolve(host, port).await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::from(format!("上游��址解析失败 {}: {}", addr, e))
            })?;

        // Build connection using BasicPeer (plain TCP, no TLS) — 使用 BasicPeer 构建连接（纯 TCP，不做 TLS）
        let peer = BasicPeer::new(&socket_addr.to_string());

        let stream = self
            .connector
            .new_stream(&peer)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                Box::from(format!("上游连接失败 {}: {}", addr, e))
            })?;

        Ok(stream)
    }
}

#[async_trait]
impl ServerApp for KongStreamProxy {
    async fn process_new(
        self: &Arc<Self>,
        session: Stream,
        _shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        self.handle_connection(session).await;
        None // Stream proxy does not reuse connections — Stream 代理不复用连接
    }
}

/// Extract client and local addresses from Stream's SocketDigest — 从 Stream 的 SocketDigest 中提取客户端和本地地址
fn extract_addrs(stream: &dyn pingora_core::protocols::IO) -> (Option<IpAddr>, Option<u16>, Option<IpAddr>, Option<u16>) {
    let digest = stream.get_socket_digest();

    let (source_ip, source_port) = digest
        .as_ref()
        .and_then(|d| d.peer_addr())
        .and_then(|addr| addr.as_inet())
        .map(|inet| (Some(inet.ip()), Some(inet.port())))
        .unwrap_or((None, None));

    let (dest_ip, dest_port) = digest
        .as_ref()
        .and_then(|d| d.local_addr())
        .and_then(|addr| addr.as_inet())
        .map(|inet| (Some(inet.ip()), Some(inet.port())))
        .unwrap_or((None, None));

    (source_ip, source_port, dest_ip, dest_port)
}

/// Bidirectional data copy (downstream ↔ upstream) — 双向数据拷贝（downstream ↔ upstream）
async fn bidirectional_copy(
    a: &mut Stream,
    b: &mut Stream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match tokio::io::copy_bidirectional(a, b).await {
        Ok((a_to_b, b_to_a)) => {
            tracing::trace!(
                "Stream 双向拷贝完成: downstream→upstream={} upstream→downstream={}",
                a_to_b,
                b_to_a
            );
            Ok(())
        }
        Err(e) => {
            // Normal close cases like connection reset are not treated as errors — 连接重置等正常关闭情况不记为错误
            if e.kind() == std::io::ErrorKind::ConnectionReset
                || e.kind() == std::io::ErrorKind::BrokenPipe
                || e.kind() == std::io::ErrorKind::UnexpectedEof
            {
                tracing::trace!("Stream 连接正常关闭: {}", e);
                Ok(())
            } else {
                Err(Box::new(e))
            }
        }
    }
}

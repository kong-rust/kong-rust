//! Stream (L4) 代理引擎 — 基于 Pingora ServerApp trait
//!
//! 支持三种代理模式：
//! 1. TCP — 明文 TCP 转发，按 source/dest IP:port 路由
//! 2. TLS Passthrough — peek ClientHello 提取 SNI，不终止 TLS，原样转发
//! 3. TLS Termination — 终止 TLS，获取 SNI 路由，转发明文到上游
//!
//! 所有 stream_listen 端口统一注册为纯 TCP（add_tcp），由应用层决定 TLS 处理方式。

use std::collections::HashMap;
use std::io::Write;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use pingora_core::apps::ServerApp;
use pingora_core::connectors::TransportConnector;
use pingora_core::protocols::Stream;
use pingora_core::server::ShutdownWatch;
use pingora_core::upstreams::peer::BasicPeer;
use uuid::Uuid;

use kong_core::models::{Protocol, Service};
use kong_router::stream::{StreamRequestContext, StreamRouter};

use crate::balancer::LoadBalancer;
use crate::stream_tls::{is_tls_handshake, parse_sni_from_client_hello, CLIENT_HELLO_PEEK_SIZE};
use crate::tls::CertificateManager;

/// Stream 代理服务 — 实现 Pingora ServerApp trait
pub struct KongStreamProxy {
    /// Stream 路由器（可热更新）
    pub stream_router: Arc<RwLock<StreamRouter>>,
    /// 负载均衡器（与 HTTP 代理共享）
    pub balancers: Arc<RwLock<HashMap<String, LoadBalancer>>>,
    /// Service 缓存（与 HTTP 代理共享）
    pub services: Arc<RwLock<HashMap<Uuid, Service>>>,
    /// TLS 证书管理器（与 HTTP 代理共享）
    pub cert_manager: Arc<CertificateManager>,
    /// 上游连接器
    pub connector: TransportConnector,
    /// Access log 文件写入器
    pub access_log_writer: Option<Arc<Mutex<std::io::BufWriter<std::fs::File>>>>,
}

impl KongStreamProxy {
    /// 创建 Stream 代理
    pub fn new(
        routes: &[kong_core::models::Route],
        balancers: Arc<RwLock<HashMap<String, LoadBalancer>>>,
        services: Arc<RwLock<HashMap<Uuid, Service>>>,
        cert_manager: Arc<CertificateManager>,
    ) -> Self {
        Self {
            stream_router: Arc::new(RwLock::new(StreamRouter::new(routes))),
            balancers,
            services,
            cert_manager,
            connector: TransportConnector::new(None),
            access_log_writer: None,
        }
    }

    /// 初始化 access log 文件写入
    pub fn init_access_log(&mut self, path: &str) {
        if path == "off" {
            return;
        }
        let log_path = std::path::Path::new(path);
        if let Some(dir) = log_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(file) => {
                self.access_log_writer =
                    Some(Arc::new(Mutex::new(std::io::BufWriter::new(file))));
                tracing::info!("Stream access log 输出到: {}", path);
            }
            Err(e) => {
                tracing::error!("Stream access log 文件打开失败: {} ({})", path, e);
            }
        }
    }

    /// 更新 Stream 路由表
    pub fn update_routes(&self, routes: &[kong_core::models::Route]) {
        if let Ok(mut router) = self.stream_router.write() {
            router.rebuild(routes);
        }
    }

    /// 解析上游地址（复用 HTTP 代理的负载均衡逻辑）
    fn resolve_upstream(&self, service: &Service) -> Option<(String, bool)> {
        let use_tls = matches!(
            service.protocol,
            Protocol::Https | Protocol::Tls | Protocol::Grpcs
        );

        // 尝试通过负载均衡器解析
        if let Ok(balancers) = self.balancers.read() {
            if let Some(lb) = balancers.get(&service.host) {
                if let Some(addr) = lb.select() {
                    return Some((addr, use_tls));
                }
            }
        }

        // 直接使用 Service 的 host:port
        Some((format!("{}:{}", service.host, service.port), use_tls))
    }

    /// 处理单个 Stream 连接
    async fn handle_connection(&self, mut downstream: Stream) {
        let start = std::time::Instant::now();

        // 获取客户端和本地地址
        let (source_ip, source_port, dest_ip, dest_port) = extract_addrs(&*downstream);
        let remote_addr = source_ip
            .map(|ip| format!("{}:{}", ip, source_port.unwrap_or(0)))
            .unwrap_or_else(|| "-".to_string());

        // 1. Peek 首字节判断是否 TLS
        let mut peek_buf = [0u8; 1];
        let is_tls = match downstream.try_peek(&mut peek_buf).await {
            Ok(true) => is_tls_handshake(peek_buf[0]),
            _ => false,
        };

        // 2. 如果是 TLS，peek ClientHello 解析 SNI
        let sni = if is_tls {
            let mut hello_buf = vec![0u8; CLIENT_HELLO_PEEK_SIZE];
            match downstream.try_peek(&mut hello_buf).await {
                Ok(true) => parse_sni_from_client_hello(&hello_buf),
                _ => None,
            }
        } else {
            None
        };

        // 3. 构建路由上下文
        let ctx = StreamRequestContext {
            source_ip,
            source_port,
            dest_ip,
            dest_port,
            sni: sni.clone(),
        };

        // 4. 路由匹配
        let route_match = {
            let router = match self.stream_router.read() {
                Ok(r) => r,
                Err(_) => {
                    tracing::error!("Stream 路由器读取失败");
                    return;
                }
            };
            router.find_route(&ctx)
        };

        let route_match = match route_match {
            Some(rm) => rm,
            None => {
                tracing::debug!(
                    "Stream 连接无匹配路由: {} sni={:?}",
                    remote_addr,
                    sni
                );
                return;
            }
        };

        // 5. 查找 Service
        let service = match route_match.service_id {
            Some(service_id) => {
                let services = match self.services.read() {
                    Ok(s) => s,
                    Err(_) => return,
                };
                services.get(&service_id).cloned()
            }
            None => None,
        };

        let service = match service {
            Some(s) if s.enabled => s,
            _ => {
                tracing::debug!("Stream 路由 {} 无有效 Service", route_match.route_id);
                return;
            }
        };

        // 6. 解析上游地址
        let (upstream_addr, upstream_tls) = match self.resolve_upstream(&service) {
            Some(r) => r,
            None => {
                tracing::error!("Stream 上游解析失败: {}", service.host);
                return;
            }
        };

        // 7. 判断代理模式并执行
        let is_passthrough = route_match
            .protocols
            .iter()
            .any(|p| matches!(p, Protocol::TlsPassthrough));

        let result = if is_passthrough {
            // TLS Passthrough：直接透传加密数据
            self.proxy_passthrough(downstream, &upstream_addr).await
        } else if is_tls && route_match.protocols.iter().any(|p| matches!(p, Protocol::Tls)) {
            // TLS Termination：终止 TLS 后转发
            // 当前简化实现：作为 TCP 透传
            // TODO: 实现完整 TLS termination（SslAcceptor + CertificateManager）
            tracing::warn!(
                "TLS Termination 模式暂作为 TCP 透传处理 (route={})",
                route_match.route_id
            );
            self.proxy_passthrough(downstream, &upstream_addr).await
        } else {
            // TCP：明文双向转发
            self.proxy_tcp(downstream, &upstream_addr, upstream_tls)
                .await
        };

        let elapsed = start.elapsed();
        let status = if result.is_ok() { "OK" } else { "ERR" };

        // 8. Access Log
        let now = chrono::Utc::now().format("%d/%b/%Y:%H:%M:%S %z");
        let mode = if is_passthrough {
            "tls_passthrough"
        } else if is_tls {
            "tls"
        } else {
            "tcp"
        };
        let log_line = format!(
            "{} [{}] {} {} -> {} sni={} elapsed={}ms status={}\n",
            remote_addr,
            now,
            mode,
            route_match
                .route_name
                .as_deref()
                .unwrap_or(&route_match.route_id.to_string()),
            upstream_addr,
            sni.as_deref().unwrap_or("-"),
            elapsed.as_millis(),
            status,
        );

        if let Some(ref writer) = self.access_log_writer {
            if let Ok(mut w) = writer.lock() {
                let _ = w.write_all(log_line.as_bytes());
                let _ = w.flush();
            }
        }

        tracing::debug!(
            "stream: {} {} -> {} sni={} {}ms {}",
            mode,
            remote_addr,
            upstream_addr,
            sni.as_deref().unwrap_or("-"),
            elapsed.as_millis(),
            status,
        );

        if let Err(e) = result {
            tracing::debug!("Stream 代理错误: {} -> {} : {}", remote_addr, upstream_addr, e);
        }
    }

    /// TCP 明文代理：建立上游连接后双向转发
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

    /// TLS Passthrough 代理：不终止 TLS，原样转发
    async fn proxy_passthrough(
        &self,
        mut downstream: Stream,
        upstream_addr: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Passthrough 模式上游不做 TLS（数据已加密）
        let mut upstream = self.connect_upstream(upstream_addr, false).await?;
        bidirectional_copy(&mut downstream, &mut upstream).await?;
        Ok(())
    }

    /// 连接上游
    async fn connect_upstream(
        &self,
        addr: &str,
        _tls: bool,
    ) -> Result<Stream, Box<dyn std::error::Error + Send + Sync>> {
        // DNS 解析
        let addr_with_port = if addr.contains(':') {
            addr.to_string()
        } else {
            format!("{}:80", addr)
        };

        let socket_addr: std::net::SocketAddr = addr_with_port
            .parse()
            .or_else(|_| {
                std::net::ToSocketAddrs::to_socket_addrs(&addr_with_port.as_str())
                    .map_err(|e| format!("上游地址解析失败 {}: {}", addr, e))
                    .and_then(|mut iter| {
                        iter.next()
                            .ok_or_else(|| format!("上游地址无可用 IP: {}", addr))
                    })
            })
            .map_err(|e: String| -> Box<dyn std::error::Error + Send + Sync> { Box::from(e) })?;

        // 使用 BasicPeer 构建连接（纯 TCP，不做 TLS）
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
        None // Stream 代理不复用连接
    }
}

/// 从 Stream 的 SocketDigest 中提取客户端和本地地址
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

/// 双向数据拷贝（downstream ↔ upstream）
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
            // 连接重置等正常关闭情况不记为错误
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

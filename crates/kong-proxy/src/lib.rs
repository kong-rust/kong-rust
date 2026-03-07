//! Kong 代理引擎 — 基于 Pingora 实现
//!
//! 职责:
//! - 接收下游 HTTP 请求
//! - 通过路由器匹配路由和服务
//! - 执行插件链（rewrite → access → header_filter → body_filter → log）
//! - 将请求转发到上游服务
//! - 支持负载均衡和健康检查

pub mod balancer;
pub mod health;
pub mod tls;

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use uuid::Uuid;

use kong_core::models::{CaCertificate, Route, Service, Target, Upstream};
use kong_core::traits::RequestCtx;
use kong_router::{RequestContext, RouteMatch, Router};
use kong_plugin_system::{PluginExecutor, PluginRegistry, ResolvedPlugin};

use crate::balancer::LoadBalancer;
use crate::tls::CertificateManager;

/// 请求级上下文 — 在 Pingora 各阶段间传递
pub struct KongCtx {
    /// 路由匹配结果
    pub route_match: Option<RouteMatch>,
    /// 匹配到的 Service
    pub service: Option<Service>,
    /// 选中的上游地址（host:port）
    pub upstream_addr: Option<String>,
    /// 是否使用 TLS 连接上游
    pub upstream_tls: bool,
    /// 上游 SNI
    pub upstream_sni: String,
    /// 插件上下文
    pub plugin_ctx: RequestCtx,
    /// 当前请求已解析的插件链
    pub resolved_plugins: Vec<ResolvedPlugin>,
}

/// Kong 代理服务 — 实现 Pingora ProxyHttp trait
#[derive(Clone)]
pub struct KongProxy {
    /// 路由器（可热更新）
    pub router: Arc<RwLock<Router>>,
    /// 插件注册表
    pub plugin_registry: Arc<PluginRegistry>,
    /// 负载均衡器（upstream_name -> LoadBalancer）
    pub balancers: Arc<RwLock<HashMap<String, LoadBalancer>>>,
    /// Service 缓存（service_id -> Service）
    pub services: Arc<RwLock<HashMap<Uuid, Service>>>,
    /// 所有插件配置
    pub plugins: Arc<RwLock<Vec<kong_core::models::Plugin>>>,
    /// TLS 证书管理器（SNI 匹配 + 客户端证书查找）
    pub cert_manager: Arc<CertificateManager>,
    /// CA 证书列表（用于上游 TLS 验证）
    pub ca_certificates: Arc<RwLock<Vec<CaCertificate>>>,
    /// Access log 文件写入器（None 表示 off/禁用）
    pub access_log_writer: Option<Arc<Mutex<std::io::BufWriter<std::fs::File>>>>,
}

impl KongProxy {
    pub fn new(
        routes: &[Route],
        router_flavor: &str,
        plugin_registry: PluginRegistry,
        cert_manager: CertificateManager,
        ca_certificates: Vec<CaCertificate>,
    ) -> Self {
        Self {
            router: Arc::new(RwLock::new(Router::new(routes, router_flavor))),
            plugin_registry: Arc::new(plugin_registry),
            balancers: Arc::new(RwLock::new(HashMap::new())),
            services: Arc::new(RwLock::new(HashMap::new())),
            plugins: Arc::new(RwLock::new(Vec::new())),
            cert_manager: Arc::new(cert_manager),
            ca_certificates: Arc::new(RwLock::new(ca_certificates)),
            access_log_writer: None,
        }
    }

    /// 初始化 access log 文件写入（路径为 "off" 时禁用）
    pub fn init_access_log(&mut self, path: &str) {
        if path == "off" {
            return;
        }
        let log_path = std::path::Path::new(path);
        if let Some(dir) = log_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => {
                self.access_log_writer = Some(Arc::new(Mutex::new(std::io::BufWriter::new(file))));
                tracing::info!("Access log 输出到: {}", path);
            }
            Err(e) => {
                tracing::error!("Access log 文件打开失败: {} ({})", path, e);
            }
        }
    }

    /// 更新路由表
    pub fn update_routes(&self, routes: &[Route]) {
        if let Ok(mut router) = self.router.write() {
            router.rebuild(routes);
        }
    }

    /// 更新服务缓存
    pub fn update_services(&self, services: Vec<Service>) {
        if let Ok(mut cache) = self.services.write() {
            cache.clear();
            for svc in services {
                cache.insert(svc.id, svc);
            }
        }
    }

    /// 更新上游和目标
    pub fn update_upstreams(&self, upstreams: Vec<Upstream>, targets: Vec<Target>) {
        if let Ok(mut balancers) = self.balancers.write() {
            balancers.clear();
            for upstream in &upstreams {
                let upstream_targets: Vec<&Target> = targets
                    .iter()
                    .filter(|t| t.upstream.id == upstream.id)
                    .collect();
                tracing::info!(
                    "更新 upstream={} targets={}",
                    upstream.name,
                    upstream_targets.len()
                );
                let lb = LoadBalancer::new(upstream, &upstream_targets);
                balancers.insert(upstream.name.clone(), lb);
            }
        }
    }

    /// 更新插件配置
    pub fn update_plugins(&self, plugins: Vec<kong_core::models::Plugin>) {
        if let Ok(mut p) = self.plugins.write() {
            *p = plugins;
        }
    }

    /// 热更新 CA 证书列表
    pub fn update_ca_certificates(&self, cas: Vec<CaCertificate>) {
        if let Ok(mut ca) = self.ca_certificates.write() {
            *ca = cas;
        }
    }

    /// 根据请求构建 RequestContext
    fn build_request_context(session: &Session) -> RequestContext {
        let req = session.req_header();
        let method = req.method.as_str().to_string();
        let uri = req.uri.path().to_string();
        let host = req
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost")
            .to_string();

        // Pingora 不直接提供 is_https() 方法
        // 通过 digest 判断是否 TLS 连接
        let is_tls = session
            .digest()
            .map(|d| d.ssl_digest.is_some())
            .unwrap_or(false);
        let scheme = if is_tls {
            "https".to_string()
        } else {
            "http".to_string()
        };

        let mut headers = HashMap::new();
        for (name, value) in req.headers.iter() {
            if let Ok(v) = value.to_str() {
                headers.insert(name.as_str().to_lowercase(), v.to_string());
            }
        }

        RequestContext {
            method,
            uri,
            host,
            scheme,
            headers,
            sni: None, // TODO: 从 TLS session 获取
        }
    }

    /// 解析上游地址
    fn resolve_upstream(
        &self,
        service: &Service,
    ) -> std::result::Result<(String, bool, String), Box<pingora_core::Error>> {
        let use_tls = matches!(
            service.protocol,
            kong_core::models::Protocol::Https
                | kong_core::models::Protocol::Grpcs
                | kong_core::models::Protocol::Tls
        );

        // 尝试通过负载均衡器解析上游地址
        if let Ok(balancers) = self.balancers.read() {
            if let Some(lb) = balancers.get(&service.host) {
                if let Some(addr) = lb.select() {
                    // SNI 优先级：upstream.host_header > target 地址的主机名部分
                    let sni = lb.host_header()
                        .unwrap_or_else(|| addr.split(':').next().unwrap_or(&addr).to_string());
                    return Ok((addr, use_tls, sni));
                }
            }
        }

        // 直接使用 Service 的 host:port
        let addr = format!("{}:{}", service.host, service.port);
        let sni = service.host.clone();
        Ok((addr, use_tls, sni))
    }
}

#[async_trait]
impl ProxyHttp for KongProxy {
    type CTX = KongCtx;

    fn new_ctx(&self) -> Self::CTX {
        KongCtx {
            route_match: None,
            service: None,
            upstream_addr: None,
            upstream_tls: false,
            upstream_sni: String::new(),
            plugin_ctx: RequestCtx::new(),
            resolved_plugins: Vec::new(),
        }
    }

    /// 请求过滤阶段 — 路由匹配 + 插件 rewrite/access
    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<bool> {
        // 1. 路由匹配
        let req_ctx = Self::build_request_context(session);

        let route_match = {
            let router = self.router.read().map_err(|_| {
                pingora_core::Error::new_str("路由器读取失败")
            })?;
            router.find_route(&req_ctx)
        };

        let route_match = match route_match {
            Some(rm) => rm,
            None => {
                // 无匹配路由，返回 404
                let _ = session.respond_error(404).await;
                return Ok(true);
            }
        };

        // 2. 查找 Service
        let service = if let Some(service_id) = route_match.service_id {
            let services = self.services.read().map_err(|_| {
                pingora_core::Error::new_str("服务缓存读取失败")
            })?;
            services.get(&service_id).cloned()
        } else {
            None
        };

        let service = match service {
            Some(s) => s,
            None => {
                let _ = session.respond_error(503).await;
                return Ok(true);
            }
        };

        // 检查 Service 是否启用
        if !service.enabled {
            let _ = session.respond_error(503).await;
            return Ok(true);
        }

        // 3. 解析上游地址
        let (upstream_addr, upstream_tls, upstream_sni) =
            self.resolve_upstream(&service).map_err(|_| {
                pingora_core::Error::new_str("上游解析失败")
            })?;

        // 4. 设置插件上下文
        ctx.plugin_ctx.route_id = Some(route_match.route_id);
        ctx.plugin_ctx.service_id = route_match.service_id;

        // 5. 解析插件链
        let resolved_plugins = {
            let plugins = self.plugins.read().map_err(|_| {
                pingora_core::Error::new_str("插件配置读取失败")
            })?;
            PluginExecutor::resolve_plugins(
                &self.plugin_registry,
                &plugins,
                Some(route_match.route_id),
                route_match.service_id,
                None, // consumer_id 在认证插件执行后确定
            )
        };

        // 6. 执行 rewrite 阶段
        if let Err(e) = PluginExecutor::execute_phase(
            &resolved_plugins,
            kong_core::traits::Phase::Rewrite,
            &mut ctx.plugin_ctx,
        )
        .await
        {
            tracing::error!("Rewrite 阶段执行失败: {}", e);
            let _ = session.respond_error(500).await;
            return Ok(true);
        }

        // 7. 检查短路
        if ctx.plugin_ctx.is_short_circuited() {
            let status = ctx.plugin_ctx.exit_status.unwrap_or(200);
            let _ = session.respond_error(status).await;
            return Ok(true);
        }

        // 8. 执行 access 阶段
        if let Err(e) = PluginExecutor::execute_phase(
            &resolved_plugins,
            kong_core::traits::Phase::Access,
            &mut ctx.plugin_ctx,
        )
        .await
        {
            tracing::error!("Access 阶段执行失败: {}", e);
            let _ = session.respond_error(500).await;
            return Ok(true);
        }

        // 9. 检查短路
        if ctx.plugin_ctx.is_short_circuited() {
            let status = ctx.plugin_ctx.exit_status.unwrap_or(200);
            let _ = session.respond_error(status).await;
            return Ok(true);
        }

        // 保存到上下文
        ctx.route_match = Some(route_match);
        ctx.service = Some(service);
        ctx.upstream_addr = Some(upstream_addr);
        ctx.upstream_tls = upstream_tls;
        ctx.upstream_sni = upstream_sni;
        ctx.resolved_plugins = resolved_plugins;

        Ok(false) // 继续到上游
    }

    /// 确定上游地址
    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<Box<HttpPeer>> {
        let raw_addr = ctx.upstream_addr.as_deref().ok_or_else(|| {
            pingora_core::Error::new_str("上游地址未设置")
        })?;

        // 确保地址包含端口，不带端口时根据协议补全默认端口
        let addr_with_port = if raw_addr.contains(':') {
            raw_addr.to_string()
        } else {
            let default_port = if ctx.upstream_tls { 443 } else { 80 };
            format!("{}:{}", raw_addr, default_port)
        };

        // DNS 解析：将域名转为 IP:port，避免 HttpPeer::new 内部 unwrap panic
        let socket_addr = std::net::ToSocketAddrs::to_socket_addrs(&addr_with_port.as_str())
            .map_err(|e| {
                tracing::error!("上游地址解析失败: {} ({})", addr_with_port, e);
                pingora_core::Error::because(
                    pingora_core::ErrorType::ConnectError,
                    "上游地址解析失败",
                    e,
                )
            })?
            .next()
            .ok_or_else(|| {
                tracing::error!("上游地址无可用 IP: {}", addr_with_port);
                pingora_core::Error::new_str("上游地址无可用 IP")
            })?;

        let mut peer = HttpPeer::new(socket_addr, ctx.upstream_tls, ctx.upstream_sni.clone());

        // 与 Kong 保持一致：上游 TLS 验证默认关闭
        // 仅当 Service 显式设置 tls_verify = true 时才启用验证
        if ctx.upstream_tls {
            let service = ctx.service.as_ref();
            let tls_verify = service.and_then(|s| s.tls_verify).unwrap_or(false);
            peer.options.verify_cert = tls_verify;
            peer.options.verify_hostname = tls_verify;

            // CA 证书信任链：当 tls_verify=true 且 Service 配置了 ca_certificates 时加载
            if tls_verify {
                if let Some(ca_ids) = service.and_then(|s| s.ca_certificates.as_ref()) {
                    if let Ok(cas) = self.ca_certificates.read() {
                        let mut x509_cas = Vec::new();
                        for ca_id in ca_ids {
                            if let Some(ca) = cas.iter().find(|c| c.id == *ca_id) {
                                match pingora_core::tls::x509::X509::from_pem(ca.cert.as_bytes()) {
                                    Ok(x509) => x509_cas.push(x509),
                                    Err(e) => tracing::warn!("CA 证书解析失败 ({}): {}", ca_id, e),
                                }
                            }
                        }
                        if !x509_cas.is_empty() {
                            peer.options.ca = Some(Arc::new(x509_cas.into_boxed_slice()));
                        }
                    }
                }
            }

            // mTLS 客户端证书：当 Service 配置了 client_certificate 时加载
            if let Some(fk) = service.and_then(|s| s.client_certificate.as_ref()) {
                if let Some(pair) = self.cert_manager.get_certificate_by_id(&fk.id) {
                    match (
                        pingora_core::tls::x509::X509::from_pem(pair.cert.as_bytes()),
                        pingora_core::tls::pkey::PKey::private_key_from_pem(pair.key.as_bytes()),
                    ) {
                        (Ok(x509), Ok(pkey)) => {
                            let cert_key = pingora_core::utils::tls::CertKey::new(vec![x509], pkey);
                            peer.client_cert_key = Some(Arc::new(cert_key));
                        }
                        (Err(e), _) => tracing::warn!("客户端证书解析失败: {}", e),
                        (_, Err(e)) => tracing::warn!("客户端私钥解析失败: {}", e),
                    }
                }
            }
        }

        Ok(Box::new(peer))
    }

    /// 修改发往上游的请求头
    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<()> {
        // 1. preserve_host 处理
        if let Some(ref rm) = ctx.route_match {
            if rm.preserve_host {
                // preserve_host=true：保持客户端原始 Host 头
                if let Some(host) = session.req_header().headers.get("host") {
                    let _ = upstream_request.insert_header("host", host);
                }
            } else {
                // preserve_host=false：使用上游实际主机名
                // 优先级：upstream.host_header > SNI（target 域名） > service.host:port
                let host_header = if !ctx.upstream_sni.is_empty() {
                    ctx.upstream_sni.clone()
                } else if let Some(ref service) = ctx.service {
                    if service.port == 80 || service.port == 443 {
                        service.host.clone()
                    } else {
                        format!("{}:{}", service.host, service.port)
                    }
                } else {
                    String::new()
                };
                if !host_header.is_empty() {
                    let _ = upstream_request.insert_header("host", &host_header);
                }
            }
        }

        // 2. 应用插件设置的上游请求头修改
        for (name, value) in ctx.plugin_ctx.upstream_headers_to_set.drain(..) {
            if let Ok(header_name) = http::header::HeaderName::from_bytes(name.as_bytes()) {
                if let Ok(header_value) = http::header::HeaderValue::from_str(&value) {
                    upstream_request.headers.insert(header_name, header_value);
                }
            }
        }
        for name in ctx.plugin_ctx.upstream_headers_to_remove.drain(..) {
            if let Ok(header_name) = http::header::HeaderName::from_bytes(name.as_bytes()) {
                upstream_request.headers.remove(header_name);
            }
        }

        // 3. strip_path 处理
        if let Some(ref rm) = ctx.route_match {
            if rm.strip_path {
                if let Some(ref matched_path) = rm.matched_path {
                    let original_uri = session.req_header().uri.path();
                    let stripped = original_uri
                        .strip_prefix(matched_path.trim_end_matches('/'))
                        .unwrap_or(original_uri);
                    let new_path = if stripped.is_empty() || !stripped.starts_with('/') {
                        format!("/{}", stripped)
                    } else {
                        stripped.to_string()
                    };

                    // 拼接 service path
                    let final_path = if let Some(ref service) = ctx.service {
                        if let Some(ref svc_path) = service.path {
                            let svc_path = svc_path.trim_end_matches('/');
                            if new_path == "/" {
                                format!("{}/", svc_path)
                            } else {
                                format!("{}{}", svc_path, new_path)
                            }
                        } else {
                            new_path
                        }
                    } else {
                        new_path
                    };

                    // 保留 query string
                    let query = session.req_header().uri.query();
                    let new_uri = if let Some(q) = query {
                        format!("{}?{}", final_path, q)
                    } else {
                        final_path
                    };

                    if let Ok(uri) = new_uri.parse() {
                        upstream_request.set_uri(uri);
                    }
                }
            }
        }

        // 4. 添加 Kong 标准头
        let _ = upstream_request.insert_header("x-forwarded-proto",
            if session.digest().map(|d| d.ssl_digest.is_some()).unwrap_or(false) { "https" } else { "http" });

        if let Some(host) = session.req_header().headers.get("host") {
            let _ = upstream_request.insert_header("x-forwarded-host", host);
        }

        Ok(())
    }

    /// 上游响应处理
    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<()> {
        // 执行 header_filter 阶段
        // 需要临时 clone plugins 以避免同时借用 ctx 的不同字段
        let plugins = ctx.resolved_plugins.clone();
        if let Err(e) = PluginExecutor::execute_phase(
            &plugins,
            kong_core::traits::Phase::HeaderFilter,
            &mut ctx.plugin_ctx,
        )
        .await
        {
            tracing::error!("HeaderFilter 阶段执行失败: {}", e);
        }

        // 应用插件设置的响应头修改
        for (name, value) in ctx.plugin_ctx.response_headers_to_set.drain(..) {
            if let Ok(header_name) = http::header::HeaderName::from_bytes(name.as_bytes()) {
                if let Ok(header_value) = http::header::HeaderValue::from_str(&value) {
                    upstream_response.headers.insert(header_name, header_value);
                }
            }
        }
        for name in ctx.plugin_ctx.response_headers_to_remove.drain(..) {
            if let Ok(header_name) = http::header::HeaderName::from_bytes(name.as_bytes()) {
                upstream_response.headers.remove(header_name);
            }
        }

        // 添加 Kong 标准响应头
        let _ = upstream_response.insert_header("via", "kong-rust/0.1.0");
        let _ = upstream_response.insert_header("x-kong-proxy-latency", "0");
        let _ = upstream_response.insert_header("x-kong-upstream-latency", "0");

        Ok(())
    }

    /// 日志阶段
    async fn logging(
        &self,
        session: &mut Session,
        error: Option<&pingora_core::Error>,
        ctx: &mut Self::CTX,
    ) {
        // Access Log：写入独立文件（与 Kong/Nginx combined 格式类似）
        let req = session.req_header();
        let method = req.method.as_str();
        let uri = req.uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
        let host = req.headers.get("host").and_then(|v| v.to_str().ok()).unwrap_or("-");
        let remote_addr = session
            .client_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|| "-".to_string());
        let status = session
            .response_written()
            .map(|r| r.status.as_u16())
            .unwrap_or(0);
        let upstream = ctx.upstream_addr.as_deref().unwrap_or("-");
        let user_agent = req.headers.get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");

        let now = chrono::Utc::now().format("%d/%b/%Y:%H:%M:%S %z");

        let log_line = if let Some(e) = error {
            format!(
                "{} - - [{}] \"{} {} HTTP/1.1\" {} - \"{}\" upstream={} error=\"{}\"\n",
                remote_addr, now, method, uri, status, user_agent, upstream, e
            )
        } else {
            format!(
                "{} - - [{}] \"{} {} HTTP/1.1\" {} - \"{}\" upstream={}\n",
                remote_addr, now, method, uri, status, user_agent, upstream
            )
        };

        // 写入 access log 文件
        if let Some(ref writer) = self.access_log_writer {
            if let Ok(mut w) = writer.lock() {
                let _ = w.write_all(log_line.as_bytes());
                let _ = w.flush();
            }
        }

        // 同时输出到 error log（debug 级别，方便调试）
        tracing::debug!("access: {} {} {} -> {} upstream={}", host, method, uri, status, upstream);

        // 执行插件 log 阶段
        if let Err(e) = PluginExecutor::execute_phase(
            &ctx.resolved_plugins,
            kong_core::traits::Phase::Log,
            &mut ctx.plugin_ctx,
        )
        .await
        {
            tracing::error!("Log 阶段执行失败: {}", e);
        }
    }
}

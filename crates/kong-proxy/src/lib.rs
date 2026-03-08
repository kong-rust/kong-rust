//! Kong 代理引擎 — 基于 Pingora 实现
//!
//! 职责:
//! - 接收下游 HTTP 请求
//! - 通过路由器匹配路由和服务
//! - 执行插件链（rewrite → access → header_filter → body_filter → log）
//! - 将请求转发到上游服务
//! - 支持负载均衡和健康检查

pub mod access_log;
pub mod balancer;
pub mod dns;
pub mod health;
pub mod phases;
pub mod stream;
pub mod stream_tls;
pub mod tls;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use bytes::Bytes;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use uuid::Uuid;

use kong_core::models::{CaCertificate, Route, Service, Target, Upstream};
use kong_core::traits::RequestCtx;
use kong_router::{RequestContext, RouteMatch, Router};
use kong_plugin_system::{PluginExecutor, PluginRegistry, ResolvedPlugin};

use crate::access_log::AccessLogWriter;
use crate::balancer::LoadBalancer;
use crate::dns::SharedDnsResolver;
use crate::phases::PhaseRunner;
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
    /// Access log 异步写入器（None 表示 off/禁用）
    pub access_log_writer: Option<AccessLogWriter>,
    /// 异步 DNS 解析器
    pub dns_resolver: SharedDnsResolver,
}

impl KongProxy {
    pub fn new(
        routes: &[Route],
        router_flavor: &str,
        plugin_registry: PluginRegistry,
        cert_manager: CertificateManager,
        ca_certificates: Vec<CaCertificate>,
        dns_resolver: SharedDnsResolver,
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
            dns_resolver,
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

        // Pingora 0.8 的 SslDigest 不直接暴露 server_name 字段
        // SNI 信息通过路由匹配中的 host 头间接获取
        let sni: Option<String> = None;

        RequestContext {
            method,
            uri,
            host,
            scheme,
            headers,
            sni,
        }
    }

    /// 填充 RequestCtx 的请求快照（供 PDK 使用）
    fn populate_request_ctx(session: &Session, ctx: &mut RequestCtx) {
        let req = session.req_header();
        ctx.request_method = req.method.as_str().to_string();
        ctx.request_path = req.uri.path().to_string();
        ctx.request_query_string = req.uri.query().unwrap_or("").to_string();

        let is_tls = session
            .digest()
            .map(|d| d.ssl_digest.is_some())
            .unwrap_or(false);
        ctx.request_scheme = if is_tls { "https".to_string() } else { "http".to_string() };

        // host 和 port
        let host_header = req
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost");

        if let Some(colon_pos) = host_header.rfind(':') {
            let (h, p) = host_header.split_at(colon_pos);
            ctx.request_host = h.to_string();
            ctx.request_port = p[1..].parse().unwrap_or(if is_tls { 443 } else { 80 });
        } else {
            ctx.request_host = host_header.to_string();
            ctx.request_port = if is_tls { 443 } else { 80 };
        }

        // 请求头快照
        ctx.request_headers.clear();
        for (name, value) in req.headers.iter() {
            if let Ok(v) = value.to_str() {
                ctx.request_headers
                    .insert(name.as_str().to_lowercase(), v.to_string());
            }
        }

        // 客户端 IP
        ctx.client_ip = session
            .client_addr()
            .map(|a| {
                // 去掉端口部分
                let s = a.to_string();
                s.split(':').next().unwrap_or(&s).to_string()
            })
            .unwrap_or_default();
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

    /// 发送短路响应（支持自定义 status + headers + body）
    async fn send_short_circuit_response(
        session: &mut Session,
        ctx: &mut RequestCtx,
    ) -> pingora_core::Result<bool> {
        let status_code = ctx.exit_status.unwrap_or(200);
        let body = ctx.exit_body.take();
        let headers = ctx.exit_headers.take();

        // 构造响应头
        let body_bytes = body.as_deref().unwrap_or("").as_bytes();
        let mut resp = ResponseHeader::build(status_code, Some(4))?;
        resp.insert_header("content-length", body_bytes.len().to_string())?;
        resp.insert_header("content-type", "application/json; charset=utf-8")?;
        resp.insert_header("server", "kong-rust/0.1.0")?;

        // 应用自定义响应头
        if let Some(hdrs) = headers {
            for (name, value) in hdrs {
                if let Ok(header_name) = http::header::HeaderName::from_bytes(name.as_bytes()) {
                    if let Ok(header_value) = http::header::HeaderValue::from_str(&value) {
                        resp.headers.insert(header_name, header_value);
                    }
                }
            }
        }

        session.write_response_header(Box::new(resp), false).await?;
        if !body_bytes.is_empty() {
            session
                .write_response_body(Some(Bytes::copy_from_slice(body_bytes)), true)
                .await?;
        } else {
            session.write_response_body(None, true).await?;
        }

        Ok(true)
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

        // 5. 填充请求快照（供 PDK 读取真实数据）
        Self::populate_request_ctx(session, &mut ctx.plugin_ctx);

        // 6. 解析插件链
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

        // 7. 执行 rewrite 阶段
        if let Err(e) = PhaseRunner::run_rewrite(&resolved_plugins, &mut ctx.plugin_ctx).await {
            tracing::error!("Rewrite 阶段执行失败: {}", e);
            let _ = session.respond_error(500).await;
            return Ok(true);
        }

        // 8. 检查短路
        if ctx.plugin_ctx.is_short_circuited() {
            // 保存插件链供 log 阶段使用
            ctx.resolved_plugins = resolved_plugins;
            return Self::send_short_circuit_response(session, &mut ctx.plugin_ctx).await;
        }

        // 9. 执行 access 阶段
        if let Err(e) = PhaseRunner::run_access(&resolved_plugins, &mut ctx.plugin_ctx).await {
            tracing::error!("Access 阶段执行失败: {}", e);
            let _ = session.respond_error(500).await;
            return Ok(true);
        }

        // 10. 检查短路
        if ctx.plugin_ctx.is_short_circuited() {
            ctx.resolved_plugins = resolved_plugins;
            return Self::send_short_circuit_response(session, &mut ctx.plugin_ctx).await;
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

        // 确保地址包含端口
        let addr_with_port = if raw_addr.contains(':') {
            raw_addr.to_string()
        } else {
            let default_port = if ctx.upstream_tls { 443 } else { 80 };
            format!("{}:{}", raw_addr, default_port)
        };

        // 异步 DNS 解析（IP 直连自动跳过 DNS 查询）
        let (host, port) = if let Some(colon_pos) = addr_with_port.rfind(':') {
            let h = &addr_with_port[..colon_pos];
            let p: u16 = addr_with_port[colon_pos + 1..].parse().unwrap_or(80);
            (h, p)
        } else {
            (addr_with_port.as_str(), 80u16)
        };
        let socket_addr = self.dns_resolver.resolve(host, port).await.map_err(|e| {
            tracing::error!("上游地址解析失败: {} ({})", addr_with_port, e);
            pingora_core::Error::new_str("上游地址解析失败")
        })?;

        let mut peer = HttpPeer::new(socket_addr, ctx.upstream_tls, ctx.upstream_sni.clone());

        // 上游 TLS 配置
        if ctx.upstream_tls {
            let service = ctx.service.as_ref();
            let tls_verify = service.and_then(|s| s.tls_verify).unwrap_or(false);
            peer.options.verify_cert = tls_verify;
            peer.options.verify_hostname = tls_verify;

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
                if let Some(host) = session.req_header().headers.get("host") {
                    let _ = upstream_request.insert_header("host", host);
                }
            } else {
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

    /// 上游响应头处理 — header_filter 阶段
    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<()> {
        // 填充响应快照到 RequestCtx
        ctx.plugin_ctx.response_status = Some(upstream_response.status.as_u16());
        ctx.plugin_ctx.response_headers.clear();
        for (name, value) in upstream_response.headers.iter() {
            if let Ok(v) = value.to_str() {
                ctx.plugin_ctx
                    .response_headers
                    .insert(name.as_str().to_lowercase(), v.to_string());
            }
        }

        // 执行 header_filter 阶段
        let plugins = ctx.resolved_plugins.clone();
        if let Err(e) = PhaseRunner::run_header_filter(&plugins, &mut ctx.plugin_ctx).await {
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

    /// 响应体过滤 — body_filter 阶段
    fn response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<Option<std::time::Duration>> {
        if ctx.resolved_plugins.is_empty() {
            return Ok(None);
        }

        // body_filter 需要同步执行（Pingora 的 response_body_filter 是同步的）
        // 使用 block_on 适配异步插件接口
        if let Some(ref mut body_bytes) = body {
            let plugins = ctx.resolved_plugins.clone();
            // 在当前 tokio 运行时中阻塞执行
            let handle = tokio::runtime::Handle::try_current();
            if let Ok(handle) = handle {
                let mut body_clone = body_bytes.clone();
                let result = handle.block_on(async {
                    PhaseRunner::run_body_filter(
                        &plugins,
                        &mut ctx.plugin_ctx,
                        &mut body_clone,
                        end_of_stream,
                    )
                    .await
                });

                if let Err(e) = result {
                    tracing::error!("BodyFilter 阶段执行失败: {}", e);
                } else {
                    *body_bytes = body_clone;
                }
            }
        }

        Ok(None)
    }

    /// 日志阶段
    async fn logging(
        &self,
        session: &mut Session,
        error: Option<&pingora_core::Error>,
        ctx: &mut Self::CTX,
    ) {
        // Access Log
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

        // 异步写入 access log 文件
        if let Some(ref writer) = self.access_log_writer {
            writer.write(log_line.clone());
        }

        tracing::debug!("access: {} {} {} -> {} upstream={}", host, method, uri, status, upstream);

        // 执行插件 log 阶段（总是执行，即使之前短路）
        if let Err(e) = PhaseRunner::run_log(&ctx.resolved_plugins, &mut ctx.plugin_ctx).await {
            tracing::error!("Log 阶段执行失败: {}", e);
        }
    }
}

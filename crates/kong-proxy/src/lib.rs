//! Kong proxy engine — built on Pingora — Kong 代理引擎 — 基于 Pingora 实现
//!
//! Responsibilities: — 职责:
//! - Receive downstream HTTP requests — 接收下游 HTTP 请求
//! - Match routes and services via the router — 通过路由器匹配路由和服务
//! - Execute plugin chain (rewrite → access → header_filter → body_filter → log) — 执行插件链（rewrite → access → header_filter → body_filter → log）
//! - Forward requests to upstream services — 将请求转发到上游服务
//! - Support load balancing and health checks — 支持负载均衡和健康检查

pub mod access_log;
pub mod balancer;
pub mod dns;
pub mod health;
pub mod phases;
pub mod spillable_buffer;
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

use kong_config::KongConfig;
use kong_core::models::{CaCertificate, Route, Service, Target, Upstream};
use kong_core::traits::RequestCtx;
use kong_plugin_system::{PluginExecutor, PluginRegistry, ResolvedPlugin};
use kong_router::{RequestContext, RouteMatch, Router};

use crate::access_log::AccessLogWriter;
use crate::balancer::LoadBalancer;
use crate::dns::SharedDnsResolver;
use crate::phases::PhaseRunner;
use crate::spillable_buffer::SpillableBuffer;
use crate::tls::CertificateManager;

/// Per-request context — passed between Pingora phases — 请求级上下文 — 在 Pingora 各阶段间传递
pub struct KongCtx {
    /// Route match result — 路由匹配结果
    pub route_match: Option<RouteMatch>,
    /// Matched Service — 匹配到的 Service
    pub service: Option<Service>,
    /// Selected upstream address (host:port) — 选中的上游地址（host:port）
    pub upstream_addr: Option<String>,
    /// Whether to use TLS for upstream connection — 是否使用 TLS 连接上游
    pub upstream_tls: bool,
    /// Upstream SNI — 上游 SNI
    pub upstream_sni: String,
    /// Plugin context — 插件上下文
    pub plugin_ctx: RequestCtx,
    /// Resolved plugin chain for the current request (Arc for cheap clone) — 当前请求已解析的插件链（Arc 包装以便廉价 clone）
    pub resolved_plugins: Arc<Vec<ResolvedPlugin>>,
    /// Request body buffer (with spill-to-disk protection) — 请求体缓冲区（带落盘保护）
    pub request_body_buf: Option<SpillableBuffer>,
    /// Response body buffer (with spill-to-disk protection) — 响应体缓冲区（带落盘保护）
    pub response_body_buf: Option<SpillableBuffer>,
    /// Whether Lua header_filter should be deferred until the buffered response body is available. — 是否要等响应体缓冲完成后再执行 Lua header_filter。
    pub deferred_header_filter: bool,
    /// Timestamp of last received body chunk (for timeout protection) — 最后收到 body chunk 的时间戳（用于超时保护）
    pub last_body_chunk_at: Option<std::time::Instant>,
    /// proxy 注入到 upstream 请求的 real-ip header 键值对，用于 access log 输出
    pub injected_real_ip_headers: Vec<(String, String)>,
    /// Request start time (for latency tracking) — 请求开始时间（用于延迟统计）
    pub request_start_time: std::time::Instant,
    /// Upstream response received time (for latency tracking) — 上游响应接收时间（用于延迟统计）
    pub upstream_response_time: Option<std::time::Instant>,
    /// Per-request unique ID (sent to both upstream and downstream) — 每请求唯一 ID（同时发送给上游和下游）
    pub request_id: String,
}

/// Kong proxy service — implements Pingora ProxyHttp trait — Kong 代理服务 — 实现 Pingora ProxyHttp trait
#[derive(Clone)]
pub struct KongProxy {
    /// Kong configuration — Kong 配置
    pub config: Arc<KongConfig>,
    /// Router (hot-reloadable) — 路由器（可热更新）
    pub router: Arc<RwLock<Router>>,
    /// Plugin registry — 插件注册表
    pub plugin_registry: Arc<PluginRegistry>,
    /// Load balancers (upstream_name -> LoadBalancer) — 负载均衡器（upstream_name -> LoadBalancer）
    pub balancers: Arc<RwLock<HashMap<String, LoadBalancer>>>,
    /// Service cache (service_id -> Service) — Service 缓存（service_id -> Service）
    pub services: Arc<RwLock<HashMap<Uuid, Service>>>,
    /// All plugin configurations — 所有插件配置
    pub plugins: Arc<RwLock<Vec<kong_core::models::Plugin>>>,
    /// TLS certificate manager (SNI matching + client certificate lookup) — TLS 证书管理器（SNI 匹配 + 客户端证书查找）
    pub cert_manager: Arc<CertificateManager>,
    /// CA certificate list (for upstream TLS verification) — CA 证书列表（用于上游 TLS 验证）
    pub ca_certificates: Arc<RwLock<Vec<CaCertificate>>>,
    /// Async access log writer (None means off/disabled) — Access log 异步写入器（None 表示 off/禁用）
    pub access_log_writer: Option<AccessLogWriter>,
    /// Async DNS resolver — 异步 DNS 解析器
    pub dns_resolver: SharedDnsResolver,
    /// Pre-computed plugin chains: (route_id, service_id) -> sorted plugin list — 预计算插件链
    pub plugin_chains: Arc<RwLock<HashMap<(Option<Uuid>, Option<Uuid>), Arc<Vec<ResolvedPlugin>>>>>,
}

impl KongProxy {
    pub fn new(
        routes: &[Route],
        router_flavor: &str,
        plugin_registry: PluginRegistry,
        cert_manager: CertificateManager,
        ca_certificates: Vec<CaCertificate>,
        dns_resolver: SharedDnsResolver,
        config: Arc<KongConfig>,
    ) -> Self {
        Self {
            config,
            router: Arc::new(RwLock::new(Router::new(routes, router_flavor))),
            plugin_registry: Arc::new(plugin_registry),
            balancers: Arc::new(RwLock::new(HashMap::new())),
            services: Arc::new(RwLock::new(HashMap::new())),
            plugins: Arc::new(RwLock::new(Vec::new())),
            cert_manager: Arc::new(cert_manager),
            ca_certificates: Arc::new(RwLock::new(ca_certificates)),
            access_log_writer: None,
            dns_resolver,
            plugin_chains: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Update routing table — 更新路由表
    pub fn update_routes(&self, routes: &[Route]) {
        if let Ok(mut router) = self.router.write() {
            router.rebuild(routes);
        }
        self.rebuild_plugin_chains();
    }

    /// Update service cache — 更新服务缓存
    pub fn update_services(&self, services: Vec<Service>) {
        if let Ok(mut cache) = self.services.write() {
            cache.clear();
            for svc in services {
                cache.insert(svc.id, svc);
            }
        }
    }

    /// Update upstreams and targets — 更新上游和目标
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

    /// Update plugin configurations — 更新插件配置
    pub fn update_plugins(&self, plugins: Vec<kong_core::models::Plugin>) {
        if let Ok(mut p) = self.plugins.write() {
            *p = plugins;
        }
        self.rebuild_plugin_chains();
    }

    /// Pre-compute plugin chains for all (route_id, service_id) combinations — 预计算所有 (route_id, service_id) 组合的插件链
    fn rebuild_plugin_chains(&self) {
        let plugins = match self.plugins.read() {
            Ok(p) => p.clone(),
            Err(_) => return,
        };

        // Collect unique (route_id, service_id) pairs from plugin configs — 从插件配置中收集唯一的 (route_id, service_id) 组合
        let mut keys: std::collections::HashSet<(Option<Uuid>, Option<Uuid>)> =
            std::collections::HashSet::new();
        // Always include (None, None) for global plugins — 始终包含 (None, None) 用于全局插件
        keys.insert((None, None));
        for plugin in &plugins {
            let route_id = plugin.route.as_ref().map(|fk| fk.id);
            let service_id = plugin.service.as_ref().map(|fk| fk.id);
            keys.insert((route_id, service_id));
            // Also include individual route/service combos — 也包含单独的 route/service 组合
            if route_id.is_some() {
                keys.insert((route_id, None));
            }
            if service_id.is_some() {
                keys.insert((None, service_id));
            }
        }

        let mut chains = HashMap::new();
        for (route_id, service_id) in keys {
            let resolved = PluginExecutor::resolve_plugins(
                &self.plugin_registry,
                &plugins,
                route_id,
                service_id,
                None, // consumer_id unknown at precompute time — 预计算时 consumer_id 未知
            );
            chains.insert((route_id, service_id), Arc::new(resolved));
        }

        if let Ok(mut pc) = self.plugin_chains.write() {
            *pc = chains;
        }
    }

    /// Hot-reload CA certificate list — 热更新 CA 证书列表
    pub fn update_ca_certificates(&self, cas: Vec<CaCertificate>) {
        if let Ok(mut ca) = self.ca_certificates.write() {
            *ca = cas;
        }
    }

    /// Populate RequestCtx and build RequestContext in a single header scan — 单次头遍历同时填充 RequestCtx 和构建 RequestContext
    fn populate_and_build_route_ctx(session: &Session, ctx: &mut RequestCtx) -> RequestContext {
        let req = session.req_header();
        let method = req.method.as_str().to_string();
        let uri_path = req.uri.path().to_string();
        let query_string = req.uri.query().unwrap_or("").to_string();

        let is_tls = session
            .digest()
            .map(|d| d.ssl_digest.is_some())
            .unwrap_or(false);
        let scheme = if is_tls {
            "https".to_string()
        } else {
            "http".to_string()
        };

        let host_header = req
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .or_else(|| req.uri.authority().map(|a| a.as_str().to_string()))
            .or_else(|| {
                req.uri.host().map(|h| {
                    if let Some(port) = req.uri.port_u16() {
                        format!("{}:{}", h, port)
                    } else {
                        h.to_string()
                    }
                })
            })
            .unwrap_or_else(|| "localhost".to_string());

        // Parse host and port from Host header — 从 Host 头解析 host 和 port
        let (host_no_port, port) = if let Some(colon_pos) = host_header.rfind(':') {
            let (h, p) = host_header.split_at(colon_pos);
            (
                h.to_string(),
                p[1..].parse().unwrap_or(if is_tls { 443 } else { 80 }),
            )
        } else {
            (host_header.to_string(), if is_tls { 443 } else { 80 })
        };

        // Single header scan — build both RequestCtx.request_headers and route headers — 单次头遍历 — 同时构建 RequestCtx.request_headers 和路由匹配 headers
        let mut headers = HashMap::new();
        ctx.request_headers.clear();
        for (name, value) in req.headers.iter() {
            if let Ok(v) = value.to_str() {
                let key = name.as_str().to_lowercase();
                let val = v.to_string();
                ctx.request_headers.insert(key.clone(), val.clone());
                headers.insert(key, val);
            }
        }

        // Fill RequestCtx fields — 填充 RequestCtx 字段
        ctx.request_method = method.clone();
        ctx.request_path = uri_path.clone();
        ctx.request_query_string = query_string;
        ctx.request_scheme = scheme.clone();
        ctx.request_host = host_no_port;
        ctx.request_port = port;

        // Client IP — 客户端 IP
        ctx.client_ip = session
            .client_addr()
            .map(|a| {
                let s = a.to_string();
                s.split(':').next().unwrap_or(&s).to_string()
            })
            .unwrap_or_default();

        RequestContext {
            method,
            uri: uri_path,
            host: host_header.to_string(),
            scheme,
            headers,
            sni: None,
        }
    }

    /// Resolve upstream address — 解析上游地址
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

        // Try resolving upstream address via load balancer — 尝试通过负载均衡器解析上游地址
        if let Ok(balancers) = self.balancers.read() {
            if let Some(lb) = balancers.get(&service.host) {
                if let Some(addr) = lb.select() {
                    // SNI priority: upstream.host_header > hostname part of target address — SNI 优先级：upstream.host_header > target 地址的主机名部分
                    let sni = lb
                        .host_header()
                        .unwrap_or_else(|| addr.split(':').next().unwrap_or(&addr).to_string());
                    return Ok((addr, use_tls, sni));
                }
            }
        }

        // Use Service's host:port directly — 直接使用 Service 的 host:port
        let addr = format!("{}:{}", service.host, service.port);
        let sni = service.host.clone();
        Ok((addr, use_tls, sni))
    }

    /// 构建响应头（公共逻辑：Content-Type + Content-Length + Server 头 + 自定义头注入）
    fn build_response_header(
        &self,
        status_code: u16,
        body_len: usize,
    ) -> pingora_core::Result<ResponseHeader> {
        let mut resp = ResponseHeader::build(status_code, Some(4))?;
        resp.insert_header("content-length", body_len.to_string())?;
        resp.insert_header("content-type", "application/json; charset=utf-8")?;

        // Server 头：根据配置决定是否添加
        if !self.config.proxy_hide_server_header {
            resp.insert_header("server", "kong-rust/0.1.0")?;
        }

        // 注入自定义响应头
        for header_str in &self.config.proxy_response_headers {
            if let Some((name, value)) = header_str.split_once(':') {
                let name = name.trim();
                let value = value.trim();
                if let (Ok(hn), Ok(hv)) = (
                    http::header::HeaderName::from_bytes(name.as_bytes()),
                    http::header::HeaderValue::from_str(value),
                ) {
                    resp.headers.insert(hn, hv);
                }
            }
        }

        Ok(resp)
    }

    /// Pre-read buffered request body so access-phase plugins can inspect it. — 预读取需缓冲的请求体，供 access 阶段插件读取。
    async fn preload_request_body_for_plugins(
        &self,
        session: &mut Session,
        plugin_ctx: &mut RequestCtx,
        request_body_buf: &mut Option<SpillableBuffer>,
    ) -> pingora_core::Result<()> {
        let has_request_body = session
            .req_header()
            .headers
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .map(|len| len > 0)
            .unwrap_or_else(|| {
                session
                    .req_header()
                    .headers
                    .contains_key("transfer-encoding")
            });

        if !has_request_body {
            return Ok(());
        }

        // Let Pingora reuse the captured downstream body when it opens the upstream request. — 让 Pingora 在建立上游请求时复用已经捕获的下游请求体。
        session.as_mut().enable_retry_buffering();

        let mut body_buf = request_body_buf.take().unwrap_or_else(SpillableBuffer::new);
        while let Some(chunk) = session.read_request_body().await? {
            body_buf.extend(&chunk);
        }

        let body_bytes = body_buf.finish();
        plugin_ctx.request_body = Some(String::from_utf8_lossy(&body_bytes).to_string());
        // Retain the fully-read body so that request_body_filter can release it in one shot
        // instead of re-buffering from scratch (which would trigger the chunk-interval timeout).
        // 保留已完整读取的 body，让 request_body_filter 一次性释放，而非从头重新缓冲（那样会触发 chunk 间隔超时）。
        let mut retained = SpillableBuffer::new();
        retained.extend(&body_bytes);
        *request_body_buf = Some(retained);

        Ok(())
    }

    /// Apply upstream target overrides staged by plugins. — 应用插件暂存的上游目标覆写。
    fn apply_plugin_upstream_overrides(
        &self,
        upstream_addr: &mut String,
        upstream_tls: &mut bool,
        upstream_sni: &mut String,
        plugin_ctx: &RequestCtx,
    ) {
        if let Some(host) = plugin_ctx.upstream_target_host.as_deref() {
            let port = plugin_ctx
                .upstream_target_port
                .unwrap_or(if *upstream_tls { 443 } else { 80 });
            *upstream_addr = format!("{host}:{port}");
            *upstream_sni = host.to_string();
        }

        if let Some(scheme) = plugin_ctx.upstream_scheme.as_deref() {
            *upstream_tls = matches!(scheme, "https" | "grpcs" | "tls");
            if plugin_ctx.upstream_target_host.is_none() && !upstream_sni.is_empty() {
                let default_port = if *upstream_tls { 443 } else { 80 };
                if !upstream_addr.contains(':') {
                    *upstream_addr = format!("{upstream_addr}:{default_port}");
                }
            }
        }
    }

    /// 发送框架级错误响应（JSON 格式，受配置控制）
    async fn send_error_response(
        &self,
        session: &mut Session,
        status_code: u16,
        message: &str,
    ) -> pingora_core::Result<bool> {
        let body = format!("{{\"message\":\"{}\"}}", message);
        let body_bytes = body.as_bytes();

        let resp = self.build_response_header(status_code, body_bytes.len())?;
        session.write_response_header(Box::new(resp), false).await?;
        session
            .write_response_body(Some(Bytes::copy_from_slice(body_bytes)), true)
            .await?;

        Ok(true)
    }

    /// Send short-circuit response (supports custom status + headers + body) — 发送短路响应（支持自定义 status + headers + body）
    async fn send_short_circuit_response(
        &self,
        session: &mut Session,
        ctx: &mut RequestCtx,
    ) -> pingora_core::Result<bool> {
        let status_code = ctx.exit_status.unwrap_or(200);
        let body = ctx.exit_body.take();
        let headers = ctx.exit_headers.take();

        let body_bytes = body.as_deref().unwrap_or("").as_bytes();
        let mut resp = self.build_response_header(status_code, body_bytes.len())?;

        // 应用插件设置的自定义响应头
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
            resolved_plugins: Arc::new(Vec::new()),
            request_body_buf: None,
            response_body_buf: None,
            deferred_header_filter: false,
            last_body_chunk_at: None,
            injected_real_ip_headers: Vec::new(),
            request_start_time: std::time::Instant::now(),
            upstream_response_time: None,
            request_id: Uuid::new_v4().simple().to_string(),
        }
    }

    /// Request filter phase — route matching + plugin rewrite/access — 请求过滤阶段 — 路由匹配 + 插件 rewrite/access
    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<bool> {
        // 1. Populate request context + build route matching context (single header scan) — 填充请求上下文 + 构建路由匹配上下文（单次头遍历）
        let req_ctx = Self::populate_and_build_route_ctx(session, &mut ctx.plugin_ctx);

        // 2. Route matching — 路由匹配
        let route_match = {
            let router = self
                .router
                .read()
                .map_err(|_| pingora_core::Error::new_str("路由器读取失败"))?;
            router.find_route(&req_ctx)
        };

        let route_match = match route_match {
            Some(rm) => rm,
            None => {
                return self
                    .send_error_response(session, 404, "no Route matched with those values")
                    .await;
            }
        };

        // 3. Find Service — 查找 Service
        let service = if let Some(service_id) = route_match.service_id {
            let services = self
                .services
                .read()
                .map_err(|_| pingora_core::Error::new_str("服务缓存读取失败"))?;
            services.get(&service_id).cloned()
        } else {
            None
        };

        let service = match service {
            Some(s) => s,
            None => {
                return self
                    .send_error_response(session, 503, "no Service found for the requested route")
                    .await;
            }
        };

        if !service.enabled {
            return self
                .send_error_response(session, 503, "Service unavailable")
                .await;
        }

        // 4. Resolve upstream address — 解析上游地址
        let (mut upstream_addr, mut upstream_tls, mut upstream_sni) = self
            .resolve_upstream(&service)
            .map_err(|_| pingora_core::Error::new_str("上游解析失败"))?;

        // 5. Set up plugin context — 设置插件上下文
        ctx.plugin_ctx.route_id = Some(route_match.route_id);
        ctx.plugin_ctx.service_id = route_match.service_id;

        // 6. Resolve plugin chain (from pre-computed cache) — 解析插件链（从预计算缓存）
        let resolved_plugins = {
            let key = (Some(route_match.route_id), route_match.service_id);
            let chains = self
                .plugin_chains
                .read()
                .map_err(|_| pingora_core::Error::new_str("插件链缓存读取失败"))?;
            chains.get(&key).cloned().unwrap_or_else(|| {
                // Fallback: compute at runtime if no pre-computed chain — 回退：如果没有预计算链则运行时计算
                let plugins = self.plugins.read().unwrap_or_else(|p| p.into_inner());
                Arc::new(PluginExecutor::resolve_plugins(
                    &self.plugin_registry,
                    &plugins,
                    Some(route_match.route_id),
                    route_match.service_id,
                    None,
                ))
            })
        };

        // 7. Pre-read request body when buffering is enabled so access-phase plugins can inspect it. — 当启用 buffering 时预读取请求体，供 access 阶段插件检查。
        if let Err(err) = self
            .preload_request_body_for_plugins(
                session,
                &mut ctx.plugin_ctx,
                &mut ctx.request_body_buf,
            )
            .await
        {
            tracing::error!("请求体预读取失败: {}", err);
            return self
                .send_error_response(session, 400, "Bad request body")
                .await;
        }

        // 8. Execute rewrite phase — 执行 rewrite 阶段
        if let Err(e) = PhaseRunner::run_rewrite(&resolved_plugins, &mut ctx.plugin_ctx).await {
            tracing::error!("Rewrite 阶段执行失败: {}", e);
            return self
                .send_error_response(session, 500, "An unexpected error occurred")
                .await;
        }

        // 9. Check short-circuit — 检查短路
        if ctx.plugin_ctx.is_short_circuited() {
            // Save plugin chain for log phase — 保存插件链供 log 阶段使用
            ctx.resolved_plugins = resolved_plugins;
            return self
                .send_short_circuit_response(session, &mut ctx.plugin_ctx)
                .await;
        }

        // 10. Execute access phase — 执行 access 阶段
        if let Err(e) = PhaseRunner::run_access(&resolved_plugins, &mut ctx.plugin_ctx).await {
            tracing::error!("Access 阶段执行失败: {}", e);
            return self
                .send_error_response(session, 500, "An unexpected error occurred")
                .await;
        }

        // 11. Check short-circuit — 检查短路
        if ctx.plugin_ctx.is_short_circuited() {
            ctx.resolved_plugins = resolved_plugins;
            return self
                .send_short_circuit_response(session, &mut ctx.plugin_ctx)
                .await;
        }

        self.apply_plugin_upstream_overrides(
            &mut upstream_addr,
            &mut upstream_tls,
            &mut upstream_sni,
            &ctx.plugin_ctx,
        );

        // Save to context — 保存到上下文
        ctx.route_match = Some(route_match);
        ctx.service = Some(service);
        ctx.upstream_addr = Some(upstream_addr);
        ctx.upstream_tls = upstream_tls;
        ctx.upstream_sni = upstream_sni;
        ctx.resolved_plugins = resolved_plugins;

        Ok(false) // Continue to upstream — 继续到上游
    }

    /// Determine upstream peer address — 确定上游地址
    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<Box<HttpPeer>> {
        let raw_addr = ctx
            .upstream_addr
            .as_deref()
            .ok_or_else(|| pingora_core::Error::new_str("上游地址未设置"))?;

        // Ensure address includes port — 确保地址包含端口
        let addr_with_port = if raw_addr.contains(':') {
            raw_addr.to_string()
        } else {
            let default_port = if ctx.upstream_tls { 443 } else { 80 };
            format!("{}:{}", raw_addr, default_port)
        };

        // Async DNS resolution (direct IP connections skip DNS lookup automatically) — 异步 DNS 解析（IP 直连自动跳过 DNS 查询）
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

        // Set ALPN to prefer HTTP/2 over HTTP/1.1 if TLS is used
        if ctx.upstream_tls {
            peer.options.alpn = pingora_core::protocols::tls::ALPN::H2H1;
        }

        // Apply Service timeouts — 应用 Service 超时设置
        if let Some(ref service) = ctx.service {
            peer.options.connection_timeout = Some(std::time::Duration::from_millis(
                service.connect_timeout as u64,
            ));
            peer.options.read_timeout = Some(std::time::Duration::from_millis(
                service.read_timeout as u64,
            ));
            peer.options.write_timeout = Some(std::time::Duration::from_millis(
                service.write_timeout as u64,
            ));
        }

        // Upstream TLS configuration — 上游 TLS 配置
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

    /// Modify request headers sent to upstream — 修改发往上游的请求头
    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<()> {
        // 1. preserve_host handling — preserve_host 处理
        if let Some(ref rm) = ctx.route_match {
            if rm.preserve_host {
                // Use the original client Host header (including port if present) — 使用原始客户端 Host 头（含端口）
                let req = session.req_header();
                let host_header = req
                    .headers
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| {
                        // If Host header lacks port but URI has a non-standard port, append it — 如果 Host 头无端口但 URI 有非标准端口，补上
                        if !s.contains(':') {
                            if let Some(port) = req.uri.port_u16() {
                                return format!("{}:{}", s, port);
                            }
                        }
                        s.to_string()
                    })
                    .or_else(|| req.uri.authority().map(|a| a.as_str().to_string()));

                if let Some(host) = host_header {
                    let _ = upstream_request.insert_header("host", &host);
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

        // 2. Apply upstream request header modifications set by plugins — 应用插件设置的上游请求头修改
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

        // 3. strip_path handling — strip_path 处理
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

        // 4. Apply plugin path/query overrides after route strip_path logic. — 在 strip_path 逻辑之后应用插件路径/查询覆写。
        if ctx.plugin_ctx.upstream_path.is_some() || ctx.plugin_ctx.upstream_query_to_set.is_some() {
            let path = ctx
                .plugin_ctx
                .upstream_path
                .as_deref()
                .unwrap_or_else(|| upstream_request.uri.path());
            let query = ctx.plugin_ctx.upstream_query_to_set.as_ref().map(|pairs| {
                pairs.iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
                    .join("&")
            });
            let new_uri = match query.as_deref() {
                Some(query) if !query.is_empty() => format!("{path}?{query}"),
                _ => path.to_string(),
            };
            if let Ok(uri) = new_uri.parse() {
                upstream_request.set_uri(uri);
            }
        }

        // 5. If a plugin replaced the upstream body, fix Content-Length for the replayed payload. — 若插件替换了上游请求体，修正回放 payload 的 Content-Length。
        if let Some(body) = ctx.plugin_ctx.upstream_body.as_ref() {
            let _ = upstream_request.insert_header("content-length", body.len().to_string());
        }

        // 6. X-Real-IP / X-Forwarded-* 头注入（按配置列表按需注入，默认全部注入）
        if !self.config.proxy_real_ip_headers.is_empty() {
            let headers_set: std::collections::HashSet<String> = self
                .config
                .proxy_real_ip_headers
                .iter()
                .map(|h| h.to_lowercase())
                .collect();

            let client_ip = session
                .client_addr()
                .map(|a| {
                    let s = a.to_string();
                    s.split(':').next().unwrap_or(&s).to_string()
                })
                .unwrap_or_default();

            if !client_ip.is_empty() {
                if headers_set.contains("x-real-ip") {
                    let _ = upstream_request.insert_header("x-real-ip", &client_ip);
                    ctx.injected_real_ip_headers
                        .push(("X-Real-IP".to_string(), client_ip.clone()));
                }
                if headers_set.contains("x-forwarded-for") {
                    let existing_xff = session
                        .req_header()
                        .headers
                        .get("x-forwarded-for")
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string());
                    let xff = match existing_xff {
                        Some(existing) => format!("{}, {}", existing, client_ip),
                        None => client_ip.clone(),
                    };
                    let _ = upstream_request.insert_header("x-forwarded-for", &xff);
                    ctx.injected_real_ip_headers
                        .push(("X-Forwarded-For".to_string(), xff));
                }
            }

            if headers_set.contains("x-forwarded-proto") {
                let proto = if session
                    .digest()
                    .map(|d| d.ssl_digest.is_some())
                    .unwrap_or(false)
                {
                    "https"
                } else {
                    "http"
                };
                let _ = upstream_request.insert_header("x-forwarded-proto", proto);
                ctx.injected_real_ip_headers
                    .push(("X-Forwarded-Proto".to_string(), proto.to_string()));
            }

            if headers_set.contains("x-forwarded-host") {
                if let Some(host) = session.req_header().headers.get("host") {
                    let _ = upstream_request.insert_header("x-forwarded-host", host);
                    if let Ok(v) = host.to_str() {
                        ctx.injected_real_ip_headers
                            .push(("X-Forwarded-Host".to_string(), v.to_string()));
                    }
                }
            }

            if headers_set.contains("x-forwarded-port") {
                let port = session.req_header().uri.port_u16().unwrap_or(
                    if session
                        .digest()
                        .map(|d| d.ssl_digest.is_some())
                        .unwrap_or(false)
                    {
                        443
                    } else {
                        80
                    },
                );
                let port_str = port.to_string();
                let _ = upstream_request.insert_header("x-forwarded-port", &port_str);
                ctx.injected_real_ip_headers
                    .push(("X-Forwarded-Port".to_string(), port_str));
            }

            if headers_set.contains("x-forwarded-path") {
                let path = session
                    .req_header()
                    .uri
                    .path_and_query()
                    .map(|pq| pq.as_str())
                    .unwrap_or("/");
                let _ = upstream_request.insert_header("x-forwarded-path", path);
                ctx.injected_real_ip_headers
                    .push(("X-Forwarded-Path".to_string(), path.to_string()));
            }

            if headers_set.contains("x-forwarded-prefix") {
                let _ = upstream_request.insert_header("x-forwarded-prefix", "");
                ctx.injected_real_ip_headers
                    .push(("X-Forwarded-Prefix".to_string(), String::new()));
            }
        }

        // 7. WebSocket 代理：透传所有 WebSocket 握手头（与 Kong 原版行为一致）
        // 除了 Upgrade/Connection，还需要转发 Sec-WebSocket-Key/Version/Protocol/Extensions 等
        {
            let is_websocket = session
                .req_header()
                .headers
                .get("upgrade")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.eq_ignore_ascii_case("websocket"))
                .unwrap_or(false);
            if is_websocket {
                let _ = upstream_request.insert_header("upgrade", "websocket");
                let _ = upstream_request.insert_header("connection", "upgrade");

                // 转发所有 sec-websocket-* 握手头 — Forward all sec-websocket-* handshake headers
                let ws_headers: Vec<(String, Vec<u8>)> = session
                    .req_header()
                    .headers
                    .iter()
                    .filter(|(name, _)| {
                        name.as_str()
                            .to_ascii_lowercase()
                            .starts_with("sec-websocket-")
                    })
                    .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
                    .collect();
                for (name, value) in ws_headers {
                    if let Ok(v) = std::str::from_utf8(&value) {
                        let _ = upstream_request.insert_header(name, v);
                    }
                }
            }
        }

        // 8. Inject X-Kong-Request-Id into upstream request (only if headers_upstream config includes it) — 向上游请求注入 X-Kong-Request-Id（仅当 headers_upstream 配置包含时）
        if self.config.headers_upstream.iter().any(|h| h.eq_ignore_ascii_case("x-kong-request-id")) {
            let _ = upstream_request.insert_header("x-kong-request-id", &ctx.request_id);
        }

        Ok(())
    }

    /// Request body filter — buffer request body when request_buffering=true — 请求体过滤 — request_buffering=true 时缓冲请求体
    async fn request_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<()> {
        if let Some(upstream_body) = ctx.plugin_ctx.upstream_body.as_ref() {
            if end_of_stream {
                *body = Some(Bytes::copy_from_slice(upstream_body.as_bytes()));
            } else {
                *body = None;
            }
            return Ok(());
        }

        if let Some(buf) = ctx.request_body_buf.take() {
            if end_of_stream {
                *body = Some(Bytes::from(buf.finish()));
            } else {
                ctx.request_body_buf = Some(buf);
                *body = None;
            }
            return Ok(());
        }

        // Default buffering=true (Kong's default) — 默认 buffering=true（Kong 默认行为）
        let buffering = ctx
            .route_match
            .as_ref()
            .map(|rm| rm.request_buffering)
            .unwrap_or(true);

        if !buffering {
            // Pass through (Pingora default streaming behavior) — 直接透传（Pingora 默认流式行为）
            return Ok(());
        }

        // Check chunk interval timeout (use service read_timeout, default 60s) — 检查 chunk 间隔超时（使用 service read_timeout，默认 60s）
        let timeout_secs = ctx.service.as_ref()
            .map(|s| s.read_timeout as u64 / 1000)
            .unwrap_or(60)
            .max(60); // minimum 60s to avoid premature timeout — 最少 60s 避免过早超时
        let now = std::time::Instant::now();
        if let Some(last_at) = ctx.last_body_chunk_at {
            if now.duration_since(last_at).as_secs() > timeout_secs {
                tracing::warn!("请求体 chunk 间隔超时 (>{}s)，终止请求", timeout_secs);
                return Err(pingora_core::Error::new_str("client body timeout"));
            }
        }

        // Collect chunks into spillable buffer, release all at end_of_stream — 收集 chunk 到可溢出缓冲区，end_of_stream 时一次性释放
        if let Some(data) = body.take() {
            ctx.last_body_chunk_at = Some(now);
            let buf = ctx
                .request_body_buf
                .get_or_insert_with(SpillableBuffer::new);
            buf.extend(&data);
        }

        if end_of_stream {
            // Release the buffered body — 释放缓冲的请求体
            if let Some(buf) = ctx.request_body_buf.take() {
                *body = Some(Bytes::from(buf.finish()));
            }
        }
        // When not end_of_stream, body remains None — suppress forwarding — 非 end_of_stream 时 body 保持 None — 抑制转发

        Ok(())
    }

    /// Upstream response header processing — header_filter phase — 上游响应头处理 — header_filter 阶段
    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<()> {
        // Record upstream response time for latency tracking — 记录上游响应时间用于延迟统计
        ctx.upstream_response_time = Some(std::time::Instant::now());

        // Populate response snapshot into RequestCtx — 填充响应快照到 RequestCtx
        ctx.plugin_ctx.response_status = Some(upstream_response.status.as_u16());
        ctx.plugin_ctx.response_headers.clear();
        for (name, value) in upstream_response.headers.iter() {
            if let Ok(v) = value.to_str() {
                ctx.plugin_ctx
                    .response_headers
                    .insert(name.as_str().to_lowercase(), v.to_string());
            }
        }

        let defer_header_filter = ctx.plugin_ctx.request_buffering_enabled;
        ctx.deferred_header_filter = defer_header_filter;

        if defer_header_filter {
            // The buffered-response path may replace the body later, so avoid locking in a stale length/encoding now. — 完整缓冲响应路径后续可能替换响应体，因此这里先不要锁死旧的长度和编码。
            upstream_response.headers.remove(http::header::CONTENT_LENGTH);
            upstream_response.headers.remove(http::header::CONTENT_ENCODING);
        } else {
            // Execute header_filter phase — 执行 header_filter 阶段
            let plugins = ctx.resolved_plugins.clone();
            if let Err(e) = PhaseRunner::run_header_filter(&plugins, &mut ctx.plugin_ctx).await {
                tracing::error!("HeaderFilter 阶段执行失败: {}", e);
            }

            // Apply response header modifications set by plugins — 应用插件设置的响应头修改
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
        }

        // Add Kong standard response headers — 添加 Kong 标准响应头
        let now = std::time::Instant::now();
        let proxy_latency = now.duration_since(ctx.request_start_time).as_millis();
        let upstream_latency = ctx
            .upstream_response_time
            .map(|t| t.duration_since(ctx.request_start_time).as_millis())
            .unwrap_or(0);
        let _ =
            upstream_response.insert_header("x-kong-proxy-latency", &proxy_latency.to_string());
        let _ = upstream_response
            .insert_header("x-kong-upstream-latency", &upstream_latency.to_string());
        let _ = upstream_response.insert_header("via", "1.1 kong/0.1.0");
        // Use per-request X-Kong-Request-Id in downstream response (only if headers config includes it) — 在下游响应中使用每请求的 X-Kong-Request-Id（仅当 headers 配置包含时）
        if self.config.headers.iter().any(|h| h.eq_ignore_ascii_case("x-kong-request-id")) {
            let _ = upstream_response.insert_header("x-kong-request-id", &ctx.request_id);
        }

        // Server 头处理：隐藏上游 Server 头并注入 Kong 标识
        if self.config.proxy_hide_server_header {
            upstream_response.remove_header("server");
        }
        let _ = upstream_response.insert_header("server", "kong-rust/0.1.0");

        // 注入自定义响应头
        for header_str in &self.config.proxy_response_headers {
            if let Some((name, value)) = header_str.split_once(':') {
                let name = name.trim();
                let value = value.trim();
                if let (Ok(hn), Ok(hv)) = (
                    http::header::HeaderName::from_bytes(name.as_bytes()),
                    http::header::HeaderValue::from_str(value),
                ) {
                    upstream_response.headers.insert(hn, hv);
                }
            }
        }

        Ok(())
    }

    /// Response body filter — body_filter phase + response buffering — 响应体过滤 — body_filter 阶段 + 响应体缓冲
    fn response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<Option<std::time::Duration>> {
        // 1. Response buffering — 响应体缓冲
        // Plugin-requested buffering must force full upstream response collection. — 插件显式请求的 buffering 必须强制启用完整上游响应缓冲。
        let buffering = ctx
            .route_match
            .as_ref()
            .map(|rm| rm.response_buffering)
            .unwrap_or(true)
            || ctx.plugin_ctx.request_buffering_enabled;

        if buffering {
            // Collect chunks into spillable buffer — 收集 chunk 到可溢出缓冲区
            if let Some(data) = body.take() {
                let buf = ctx
                    .response_body_buf
                    .get_or_insert_with(SpillableBuffer::new);
                buf.extend(&data);
            }

            if end_of_stream {
                // Release the buffered body — 释放缓冲的响应体
                if let Some(buf) = ctx.response_body_buf.take() {
                    let buffered = buf.finish();
                    ctx.plugin_ctx.service_response_body =
                        Some(String::from_utf8_lossy(&buffered).to_string());
                    *body = Some(Bytes::from(buffered));
                }
            }
            // When not end_of_stream, body remains None — suppress sending to client — 非 end_of_stream 时 body 保持 None — 抑制发送
        }

        if end_of_stream && ctx.deferred_header_filter {
            let plugins = ctx.resolved_plugins.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let result = tokio::task::block_in_place(|| {
                    handle.block_on(async {
                        PhaseRunner::run_header_filter(&plugins, &mut ctx.plugin_ctx).await
                    })
                });
                if let Err(e) = result {
                    tracing::error!("Deferred HeaderFilter 阶段执行失败: {}", e);
                }
            }
            ctx.deferred_header_filter = false;
        }

        // 2. Plugin body_filter phase — 插件 body_filter 阶段
        if ctx.resolved_plugins.is_empty() {
            return Ok(None);
        }

        if !ctx
            .resolved_plugins
            .iter()
            .any(|plugin| plugin.handler.has_body_filter())
        {
            return Ok(None);
        }

        // body_filter must execute synchronously (Pingora's response_body_filter is synchronous) — body_filter 需要同步执行（Pingora 的 response_body_filter 是同步的）
        // Using block_on to adapt async plugin interface — 使用 block_on 适配异步插件接口
        let plugins = ctx.resolved_plugins.clone();
        // Block on execution within the current tokio runtime — 在当前 tokio 运行时中阻塞执行
        let handle = tokio::runtime::Handle::try_current();
        if let Ok(handle) = handle {
            let mut body_clone = body.clone();
            let result = tokio::task::block_in_place(|| {
                handle.block_on(async {
                    PhaseRunner::run_body_filter(
                        &plugins,
                        &mut ctx.plugin_ctx,
                        &mut body_clone,
                        end_of_stream,
                    )
                    .await
                })
            });

            if let Err(e) = result {
                tracing::error!("BodyFilter 阶段执行失败: {}", e);
            } else {
                *body = body_clone;
            }
        }

        Ok(None)
    }

    /// Handle upstream connection/proxy failures — 处理上游连接/代理失败
    async fn fail_to_proxy(
        &self,
        session: &mut Session,
        e: &pingora_core::Error,
        ctx: &mut Self::CTX,
    ) -> pingora_proxy::FailToProxy
    where
        Self::CTX: Send + Sync,
    {
        let error_msg = format!("{}", e);
        let (status, body) = if error_msg.contains("timeout") || error_msg.contains("Timeout") {
            (504u16, serde_json::json!({"message": "The upstream server is timing out"}))
        } else {
            (502u16, serde_json::json!({"message": "An invalid response was received from the upstream server"}))
        };

        let body_bytes = serde_json::to_vec(&body).unwrap_or_default();
        if let Ok(mut resp) = ResponseHeader::build(status, Some(4)) {
            let _ = resp.insert_header("content-type", "application/json; charset=utf-8");
            let _ = resp.insert_header("content-length", body_bytes.len().to_string());
            let _ = resp.insert_header("server", "kong-rust/0.1.0");
            let _ = session.write_response_header(Box::new(resp), false).await;
            let _ = session
                .write_response_body(Some(bytes::Bytes::from(body_bytes)), true)
                .await;
        }

        ctx.plugin_ctx.response_status = Some(status);
        ctx.plugin_ctx.response_source = Some("error".to_string());

        pingora_proxy::FailToProxy {
            error_code: 0, // 0 = we already sent the response — 0 = 已发送响应
            can_reuse_downstream: false,
        }
    }

    /// Logging phase — 日志阶段
    async fn logging(
        &self,
        session: &mut Session,
        error: Option<&pingora_core::Error>,
        ctx: &mut Self::CTX,
    ) {
        // Access Log
        let req = session.req_header();
        let method = req.method.as_str();
        let uri = req
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let host = req
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        let remote_addr = session
            .client_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|| "-".to_string());
        let status = session
            .response_written()
            .map(|r| r.status.as_u16())
            .unwrap_or(0);
        let upstream = ctx.upstream_addr.as_deref().unwrap_or("-");
        let user_agent = req
            .headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");

        // 从 ctx 读取 proxy 注入到 upstream 的 real-ip header 值用于日志输出
        let headers_str = if !ctx.injected_real_ip_headers.is_empty() {
            let header_values: Vec<String> = ctx
                .injected_real_ip_headers
                .iter()
                .map(|(name, value)| format!("{}: {}", name, value))
                .collect();
            if !header_values.is_empty() {
                format!("headers=\"{}\"", header_values.join(", "))
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let now = chrono::Utc::now().format("%d/%b/%Y:%H:%M:%S %z");

        let log_line = if let Some(e) = error {
            if headers_str.is_empty() {
                format!(
                    "{} - - [{}] \"{} {} {:?}\" {} - \"{}\" upstream={} error=\"{}\"\n",
                    remote_addr, now, method, uri, req.version, status, user_agent, upstream, e
                )
            } else {
                format!(
                    "{} - - [{}] \"{} {} {:?}\" {} - \"{}\" upstream={} error=\"{}\" {}\n",
                    remote_addr,
                    now,
                    method,
                    uri,
                    req.version,
                    status,
                    user_agent,
                    upstream,
                    e,
                    headers_str
                )
            }
        } else {
            if headers_str.is_empty() {
                format!(
                    "{} - - [{}] \"{} {} {:?}\" {} - \"{}\" upstream={}\n",
                    remote_addr, now, method, uri, req.version, status, user_agent, upstream
                )
            } else {
                format!(
                    "{} - - [{}] \"{} {} {:?}\" {} - \"{}\" upstream={} {}\n",
                    remote_addr,
                    now,
                    method,
                    uri,
                    req.version,
                    status,
                    user_agent,
                    upstream,
                    headers_str
                )
            }
        };

        // Async write to access log file — 异步写入 access log 文件
        if let Some(ref writer) = self.access_log_writer {
            writer.write(log_line.clone());
        }

        tracing::debug!(
            "access: {} {} {} -> {} upstream={}",
            host,
            method,
            uri,
            status,
            upstream
        );

        kong_lua_bridge::metrics::record_http_request();
        if status > 0 {
            ctx.plugin_ctx.response_status = Some(status);
        }
        if ctx.plugin_ctx.response_source.is_none() {
            ctx.plugin_ctx.response_source = Some("service".to_string());
        }

        // Calculate latencies for prometheus plugin — 计算延迟指标供 prometheus 插件使用
        let now = std::time::Instant::now();
        let request_latency = now.duration_since(ctx.request_start_time).as_millis() as i64;
        let (kong_latency, proxy_latency) = if let Some(upstream_time) = ctx.upstream_response_time {
            let proxy = upstream_time.duration_since(ctx.request_start_time).as_millis() as i64;
            let kong = now.duration_since(upstream_time).as_millis() as i64;
            (kong, proxy)
        } else {
            // If no upstream response time recorded (e.g., short-circuited), all latency is Kong latency — 如果没记录到上游响应时间（如短路），所有延迟都算 Kong 延迟
            (request_latency, 0)
        };

        // Build latencies object for kong.log.serialize() — 构建 latencies 对象供 kong.log.serialize() 使用
        let latencies = serde_json::json!({
            "kong": kong_latency,
            "request": request_latency,
            "proxy": proxy_latency,
            "session": null
        });

        // Populate log_serialize for Lua plugins (prometheus plugin expects this) — 填充 log_serialize 供 Lua 插件使用（prometheus 插件依赖此数据）
        let service_name = ctx.service.as_ref().and_then(|s| s.name.clone()).unwrap_or_default();
        let route_id = ctx.route_match.as_ref().map(|rm| rm.route_id.to_string()).unwrap_or_default();
        let route_name = ctx.route_match.as_ref().and_then(|rm| rm.route_name.as_ref().map(|n| n.to_string())).unwrap_or_else(|| route_id.clone());

        // Calculate request size: header line + headers + body — 计算请求大小：请求行 + 头 + 体
        let req_header_size: usize = session.req_header().headers.iter().map(|(k, v)| k.as_str().len() + v.len() + 4).sum();
        let req_body_size = ctx.plugin_ctx.request_body.as_ref().map(|b| b.len()).unwrap_or(0);
        let request_size = (req_header_size + req_body_size) as i64;

        // Calculate response size: headers + body — 计算响应大小：头 + 体
        let resp_header_size: usize = session.response_written()
            .map(|r| r.headers.iter().map(|(k, v)| k.as_str().len() + v.len() + 4).sum::<usize>())
            .unwrap_or(0);
        let resp_body_size = ctx.plugin_ctx.service_response_body.as_ref().map(|b| b.len()).unwrap_or(0);
        let response_size = (resp_header_size + resp_body_size) as i64;

        // Extract consumer username from authenticated_consumer — 从 authenticated_consumer 提取消费者用户名
        let consumer_value = ctx.plugin_ctx.authenticated_consumer.as_ref()
            .and_then(|c| c.get("username").and_then(|u| u.as_str()))
            .unwrap_or("");

        ctx.plugin_ctx.log_serialize = Some(serde_json::json!({
            "service": {
                "id": ctx.service.as_ref().map(|s| s.id.to_string()).unwrap_or_default(),
                "name": service_name,
                "host": ctx.service.as_ref().map(|s| s.host.clone()).unwrap_or_default()
            },
            "route": {
                "id": route_id.clone(),
                "name": route_name
            },
            "request": {
                "method": method,
                "path": uri,
                "size": request_size
            },
            "response": {
                "status": status,
                "size": response_size
            },
            "latencies": latencies,
            "consumer": consumer_value,
            "workspace_name": "default"
        }));

        // Execute plugin log phase (always executes, even after short-circuit) — 执行插件 log 阶段（总是执行，即使之前短路）
        if let Err(e) = PhaseRunner::run_log(&ctx.resolved_plugins, &mut ctx.plugin_ctx).await {
            tracing::error!("Log 阶段执行失败: {}", e);
        }
    }
}

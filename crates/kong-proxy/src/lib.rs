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
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use uuid::Uuid;

use kong_core::models::{Route, Service, Target, Upstream};
use kong_core::traits::RequestCtx;
use kong_router::{RequestContext, RouteMatch, Router};
use kong_plugin_system::{PluginExecutor, PluginRegistry, ResolvedPlugin};

use crate::balancer::LoadBalancer;

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
}

impl KongProxy {
    pub fn new(
        routes: &[Route],
        router_flavor: &str,
        plugin_registry: PluginRegistry,
    ) -> Self {
        Self {
            router: Arc::new(RwLock::new(Router::new(routes, router_flavor))),
            plugin_registry: Arc::new(plugin_registry),
            balancers: Arc::new(RwLock::new(HashMap::new())),
            services: Arc::new(RwLock::new(HashMap::new())),
            plugins: Arc::new(RwLock::new(Vec::new())),
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
                    let sni = service.host.clone();
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
        let addr = ctx.upstream_addr.as_deref().ok_or_else(|| {
            pingora_core::Error::new_str("上游地址未设置")
        })?;

        let mut peer = HttpPeer::new(addr, ctx.upstream_tls, ctx.upstream_sni.clone());

        // 与 Kong 保持一致：上游 TLS 验证默认关闭
        // 仅当 Service 显式设置 tls_verify = true 时才启用验证
        if ctx.upstream_tls {
            let service = ctx.service.as_ref();
            let tls_verify = service.and_then(|s| s.tls_verify).unwrap_or(false);
            peer.options.verify_cert = tls_verify;
            peer.options.verify_hostname = tls_verify;

            // TODO: 当 tls_verify=true 时，从 Service.ca_certificates 加载 CA 构建信任链
            // TODO: 支持 Service.tls_verify_depth
            // TODO: 支持 Service.client_certificate (mTLS)
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
            } else if let Some(ref service) = ctx.service {
                // 设置上游 Host 头
                let host_header = if service.port == 80 || service.port == 443 {
                    service.host.clone()
                } else {
                    format!("{}:{}", service.host, service.port)
                };
                let _ = upstream_request.insert_header("host", &host_header);
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
        _session: &mut Session,
        _error: Option<&pingora_core::Error>,
        ctx: &mut Self::CTX,
    ) {
        // 执行 log 阶段
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

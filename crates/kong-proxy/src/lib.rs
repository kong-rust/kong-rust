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
pub mod grpc;
pub mod health;
pub mod phases;
pub mod spillable_buffer;
pub mod stream;
pub mod stream_tls;
pub mod tls;

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::FutureExt;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use uuid::Uuid;

use kong_config::KongConfig;
use kong_core::models::{CaCertificate, Route, Service, Target, Upstream};
use kong_core::traits::{
    LifecyclePhase, RequestCtx, RequestTransportError, RequestTransportErrorKind,
    RequestTransportSource,
};
use kong_plugin_system::{
    DispatchAbortCause, DispatchAbortKind, DispatchFailurePolicy, DispatchFailureResponse,
    DispatchFailureResponseFormat, LifecycleHookTimeouts, PluginExecutor, PluginRegistry,
    RequestDispatchAbortHandler, RequestDispatchHook, RequestFinalizer, RequestLifecycleObserver,
    ResolvedPlugin,
};
use kong_router::{RequestContext, RouteMatch, Router};

use crate::access_log::AccessLogWriter;
use crate::balancer::LoadBalancer;
use crate::dns::SharedDnsResolver;
use crate::phases::PhaseRunner;
use crate::spillable_buffer::SpillableBuffer;
use crate::tls::CertificateManager;

fn set_upstream_header(upstream_request: &mut RequestHeader, name: String, value: &str) {
    let _ = upstream_request.insert_header(name, value);
}

fn apply_proxy_response_headers(response: &mut ResponseHeader, headers: &[String]) {
    for header in headers {
        if let Some((name, value)) = header.split_once(':') {
            let _ = response.insert_header(name.trim().to_string(), value.trim().to_string());
        }
    }
}

fn set_dispatch_failure_response(ctx: &mut RequestCtx, response: &DispatchFailureResponse) {
    let format = ctx
        .extensions
        .get::<DispatchFailureResponseFormat>()
        .copied()
        .unwrap_or(DispatchFailureResponseFormat::Generic);
    let error_type = match response.status {
        401 => "invalid_request_error",
        403 => "insufficient_quota",
        429 => "rate_limit_error",
        500..=599 => "server_error",
        _ => "gateway_error",
    };
    let body = match format {
        DispatchFailureResponseFormat::Generic => serde_json::json!({
            "error": {
                "code": response.code,
                "message": response.message,
            }
        }),
        DispatchFailureResponseFormat::OpenAi => serde_json::json!({
            "error": {
                "message": response.message,
                "type": error_type,
                "param": null,
                "code": response.code,
            }
        }),
        DispatchFailureResponseFormat::Anthropic => serde_json::json!({
            "type": "error",
            "error": {
                "type": response.code,
                "message": response.message,
            },
            "request_id": ctx.lifecycle.request_id,
        }),
    };
    ctx.short_circuited = true;
    ctx.exit_status = Some(response.status);
    ctx.exit_body = Some(body.to_string());
}

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
    /// Whether upstream is gRPC (needs h2c for plaintext) — 上游是否为 gRPC（明文时需要 h2c）
    pub upstream_is_grpc: bool,
    /// Whether the incoming request is gRPC (content-type: application/grpc) — 入站请求是否为 gRPC
    pub is_grpc_request: bool,
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
    /// Upstream response received time (for latency tracking) — 上游响应接收时间（用于延迟统计）
    pub upstream_response_time: Option<std::time::Instant>,
}

impl KongCtx {
    /// 生命周期中的唯一请求 ID。
    pub fn request_id(&self) -> &str {
        &self.plugin_ctx.lifecycle.request_id
    }

    /// 生命周期中的单调起始时钟。
    pub fn request_start_time(&self) -> std::time::Instant {
        self.plugin_ctx.lifecycle.started_mono
    }
}

/// Kong proxy service — implements Pingora ProxyHttp trait — Kong 代理服务 — 实现 Pingora ProxyHttp trait
struct RoutingState {
    router: Router,
    routes_by_id: HashMap<Uuid, Route>,
}

#[derive(Clone)]
pub struct KongProxy {
    /// Kong configuration — Kong 配置
    pub config: Arc<KongConfig>,
    /// 原子更新的 Router 与 Route 快照缓存。
    routing: Arc<RwLock<RoutingState>>,
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
    /// Request lifecycle observers — 请求生命周期观察器
    pub lifecycle_observers: Arc<Vec<Arc<dyn RequestLifecycleObserver>>>,
    /// 上游派发前执行的异步 hook。
    pub dispatch_hooks: Arc<Vec<Arc<dyn RequestDispatchHook>>>,
    /// critical 派发失败后、响应发出前执行的独立补偿器。
    pub dispatch_abort_handlers: Arc<Vec<Arc<dyn RequestDispatchAbortHandler>>>,
    /// 请求结果冻结后、普通插件 log 前执行的异步 finalizer。
    pub request_finalizers: Arc<Vec<Arc<dyn RequestFinalizer>>>,
    /// 每个生命周期 hook 的独立超时。
    pub lifecycle_hook_timeouts: LifecycleHookTimeouts,
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
        let routes_by_id = routes
            .iter()
            .map(|route| (route.id, route.clone()))
            .collect();
        Self {
            config,
            routing: Arc::new(RwLock::new(RoutingState {
                router: Router::new(routes, router_flavor),
                routes_by_id,
            })),
            plugin_registry: Arc::new(plugin_registry),
            balancers: Arc::new(RwLock::new(HashMap::new())),
            services: Arc::new(RwLock::new(HashMap::new())),
            plugins: Arc::new(RwLock::new(Vec::new())),
            cert_manager: Arc::new(cert_manager),
            ca_certificates: Arc::new(RwLock::new(ca_certificates)),
            access_log_writer: None,
            dns_resolver,
            plugin_chains: Arc::new(RwLock::new(HashMap::new())),
            lifecycle_observers: Arc::new(Vec::new()),
            dispatch_hooks: Arc::new(Vec::new()),
            dispatch_abort_handlers: Arc::new(Vec::new()),
            request_finalizers: Arc::new(Vec::new()),
            lifecycle_hook_timeouts: LifecycleHookTimeouts::default(),
        }
    }

    /// 装配同步请求生命周期观察器，保留 `new` 的默认无观察器行为。
    pub fn with_lifecycle_observers(
        mut self,
        observers: Vec<Arc<dyn RequestLifecycleObserver>>,
    ) -> Self {
        self.lifecycle_observers = Arc::new(observers);
        self
    }

    /// 装配异步请求生命周期扩展。
    ///
    /// 三类组件显式分开，保证 critical dispatch hook 自身失败时，补偿不会再次
    /// 回调同一个实现。每个组件由代理逐项施加 timeout 与 panic 隔离。
    pub fn with_async_lifecycle_hooks(
        self,
        dispatch_hooks: Vec<Arc<dyn RequestDispatchHook>>,
        dispatch_abort_handlers: Vec<Arc<dyn RequestDispatchAbortHandler>>,
        request_finalizers: Vec<Arc<dyn RequestFinalizer>>,
        timeouts: LifecycleHookTimeouts,
    ) -> Self {
        self.try_with_async_lifecycle_hooks(
            dispatch_hooks,
            dispatch_abort_handlers,
            request_finalizers,
            timeouts,
        )
        .expect("critical dispatch hook 必须配套同域独立 abort handler")
    }

    /// 校验并装配异步请求生命周期扩展。
    pub fn try_with_async_lifecycle_hooks(
        mut self,
        dispatch_hooks: Vec<Arc<dyn RequestDispatchHook>>,
        dispatch_abort_handlers: Vec<Arc<dyn RequestDispatchAbortHandler>>,
        request_finalizers: Vec<Arc<dyn RequestFinalizer>>,
        timeouts: LifecycleHookTimeouts,
    ) -> Result<Self, &'static str> {
        for hook in dispatch_hooks
            .iter()
            .filter(|hook| matches!(hook.failure_policy(), DispatchFailurePolicy::FailClosed(_)))
        {
            let has_matching_compensator = dispatch_abort_handlers
                .iter()
                .any(|handler| handler.compensation_domain() == hook.compensation_domain());
            if !has_matching_compensator {
                return Err("critical dispatch hook 必须配套同域独立 abort handler");
            }
        }
        self.dispatch_hooks = Arc::new(dispatch_hooks);
        self.dispatch_abort_handlers = Arc::new(dispatch_abort_handlers);
        self.request_finalizers = Arc::new(request_finalizers);
        self.lifecycle_hook_timeouts = timeouts;
        Ok(self)
    }

    fn notify_plugins_resolved(&self, plugins: &[ResolvedPlugin], ctx: &mut RequestCtx) {
        for observer in self.lifecycle_observers.iter() {
            observer.on_plugins_resolved(plugins, ctx);
        }
    }

    fn notify_request_finalizing(&self, plugins: &[ResolvedPlugin], ctx: &mut RequestCtx) {
        for observer in self.lifecycle_observers.iter() {
            observer.on_request_finalizing(plugins, ctx);
        }
    }

    async fn run_dispatch_hooks(&self, plugins: &[ResolvedPlugin], ctx: &mut RequestCtx) -> bool {
        let mut abort: Option<(DispatchAbortCause, DispatchFailureResponse)> = None;

        for hook in self.dispatch_hooks.iter() {
            let hook_name = hook.name();
            let policy = hook.failure_policy();
            let future =
                AssertUnwindSafe(hook.before_upstream_dispatch(plugins, ctx)).catch_unwind();
            let outcome = tokio::time::timeout(self.lifecycle_hook_timeouts.dispatch, future).await;

            let failure_kind = match outcome {
                Ok(Ok(Ok(()))) => None,
                Ok(Ok(Err(error))) => {
                    tracing::error!(
                        hook = hook_name,
                        error_code = %error.code,
                        "上游派发 hook 执行失败: {}",
                        error
                    );
                    Some(DispatchAbortKind::Error)
                }
                Ok(Err(_panic)) => {
                    tracing::error!(hook = hook_name, "上游派发 hook panic");
                    Some(DispatchAbortKind::Panic)
                }
                Err(_) => {
                    tracing::error!(hook = hook_name, "上游派发 hook 超时");
                    Some(DispatchAbortKind::Timeout)
                }
            };

            if let Some(kind) = failure_kind {
                if let DispatchFailurePolicy::FailClosed(response) = &policy {
                    ctx.forbid_upstream_dispatch();
                    if abort.is_none() {
                        abort = Some((DispatchAbortCause::new(hook_name, kind), response.clone()));
                    }
                }
            }

            // hook 可在预期领域失败时自行写入固定短路响应。无论 failure policy
            // 如何，只要它已短路或显式禁止派发，就必须走独立补偿。
            if ctx.is_short_circuited() || ctx.is_upstream_dispatch_forbidden() {
                ctx.forbid_upstream_dispatch();
                if abort.is_none() {
                    let response = match policy {
                        DispatchFailurePolicy::FailClosed(response) => response,
                        DispatchFailurePolicy::Continue => DispatchFailureResponse::new(
                            503,
                            "upstream_dispatch_forbidden",
                            "Upstream dispatch unavailable",
                        ),
                    };
                    abort = Some((
                        DispatchAbortCause::new(hook_name, DispatchAbortKind::Explicit),
                        response,
                    ));
                }
            }
        }

        let Some((cause, failure_response)) = abort else {
            return !ctx.is_upstream_dispatch_forbidden();
        };

        // 先不可逆禁止联网并冻结根错误，再调用与失败 hook 相互独立的补偿器。
        // 补偿器可以清理不可信 quota headers，但不能把 budget/dispatch 根因改写成
        // 自己的次生错误。
        ctx.forbid_upstream_dispatch();
        if !ctx.is_short_circuited() {
            set_dispatch_failure_response(ctx, &failure_response);
        }
        let root_failure = (
            ctx.exit_status,
            ctx.exit_body.clone(),
            ctx.exit_headers.clone(),
        );
        for handler in self.dispatch_abort_handlers.iter() {
            let handler_name = handler.name();
            let future = AssertUnwindSafe(handler.compensate_before_response(ctx, cause.clone()))
                .catch_unwind();
            match tokio::time::timeout(self.lifecycle_hook_timeouts.abort_compensation, future)
                .await
            {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => tracing::error!(
                    handler = handler_name,
                    error_code = %error.code,
                    "派发中止补偿失败: {}",
                    error
                ),
                Ok(Err(_panic)) => {
                    tracing::error!(handler = handler_name, "派发中止补偿 panic")
                }
                Err(_) => tracing::error!(handler = handler_name, "派发中止补偿超时"),
            }
        }
        ctx.short_circuited = true;
        ctx.exit_status = root_failure.0;
        ctx.exit_body = root_failure.1;
        ctx.exit_headers = root_failure.2;
        ctx.lifecycle
            .mark_gateway_error(LifecyclePhase::Upstream, "request_dispatch_hook");
        false
    }

    async fn run_request_finalizers(&self, plugins: &[ResolvedPlugin], ctx: &mut RequestCtx) {
        for finalizer in self.request_finalizers.iter() {
            let finalizer_name = finalizer.name();
            let future = AssertUnwindSafe(finalizer.finalize(plugins, ctx)).catch_unwind();
            match tokio::time::timeout(self.lifecycle_hook_timeouts.finalizer, future).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => tracing::error!(
                    finalizer = finalizer_name,
                    error_code = %error.code,
                    "请求 finalizer 执行失败: {}",
                    error
                ),
                Ok(Err(_panic)) => {
                    tracing::error!(finalizer = finalizer_name, "请求 finalizer panic")
                }
                Err(_) => tracing::error!(finalizer = finalizer_name, "请求 finalizer 超时"),
            }
        }
    }

    fn transport_error_from_pingora(error: &pingora_core::Error) -> RequestTransportError {
        use pingora_core::{ErrorSource, ErrorType};

        let source = match error.esource() {
            ErrorSource::Upstream => RequestTransportSource::Upstream,
            ErrorSource::Downstream => RequestTransportSource::Downstream,
            ErrorSource::Internal => RequestTransportSource::Internal,
            ErrorSource::Unset => RequestTransportSource::Unknown,
        };
        let kind = match error.root_etype() {
            ErrorType::ConnectTimedout => RequestTransportErrorKind::ConnectTimeout,
            ErrorType::ConnectRefused => RequestTransportErrorKind::ConnectRefused,
            ErrorType::ConnectNoRoute => RequestTransportErrorKind::ConnectNoRoute,
            ErrorType::TLSWantX509Lookup
            | ErrorType::TLSHandshakeFailure
            | ErrorType::TLSHandshakeTimedout
            | ErrorType::InvalidCert
            | ErrorType::HandshakeError => RequestTransportErrorKind::Tls,
            ErrorType::ConnectError | ErrorType::ConnectProxyFailure => {
                RequestTransportErrorKind::Connect
            }
            ErrorType::BindError => RequestTransportErrorKind::Bind,
            ErrorType::AcceptError => RequestTransportErrorKind::Accept,
            ErrorType::SocketError => RequestTransportErrorKind::Socket,
            ErrorType::InvalidHTTPHeader
            | ErrorType::H1Error
            | ErrorType::H2Error
            | ErrorType::H2Downgrade
            | ErrorType::InvalidH2 => RequestTransportErrorKind::Protocol,
            ErrorType::ReadError => RequestTransportErrorKind::Read,
            ErrorType::WriteError => RequestTransportErrorKind::Write,
            ErrorType::ReadTimedout => RequestTransportErrorKind::ReadTimeout,
            ErrorType::WriteTimedout => RequestTransportErrorKind::WriteTimeout,
            ErrorType::ConnectionClosed => RequestTransportErrorKind::ConnectionClosed,
            ErrorType::HTTPStatus(status) => RequestTransportErrorKind::HttpStatus(*status),
            ErrorType::FileOpenError
            | ErrorType::FileCreateError
            | ErrorType::FileReadError
            | ErrorType::FileWriteError => RequestTransportErrorKind::File,
            ErrorType::InternalError => RequestTransportErrorKind::Internal,
            ErrorType::UnknownError => RequestTransportErrorKind::Unknown,
            ErrorType::Custom(_) | ErrorType::CustomCode(_, _) => RequestTransportErrorKind::Custom,
        };

        RequestTransportError::new(source, kind)
    }

    fn proxy_failure_status(error: &pingora_core::Error) -> u16 {
        use pingora_core::ErrorType;

        match error.root_etype() {
            ErrorType::ConnectTimedout
            | ErrorType::TLSHandshakeTimedout
            | ErrorType::ReadTimedout
            | ErrorType::WriteTimedout => 504,
            _ => 502,
        }
    }

    fn find_route_with_snapshot(
        &self,
        request: &RequestContext,
    ) -> Option<(RouteMatch, Option<Route>)> {
        let routing = self
            .routing
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let route_match = routing.router.find_route(request)?;
        let route = routing.routes_by_id.get(&route_match.route_id).cloned();
        Some((route_match, route))
    }

    /// Update routing table — 更新路由表
    pub fn update_routes(&self, routes: &[Route]) {
        let mut routing = self
            .routing
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        routing.router.rebuild(routes);
        routing.routes_by_id = routes
            .iter()
            .map(|route| (route.id, route.clone()))
            .collect();
        drop(routing);
        self.rebuild_plugin_chains();
    }

    /// Update service cache — 更新服务缓存
    pub fn update_services(&self, services: Vec<Service>) {
        let mut cache = self
            .services
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.clear();
        for svc in services {
            cache.insert(svc.id, svc);
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
        *self
            .plugins
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = plugins;
        self.rebuild_plugin_chains();
    }

    /// Pre-compute plugin chains for all (route_id, service_id) combinations — 预计算所有 (route_id, service_id) 组合的插件链
    fn rebuild_plugin_chains(&self) {
        let plugins = self
            .plugins
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();

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

        *self
            .plugin_chains
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = chains;
    }

    /// Hot-reload CA certificate list — 热更新 CA 证书列表
    pub fn update_ca_certificates(&self, cas: Vec<CaCertificate>) {
        if let Ok(mut ca) = self.ca_certificates.write() {
            *ca = cas;
        }
    }

    /// Populate RequestCtx and build RequestContext in a single header scan — 单次头遍历同时填充 RequestCtx 和构建 RequestContext
    /// `default_port`: the actual proxy listening port (from config) — 实际的代理监听端口（来自配置）
    fn populate_and_build_route_ctx(
        session: &Session,
        ctx: &mut RequestCtx,
        default_port: u16,
    ) -> RequestContext {
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
        // When Host header has no port, use the actual server listening port — 当 Host 头没有端口时，使用实际的服务器监听端口
        let (host_no_port, port) = if let Some(colon_pos) = host_header.rfind(':') {
            let (h, p) = host_header.split_at(colon_pos);
            (h.to_string(), p[1..].parse().unwrap_or(default_port))
        } else {
            (host_header.to_string(), default_port)
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
    /// Returns (addr, use_tls, sni, is_grpc) — 返回 (地址, 是否TLS, SNI, 是否gRPC)
    fn resolve_upstream(
        &self,
        service: &Service,
    ) -> std::result::Result<(String, bool, String, bool), Box<pingora_core::Error>> {
        let use_tls = matches!(
            service.protocol,
            kong_core::models::Protocol::Https
                | kong_core::models::Protocol::Grpcs
                | kong_core::models::Protocol::Tls
        );

        let is_grpc = matches!(
            service.protocol,
            kong_core::models::Protocol::Grpc | kong_core::models::Protocol::Grpcs
        );

        // Try resolving upstream address via load balancer — 尝试通过负载均衡器解析上游地址
        if let Ok(balancers) = self.balancers.read() {
            if let Some(lb) = balancers.get(&service.host) {
                if let Some(addr) = lb.select() {
                    // SNI priority: upstream.host_header > hostname part of target address — SNI 优先级：upstream.host_header > target 地址的主机名部分
                    let sni = lb
                        .host_header()
                        .unwrap_or_else(|| addr.split(':').next().unwrap_or(&addr).to_string());
                    return Ok((addr, use_tls, sni, is_grpc));
                }
            }
        }

        // Use Service's host:port directly — 直接使用 Service 的 host:port
        let addr = format!("{}:{}", service.host, service.port);
        let sni = service.host.clone();
        Ok((addr, use_tls, sni, is_grpc))
    }

    // Helper: check if a specific header feature is enabled in config.headers — 检查配置中是否启用了特定 header 功能
    fn has_header_feature(&self, feature: &str) -> bool {
        self.config
            .headers
            .iter()
            .any(|h| h.eq_ignore_ascii_case(feature))
    }

    // Helper: check if Server header should be included (server_tokens or explicit "Server") — 检查是否应包含 Server 头
    fn should_include_server(&self) -> bool {
        self.has_header_feature("server_tokens") || self.has_header_feature("server")
    }

    // Helper: check if Via header should be included (server_tokens or explicit "Via") — 检查是否应包含 Via 头
    fn should_include_via(&self) -> bool {
        self.has_header_feature("server_tokens") || self.has_header_feature("via")
    }

    // Helper: check if X-Kong-Proxy-Latency should be included — 检查是否应包含 X-Kong-Proxy-Latency
    fn should_include_proxy_latency(&self) -> bool {
        self.has_header_feature("latency_tokens") || self.has_header_feature("x-kong-proxy-latency")
    }

    // Helper: check if X-Kong-Upstream-Latency should be included — 检查是否应包含 X-Kong-Upstream-Latency
    fn should_include_upstream_latency(&self) -> bool {
        self.has_header_feature("latency_tokens")
            || self.has_header_feature("x-kong-upstream-latency")
    }

    // Helper: check if X-Kong-Response-Latency should be included (for non-proxied responses) — 检查是否应包含 X-Kong-Response-Latency（非代理响应）
    fn should_include_response_latency(&self) -> bool {
        self.has_header_feature("latency_tokens")
            || self.has_header_feature("x-kong-response-latency")
    }

    /// 构建非代理响应头（404/错误/短路）— Content-Type + Content-Length + Server 头 + 延迟头 + 自定义头注入
    /// Build non-proxied response header (404/error/short-circuit)
    fn build_response_header(
        &self,
        status_code: u16,
        body_len: usize,
    ) -> pingora_core::Result<ResponseHeader> {
        let mut resp = ResponseHeader::build(status_code, Some(8))?;
        // 204 No Content: RFC 7230 forbids Content-Length — 204 无内容：RFC 7230 禁止 Content-Length
        if status_code != 204 {
            resp.insert_header("content-length", body_len.to_string())?;
            resp.insert_header("content-type", "application/json; charset=utf-8")?;
        }

        // Server 头：仅当配置中包含 server_tokens 或 Server 时添加
        // Server header: only add when server_tokens or Server is in headers config
        if self.should_include_server() {
            resp.insert_header("server", "kong/3.10.0")?;
        }

        // X-Kong-Response-Latency：非代理响应中添加
        // X-Kong-Response-Latency: add in non-proxied responses
        if self.should_include_response_latency() {
            resp.insert_header("x-kong-response-latency", "0")?;
        }

        // 注入自定义响应头
        apply_proxy_response_headers(&mut resp, &self.config.proxy_response_headers);

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
            let port =
                plugin_ctx
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

    /// Send framework-level error — auto-detect gRPC vs HTTP format — 发送框架级错误 — 自动检测 gRPC 或 HTTP 格式
    async fn send_error_or_grpc(
        &self,
        session: &mut Session,
        status_code: u16,
        message: &str,
        ctx: &mut KongCtx,
    ) -> pingora_core::Result<bool> {
        ctx.plugin_ctx.lifecycle.mark_downstream_send_attempted();
        let request_id = ctx.request_id().to_string();
        if ctx.is_grpc_request {
            grpc::send_grpc_error(
                session,
                status_code,
                message,
                Some(&request_id),
                self.has_header_feature("x-kong-request-id"),
            )
            .await
        } else {
            self.send_error_response(session, status_code, message, Some(&request_id))
                .await
        }
    }

    /// 发送框架级错误响应（JSON 格式，受配置控制）
    async fn send_error_response(
        &self,
        session: &mut Session,
        status_code: u16,
        message: &str,
        request_id: Option<&str>,
    ) -> pingora_core::Result<bool> {
        let body = format!("{{\"message\":\"{}\"}}", message);
        let body_bytes = body.as_bytes();

        let mut resp = self.build_response_header(status_code, body_bytes.len())?;

        // Inject X-Kong-Request-Id in error responses — 在错误响应中注入 X-Kong-Request-Id
        if let Some(rid) = request_id {
            if self
                .config
                .headers
                .iter()
                .any(|h| h.eq_ignore_ascii_case("x-kong-request-id"))
            {
                let _ = resp.insert_header("x-kong-request-id", rid);
            }
        }

        session.write_response_header(Box::new(resp), false).await?;
        session
            .write_response_body(Some(Bytes::copy_from_slice(body_bytes)), true)
            .await?;

        Ok(true)
    }

    /// Send short-circuit response (supports custom status + headers + body) — 发送短路响应（支持自定义 status + headers + body）
    /// Also runs header_filter phase so response-transformer and other plugins can modify headers — 同时运行 header_filter 阶段，使 response-transformer 等插件可以修改响应头
    async fn send_short_circuit_response(
        &self,
        session: &mut Session,
        ctx: &mut RequestCtx,
        resolved_plugins: &[kong_plugin_system::ResolvedPlugin],
    ) -> pingora_core::Result<bool> {
        ctx.lifecycle.mark_downstream_send_attempted();
        let request_id = ctx.lifecycle.request_id.clone();
        let status_code = ctx.exit_status.unwrap_or(200);
        let body = ctx.exit_body.take();
        let headers = ctx.exit_headers.take();

        let body_bytes = body.as_deref().unwrap_or("").as_bytes();
        let mut resp = self.build_response_header(status_code, body_bytes.len())?;

        // Inject X-Kong-Request-Id in short-circuit responses — 在短路响应中注入 X-Kong-Request-Id
        if self
            .config
            .headers
            .iter()
            .any(|h| h.eq_ignore_ascii_case("x-kong-request-id"))
        {
            let _ = resp.insert_header("x-kong-request-id", &request_id);
        }

        // Apply exit_headers from the short-circuiting plugin — 应用短路插件设置的自定义响应头
        // Use insert_header with HeaderName to maintain Pingora's internal tracking — 使用 insert_header + HeaderName 维护 Pingora 内部跟踪
        if let Some(hdrs) = headers {
            for (name, value) in hdrs {
                if let Ok(hn) = http::header::HeaderName::from_bytes(name.as_bytes()) {
                    if let Ok(hv) = http::header::HeaderValue::from_str(&value) {
                        let _ = resp.insert_header(hn, hv);
                    }
                }
            }
        }

        // Run header_filter phase on short-circuited response (Kong compatibility) — 在短路响应上运行 header_filter 阶段（Kong 兼容）
        // Populate response context so header_filter plugins can inspect/modify — 填充响应上下文供 header_filter 插件检查/修改
        ctx.response_status = Some(status_code);
        ctx.response_headers.clear();
        for (name, value) in resp.headers.iter() {
            if let Ok(v) = value.to_str() {
                ctx.response_headers
                    .insert(name.as_str().to_lowercase(), v.to_string());
            }
        }
        if let Err(e) = PhaseRunner::run_header_filter(resolved_plugins, ctx).await {
            tracing::warn!("Short-circuit header_filter 阶段执行失败: {}", e);
        }
        // Apply header modifications from header_filter plugins — 应用 header_filter 插件的头修改
        // Use insert_header/remove_header with HeaderName to maintain Pingora's internal tracking — 使用 insert_header/remove_header + HeaderName 维护 Pingora 内部跟踪
        for (name, value) in ctx.response_headers_to_set.drain(..) {
            if let Ok(hn) = http::header::HeaderName::from_bytes(name.as_bytes()) {
                if let Ok(hv) = http::header::HeaderValue::from_str(&value) {
                    let _ = resp.insert_header(hn, hv);
                }
            }
        }
        for name in ctx.response_headers_to_remove.drain(..) {
            if let Ok(hn) = http::header::HeaderName::from_bytes(name.as_bytes()) {
                let _ = resp.remove_header(&hn);
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
            upstream_is_grpc: false,
            is_grpc_request: false,
            upstream_sni: String::new(),
            plugin_ctx: RequestCtx::new(),
            resolved_plugins: Arc::new(Vec::new()),
            request_body_buf: None,
            response_body_buf: None,
            deferred_header_filter: false,
            last_body_chunk_at: None,
            injected_real_ip_headers: Vec::new(),
            upstream_response_time: None,
        }
    }

    /// Request filter phase — route matching + plugin rewrite/access — 请求过滤阶段 — 路由匹配 + 插件 rewrite/access
    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<bool> {
        // 1. Populate request context + build route matching context (single header scan) — 填充请求上下文 + 构建路由匹配上下文（单次头遍历）
        // Get default proxy port from config — 从配置获取默认代理端口
        let default_port = self
            .config
            .proxy_listen
            .first()
            .and_then(|l| l.address.rsplit(':').next())
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(8000);
        let req_ctx =
            Self::populate_and_build_route_ctx(session, &mut ctx.plugin_ctx, default_port);

        // 1.5 Detect gRPC request (content-type: application/grpc) — 检测 gRPC 请求
        ctx.is_grpc_request = grpc::is_grpc_request(session);

        // 2. Route matching — 路由匹配
        let (route_match, matched_route) = match self.find_route_with_snapshot(&req_ctx) {
            Some(result) => result,
            None => {
                return self
                    .send_error_or_grpc(session, 404, "no Route matched with those values", ctx)
                    .await;
            }
        };

        // 3. Find Service (optional — serviceless routes are allowed) — 查找 Service（可选 — 允许无服务路由）
        let service = if let Some(service_id) = route_match.service_id {
            let services = self
                .services
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            services.get(&service_id).cloned()
        } else {
            None
        };

        // 4. Set up plugin context (before service check so plugins can short-circuit serviceless routes) — 设置插件上下文（在服务检查之前，以便插件可以短路无服务路由）
        ctx.plugin_ctx.route_id = Some(route_match.route_id);
        ctx.plugin_ctx.route_name = route_match.route_name.as_ref().map(|name| name.to_string());
        ctx.plugin_ctx.service_id = route_match.service_id;
        ctx.plugin_ctx.service_name = service.as_ref().and_then(|value| value.name.clone());
        // Pass URI captures from regex path matching to plugin context — 将正则路径匹配的 URI 捕获组传递给插件上下文
        ctx.plugin_ctx.uri_captures_named = route_match.uri_captures.named.clone();
        ctx.plugin_ctx.uri_captures_unnamed = route_match.uri_captures.unnamed.clone();

        // Populate matched route JSON for kong.router.get_route() — 填充匹配路由 JSON 供 kong.router.get_route() 使用
        if let Some(route) = matched_route {
            ctx.plugin_ctx.workspace_id = route.ws_id;
            if ctx.plugin_ctx.route_name.is_none() {
                ctx.plugin_ctx.route_name = route.name.clone();
            }
            if let Ok(route_json) = serde_json::to_value(route) {
                ctx.plugin_ctx.matched_route_json = Some(route_json);
            }
        }

        // Route/Service 快照需在请求体和插件阶段之前可见。
        ctx.route_match = Some(route_match.clone());
        ctx.service = service.clone();

        // 5. Resolve plugin chain (from pre-computed cache) — 解析插件链（从预计算缓存）
        let resolved_plugins = {
            let key = (Some(route_match.route_id), route_match.service_id);
            let chains = self
                .plugin_chains
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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

        // 保存链并同步通知观察器，确保后续任意早期失败仍能在 logging 收口。
        ctx.resolved_plugins = resolved_plugins;
        let plugins_ref = Arc::clone(&ctx.resolved_plugins);
        self.notify_plugins_resolved(&plugins_ref, &mut ctx.plugin_ctx);

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
            let transport_error = Self::transport_error_from_pingora(&err);
            if transport_error.source != RequestTransportSource::Unknown {
                ctx.plugin_ctx
                    .lifecycle
                    .mark_transport_error(transport_error);
            }
            ctx.plugin_ctx
                .lifecycle
                .mark_gateway_error(LifecyclePhase::RequestBody, "request_body_preload");
            return self
                .send_error_or_grpc(session, 400, "Bad request body", ctx)
                .await;
        }

        // 8. Execute rewrite phase — 执行 rewrite 阶段
        if let Err(e) = PhaseRunner::run_rewrite(&plugins_ref, &mut ctx.plugin_ctx).await {
            tracing::error!("Rewrite 阶段执行失败: {}", e);
            return self
                .send_error_or_grpc(session, 500, "An unexpected error occurred", ctx)
                .await;
        }

        // 9. Check short-circuit — 检查短路
        if ctx.plugin_ctx.is_short_circuited() {
            return self
                .send_short_circuit_response(session, &mut ctx.plugin_ctx, &plugins_ref)
                .await;
        }

        // 10. Execute access phase — 执行 access 阶段
        if let Err(e) = PhaseRunner::run_access(&plugins_ref, &mut ctx.plugin_ctx).await {
            tracing::error!("Access 阶段执行失败: {}", e);
            return self
                .send_error_or_grpc(session, 500, "An unexpected error occurred", ctx)
                .await;
        }

        // 11. Check short-circuit — 检查短路
        if ctx.plugin_ctx.is_short_circuited() {
            return self
                .send_short_circuit_response(session, &mut ctx.plugin_ctx, &plugins_ref)
                .await;
        }

        // After plugins executed: if no service, return 503 — 插件执行完后：如果没有 service，返回 503
        let service = match service {
            Some(s) => s,
            None => {
                ctx.plugin_ctx
                    .lifecycle
                    .mark_gateway_error(LifecyclePhase::Service, "service_lookup");
                return self
                    .send_error_or_grpc(
                        session,
                        503,
                        "no Service found for the requested route",
                        ctx,
                    )
                    .await;
            }
        };

        if !service.enabled {
            ctx.plugin_ctx
                .lifecycle
                .mark_gateway_error(LifecyclePhase::Service, "service_disabled");
            return self
                .send_error_or_grpc(session, 503, "Service unavailable", ctx)
                .await;
        }

        // Resolve upstream address — 解析上游地址
        let (mut upstream_addr, mut upstream_tls, mut upstream_sni, upstream_is_grpc) =
            match self.resolve_upstream(&service) {
                Ok(value) => value,
                Err(_) => {
                    ctx.plugin_ctx
                        .lifecycle
                        .mark_gateway_error(LifecyclePhase::Upstream, "upstream_configuration");
                    return Err(pingora_core::Error::new_str("上游解析失败"));
                }
            };

        self.apply_plugin_upstream_overrides(
            &mut upstream_addr,
            &mut upstream_tls,
            &mut upstream_sni,
            &ctx.plugin_ctx,
        );

        // Save to context — 保存到上下文
        ctx.service = Some(service);
        ctx.upstream_addr = Some(upstream_addr);
        ctx.upstream_tls = upstream_tls;
        ctx.upstream_sni = upstream_sni;
        ctx.upstream_is_grpc = upstream_is_grpc;

        if !self
            .run_dispatch_hooks(&plugins_ref, &mut ctx.plugin_ctx)
            .await
        {
            return self
                .send_short_circuit_response(session, &mut ctx.plugin_ctx, &plugins_ref)
                .await;
        }

        Ok(false) // Continue to upstream — 继续到上游
    }

    /// Determine upstream peer address — 确定上游地址
    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<Box<HttpPeer>> {
        let raw_addr = match ctx.upstream_addr.clone() {
            Some(value) => value,
            None => {
                ctx.plugin_ctx
                    .lifecycle
                    .mark_gateway_error(LifecyclePhase::Upstream, "upstream_address");
                return Err(pingora_core::Error::new_str("上游地址未设置"));
            }
        };
        // Ensure address includes port — 确保地址包含端口
        let addr_with_port = if raw_addr.contains(':') {
            raw_addr
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
        let socket_addr = match self.dns_resolver.resolve(host, port).await {
            Ok(value) => value,
            Err(error) => {
                tracing::error!("上游地址解析失败: {} ({})", addr_with_port, error);
                ctx.plugin_ctx
                    .lifecycle
                    .mark_transport_error(RequestTransportError::upstream(
                        RequestTransportErrorKind::ConnectNoRoute,
                    ));
                return Err(pingora_core::Error::new_up(
                    pingora_core::ErrorType::ConnectNoRoute,
                ));
            }
        };

        let mut peer = HttpPeer::new(socket_addr, ctx.upstream_tls, ctx.upstream_sni.clone());

        // Set HTTP version for upstream connection — 设置上游连接的 HTTP 版本
        if ctx.plugin_ctx.upstream_force_http1 {
            // Plugin requested HTTP/1.1 (e.g. AI proxy avoids H2 multiplexing issues) — 插件请求 HTTP/1.1（如 AI proxy 避免 H2 多路复用问题）
            peer.options.alpn = pingora_core::protocols::tls::ALPN::H1;
        } else if ctx.upstream_tls {
            // TLS: prefer HTTP/2 via ALPN — TLS：通过 ALPN 优先使用 HTTP/2
            peer.options.alpn = pingora_core::protocols::tls::ALPN::H2H1;
        } else if ctx.upstream_is_grpc {
            // Plaintext gRPC: force HTTP/2 (h2c prior knowledge) — 明文 gRPC：强制 HTTP/2（h2c 先验知识）
            peer.options.set_http_version(2, 2);
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

        // DNS 与 peer 配置均已成功，下一步才会真正进入 Pingora 建连路径。
        ctx.plugin_ctx.lifecycle.mark_upstream_attempted();
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
            set_upstream_header(upstream_request, name, &value);
        }
        for name in ctx.plugin_ctx.upstream_headers_to_remove.drain(..) {
            upstream_request.remove_header(name.as_str());
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
        if ctx.plugin_ctx.upstream_path.is_some() || ctx.plugin_ctx.upstream_query_to_set.is_some()
        {
            let path = ctx
                .plugin_ctx
                .upstream_path
                .as_deref()
                .unwrap_or_else(|| upstream_request.uri.path());
            let query = ctx.plugin_ctx.upstream_query_to_set.as_ref().map(|pairs| {
                pairs
                    .iter()
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

        // 4.5 Remove hop-by-hop headers from upstream request (RFC 7230 §6.1) — 移除逐跳头（RFC 7230 §6.1）
        // Only remove headers that Kong explicitly strips; Pingora manages Connection/TE/Transfer-Encoding
        // 仅移除 Kong 明确要剥离的头；Pingora 管理 Connection/TE/Transfer-Encoding
        {
            upstream_request.remove_header("keep-alive");
            upstream_request.remove_header("proxy-authenticate");
            // gRPC uses HTTP/2 trailers for grpc-status/grpc-message; do not strip — gRPC 使用 HTTP/2 trailer 传输 grpc-status/grpc-message，不能剥离
            if !ctx.is_grpc_request {
                upstream_request.remove_header("trailer");
            }
        }

        // 5. If a plugin replaced the upstream body, fix Content-Length for the replayed payload. — 若插件替换了上游请求体，修正回放 payload 的 Content-Length。
        if let Some(body) = ctx.plugin_ctx.upstream_body.as_ref() {
            let _ = upstream_request.insert_header("content-length", body.len().to_string());
        }

        // 6. X-Real-IP / X-Forwarded-* 头注入（按配置列表按需注入，实现 Kong trusted_ips 信任模型）
        // Kong trust model: if client IP is in trusted_ips, preserve original headers; otherwise replace them.
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

            // Check if client IP is trusted — 检查客户端 IP 是否可信
            let is_trusted = !self.config.trusted_ips.is_empty()
                && self.config.trusted_ips.iter().any(|tip| {
                    let tip = tip.trim();
                    if tip.contains('/') {
                        // CIDR match — CIDR 匹配
                        cidr_contains(tip, &client_ip)
                    } else {
                        tip == client_ip
                    }
                });

            if !client_ip.is_empty() {
                if headers_set.contains("x-real-ip") {
                    if is_trusted {
                        // Trusted: preserve original X-Real-IP if present — 可信客户端：保留原始 X-Real-IP
                        let existing = session
                            .req_header()
                            .headers
                            .get("x-real-ip")
                            .and_then(|v| v.to_str().ok());
                        if existing.is_none() {
                            let _ = upstream_request.insert_header("x-real-ip", &client_ip);
                        }
                        let val = existing.unwrap_or(&client_ip).to_string();
                        ctx.injected_real_ip_headers
                            .push(("X-Real-IP".to_string(), val));
                    } else {
                        // Untrusted: always replace with real client IP — 不可信客户端：替换为真实 IP
                        let _ = upstream_request.insert_header("x-real-ip", &client_ip);
                        ctx.injected_real_ip_headers
                            .push(("X-Real-IP".to_string(), client_ip.clone()));
                    }
                }
                if headers_set.contains("x-forwarded-for") {
                    // X-Forwarded-For: always append (trusted or not) — X-Forwarded-For：始终追加
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
                if is_trusted {
                    // Trusted: preserve original if present — 可信客户端：保留原值
                    if session
                        .req_header()
                        .headers
                        .get("x-forwarded-proto")
                        .is_none()
                    {
                        let _ = upstream_request.insert_header("x-forwarded-proto", proto);
                    }
                } else {
                    let _ = upstream_request.insert_header("x-forwarded-proto", proto);
                }
                ctx.injected_real_ip_headers
                    .push(("X-Forwarded-Proto".to_string(), proto.to_string()));
            }

            if headers_set.contains("x-forwarded-host") {
                if let Some(host) = session.req_header().headers.get("host") {
                    if is_trusted {
                        // Trusted: preserve original if present — 可信客户端：保留原值
                        if session
                            .req_header()
                            .headers
                            .get("x-forwarded-host")
                            .is_none()
                        {
                            let _ = upstream_request.insert_header("x-forwarded-host", host);
                        }
                    } else {
                        let _ = upstream_request.insert_header("x-forwarded-host", host);
                    }
                    if let Ok(v) = host.to_str() {
                        ctx.injected_real_ip_headers
                            .push(("X-Forwarded-Host".to_string(), v.to_string()));
                    }
                }
            }

            if headers_set.contains("x-forwarded-port") {
                // Use the proxy's actual listening port, not the URI port — 使用代理的实际监听端口
                let port = session
                    .server_addr()
                    .map(|a| a.as_inet().map(|s| s.port()).unwrap_or(0))
                    .unwrap_or(0);
                let port = if port == 0 {
                    session.req_header().uri.port_u16().unwrap_or(
                        if session
                            .digest()
                            .map(|d| d.ssl_digest.is_some())
                            .unwrap_or(false)
                        {
                            443
                        } else {
                            80
                        },
                    )
                } else {
                    port
                };
                let port_str = port.to_string();
                if is_trusted {
                    if session
                        .req_header()
                        .headers
                        .get("x-forwarded-port")
                        .is_none()
                    {
                        let _ = upstream_request.insert_header("x-forwarded-port", &port_str);
                    }
                } else {
                    let _ = upstream_request.insert_header("x-forwarded-port", &port_str);
                }
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
                if is_trusted {
                    if session
                        .req_header()
                        .headers
                        .get("x-forwarded-path")
                        .is_none()
                    {
                        let _ = upstream_request.insert_header("x-forwarded-path", path);
                    }
                } else {
                    let _ = upstream_request.insert_header("x-forwarded-path", path);
                }
                ctx.injected_real_ip_headers
                    .push(("X-Forwarded-Path".to_string(), path.to_string()));
            }

            if headers_set.contains("x-forwarded-prefix") {
                if is_trusted {
                    // Trusted: preserve original X-Forwarded-Prefix — 可信客户端：保留原值
                } else {
                    // Untrusted: set to matched path prefix if strip_path is active — 不可信客户端：若 strip_path 生效则设置为匹配路径前缀
                    if let Some(ref rm) = ctx.route_match {
                        if rm.strip_path {
                            if let Some(ref matched) = rm.matched_path {
                                if matched != "/" {
                                    let _ = upstream_request
                                        .insert_header("x-forwarded-prefix", matched.as_str());
                                    ctx.injected_real_ip_headers
                                        .push(("X-Forwarded-Prefix".to_string(), matched.clone()));
                                } else {
                                    // Root path: remove prefix header — 根路径：移除前缀头
                                    upstream_request.remove_header("x-forwarded-prefix");
                                }
                            }
                        } else {
                            // strip_path=false: remove prefix header — strip_path=false：移除前缀头
                            upstream_request.remove_header("x-forwarded-prefix");
                        }
                    }
                }
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
        if self
            .config
            .headers_upstream
            .iter()
            .any(|h| h.eq_ignore_ascii_case("x-kong-request-id"))
        {
            let _ = upstream_request.insert_header("x-kong-request-id", ctx.request_id());
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

        // Default buffering=true (Kong's default), but gRPC always streams — 默认 buffering=true（Kong 默认），但 gRPC 始终流式
        let buffering = if ctx.is_grpc_request {
            false // gRPC streaming: never buffer — gRPC 流式：永不缓冲
        } else {
            ctx.route_match
                .as_ref()
                .map(|rm| rm.request_buffering)
                .unwrap_or(true)
        };

        if !buffering {
            // Pass through (Pingora default streaming behavior) — 直接透传（Pingora 默认流式行为）
            return Ok(());
        }

        // Check chunk interval timeout (use service read_timeout, default 60s) — 检查 chunk 间隔超时（使用 service read_timeout，默认 60s）
        let timeout_secs = ctx
            .service
            .as_ref()
            .map(|s| s.read_timeout as u64 / 1000)
            .unwrap_or(60)
            .max(60); // minimum 60s to avoid premature timeout — 最少 60s 避免过早超时
        let now = std::time::Instant::now();
        if let Some(last_at) = ctx.last_body_chunk_at {
            if now.duration_since(last_at).as_secs() > timeout_secs {
                tracing::warn!("请求体 chunk 间隔超时 (>{}s)，终止请求", timeout_secs);
                ctx.plugin_ctx
                    .lifecycle
                    .mark_transport_error(RequestTransportError::downstream(
                        RequestTransportErrorKind::ReadTimeout,
                    ));
                return Err(pingora_core::Error::new_down(
                    pingora_core::ErrorType::ReadTimedout,
                ));
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
        ctx.plugin_ctx
            .lifecycle
            .mark_upstream_status(upstream_response.status.as_u16());
        ctx.plugin_ctx.lifecycle.mark_downstream_send_attempted();

        // Record upstream response time for latency tracking — 记录上游响应时间用于延迟统计
        ctx.upstream_response_time = Some(std::time::Instant::now());

        // Remove hop-by-hop headers from upstream response — 移除上游响应中的逐跳头
        upstream_response.remove_header("keep-alive");
        upstream_response.remove_header("proxy-authenticate");
        // gRPC uses HTTP/2 trailers for grpc-status/grpc-message; do not strip — gRPC 使用 HTTP/2 trailer，不能剥离
        if !ctx.is_grpc_request {
            upstream_response.remove_header("trailer");
        }

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
            upstream_response.remove_header(&http::header::CONTENT_LENGTH);
            upstream_response.remove_header(&http::header::CONTENT_ENCODING);
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
                        let _ = upstream_response.insert_header(header_name, header_value);
                    }
                }
            }
            for name in ctx.plugin_ctx.response_headers_to_remove.drain(..) {
                if let Ok(header_name) = http::header::HeaderName::from_bytes(name.as_bytes()) {
                    upstream_response.remove_header(&header_name);
                }
            }
        }

        // Add Kong standard response headers for proxied responses — 添加代理响应的 Kong 标准响应头
        // Latency headers: X-Kong-Proxy-Latency and X-Kong-Upstream-Latency — 延迟头
        let now = std::time::Instant::now();
        let proxy_latency = now.duration_since(ctx.request_start_time()).as_millis();
        let upstream_latency = ctx
            .upstream_response_time
            .map(|t| t.duration_since(ctx.request_start_time()).as_millis())
            .unwrap_or(0);
        if self.should_include_proxy_latency() {
            let _ =
                upstream_response.insert_header("x-kong-proxy-latency", &proxy_latency.to_string());
        }
        if self.should_include_upstream_latency() {
            let _ = upstream_response
                .insert_header("x-kong-upstream-latency", &upstream_latency.to_string());
        }

        // Via header: only for proxied responses (server_tokens or Via) — Via 头：仅代理响应
        if self.should_include_via() {
            let _ = upstream_response.insert_header("via", "1.1 kong/3.10.0");
        }

        // Use per-request X-Kong-Request-Id in downstream response (only if headers config includes it) — 在下游响应中使用每请求的 X-Kong-Request-Id（仅当 headers 配置包含时）
        if self.has_header_feature("x-kong-request-id") {
            let _ = upstream_response.insert_header("x-kong-request-id", ctx.request_id());
        }

        // For proxied responses: do NOT set Kong's Server header — 代理响应：不要设置 Kong 的 Server 头
        // The upstream's Server header is preserved as-is — 保留上游的 Server 头原样

        // 注入自定义响应头
        apply_proxy_response_headers(upstream_response, &self.config.proxy_response_headers);

        Ok(())
    }

    /// Response filter — runs after caching, before sending to downstream — 响应过滤 — 在缓存后、发送到下游前执行
    /// Ensures X-Kong-Request-Id and other critical headers are present in ALL responses — 确保所有响应中都包含 X-Kong-Request-Id 等关键头
    async fn response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<()> {
        ctx.plugin_ctx.lifecycle.mark_downstream_send_attempted();

        // Ensure X-Kong-Request-Id is set in downstream response (defense in depth) — 确保下游响应中设置了 X-Kong-Request-Id（纵深防御）
        if self
            .config
            .headers
            .iter()
            .any(|h| h.eq_ignore_ascii_case("x-kong-request-id"))
        {
            let _ = upstream_response.insert_header("x-kong-request-id", ctx.request_id());
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
        ctx.plugin_ctx.lifecycle.mark_downstream_send_attempted();

        // 1. Response buffering — 响应体缓冲
        // gRPC always streams; plugin-requested buffering must force full upstream response collection.
        // gRPC 始终流式；插件显式请求的 buffering 必须强制启用完整上游响应缓冲。
        let buffering = if ctx.is_grpc_request {
            false // gRPC streaming: never buffer responses — gRPC 流式：永不缓冲响应
        } else {
            ctx.route_match
                .as_ref()
                .map(|rm| rm.response_buffering)
                .unwrap_or(true)
                || ctx.plugin_ctx.request_buffering_enabled
        };

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
        ctx.plugin_ctx
            .lifecycle
            .mark_transport_error(Self::transport_error_from_pingora(e));
        let status = Self::proxy_failure_status(e);
        let body = if status == 504 {
            serde_json::json!({"message": "The upstream server is timing out"})
        } else {
            serde_json::json!({"message": "An invalid response was received from the upstream server"})
        };

        let body_bytes = serde_json::to_vec(&body).unwrap_or_default();
        if let Ok(mut resp) = ResponseHeader::build(status, Some(8)) {
            let _ = resp.insert_header("content-type", "application/json; charset=utf-8");
            let _ = resp.insert_header("content-length", body_bytes.len().to_string());
            // 502/504 是代理失败，按代理响应处理头 — 502/504 are proxy failures, treat as proxied response headers
            if self.should_include_server() {
                let _ = resp.insert_header("server", "kong/3.10.0");
            }
            if self.should_include_via() {
                let _ = resp.insert_header("via", "1.1 kong/3.10.0");
            }
            // Latency headers for proxy failures — 代理失败的延迟头
            let now = std::time::Instant::now();
            let proxy_latency = now.duration_since(ctx.request_start_time()).as_millis();
            let upstream_latency = ctx
                .upstream_response_time
                .map(|t| t.duration_since(ctx.request_start_time()).as_millis())
                .unwrap_or(0);
            if self.should_include_proxy_latency() {
                let _ = resp.insert_header("x-kong-proxy-latency", &proxy_latency.to_string());
            }
            if self.should_include_upstream_latency() {
                let _ =
                    resp.insert_header("x-kong-upstream-latency", &upstream_latency.to_string());
            }
            if self.has_header_feature("x-kong-request-id") {
                let _ = resp.insert_header("x-kong-request-id", ctx.request_id());
            }
            ctx.plugin_ctx.lifecycle.mark_downstream_send_attempted();
            let write_result = match session.write_response_header(Box::new(resp), false).await {
                Ok(()) => {
                    session
                        .write_response_body(Some(bytes::Bytes::from(body_bytes)), true)
                        .await
                }
                Err(error) => Err(error),
            };
            if let Err(write_error) = write_result {
                let mapped = Self::transport_error_from_pingora(&write_error);
                let mapped = if mapped.source == RequestTransportSource::Unknown {
                    RequestTransportError::downstream(mapped.kind)
                } else {
                    mapped
                };
                ctx.plugin_ctx.lifecycle.mark_transport_error(mapped);
            }
        } else {
            ctx.plugin_ctx
                .lifecycle
                .mark_gateway_error(LifecyclePhase::Response, "proxy_error_response");
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
        if let Some(error) = error {
            ctx.plugin_ctx
                .lifecycle
                .mark_transport_error(Self::transport_error_from_pingora(error));
        } else {
            ctx.plugin_ctx.lifecycle.mark_downstream_completed();
        }
        let final_status = (status > 0).then_some(status);
        ctx.plugin_ctx.lifecycle.finish(final_status);
        if let Some(status) = final_status {
            ctx.plugin_ctx.response_status = Some(status);
        }
        if ctx.plugin_ctx.response_source.is_none() {
            ctx.plugin_ctx.response_source = Some("service".to_string());
        }
        let plugins = Arc::clone(&ctx.resolved_plugins);
        self.notify_request_finalizing(&plugins, &mut ctx.plugin_ctx);

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

        // Calculate latencies for prometheus plugin — 计算延迟指标供 prometheus 插件使用
        let now = std::time::Instant::now();
        let request_latency = now.duration_since(ctx.request_start_time()).as_millis() as i64;
        let (kong_latency, proxy_latency) = if let Some(upstream_time) = ctx.upstream_response_time
        {
            let proxy = upstream_time
                .duration_since(ctx.request_start_time())
                .as_millis() as i64;
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
        let service_name = ctx
            .service
            .as_ref()
            .and_then(|s| s.name.clone())
            .unwrap_or_default();
        let route_id = ctx
            .route_match
            .as_ref()
            .map(|rm| rm.route_id.to_string())
            .unwrap_or_default();
        let route_name = ctx
            .route_match
            .as_ref()
            .and_then(|rm| rm.route_name.as_ref().map(|n| n.to_string()))
            .unwrap_or_else(|| route_id.clone());

        // Calculate request size: header line + headers + body — 计算请求大小：请求行 + 头 + 体
        let req_header_size: usize = session
            .req_header()
            .headers
            .iter()
            .map(|(k, v)| k.as_str().len() + v.len() + 4)
            .sum();
        let req_body_size = ctx
            .plugin_ctx
            .request_body
            .as_ref()
            .map(|b| b.len())
            .unwrap_or(0);
        let request_size = (req_header_size + req_body_size) as i64;

        // Calculate response size: headers + body — 计算响应大小：头 + 体
        let resp_header_size: usize = session
            .response_written()
            .map(|r| {
                r.headers
                    .iter()
                    .map(|(k, v)| k.as_str().len() + v.len() + 4)
                    .sum::<usize>()
            })
            .unwrap_or(0);
        let resp_body_size = ctx
            .plugin_ctx
            .service_response_body
            .as_ref()
            .map(|b| b.len())
            .unwrap_or(0);
        let response_size = (resp_header_size + resp_body_size) as i64;

        // Extract consumer username from authenticated_consumer — 从 authenticated_consumer 提取消费者用户名
        let consumer_value = ctx
            .plugin_ctx
            .authenticated_consumer
            .as_ref()
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

        // observer 已形成不可变 usage fact，上面的延迟、大小与 log snapshot 也已冻结；
        // 此后 finalizer 的 I/O 不计入客户端请求时延，并彼此隔离。
        self.run_request_finalizers(&plugins, &mut ctx.plugin_ctx)
            .await;

        // Execute plugin log phase (always executes, even after short-circuit) — 执行插件 log 阶段（总是执行，即使之前短路）
        if let Err(e) = PhaseRunner::run_log(&ctx.resolved_plugins, &mut ctx.plugin_ctx).await {
            tracing::error!("Log 阶段执行失败: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use async_trait::async_trait;
    use kong_config::KongConfig;
    use kong_core::models::Route;
    use kong_core::traits::{RequestCtx, RequestTransportError, RequestTransportErrorKind};
    use kong_plugin_system::{
        DispatchAbortCause, DispatchFailurePolicy, DispatchFailureResponse,
        DispatchFailureResponseFormat, LifecycleHookError, LifecycleHookTimeouts, PluginRegistry,
        RequestDispatchAbortHandler, RequestDispatchHook, RequestFinalizer,
        RequestLifecycleObserver, ResolvedPlugin,
    };
    use kong_router::RequestContext;
    use pingora_core::{Error, ErrorType};
    use pingora_http::{RequestHeader, ResponseHeader};

    use super::{apply_proxy_response_headers, set_upstream_header, KongProxy};
    use crate::dns::DnsResolver;
    use crate::tls::CertificateManager;

    fn test_proxy() -> KongProxy {
        let config = Arc::new(KongConfig::default());
        let dns_resolver = Arc::new(DnsResolver::new(&config));
        KongProxy::new(
            &[],
            "traditional",
            PluginRegistry::new(),
            CertificateManager::new(),
            Vec::new(),
            dns_resolver,
            config,
        )
    }

    struct CountingObserver {
        resolved: Arc<AtomicUsize>,
        finalizing: Arc<AtomicUsize>,
    }

    impl RequestLifecycleObserver for CountingObserver {
        fn on_plugins_resolved(&self, _plugins: &[ResolvedPlugin], _ctx: &mut RequestCtx) {
            self.resolved.fetch_add(1, Ordering::SeqCst);
        }

        fn on_request_finalizing(&self, _plugins: &[ResolvedPlugin], _ctx: &mut RequestCtx) {
            self.finalizing.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Clone, Copy)]
    enum AsyncHookBehavior {
        Success,
        Error,
        Panic,
        Timeout,
    }

    struct TestDispatchHook {
        name: &'static str,
        compensation_domain: &'static str,
        behavior: AsyncHookBehavior,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl RequestDispatchHook for TestDispatchHook {
        fn name(&self) -> &'static str {
            self.name
        }

        fn compensation_domain(&self) -> &'static str {
            self.compensation_domain
        }

        fn failure_policy(&self) -> DispatchFailurePolicy {
            DispatchFailurePolicy::FailClosed(DispatchFailureResponse::new(
                503,
                "test_dispatch_failed",
                "test dispatch failed",
            ))
        }

        async fn before_upstream_dispatch(
            &self,
            _plugins: &[ResolvedPlugin],
            _ctx: &mut RequestCtx,
        ) -> Result<(), LifecycleHookError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.behavior {
                AsyncHookBehavior::Success => Ok(()),
                AsyncHookBehavior::Error => {
                    Err(LifecycleHookError::new("test_error", "test failure"))
                }
                AsyncHookBehavior::Panic => panic!("test dispatch panic"),
                AsyncHookBehavior::Timeout => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok(())
                }
            }
        }
    }

    struct TestAbortHandler {
        compensation_domain: &'static str,
        calls: Arc<AtomicUsize>,
        saw_forbidden: Arc<AtomicBool>,
    }

    #[async_trait]
    impl RequestDispatchAbortHandler for TestAbortHandler {
        fn name(&self) -> &'static str {
            "test-abort"
        }

        fn compensation_domain(&self) -> &'static str {
            self.compensation_domain
        }

        async fn compensate_before_response(
            &self,
            ctx: &mut RequestCtx,
            _cause: DispatchAbortCause,
        ) -> Result<(), LifecycleHookError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.saw_forbidden
                .store(ctx.is_upstream_dispatch_forbidden(), Ordering::SeqCst);
            Ok(())
        }
    }

    struct OverwritingAbortHandler;

    #[async_trait]
    impl RequestDispatchAbortHandler for OverwritingAbortHandler {
        fn name(&self) -> &'static str {
            "overwriting-abort"
        }

        fn compensation_domain(&self) -> &'static str {
            "test"
        }

        async fn compensate_before_response(
            &self,
            ctx: &mut RequestCtx,
            _cause: DispatchAbortCause,
        ) -> Result<(), LifecycleHookError> {
            ctx.short_circuited = true;
            ctx.exit_status = Some(503);
            ctx.exit_body = Some(r#"{"error":{"code":"secondary_failure"}}"#.to_string());
            Ok(())
        }
    }

    struct TestFinalizer {
        name: &'static str,
        behavior: AsyncHookBehavior,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl RequestFinalizer for TestFinalizer {
        fn name(&self) -> &'static str {
            self.name
        }

        async fn finalize(
            &self,
            _plugins: &[ResolvedPlugin],
            _ctx: &mut RequestCtx,
        ) -> Result<(), LifecycleHookError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.behavior {
                AsyncHookBehavior::Success => Ok(()),
                AsyncHookBehavior::Error => {
                    Err(LifecycleHookError::new("test_error", "test failure"))
                }
                AsyncHookBehavior::Panic => panic!("test finalizer panic"),
                AsyncHookBehavior::Timeout => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok(())
                }
            }
        }
    }

    #[test]
    fn lifecycle_observers_are_optional_and_invoked_synchronously() {
        let resolved = Arc::new(AtomicUsize::new(0));
        let finalizing = Arc::new(AtomicUsize::new(0));
        let proxy = test_proxy().with_lifecycle_observers(vec![Arc::new(CountingObserver {
            resolved: Arc::clone(&resolved),
            finalizing: Arc::clone(&finalizing),
        })]);
        let mut ctx = RequestCtx::new();

        proxy.notify_plugins_resolved(&[], &mut ctx);
        proxy.notify_request_finalizing(&[], &mut ctx);

        assert_eq!(resolved.load(Ordering::SeqCst), 1);
        assert_eq!(finalizing.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn critical_dispatch_hook_requires_an_independent_abort_handler() {
        let hooks: Vec<Arc<dyn RequestDispatchHook>> = vec![Arc::new(TestDispatchHook {
            name: "critical",
            compensation_domain: "test",
            behavior: AsyncHookBehavior::Success,
            calls: Arc::new(AtomicUsize::new(0)),
        })];

        let result = test_proxy().try_with_async_lifecycle_hooks(
            hooks,
            Vec::new(),
            Vec::new(),
            LifecycleHookTimeouts::default(),
        );

        assert!(matches!(
            result,
            Err("critical dispatch hook 必须配套同域独立 abort handler")
        ));
    }

    #[test]
    fn budget_dispatch_hook_rejects_quota_only_abort_handler() {
        let hooks: Vec<Arc<dyn RequestDispatchHook>> = vec![Arc::new(TestDispatchHook {
            name: "ai-budget-dispatch",
            compensation_domain: "ai-budget",
            behavior: AsyncHookBehavior::Success,
            calls: Arc::new(AtomicUsize::new(0)),
        })];
        let handlers: Vec<Arc<dyn RequestDispatchAbortHandler>> =
            vec![Arc::new(TestAbortHandler {
                compensation_domain: "ai-quota",
                calls: Arc::new(AtomicUsize::new(0)),
                saw_forbidden: Arc::new(AtomicBool::new(false)),
            })];

        let result = test_proxy().try_with_async_lifecycle_hooks(
            hooks,
            handlers,
            Vec::new(),
            LifecycleHookTimeouts::default(),
        );

        assert!(matches!(
            result,
            Err("critical dispatch hook 必须配套同域独立 abort handler")
        ));
    }

    #[test]
    fn budget_dispatch_hook_accepts_matching_abort_handler() {
        let hooks: Vec<Arc<dyn RequestDispatchHook>> = vec![Arc::new(TestDispatchHook {
            name: "ai-budget-dispatch",
            compensation_domain: "ai-budget",
            behavior: AsyncHookBehavior::Success,
            calls: Arc::new(AtomicUsize::new(0)),
        })];
        let handlers: Vec<Arc<dyn RequestDispatchAbortHandler>> =
            vec![Arc::new(TestAbortHandler {
                compensation_domain: "ai-budget",
                calls: Arc::new(AtomicUsize::new(0)),
                saw_forbidden: Arc::new(AtomicBool::new(false)),
            })];

        let result = test_proxy().try_with_async_lifecycle_hooks(
            hooks,
            handlers,
            Vec::new(),
            LifecycleHookTimeouts::default(),
        );

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn critical_dispatch_failures_are_isolated_and_compensated_before_response() {
        let calls = Arc::new(AtomicUsize::new(0));
        let compensation_calls = Arc::new(AtomicUsize::new(0));
        let saw_forbidden = Arc::new(AtomicBool::new(false));
        let hooks: Vec<Arc<dyn RequestDispatchHook>> = vec![
            Arc::new(TestDispatchHook {
                name: "error",
                compensation_domain: "test",
                behavior: AsyncHookBehavior::Error,
                calls: Arc::clone(&calls),
            }),
            Arc::new(TestDispatchHook {
                name: "timeout",
                compensation_domain: "test",
                behavior: AsyncHookBehavior::Timeout,
                calls: Arc::clone(&calls),
            }),
            Arc::new(TestDispatchHook {
                name: "panic",
                compensation_domain: "test",
                behavior: AsyncHookBehavior::Panic,
                calls: Arc::clone(&calls),
            }),
        ];
        let proxy = test_proxy().with_async_lifecycle_hooks(
            hooks,
            vec![Arc::new(TestAbortHandler {
                compensation_domain: "test",
                calls: Arc::clone(&compensation_calls),
                saw_forbidden: Arc::clone(&saw_forbidden),
            })],
            Vec::new(),
            LifecycleHookTimeouts {
                dispatch: Duration::from_millis(5),
                abort_compensation: Duration::from_millis(5),
                finalizer: Duration::from_millis(5),
            },
        );
        let mut ctx = RequestCtx::new();

        assert!(!proxy.run_dispatch_hooks(&[], &mut ctx).await);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(compensation_calls.load(Ordering::SeqCst), 1);
        assert!(saw_forbidden.load(Ordering::SeqCst));
        assert!(ctx.is_upstream_dispatch_forbidden());
        assert_eq!(ctx.exit_status, Some(503));
        assert!(ctx
            .exit_body
            .as_deref()
            .is_some_and(|body| body.contains("test_dispatch_failed")));
    }

    #[tokio::test]
    async fn dispatch_compensation_preserves_anthropic_root_failure() {
        let hooks: Vec<Arc<dyn RequestDispatchHook>> = vec![Arc::new(TestDispatchHook {
            name: "panic",
            compensation_domain: "test",
            behavior: AsyncHookBehavior::Panic,
            calls: Arc::new(AtomicUsize::new(0)),
        })];
        let proxy = test_proxy().with_async_lifecycle_hooks(
            hooks,
            vec![Arc::new(OverwritingAbortHandler)],
            Vec::new(),
            LifecycleHookTimeouts {
                dispatch: Duration::from_millis(5),
                abort_compensation: Duration::from_millis(5),
                finalizer: Duration::from_millis(5),
            },
        );
        let mut ctx = RequestCtx::new();
        ctx.extensions
            .insert(DispatchFailureResponseFormat::Anthropic);

        assert!(!proxy.run_dispatch_hooks(&[], &mut ctx).await);
        assert_eq!(ctx.exit_status, Some(503));
        let body: serde_json::Value =
            serde_json::from_str(ctx.exit_body.as_deref().unwrap()).unwrap();
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "test_dispatch_failed");
        assert_eq!(body["error"]["message"], "test dispatch failed");
        assert_eq!(body["request_id"], ctx.lifecycle.request_id);
        assert!(!ctx
            .exit_body
            .as_deref()
            .unwrap()
            .contains("secondary_failure"));
    }

    #[tokio::test]
    async fn finalizer_timeout_and_panic_do_not_block_later_finalizers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let finalizers: Vec<Arc<dyn RequestFinalizer>> = vec![
            Arc::new(TestFinalizer {
                name: "timeout",
                behavior: AsyncHookBehavior::Timeout,
                calls: Arc::clone(&calls),
            }),
            Arc::new(TestFinalizer {
                name: "panic",
                behavior: AsyncHookBehavior::Panic,
                calls: Arc::clone(&calls),
            }),
            Arc::new(TestFinalizer {
                name: "success",
                behavior: AsyncHookBehavior::Success,
                calls: Arc::clone(&calls),
            }),
        ];
        let proxy = test_proxy().with_async_lifecycle_hooks(
            Vec::new(),
            Vec::new(),
            finalizers,
            LifecycleHookTimeouts {
                dispatch: Duration::from_millis(5),
                abort_compensation: Duration::from_millis(5),
                finalizer: Duration::from_millis(5),
            },
        );
        let mut ctx = RequestCtx::new();

        proxy.run_request_finalizers(&[], &mut ctx).await;

        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn route_match_and_workspace_snapshot_are_updated_atomically() {
        let proxy = Arc::new(test_proxy());
        let route_id = uuid::Uuid::new_v4();
        let workspace_a = uuid::Uuid::new_v4();
        let workspace_b = uuid::Uuid::new_v4();
        let route = move |name: &str, workspace_id| Route {
            id: route_id,
            name: Some(name.to_string()),
            paths: Some(vec!["/ai".to_string()]),
            ws_id: Some(workspace_id),
            ..Route::default()
        };
        proxy.update_routes(&[route("a", workspace_a)]);
        let request = RequestContext {
            method: "GET".to_string(),
            uri: "/ai".to_string(),
            host: "localhost".to_string(),
            scheme: "http".to_string(),
            ..RequestContext::default()
        };
        let barrier = Arc::new(Barrier::new(2));
        let updater_proxy = Arc::clone(&proxy);
        let updater_barrier = Arc::clone(&barrier);
        let updater = std::thread::spawn(move || {
            updater_barrier.wait();
            for index in 0..5_000 {
                let updated = if index % 2 == 0 {
                    route("a", workspace_a)
                } else {
                    route("b", workspace_b)
                };
                updater_proxy.update_routes(&[updated]);
            }
        });

        barrier.wait();
        for _ in 0..5_000 {
            let (matched, snapshot) = proxy.find_route_with_snapshot(&request).unwrap();
            let snapshot = snapshot.unwrap();
            assert_eq!(matched.route_name.as_deref(), snapshot.name.as_deref());
            match snapshot.name.as_deref() {
                Some("a") => assert_eq!(snapshot.ws_id, Some(workspace_a)),
                Some("b") => assert_eq!(snapshot.ws_id, Some(workspace_b)),
                value => panic!("意外的 Route 快照名称: {value:?}"),
            }
            std::thread::yield_now();
        }
        updater.join().unwrap();
    }

    #[test]
    fn pingora_transport_error_is_mapped_without_message_matching() {
        let downstream = Error::new_down(ErrorType::ConnectionClosed);
        let upstream = Error::new_up(ErrorType::ConnectTimedout);
        let misleading_message = Error::new_str("timeout");

        assert_eq!(
            KongProxy::transport_error_from_pingora(&downstream),
            RequestTransportError::downstream(RequestTransportErrorKind::ConnectionClosed,)
        );
        assert_eq!(
            KongProxy::transport_error_from_pingora(&upstream),
            RequestTransportError::upstream(RequestTransportErrorKind::ConnectTimeout,)
        );
        assert_eq!(KongProxy::proxy_failure_status(&upstream), 504);
        assert_eq!(KongProxy::proxy_failure_status(&downstream), 502);
        assert_eq!(KongProxy::proxy_failure_status(&misleading_message), 502);
    }

    #[test]
    fn plugin_header_insertion_keeps_pingora_case_map_in_sync() {
        let mut request = RequestHeader::build("GET", b"/", Some(2)).unwrap();
        request.insert_header("X-Existing", "present").unwrap();

        set_upstream_header(
            &mut request,
            "Authorization".to_string(),
            "Bearer test-placeholder",
        );

        assert_eq!(
            request.headers.get("authorization").unwrap(),
            "Bearer test-placeholder"
        );
        assert_eq!(
            request.case_header_iter().count(),
            request.headers.iter().count()
        );

        let mut wire = Vec::new();
        request.header_to_h1_wire(&mut wire);
        let wire = String::from_utf8(wire).unwrap();
        assert!(wire.contains("Authorization: Bearer test-placeholder\r\n"));
    }

    #[test]
    fn response_header_mutations_keep_pingora_case_map_in_sync() {
        let mut response = ResponseHeader::build(200, Some(4)).unwrap();
        response.insert_header("Content-Length", "10").unwrap();
        response.insert_header("Connection", "close").unwrap();

        response.remove_header("content-length");
        apply_proxy_response_headers(&mut response, &["X-Proxy-Test: present".to_string()]);

        assert_eq!(
            response.case_header_iter().count(),
            response.headers.iter().count()
        );

        let mut wire = Vec::new();
        response.header_to_h1_wire(&mut wire);
        let wire = String::from_utf8(wire).unwrap();
        assert!(!wire.to_ascii_lowercase().contains("content-length:"));
        assert!(wire.contains("X-Proxy-Test: present\r\n"));
    }
}

/// Check if a client IP is within a CIDR range — 检查客户端 IP 是否在 CIDR 范围内
fn cidr_contains(cidr: &str, client_ip: &str) -> bool {
    let Some((net_str, prefix_str)) = cidr.split_once('/') else {
        return false;
    };
    let Ok(prefix_len) = prefix_str.parse::<u8>() else {
        return false;
    };
    let Ok(net_ip) = net_str.parse::<std::net::IpAddr>() else {
        return false;
    };
    let Ok(client) = client_ip.parse::<std::net::IpAddr>() else {
        return false;
    };
    match (net_ip, client) {
        (std::net::IpAddr::V4(net), std::net::IpAddr::V4(cli)) => {
            let mask = if prefix_len >= 32 {
                u32::MAX
            } else {
                u32::MAX << (32 - prefix_len)
            };
            (u32::from(net) & mask) == (u32::from(cli) & mask)
        }
        (std::net::IpAddr::V6(net), std::net::IpAddr::V6(cli)) => {
            let mask = if prefix_len >= 128 {
                u128::MAX
            } else {
                u128::MAX << (128 - prefix_len)
            };
            (u128::from(net) & mask) == (u128::from(cli) & mask)
        }
        _ => false, // v4 vs v6 mismatch — IPv4 与 IPv6 不匹配
    }
}

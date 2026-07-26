use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::time::Instant;
use uuid::Uuid;

use crate::error::Result;

/// 网关请求生命周期中的框架阶段。
///
/// 插件阶段继续使用 [`Phase`]；该枚举补充路由、请求体、上游等非插件阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecyclePhase {
    Initialization,
    Certificate,
    Routing,
    Service,
    RequestBody,
    Rewrite,
    Access,
    Upstream,
    Response,
    HeaderFilter,
    BodyFilter,
    Logging,
    Internal,
}

impl From<Phase> for LifecyclePhase {
    fn from(phase: Phase) -> Self {
        match phase {
            Phase::InitWorker => Self::Initialization,
            Phase::Certificate => Self::Certificate,
            Phase::Rewrite => Self::Rewrite,
            Phase::Access => Self::Access,
            Phase::Response => Self::Response,
            Phase::HeaderFilter => Self::HeaderFilter,
            Phase::BodyFilter => Self::BodyFilter,
            Phase::Log => Self::Logging,
        }
    }
}

/// 请求结束原因的结构化提示。
///
/// 这里只保存分类和组件名，不携带请求正文、上游响应或完整错误文本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestTerminationHint {
    PolicyRejected {
        phase: Phase,
        plugin: String,
    },
    GatewayError {
        phase: LifecyclePhase,
        component: String,
    },
    UpstreamSemanticError {
        provider_type: Option<String>,
    },
}

impl RequestTerminationHint {
    fn priority(&self) -> u8 {
        match self {
            Self::PolicyRejected { .. } => 3,
            Self::GatewayError { .. } => 2,
            Self::UpstreamSemanticError { .. } => 1,
        }
    }
}

/// 传输错误发生的一侧。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestTransportSource {
    Upstream,
    Downstream,
    Internal,
    Unknown,
}

impl RequestTransportSource {
    fn priority(self) -> u8 {
        match self {
            Self::Downstream => 4,
            Self::Upstream => 3,
            Self::Internal => 2,
            Self::Unknown => 1,
        }
    }
}

/// 与具体代理实现解耦的传输错误类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestTransportErrorKind {
    ConnectTimeout,
    ConnectRefused,
    ConnectNoRoute,
    Tls,
    Connect,
    Bind,
    Accept,
    Socket,
    Protocol,
    Read,
    Write,
    ReadTimeout,
    WriteTimeout,
    ConnectionClosed,
    HttpStatus(u16),
    File,
    Internal,
    Unknown,
    Custom,
}

/// 请求级结构化传输错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestTransportError {
    pub source: RequestTransportSource,
    pub kind: RequestTransportErrorKind,
}

impl RequestTransportError {
    pub const fn new(source: RequestTransportSource, kind: RequestTransportErrorKind) -> Self {
        Self { source, kind }
    }

    pub const fn upstream(kind: RequestTransportErrorKind) -> Self {
        Self::new(RequestTransportSource::Upstream, kind)
    }

    pub const fn downstream(kind: RequestTransportErrorKind) -> Self {
        Self::new(RequestTransportSource::Downstream, kind)
    }

    pub const fn internal(kind: RequestTransportErrorKind) -> Self {
        Self::new(RequestTransportSource::Internal, kind)
    }

    pub const fn unknown(kind: RequestTransportErrorKind) -> Self {
        Self::new(RequestTransportSource::Unknown, kind)
    }

    pub const fn is_upstream(self) -> bool {
        matches!(self.source, RequestTransportSource::Upstream)
    }

    pub const fn is_downstream(self) -> bool {
        matches!(self.source, RequestTransportSource::Downstream)
    }
}

/// 从接收请求到最终日志回调的通用生命周期。
#[derive(Debug, Clone)]
pub struct RequestLifecycle {
    pub request_id: String,
    pub started_at: DateTime<Utc>,
    pub started_mono: Instant,
    pub finished_at: Option<DateTime<Utc>>,
    pub final_status: Option<u16>,
    pub upstream_status: Option<u16>,
    pub upstream_attempted: bool,
    pub upstream_response_started: bool,
    pub downstream_send_attempted: bool,
    pub downstream_response_completed: bool,
    pub termination_hint: Option<RequestTerminationHint>,
    pub transport_error: Option<RequestTransportError>,
}

impl RequestLifecycle {
    pub fn new() -> Self {
        Self {
            request_id: Uuid::new_v4().simple().to_string(),
            started_at: Utc::now(),
            started_mono: Instant::now(),
            finished_at: None,
            final_status: None,
            upstream_status: None,
            upstream_attempted: false,
            upstream_response_started: false,
            downstream_send_attempted: false,
            downstream_response_completed: false,
            termination_hint: None,
            transport_error: None,
        }
    }

    pub fn mark_policy_rejected(&mut self, phase: Phase, plugin: impl Into<String>) {
        self.set_termination_hint(RequestTerminationHint::PolicyRejected {
            phase,
            plugin: plugin.into(),
        });
    }

    pub fn mark_gateway_error(&mut self, phase: LifecyclePhase, component: impl Into<String>) {
        self.set_termination_hint(RequestTerminationHint::GatewayError {
            phase,
            component: component.into(),
        });
    }

    pub fn mark_upstream_semantic_error(&mut self, provider_type: Option<String>) {
        self.set_termination_hint(RequestTerminationHint::UpstreamSemanticError { provider_type });
    }

    fn set_termination_hint(&mut self, hint: RequestTerminationHint) {
        let should_replace = self
            .termination_hint
            .as_ref()
            .map(|current| hint.priority() > current.priority())
            .unwrap_or(true);
        if should_replace {
            self.termination_hint = Some(hint);
        }
    }

    pub fn mark_upstream_attempted(&mut self) {
        self.upstream_attempted = true;
    }

    pub fn mark_upstream_started(&mut self) {
        self.upstream_attempted = true;
        self.upstream_response_started = true;
    }

    pub fn mark_upstream_status(&mut self, status: u16) {
        self.mark_upstream_started();
        self.upstream_status = Some(status);
    }

    pub fn mark_downstream_send_attempted(&mut self) {
        self.downstream_send_attempted = true;
    }

    pub fn mark_downstream_completed(&mut self) {
        self.downstream_send_attempted = true;
        self.downstream_response_completed = true;
    }

    pub fn mark_transport_error(&mut self, error: RequestTransportError) {
        let should_replace = self
            .transport_error
            .map(|current| error.source.priority() > current.source.priority())
            .unwrap_or(true);
        if should_replace {
            self.transport_error = Some(error);
        }
    }

    /// 在最终日志回调中收口 wall clock 与最终状态。
    pub fn finish(&mut self, final_status: Option<u16>) {
        if self.finished_at.is_some() {
            return;
        }

        self.final_status = final_status;
        let elapsed =
            ChronoDuration::from_std(self.started_mono.elapsed()).unwrap_or(ChronoDuration::MAX);
        self.finished_at = Some(
            self.started_at
                .checked_add_signed(elapsed)
                .unwrap_or(DateTime::<Utc>::MAX_UTC),
        );
    }
}

impl Default for RequestLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

/// Request context — passed throughout the request lifecycle for plugins to read and write — 请求上下文 — 在整个请求生命周期中传递，供插件读写
pub struct RequestCtx {
    /// Request lifecycle shared by proxy and observers — 代理与观察器共享的请求生命周期
    pub lifecycle: RequestLifecycle,
    /// Matched route ID — 匹配的路由 ID
    pub route_id: Option<uuid::Uuid>,
    /// Matched route name snapshot — 匹配时的 Route 名称快照
    pub route_name: Option<String>,
    /// Matched service ID — 匹配的服务 ID
    pub service_id: Option<uuid::Uuid>,
    /// Matched service name snapshot — 匹配时的 Service 名称快照
    pub service_name: Option<String>,
    /// Matched workspace snapshot — 匹配时的 Workspace 快照
    pub workspace_id: Option<uuid::Uuid>,
    /// Matched consumer ID — 匹配的消费者 ID
    pub consumer_id: Option<uuid::Uuid>,
    /// Request-level shared data (corresponds to kong.ctx.shared) — 请求级别的共享数据（对应 kong.ctx.shared）
    pub shared: std::collections::HashMap<String, serde_json::Value>,
    /// Whether a plugin has short-circuited the request (e.g. kong.response.exit) — 是否已经由某个插件短路（如 kong.response.exit）
    pub short_circuited: bool,
    /// Status code when short-circuited — 短路时的状态码
    pub exit_status: Option<u16>,
    /// Response body when short-circuited — 短路时的响应体
    pub exit_body: Option<String>,
    /// Response headers when short-circuited — 短路时的响应头
    pub exit_headers: Option<std::collections::HashMap<String, String>>,
    /// Upstream request header modification queue — 上游请求头修改队列
    pub upstream_headers_to_set: Vec<(String, String)>,
    /// Upstream request header removal queue — 上游请求头删除队列
    pub upstream_headers_to_remove: Vec<String>,
    /// Upstream query string replacement staged by plugins — 插件暂存的上游查询参数替换
    pub upstream_query_to_set: Option<std::collections::HashMap<String, String>>,
    /// Upstream request body replacement staged by plugins — 插件暂存的上游请求体替换
    pub upstream_body: Option<String>,
    /// Upstream request path override staged by plugins — 插件暂存的上游请求路径覆写
    pub upstream_path: Option<String>,
    /// Upstream request scheme override staged by plugins — 插件暂存的上游请求 scheme 覆写
    pub upstream_scheme: Option<String>,
    /// Upstream target host override staged by plugins — 插件暂存的上游目标主机覆写
    pub upstream_target_host: Option<String>,
    /// Upstream target port override staged by plugins — 插件暂存的上游目标端口覆写
    pub upstream_target_port: Option<u16>,
    /// Whether request buffering was explicitly enabled by the plugin — 插件是否显式开启了请求缓冲
    pub request_buffering_enabled: bool,
    /// Whether to force HTTP/1.1 for upstream connection (avoid H2 multiplexing issues) — 是否强制上游使用 HTTP/1.1（避免 H2 多路复用问题）
    pub upstream_force_http1: bool,
    /// Whether a retry callback was registered by the plugin — 插件是否注册了重试回调
    pub upstream_retry_callback_registered: bool,
    /// 上游派发是否已被生命周期 hook 不可逆地禁止。
    ///
    /// 一旦置为 `true`，代理不得再进行 DNS、建连或写入上游。该状态独立于
    /// `short_circuited`，便于先完成补偿，再统一生成短路响应。
    pub upstream_dispatch_forbidden: bool,
    /// Response header modification queue — 响应头修改队列
    pub response_headers_to_set: Vec<(String, String)>,
    /// Response header removal queue — 响应头删除队列
    pub response_headers_to_remove: Vec<String>,
    /// Authenticated credential info — 认证后的凭据信息
    pub authenticated_credential: Option<serde_json::Value>,
    /// Authenticated consumer info — 认证后的消费者信息
    pub authenticated_consumer: Option<serde_json::Value>,

    // ====== Request snapshot fields (used by PDK) — 请求快照字段（PDK 使用） ======
    /// Request method — 请求方法
    pub request_method: String,
    /// Request path — 请求路径
    pub request_path: String,
    /// Request scheme (http/https) — 请求 scheme（http/https）
    pub request_scheme: String,
    /// Request host — 请求 host
    pub request_host: String,
    /// Request port — 请求端口
    pub request_port: u16,
    /// Request headers snapshot — 请求头快照
    pub request_headers: std::collections::HashMap<String, String>,
    /// Client IP — 客户端 IP
    pub client_ip: String,
    /// Query string — 查询字符串
    pub request_query_string: String,
    /// Raw request body snapshot used by Lua plugins — 供 Lua 插件读取的原始请求体快照
    pub request_body: Option<String>,
    /// Upstream response status code (available in header_filter/log phases) — 上游响应状态码（header_filter/log 阶段可用）
    pub response_status: Option<u16>,
    /// Upstream response headers — 上游响应头
    pub response_headers: std::collections::HashMap<String, String>,
    /// Buffered upstream response body for PDK helpers such as kong.service.response.get_raw_body(). — 供 PDK 辅助接口（如 kong.service.response.get_raw_body()）读取的缓冲上游响应体。
    pub service_response_body: Option<String>,
    /// Optional payload returned by kong.log.serialize() for Lua plugins that
    /// depend on the Kong logging schema.
    pub log_serialize: Option<serde_json::Value>,
    /// Response source reported by kong.response.get_source()
    pub response_source: Option<String>,
    /// Matched route JSON for kong.router.get_route() — 匹配的路由 JSON，用于 kong.router.get_route()
    pub matched_route_json: Option<serde_json::Value>,
    /// URI captures from route matching — 路由匹配的 URI 捕获
    pub uri_captures_named: std::collections::HashMap<String, String>,
    /// Unnamed URI captures (positional) — 未命名的 URI 捕获（按位置）
    pub uri_captures_unnamed: Vec<String>,
    /// Typed extension map for plugins to share typed data (e.g. AI context) — 供插件共享类型化数据的扩展 map（如 AI 上下文）
    pub extensions: anymap2::SendSyncAnyMap,
}

impl RequestCtx {
    /// Create a new request context — 创建新的请求上下文
    pub fn new() -> Self {
        Self {
            lifecycle: RequestLifecycle::new(),
            route_id: None,
            route_name: None,
            service_id: None,
            service_name: None,
            workspace_id: None,
            consumer_id: None,
            shared: std::collections::HashMap::new(),
            short_circuited: false,
            exit_status: None,
            exit_body: None,
            exit_headers: None,
            upstream_headers_to_set: Vec::new(),
            upstream_headers_to_remove: Vec::new(),
            upstream_query_to_set: None,
            upstream_body: None,
            upstream_path: None,
            upstream_scheme: None,
            upstream_target_host: None,
            upstream_target_port: None,
            request_buffering_enabled: false,
            upstream_force_http1: false,
            upstream_retry_callback_registered: false,
            upstream_dispatch_forbidden: false,
            response_headers_to_set: Vec::new(),
            response_headers_to_remove: Vec::new(),
            authenticated_credential: None,
            authenticated_consumer: None,
            request_method: String::new(),
            request_path: String::new(),
            request_scheme: String::new(),
            request_host: String::new(),
            request_port: 0,
            request_headers: std::collections::HashMap::new(),
            client_ip: String::new(),
            request_query_string: String::new(),
            request_body: None,
            response_status: None,
            response_headers: std::collections::HashMap::new(),
            service_response_body: None,
            log_serialize: None,
            response_source: None,
            matched_route_json: None,
            uri_captures_named: std::collections::HashMap::new(),
            uri_captures_unnamed: Vec::new(),
            extensions: anymap2::SendSyncAnyMap::new(),
        }
    }

    /// Check if the request has been short-circuited — 检查是否已短路
    pub fn is_short_circuited(&self) -> bool {
        self.short_circuited
    }

    /// 不可逆地禁止本请求派发到上游。
    pub fn forbid_upstream_dispatch(&mut self) {
        self.upstream_dispatch_forbidden = true;
    }

    /// 返回本请求是否仍允许派发到上游。
    pub fn is_upstream_dispatch_forbidden(&self) -> bool {
        self.upstream_dispatch_forbidden
    }
}

impl Default for RequestCtx {
    fn default() -> Self {
        Self::new()
    }
}

/// Plugin configuration — parsed from the database Plugin.config field — 插件配置 — 从数据库 Plugin.config 字段解析
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginConfig {
    /// Plugin name — 插件名称
    pub name: String,
    /// Plugin configuration JSON — 插件配置 JSON
    pub config: serde_json::Value,
}

/// Plugin execution phase — 插件执行阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Worker process initialization — Worker 进程初始化
    InitWorker,
    /// TLS certificate selection phase — TLS 证书选择阶段
    Certificate,
    /// Request rewrite phase — 请求重写阶段
    Rewrite,
    /// Access control phase (most commonly used) — 访问控制阶段（最常用）
    Access,
    /// Response processing phase (handles both headers and body) — 响应处理阶段（同时处理头和体）
    Response,
    /// Response header filter phase — 响应头过滤阶段
    HeaderFilter,
    /// Response body filter phase — 响应体过滤阶段
    BodyFilter,
    /// Log phase (after request completion) — 日志阶段（请求完成后）
    Log,
}

/// Plugin lifecycle trait — all plugins (native Rust or Lua) must implement this — 插件生命周期 trait — 所有插件（Rust 原生或 Lua）必须实现
#[async_trait]
pub trait PluginHandler: Send + Sync {
    /// Plugin priority (higher number executes first, consistent with Kong) — 插件优先级（数字越大越先执行，与 Kong 一致）
    fn priority(&self) -> i32;

    /// Plugin version — 插件版本
    fn version(&self) -> &str;

    /// Plugin name — 插件名称
    fn name(&self) -> &str;

    /// Whether the plugin implements the body_filter phase. — 插件是否实现了 body_filter 阶段。
    fn has_body_filter(&self) -> bool {
        false
    }

    /// Worker process initialization — Worker 进程初始化
    async fn init_worker(&self, _config: &PluginConfig) -> Result<()> {
        Ok(())
    }

    /// TLS certificate selection phase — TLS 证书选择阶段
    async fn certificate(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        Ok(())
    }

    /// Request rewrite phase — 请求重写阶段
    async fn rewrite(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        Ok(())
    }

    /// Access control phase — 访问控制阶段
    async fn access(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        Ok(())
    }

    /// Response processing phase (header + body combined) — 响应处理阶段（header + body 一起处理）
    async fn response(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        Ok(())
    }

    /// Response header filter phase — 响应头过滤阶段
    async fn header_filter(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        Ok(())
    }

    /// Response body filter phase — 响应体过滤阶段
    async fn body_filter(
        &self,
        _config: &PluginConfig,
        _ctx: &mut RequestCtx,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
    ) -> Result<()> {
        Ok(())
    }

    /// Log phase (always executes after request completion) — 日志阶段（请求完成后，总是执行）
    async fn log(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        Ok(())
    }
}

/// Plugin factory trait — used to create plugin instances — 插件工厂 trait — 用于创建插件实例
pub trait PluginFactory: Send + Sync {
    /// Create a plugin handler instance — 创建插件 handler 实例
    fn create(&self) -> Box<dyn PluginHandler>;
    /// Plugin name — 插件名称
    fn name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::{
        LifecyclePhase, Phase, RequestLifecycle, RequestTerminationHint, RequestTransportError,
        RequestTransportErrorKind,
    };

    #[test]
    fn lifecycle_uses_one_lowercase_simple_uuid() {
        let lifecycle = RequestLifecycle::new();

        assert_eq!(lifecycle.request_id.len(), 32);
        assert!(lifecycle
            .request_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    }

    #[test]
    fn lifecycle_finish_is_idempotent_and_uses_monotonic_elapsed_time() {
        let mut lifecycle = RequestLifecycle::new();
        lifecycle.finish(Some(204));
        let finished_at = lifecycle.finished_at;

        lifecycle.finish(Some(500));

        assert_eq!(lifecycle.final_status, Some(204));
        assert_eq!(lifecycle.finished_at, finished_at);
        assert!(lifecycle.finished_at.unwrap() >= lifecycle.started_at);
    }

    #[test]
    fn lifecycle_finish_does_not_fill_status_after_finalization() {
        let mut lifecycle = RequestLifecycle::new();
        lifecycle.finish(None);
        let finished_at = lifecycle.finished_at;

        lifecycle.finish(Some(200));

        assert_eq!(lifecycle.final_status, None);
        assert_eq!(lifecycle.finished_at, finished_at);
    }

    #[test]
    fn stronger_termination_hint_is_preserved() {
        let mut lifecycle = RequestLifecycle::new();
        lifecycle.mark_upstream_semantic_error(Some("openai".to_string()));
        lifecycle.mark_gateway_error(LifecyclePhase::BodyFilter, "response_transform");
        lifecycle.mark_policy_rejected(Phase::Access, "ai-key-auth");
        lifecycle.mark_gateway_error(LifecyclePhase::Internal, "late_error");

        assert_eq!(
            lifecycle.termination_hint,
            Some(RequestTerminationHint::PolicyRejected {
                phase: Phase::Access,
                plugin: "ai-key-auth".to_string(),
            })
        );
    }

    #[test]
    fn downstream_transport_error_overrides_upstream_error() {
        let mut lifecycle = RequestLifecycle::new();
        lifecycle.mark_transport_error(RequestTransportError::upstream(
            RequestTransportErrorKind::ConnectTimeout,
        ));
        lifecycle.mark_transport_error(RequestTransportError::downstream(
            RequestTransportErrorKind::ConnectionClosed,
        ));
        lifecycle.mark_transport_error(RequestTransportError::upstream(
            RequestTransportErrorKind::Read,
        ));

        assert_eq!(
            lifecycle.transport_error,
            Some(RequestTransportError::downstream(
                RequestTransportErrorKind::ConnectionClosed,
            ))
        );
    }

    #[test]
    fn upstream_status_implies_attempt_and_response_start() {
        let mut lifecycle = RequestLifecycle::new();
        lifecycle.mark_upstream_status(429);

        assert!(lifecycle.upstream_attempted);
        assert!(lifecycle.upstream_response_started);
        assert_eq!(lifecycle.upstream_status, Some(429));
    }
}

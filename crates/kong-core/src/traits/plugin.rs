use async_trait::async_trait;
use bytes::Bytes;

use crate::error::Result;

/// Request context — passed throughout the request lifecycle for plugins to read and write — 请求上下文 — 在整个请求生命周期中传递，供插件读写
pub struct RequestCtx {
    /// Matched route ID — 匹配的路由 ID
    pub route_id: Option<uuid::Uuid>,
    /// Matched service ID — 匹配的服务 ID
    pub service_id: Option<uuid::Uuid>,
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
            route_id: None,
            service_id: None,
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

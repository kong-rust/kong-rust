use async_trait::async_trait;
use bytes::Bytes;

use crate::error::Result;

/// 请求上下文 — 在整个请求生命周期中传递，供插件读写
pub struct RequestCtx {
    /// 匹配的路由 ID
    pub route_id: Option<uuid::Uuid>,
    /// 匹配的服务 ID
    pub service_id: Option<uuid::Uuid>,
    /// 匹配的消费者 ID
    pub consumer_id: Option<uuid::Uuid>,
    /// 请求级别的共享数据（对应 kong.ctx.shared）
    pub shared: std::collections::HashMap<String, serde_json::Value>,
    /// 是否已经由某个插件短路（如 kong.response.exit）
    pub short_circuited: bool,
    /// 短路时的状态码
    pub exit_status: Option<u16>,
    /// 短路时的响应体
    pub exit_body: Option<String>,
    /// 短路时的响应头
    pub exit_headers: Option<std::collections::HashMap<String, String>>,
    /// 上游请求头修改队列
    pub upstream_headers_to_set: Vec<(String, String)>,
    /// 上游请求头删除队列
    pub upstream_headers_to_remove: Vec<String>,
    /// 响应头修改队列
    pub response_headers_to_set: Vec<(String, String)>,
    /// 响应头删除队列
    pub response_headers_to_remove: Vec<String>,
    /// 认证后的凭据信息
    pub authenticated_credential: Option<serde_json::Value>,
    /// 认证后的消费者信息
    pub authenticated_consumer: Option<serde_json::Value>,

    // ====== 请求快照字段（PDK 使用） ======

    /// 请求方法
    pub request_method: String,
    /// 请求路径
    pub request_path: String,
    /// 请求 scheme（http/https）
    pub request_scheme: String,
    /// 请求 host
    pub request_host: String,
    /// 请求端口
    pub request_port: u16,
    /// 请求头快照
    pub request_headers: std::collections::HashMap<String, String>,
    /// 客户端 IP
    pub client_ip: String,
    /// 查询字符串
    pub request_query_string: String,
    /// 上游响应状态码（header_filter/log 阶段可用）
    pub response_status: Option<u16>,
    /// 上游响应头
    pub response_headers: std::collections::HashMap<String, String>,
}

impl RequestCtx {
    /// 创建新的请求上下文
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
            response_status: None,
            response_headers: std::collections::HashMap::new(),
        }
    }

    /// 检查是否已短路
    pub fn is_short_circuited(&self) -> bool {
        self.short_circuited
    }
}

impl Default for RequestCtx {
    fn default() -> Self {
        Self::new()
    }
}

/// 插件配置 — 从数据库 Plugin.config 字段解析
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginConfig {
    /// 插件名称
    pub name: String,
    /// 插件配置 JSON
    pub config: serde_json::Value,
}

/// 插件执行阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Worker 进程初始化
    InitWorker,
    /// TLS 证书选择阶段
    Certificate,
    /// 请求重写阶段
    Rewrite,
    /// 访问控制阶段（最常用）
    Access,
    /// 响应处理阶段（同时处理头和体）
    Response,
    /// 响应头过滤阶段
    HeaderFilter,
    /// 响应体过滤阶段
    BodyFilter,
    /// 日志阶段（请求完成后）
    Log,
}

/// 插件生命周期 trait — 所有插件（Rust 原生或 Lua）必须实现
#[async_trait]
pub trait PluginHandler: Send + Sync {
    /// 插件优先级（数字越大越先执行，与 Kong 一致）
    fn priority(&self) -> i32;

    /// 插件版本
    fn version(&self) -> &str;

    /// 插件名称
    fn name(&self) -> &str;

    /// Worker 进程初始化
    async fn init_worker(&self, _config: &PluginConfig) -> Result<()> {
        Ok(())
    }

    /// TLS 证书选择阶段
    async fn certificate(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        Ok(())
    }

    /// 请求重写阶段
    async fn rewrite(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        Ok(())
    }

    /// 访问控制阶段
    async fn access(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        Ok(())
    }

    /// 响应处理阶段（header + body 一起处理）
    async fn response(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        Ok(())
    }

    /// 响应头过滤阶段
    async fn header_filter(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        Ok(())
    }

    /// 响应体过滤阶段
    async fn body_filter(
        &self,
        _config: &PluginConfig,
        _ctx: &mut RequestCtx,
        _body: &mut Bytes,
        _end_of_stream: bool,
    ) -> Result<()> {
        Ok(())
    }

    /// 日志阶段（请求完成后，总是执行）
    async fn log(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
        Ok(())
    }
}

/// 插件工厂 trait — 用于创建插件实例
pub trait PluginFactory: Send + Sync {
    /// 创建插件 handler 实例
    fn create(&self) -> Box<dyn PluginHandler>;
    /// 插件名称
    fn name(&self) -> &str;
}

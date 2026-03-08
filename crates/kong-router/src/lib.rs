//! Kong routing engine — Kong 路由引擎
//!
//! Supports two routing flavors — 支持两种路由风格:
//! - traditional / traditional_compatible: category-based matching on host/path/method/header/sni — 基于 host/path/method/header/sni 的分类匹配
//! - expressions: priority-based matching using ATC expression syntax — 基于 ATC 表达式语法的优先级匹配

pub mod expressions;
pub mod stream;
pub mod traditional;

use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use kong_core::models::{PathHandling, Route};

/// Request context — fields extracted from the HTTP request for matching — 请求上下文 — 从 HTTP 请求中提取的匹配字段
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    /// HTTP method (GET, POST, ...) — HTTP 方法 (GET, POST, ...)
    pub method: String,
    /// Request path (e.g. /api/users) — 请求路径 (如 /api/users)
    pub uri: String,
    /// Host header (may include port) — Host 头（可能包含端口）
    pub host: String,
    /// Protocol (http / https) — 协议 (http / https)
    pub scheme: String,
    /// Request headers (keys lowercased) — 请求头（key 小写）
    pub headers: HashMap<String, String>,
    /// TLS SNI (if TLS connection) — TLS SNI（如果是 TLS 连接）
    pub sni: Option<String>,
}

/// Route match result — 路由匹配结果
#[derive(Debug, Clone)]
pub struct RouteMatch {
    /// Matched route ID — 匹配到的路由 ID
    pub route_id: Uuid,
    /// Associated Service ID — 关联的 Service ID
    pub service_id: Option<Uuid>,
    /// Route name — 路由名称
    pub route_name: Option<Arc<str>>,
    /// Whether to strip the matched path prefix — 是否去除匹配的路径前缀
    pub strip_path: bool,
    /// Whether to preserve the original Host header — 是否保留原始 Host 头
    pub preserve_host: bool,
    /// Path handling mode — 路径处理方式
    pub path_handling: PathHandling,
    /// Matched path (used for strip_path) — 匹配的路径（用于 strip_path）
    pub matched_path: Option<String>,
    /// Protocol list — 协议列表
    pub protocols: Arc<Vec<String>>,
    /// Whether to buffer the request body before forwarding — 是否在转发前缓冲请求体
    pub request_buffering: bool,
    /// Whether to buffer the response body before sending to client — 是否在发送给客户端前缓冲响应体
    pub response_buffering: bool,
}

/// Unified router — selects concrete implementation based on router_flavor — 统一路由器 — 根据 router_flavor 选择具体实现
pub enum Router {
    /// Traditional routing (traditional / traditional_compatible) — 传统路由（traditional / traditional_compatible）
    Traditional(traditional::TraditionalRouter),
    /// Expression-based routing (expressions) — 表达式路由（expressions）
    Expressions(expressions::ExpressionsRouter),
}

impl Router {
    /// Build a router from route list and router flavor — 从路由列表和路由风格构建路由器
    pub fn new(routes: &[Route], router_flavor: &str) -> Self {
        match router_flavor {
            "expressions" => {
                Router::Expressions(expressions::ExpressionsRouter::new(routes))
            }
            // Both traditional / traditional_compatible use the traditional router — traditional / traditional_compatible 都使用传统路由器
            _ => {
                Router::Traditional(traditional::TraditionalRouter::new(routes))
            }
        }
    }

    /// Match a request — 匹配请求
    pub fn find_route(&self, ctx: &RequestContext) -> Option<RouteMatch> {
        match self {
            Router::Traditional(r) => r.find_route(ctx),
            Router::Expressions(r) => r.find_route(ctx),
        }
    }

    /// Number of routes — 路由数量
    pub fn route_count(&self) -> usize {
        match self {
            Router::Traditional(r) => r.route_count(),
            Router::Expressions(r) => r.route_count(),
        }
    }

    /// Rebuild the routing table — 重建路由表
    pub fn rebuild(&mut self, routes: &[Route]) {
        match self {
            Router::Traditional(r) => *r = traditional::TraditionalRouter::new(routes),
            Router::Expressions(r) => *r = expressions::ExpressionsRouter::new(routes),
        }
    }
}

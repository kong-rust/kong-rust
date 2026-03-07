//! Kong 路由引擎
//!
//! 支持两种路由风格:
//! - traditional / traditional_compatible: 基于 host/path/method/header/sni 的分类匹配
//! - expressions: 基于 ATC 表达式语法的优先级匹配

pub mod expressions;
pub mod traditional;

use std::collections::HashMap;
use uuid::Uuid;

use kong_core::models::Route;

/// 请求上下文 — 从 HTTP 请求中提取的匹配字段
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    /// HTTP 方法 (GET, POST, ...)
    pub method: String,
    /// 请求路径 (如 /api/users)
    pub uri: String,
    /// Host 头（可能包含端口）
    pub host: String,
    /// 协议 (http / https)
    pub scheme: String,
    /// 请求头（key 小写）
    pub headers: HashMap<String, String>,
    /// TLS SNI（如果是 TLS 连接）
    pub sni: Option<String>,
}

/// 路由匹配结果
#[derive(Debug, Clone)]
pub struct RouteMatch {
    /// 匹配到的路由 ID
    pub route_id: Uuid,
    /// 关联的 Service ID
    pub service_id: Option<Uuid>,
    /// 路由名称
    pub route_name: Option<String>,
    /// 是否去除匹配的路径前缀
    pub strip_path: bool,
    /// 是否保留原始 Host 头
    pub preserve_host: bool,
    /// 路径处理方式 ("v0" / "v1")
    pub path_handling: String,
    /// 匹配的路径（用于 strip_path）
    pub matched_path: Option<String>,
    /// 协议列表
    pub protocols: Vec<String>,
}

/// 统一路由器 — 根据 router_flavor 选择具体实现
pub enum Router {
    /// 传统路由（traditional / traditional_compatible）
    Traditional(traditional::TraditionalRouter),
    /// 表达式路由（expressions）
    Expressions(expressions::ExpressionsRouter),
}

impl Router {
    /// 从路由列表和路由风格构建路由器
    pub fn new(routes: &[Route], router_flavor: &str) -> Self {
        match router_flavor {
            "expressions" => {
                Router::Expressions(expressions::ExpressionsRouter::new(routes))
            }
            // traditional / traditional_compatible 都使用传统路由器
            _ => {
                Router::Traditional(traditional::TraditionalRouter::new(routes))
            }
        }
    }

    /// 匹配请求
    pub fn find_route(&self, ctx: &RequestContext) -> Option<RouteMatch> {
        match self {
            Router::Traditional(r) => r.find_route(ctx),
            Router::Expressions(r) => r.find_route(ctx),
        }
    }

    /// 路由数量
    pub fn route_count(&self) -> usize {
        match self {
            Router::Traditional(r) => r.route_count(),
            Router::Expressions(r) => r.route_count(),
        }
    }

    /// 重建路由表
    pub fn rebuild(&mut self, routes: &[Route]) {
        match self {
            Router::Traditional(r) => *r = traditional::TraditionalRouter::new(routes),
            Router::Expressions(r) => *r = expressions::ExpressionsRouter::new(routes),
        }
    }
}

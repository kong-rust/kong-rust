#![recursion_limit = "512"]
//! Kong Admin API — 100% compatible REST Admin API for Kong — Kong Admin API — 100% 兼容 Kong 的 REST Admin API
//!
//! Built on axum, supports: — 基于 axum 实现，支持:
//! - CRUD for all core entities — 所有核心实体的 CRUD
//! - Pagination and tag filtering — 分页、标签过滤
//! - Nested endpoints (e.g. /services/{service}/routes) — 嵌套端点（如 /services/{service}/routes）
//! - Special endpoints (/, /status, /config) — 特殊端点（/, /status, /config）

pub mod extractors;
pub mod handlers;

use std::sync::Arc;

use std::sync::RwLock;

use axum::http::{Method, StatusCode};
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use kong_core::models::*;
use kong_core::traits::Dao;
use kong_router::stream::StreamRouter;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;

/// Admin API application state — Admin API 应用状态
#[derive(Clone)]
pub struct AdminState {
    pub services: Arc<dyn Dao<Service>>,
    pub routes: Arc<dyn Dao<Route>>,
    pub consumers: Arc<dyn Dao<Consumer>>,
    pub plugins: Arc<dyn Dao<Plugin>>,
    pub upstreams: Arc<dyn Dao<Upstream>>,
    pub targets: Arc<dyn Dao<Target>>,
    pub certificates: Arc<dyn Dao<Certificate>>,
    pub snis: Arc<dyn Dao<Sni>>,
    pub ca_certificates: Arc<dyn Dao<CaCertificate>>,
    pub vaults: Arc<dyn Dao<Vault>>,
    pub node_id: uuid::Uuid,
    pub config: Arc<kong_config::KongConfig>,
    /// Proxy engine reference (Clone semantics, sharing underlying Arc data), used to refresh in-memory cache after write operations — 代理引擎引用（Clone 语义，共享底层 Arc 数据），用于写操作后刷新内存缓存
    pub proxy: kong_proxy::KongProxy,
    /// Cache refresh debounce signal sender: sends entity type name after CUD operations, background task merges and executes — 缓存刷新防抖信号发送端：CUD 操作后发送实体类型名，后台任务合并执行
    pub refresh_tx: tokio::sync::mpsc::UnboundedSender<&'static str>,
    /// Stream router reference (shared with Stream Proxy), synced on route changes — Stream 路由器引用（与 Stream Proxy 共享），路由变更时同步更新
    pub stream_router: Option<Arc<RwLock<StreamRouter>>>,
}

/// Cache refresh debounce loop: waits for the first signal, then collects all refresh requests within 100ms before executing — 缓存刷新防抖循环：收到第一个信号后等待 100ms，合并期间所有刷新请求后一次性执行
pub async fn run_cache_refresher(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<&'static str>,
    state: AdminState,
) {
    use std::collections::HashSet;
    use tokio::time::{Duration, Instant};

    loop {
        // Block waiting for the first refresh signal — 阻塞等待第一个刷新信号
        let Some(first) = rx.recv().await else {
            break;
        };
        let mut pending = HashSet::new();
        pending.insert(first);

        // Collect all pending entity types within a 100ms window — 100ms 窗口内收集所有待刷新实体类型
        let deadline = Instant::now() + Duration::from_millis(100);
        loop {
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Some(t) => { pending.insert(t); }
                        None => return,
                    }
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }

        // Refresh all at once after merging — 合并后一次性刷新
        for entity_type in &pending {
            state.refresh_proxy_cache(entity_type).await;
        }
        tracing::debug!("防抖刷新完成: {:?}", pending);
    }
}

/// Admin latency middleware — inject X-Kong-Admin-Latency header into responses — 注入 X-Kong-Admin-Latency 响应头
async fn admin_latency_middleware(
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let start = std::time::Instant::now();
    let mut response = next.run(req).await;
    let latency_ms = start.elapsed().as_millis();
    if let Ok(val) = axum::http::HeaderValue::from_str(&latency_ms.to_string()) {
        response.headers_mut().insert("X-Kong-Admin-Latency", val);
    }
    response
}

/// Issue 4: OPTIONS middleware — return 204 with CORS headers for OPTIONS requests (Kong-compatible)
/// OPTIONS 中间件 — 对 OPTIONS 请求返回 204 并带 CORS 头（兼容 Kong）
async fn options_middleware(
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    if req.method() == Method::OPTIONS {
        return axum::http::Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("Allow", "GET, HEAD, POST, PATCH, PUT, DELETE, OPTIONS")
            .header(
                "Access-Control-Allow-Methods",
                "GET, HEAD, POST, PATCH, PUT, DELETE, OPTIONS",
            )
            .header("Access-Control-Allow-Headers", "Content-Type")
            .header("Access-Control-Allow-Origin", "*")
            .body(axum::body::Body::empty())
            .unwrap()
            .into_response();
    }
    next.run(req).await
}

/// Build the Admin API router — 构建 Admin API 路由
pub fn build_admin_router(state: AdminState) -> Router {
    use handlers::*;

    Router::new()
        // Root info endpoint — 根信息端点
        .route("/", get(root_info))
        .route("/status", get(status_info))
        .route("/schemas/plugins/{name}", get(get_plugin_schema))
        .route("/schemas/{entity_name}", get(get_entity_schema))
        // Tags — 标签 API
        .route("/tags", get(list_all_tags))
        .route("/tags/{tag}", get(list_by_tag))
        // Services
        .route("/services", get(list_services).post(create_service))
        .route(
            "/services/{id_or_name}",
            get(get_service)
                .patch(update_service)
                .put(upsert_service)
                .delete(delete_service),
        )
        // Routes
        .route("/routes", get(list_routes).post(create_route))
        .route(
            "/routes/{id_or_name}",
            get(get_route)
                .patch(update_route)
                .put(upsert_route)
                .delete(delete_route),
        )
        // Nested: Routes and Plugins under Service — 嵌套: Service 下的 Routes 和 Plugins
        .route(
            "/services/{service_id_or_name}/routes",
            get(list_nested_routes).post(create_nested_route),
        )
        .route(
            "/services/{service_id_or_name}/plugins",
            get(list_service_plugins).post(create_service_plugin),
        )
        .route(
            "/services/{service_id_or_name}/plugins/{plugin_id_or_name}",
            get(get_service_plugin)
                .patch(update_service_plugin)
                .put(upsert_service_plugin)
                .delete(delete_service_plugin),
        )
        // Nested: Plugins under Route — 嵌套: Route 下的 Plugins
        .route(
            "/routes/{route_id_or_name}/plugins",
            get(list_route_plugins).post(create_route_plugin),
        )
        .route(
            "/routes/{route_id_or_name}/plugins/{plugin_id_or_name}",
            get(get_route_plugin)
                .patch(update_route_plugin)
                .put(upsert_route_plugin)
                .delete(delete_route_plugin),
        )
        // Consumers
        .route("/consumers", get(list_consumers).post(create_consumer))
        .route(
            "/consumers/{id_or_name}",
            get(get_consumer)
                .patch(update_consumer)
                .put(upsert_consumer)
                .delete(delete_consumer),
        )
        // Nested: Plugins under Consumer — 嵌套: Consumer 下的 Plugins
        .route(
            "/consumers/{consumer_id_or_name}/plugins",
            get(list_consumer_plugins).post(create_consumer_plugin),
        )
        .route(
            "/consumers/{consumer_id_or_name}/plugins/{plugin_id_or_name}",
            get(get_consumer_plugin)
                .patch(update_consumer_plugin)
                .put(upsert_consumer_plugin)
                .delete(delete_consumer_plugin),
        )
        // Plugins
        .route("/plugins", get(list_plugins).post(create_plugin))
        .route("/plugins/enabled", get(list_enabled_plugins))
        .route(
            "/plugins/{id_or_name}",
            get(get_plugin)
                .patch(update_plugin)
                .put(upsert_plugin)
                .delete(delete_plugin),
        )
        // Upstreams
        .route("/upstreams", get(list_upstreams).post(create_upstream))
        .route(
            "/upstreams/{id_or_name}",
            get(get_upstream)
                .patch(update_upstream)
                .put(upsert_upstream)
                .delete(delete_upstream),
        )
        // Targets (nested under upstreams)
        .route(
            "/upstreams/{upstream_id_or_name}/targets",
            get(list_nested_targets).post(create_nested_target),
        )
        .route(
            "/upstreams/{upstream_id_or_name}/targets/{id_or_name}",
            get(get_nested_target)
                .patch(update_nested_target)
                .delete(delete_nested_target),
        )
        // Certificates
        .route(
            "/certificates",
            get(list_certificates).post(create_certificate),
        )
        .route(
            "/certificates/{id}",
            get(get_certificate)
                .patch(update_certificate)
                .put(upsert_certificate)
                .delete(delete_certificate),
        )
        // SNIs
        .route("/snis", get(list_snis).post(create_sni))
        .route(
            "/snis/{id_or_name}",
            get(get_sni)
                .patch(update_sni)
                .put(upsert_sni)
                .delete(delete_sni),
        )
        // CA Certificates
        .route(
            "/ca_certificates",
            get(list_ca_certificates).post(create_ca_certificate),
        )
        .route(
            "/ca_certificates/{id}",
            get(get_ca_certificate)
                .patch(update_ca_certificate)
                .put(upsert_ca_certificate)
                .delete(delete_ca_certificate),
        )
        // Vaults
        .route("/vaults", get(list_vaults).post(create_vault))
        .route(
            "/vaults/{id_or_name}",
            get(get_vault)
                .patch(update_vault)
                .put(upsert_vault)
                .delete(delete_vault),
        )
        .fallback(admin_fallback)
        // Return JSON body for 405 Method Not Allowed — 405 方法不允许时返回 JSON 响应体
        .method_not_allowed_fallback(method_not_allowed_handler)
        // Issue 4: OPTIONS requests return 204 (Kong-compatible) — OPTIONS 请求返回 204（兼容 Kong）
        .layer(middleware::from_fn(options_middleware))
        // Admin latency header — Admin 延迟响应头
        .layer(middleware::from_fn(admin_latency_middleware))
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::mirror_request())
                .allow_methods(AllowMethods::mirror_request())
                .allow_headers(AllowHeaders::mirror_request())
                .allow_credentials(true)
                .expose_headers(tower_http::cors::ExposeHeaders::list([
                    axum::http::header::CONTENT_TYPE,
                    axum::http::header::CONTENT_LENGTH,
                ])),
        )
        .with_state(state)
}

/// Admin API 405 handler — Kong 兼容的 405 Method Not Allowed JSON 响应
async fn method_not_allowed_handler() -> (StatusCode, Json<Value>) {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({
            "message": "Method not allowed",
        })),
    )
}

/// Admin API 404 fallback — Kong 兼容的 404 JSON 响应
async fn admin_fallback() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "message": "Not found",
            "name": "not found",
            "code": 3,
        })),
    )
}

/// Build the Status API router — 构建 Status API 路由
pub fn build_status_router(state: AdminState) -> Router {
    use handlers::*;

    Router::new()
        .route("/status", get(status_info))
        .route("/metrics", get(status_metrics))
        .with_state(state)
}

/// Build the Kong Manager GUI router (static file server for SPA) — 构建 Kong Manager GUI 路由（SPA 静态文件服务）
///
/// - `GET /` → 301 redirect to `/__km_base__/` — 301 重定向到 `/__km_base__/`
/// - `GET /__km_base__/kconfig.js` → dynamic runtime config — 动态运行时配置
/// - `GET /__km_base__/*` → serve static files from `gui_dir`, SPA fallback to index.html — 从 `gui_dir` 提供静态文件，SPA 回退到 index.html
pub fn build_gui_router(gui_dir: &str, admin_api_url: &str) -> Router {
    async fn serve_gui_index(index_path: std::path::PathBuf) -> axum::response::Response {
        match tokio::fs::read(&index_path).await {
            Ok(body) => axum::http::Response::builder()
                .header("content-type", "text/html; charset=utf-8")
                .body(axum::body::Body::from(body))
                .unwrap()
                .into_response(),
            Err(_) => axum::http::Response::builder()
                .status(404)
                .body(axum::body::Body::from("Kong Manager GUI not found"))
                .unwrap()
                .into_response(),
        }
    }

    let kconfig_js = format!(
        "window.K_CONFIG = {{\n  ADMIN_API_URL: \"{}\",\n  ADMIN_API_PORT: \"{}\"\n}};\n",
        admin_api_url,
        // 从 admin_api_url 中提取端口，默认 8001 — Extract port from admin_api_url, default 8001
        url::Url::parse(admin_api_url)
            .ok()
            .and_then(|u| u.port())
            .unwrap_or(8001),
    );

    let index_path = std::path::PathBuf::from(gui_dir).join("index.html");
    let index_fallback = index_path.clone();
    let root_index_path = index_path.clone();
    let spa_fallback_index_path = index_path.clone();
    let serve_dir = ServeDir::new(gui_dir).not_found_service(tower::service_fn(
        move |_req: axum::http::Request<axum::body::Body>| {
            let path = index_fallback.clone();
            async move {
                // SPA fallback: serve index.html for unknown paths — SPA 回退：未知路径返回 index.html
                match tokio::fs::read(&path).await {
                    Ok(body) => Ok(axum::http::Response::builder()
                        .header("content-type", "text/html; charset=utf-8")
                        .body(axum::body::Body::from(body))
                        .unwrap()),
                    Err(_) => Ok(axum::http::Response::builder()
                        .status(404)
                        .body(axum::body::Body::from("Kong Manager GUI not found"))
                        .unwrap()),
                }
            }
        },
    ));

    Router::new()
        .route(
            "/",
            get(move || async move {
                // Serve index.html directly at root, no redirect — 根路径直接返回 index.html，不重定向
                serve_gui_index(root_index_path.clone()).await
            }),
        )
        .route(
            "/__km_base__/kconfig.js",
            get(move || async move {
                axum::http::Response::builder()
                    .header("content-type", "application/javascript; charset=utf-8")
                    .header("cache-control", "no-cache")
                    .body(axum::body::Body::from(kconfig_js.clone()))
                    .unwrap()
                    .into_response()
            }),
        )
        .nest_service("/__km_base__", serve_dir)
        .route(
            "/{*path}",
            get(move || async move {
                // SPA fallback for client-side routes like /services — 处理 /services 这类前端路由刷新
                serve_gui_index(spa_fallback_index_path.clone()).await
            }),
        )
}

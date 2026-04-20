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

use std::collections::HashMap;
use std::sync::Arc;

use std::sync::RwLock;

use axum::extract::State;
use axum::http::{Method, StatusCode};
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::{get, put};
use axum::{Json, Router};
use serde_json::{json, Value};
use kong_core::models::*;
use kong_core::traits::Dao;
use kong_db::KongCache;
use kong_router::stream::StreamRouter;
use tower_http::services::ServeDir;

/// Runtime log level updater — invoked by `/debug/node/log-level` to reload `tracing_subscriber`'s EnvFilter.
/// 运行时日志级别更新器 — 由 `/debug/node/log-level` 端点调用，重载 `tracing_subscriber` 的 EnvFilter。
///
/// Returns `Ok(())` on success, or a human-readable error message on failure.
/// 成功返回 `Ok(())`，失败返回人类可读的错误信息。
pub type LogLevelUpdater = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

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
    pub ai_providers: Arc<dyn Dao<kong_ai::models::AiProviderConfig>>,
    pub ai_models: Arc<dyn Dao<kong_ai::models::AiModel>>,
    pub ai_virtual_keys: Arc<dyn Dao<kong_ai::models::AiVirtualKey>>,
    pub node_id: uuid::Uuid,
    pub config: Arc<kong_config::KongConfig>,
    /// Proxy engine reference (Clone semantics, sharing underlying Arc data), used to refresh in-memory cache after write operations — 代理引擎引用（Clone 语义，共享底层 Arc 数据），用于写操作后刷新内存缓存
    pub proxy: kong_proxy::KongProxy,
    /// Cache refresh debounce signal sender: sends entity type name after CUD operations, background task merges and executes — 缓存刷新防抖信号发送端：CUD 操作后发送实体类型名，后台任务合并执行
    pub refresh_tx: tokio::sync::mpsc::UnboundedSender<&'static str>,
    /// Stream router reference (shared with Stream Proxy), synced on route changes — Stream 路由器引用（与 Stream Proxy 共享），路由变更时同步更新
    pub stream_router: Option<Arc<RwLock<StreamRouter>>>,
    /// Configuration hash for db-less mode — db-less 模式下的配置哈希值
    /// Default is all zeros (empty config); updated via POST /config — 默认全零（空配置）；通过 POST /config 更新
    pub configuration_hash: Arc<RwLock<String>>,
    /// DB-less store reference for hot-reloading via POST /config — DB-less 存储引用，用于 POST /config 热重载
    /// None in PostgreSQL mode — PostgreSQL 模式下为 None
    pub dbless_store: Option<Arc<kong_db::dbless::DblessStore>>,
    /// In-memory target health status store: key = "upstream_id:target_address", value = health status string
    /// 内存中 target 健康状态存储：key = "upstream_id:target地址"，value = 健康状态字符串
    pub target_health: Arc<RwLock<HashMap<String, String>>>,
    /// Control Plane reference (only set when role=control_plane) — 控制面引用（仅 role=control_plane 时设置）
    pub cp: Option<Arc<kong_cluster::cp::ControlPlane>>,
    /// Shared Kong cache instance — exposed via `/cache/{key}` endpoints (task 16.3)
    /// 共享 Kong 缓存实例 — 通过 `/cache/{key}` 端点暴露（任务 16.3）
    pub cache: Arc<KongCache>,
    /// Runtime log level updater — `None` if logging was initialised without reload support
    /// 运行时日志级别更新器 — 若日志初始化时未启用 reload 则为 `None`
    pub log_updater: Option<LogLevelUpdater>,
    /// Current Kong-style log level string, kept in sync with `log_updater` writes
    /// 当前 Kong 风格日志级别字符串，与 `log_updater` 写入保持同步
    pub current_log_level: Arc<RwLock<String>>,
}

/// Export current config from DB and push to all DPs — 从 DB 导出当前配置并推送给所有 DP
async fn push_config_to_dps(
    cp: &kong_cluster::cp::ControlPlane,
    state: &AdminState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use kong_core::traits::PageParams;

    // Fetch all pages of an entity type — 获取某实体类型的所有分页数据
    macro_rules! fetch_all {
        ($dao:expr) => {{
            let mut all_items = Vec::new();
            let mut offset: Option<String> = None;
            loop {
                let params = PageParams { size: 1000, offset: offset.clone(), ..Default::default() };
                match $dao.page(&params).await {
                    Ok(page) => {
                        all_items.extend(page.data);
                        if page.offset.is_none() {
                            break;
                        }
                        offset = page.offset;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to fetch page: {} — 获取分页失败: {}", e, e);
                        break;
                    }
                }
            }
            all_items
        }};
    }

    // Export all entities with full pagination — 导出所有实体（完整分页）
    let services = fetch_all!(state.services);
    let routes = fetch_all!(state.routes);
    let plugins = fetch_all!(state.plugins);
    let upstreams = fetch_all!(state.upstreams);
    let targets = fetch_all!(state.targets);
    let consumers = fetch_all!(state.consumers);
    let certificates = fetch_all!(state.certificates);
    let snis = fetch_all!(state.snis);
    let ca_certificates = fetch_all!(state.ca_certificates);

    // Build declarative config JSON — 构建声明式配置 JSON
    let config_table = serde_json::json!({
        "_format_version": "3.0",
        "services": serde_json::to_value(&services)?,
        "routes": serde_json::to_value(&routes)?,
        "plugins": serde_json::to_value(&plugins)?,
        "upstreams": serde_json::to_value(&upstreams)?,
        "targets": serde_json::to_value(&targets)?,
        "consumers": serde_json::to_value(&consumers)?,
        "certificates": serde_json::to_value(&certificates)?,
        "snis": serde_json::to_value(&snis)?,
        "ca_certificates": serde_json::to_value(&ca_certificates)?,
    });

    cp.push_config(&config_table).await.map_err(|e| {
        Box::new(e) as Box<dyn std::error::Error + Send + Sync>
    })?;

    tracing::info!("Config pushed to DPs after CUD — CUD 后已推送配置给 DP");
    Ok(())
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

        // If CP mode, push config to all connected DPs — 如果是 CP 模式，推送配置给所有已连接的 DP
        if let Some(ref cp) = state.cp {
            if let Err(e) = push_config_to_dps(cp, &state).await {
                tracing::error!("Failed to push config to DPs — 推送配置给 DP 失败: {}", e);
            }
        }
    }
}

/// Admin headers middleware — inject X-Kong-Admin-Latency, Server, and CORS headers based on config
/// 管理头中间件 — 根据配置注入 X-Kong-Admin-Latency、Server 和 CORS 响应头
async fn admin_headers_middleware(
    axum::extract::State(state): axum::extract::State<AdminState>,
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let start = std::time::Instant::now();
    // Extract Origin header before passing request — 在传递请求前提取 Origin 头
    let origin = req
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let mut response = next.run(req).await;
    let headers_config = &state.config.headers;

    // Check if admin latency should be included — 检查是否应包含 admin 延迟头
    let has_latency = headers_config.iter().any(|h| {
        h.eq_ignore_ascii_case("latency_tokens") || h.eq_ignore_ascii_case("x-kong-admin-latency")
    });
    if has_latency {
        let latency_ms = start.elapsed().as_millis();
        if let Ok(val) = axum::http::HeaderValue::from_str(&latency_ms.to_string()) {
            response.headers_mut().insert("X-Kong-Admin-Latency", val);
        }
    }

    // Check if Server header should be included — 检查是否应包含 Server 头
    let has_server = headers_config.iter().any(|h| {
        h.eq_ignore_ascii_case("server_tokens") || h.eq_ignore_ascii_case("server")
    });
    if has_server {
        response.headers_mut().insert(
            axum::http::header::SERVER,
            axum::http::HeaderValue::from_static("kong/3.10.0"),
        );
    } else {
        response.headers_mut().remove(axum::http::header::SERVER);
    }

    // CORS headers — Kong Admin API always returns CORS headers (compatible with official Kong)
    // CORS 头 — Kong Admin API 始终返回 CORS 头（兼容官方 Kong 行为）
    let cors_origin = origin.unwrap_or_else(|| "*".to_string());
    if let Ok(val) = axum::http::HeaderValue::from_str(&cors_origin) {
        response
            .headers_mut()
            .insert(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, val);
    }
    response.headers_mut().insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        axum::http::HeaderValue::from_static("true"),
    );

    response
}

/// Check if a request path matches a known Admin API route pattern — 检查请求路径是否匹配已知的 Admin API 路由模式
fn is_known_route(path: &str) -> bool {
    // Static routes — 静态路由
    let static_routes = [
        "/", "/status", "/config", "/endpoints", "/plugins/enabled", "/plugins",
        "/services", "/routes", "/consumers", "/upstreams",
        "/certificates", "/snis", "/ca_certificates", "/vaults", "/tags",
        "/ai-providers", "/ai-models", "/ai-model-groups", "/ai-virtual-keys",
        "/clustering/data-planes", "/clustering/status",
        "/cache", "/debug/node/log-level", "/timers",
    ];
    if static_routes.contains(&path) {
        return true;
    }

    // Dynamic route patterns: split path segments and match — 动态路由模式：按路径段匹配
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    match segments.as_slice() {
        // /entity/{id}
        [entity, _id] if matches!(*entity, "services" | "routes" | "consumers" | "plugins"
            | "upstreams" | "certificates" | "snis" | "ca_certificates" | "vaults" | "tags"
            | "ai-providers" | "ai-models" | "ai-virtual-keys" | "cache") => true,
        // /debug/node/log-level/{level}
        ["debug", "node", "log-level", _] => true,
        // /schemas/{entity}
        ["schemas", _] => true,
        // /schemas/{entity}/validate or /schemas/plugins/validate
        ["schemas", _, "validate"] => true,
        // /services/{id}/routes, /services/{id}/plugins
        ["services", _, sub] if matches!(*sub, "routes" | "plugins") => true,
        // /services/{id}/plugins/{id}
        ["services", _, "plugins", _] => true,
        // /routes/{id}/service
        ["routes", _, "service"] => true,
        // /routes/{id}/plugins
        ["routes", _, "plugins"] => true,
        // /routes/{id}/plugins/{id}
        ["routes", _, "plugins", _] => true,
        // /consumers/{id}/plugins
        ["consumers", _, "plugins"] => true,
        // /consumers/{id}/plugins/{id}
        ["consumers", _, "plugins", _] => true,
        // /certificates/{id}/snis
        ["certificates", _, "snis"] => true,
        // /upstreams/{id}/targets and /upstreams/{id}/health
        ["upstreams", _, "targets"] => true,
        ["upstreams", _, "health"] => true,
        // /upstreams/{id}/targets/{id}
        ["upstreams", _, "targets", _] => true,
        // /ai-providers/{id}/ai-models
        ["ai-providers", _, "ai-models"] => true,
        // /ai-virtual-keys/{id}/rotate
        ["ai-virtual-keys", _, "rotate"] => true,
        _ => false,
    }
}

/// Determine allowed HTTP methods based on endpoint type — 根据端点类型确定允许的 HTTP 方法
fn determine_allowed_methods(path: &str) -> &'static str {
    // Read-only endpoints — 只读端点
    match path {
        "/" | "/status" | "/endpoints" | "/plugins/enabled" => return "GET, HEAD, OPTIONS",
        _ => {}
    }

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    match segments.len() {
        // Collection endpoints: /services, /routes, etc. — 集合端点
        1 => "GET, HEAD, OPTIONS, POST",
        2 => {
            match segments[0] {
                // /schemas/{entity} — read-only
                "schemas" => "GET, HEAD, OPTIONS",
                // /tags/{value} — read-only
                "tags" => "GET, HEAD, OPTIONS",
                // /entity/{id} — entity endpoints support CRUD
                _ => "DELETE, GET, HEAD, OPTIONS, PATCH, PUT",
            }
        }
        3 => {
            match segments[2] {
                // /schemas/{entity}/validate — POST only
                "validate" => "OPTIONS, POST",
                // /entity/{id}/sub-collection — collection endpoint
                _ => "GET, HEAD, OPTIONS, POST",
            }
        }
        // /entity/{id}/sub/{id} — entity endpoint
        4 => "DELETE, GET, HEAD, OPTIONS, PATCH, PUT",
        _ => "GET, HEAD, OPTIONS",
    }
}

/// Issue 4: OPTIONS middleware — return 204 for known routes, 404 for unknown (Kong-compatible)
/// OPTIONS 中间件 — 已知路由返回 204，未知路由返回 404（兼容 Kong）
async fn options_middleware(
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    if req.method() == Method::OPTIONS {
        let path = req.uri().path().to_string();

        // Unknown routes return 404 — 未知路由返回 404
        if !is_known_route(&path) {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "message": "Not found",
                    "name": "not found",
                    "code": 3,
                })),
            ).into_response();
        }

        // Determine allowed methods based on endpoint type — 根据端点类型确定允许的方法
        let allow = determine_allowed_methods(&path);

        // NOTE: CORS origin/credentials headers are added by admin_headers_middleware (outer layer)
        // 注意：CORS origin/credentials 头由外层 admin_headers_middleware 统一添加
        return axum::http::Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("Allow", allow)
            .header("Access-Control-Allow-Methods", allow)
            .header("Access-Control-Allow-Headers", "Content-Type")
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
        .route("/config", axum::routing::post(post_config))
        .route("/endpoints", get(list_endpoints))
        .route("/schemas/plugins/validate", axum::routing::post(validate_plugin_schema))
        .route("/schemas/plugins/{name}", get(get_plugin_schema))
        .route("/schemas/vaults/validate", axum::routing::post(validate_vault_schema))
        .route("/schemas/vaults/{name}", get(get_vault_schema))
        .route("/schemas/{entity_name}", get(get_entity_schema))
        .route("/schemas/{entity_name}/validate", axum::routing::post(validate_entity_schema))
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
            "/upstreams/{upstream_id_or_name}/targets/all",
            get(list_nested_targets),
        )
        .route(
            "/upstreams/{upstream_id_or_name}/targets/{id_or_name}",
            get(get_nested_target)
                .patch(update_nested_target)
                .put(upsert_nested_target)
                .delete(delete_nested_target),
        )
        .route(
            "/upstreams/{upstream_id_or_name}/targets/{id_or_name}/healthy",
            put(handlers::set_target_health),
        )
        .route(
            "/upstreams/{upstream_id_or_name}/targets/{id_or_name}/unhealthy",
            put(handlers::set_target_health),
        )
        .route(
            "/upstreams/{upstream_id_or_name}/health",
            get(handlers::upstream_health),
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
        // Routes nested service — 路由嵌套 service 端点
        .route(
            "/routes/{route_id_or_name}/service",
            get(handlers::get_route_service)
                .patch(handlers::update_route_service)
                .put(handlers::upsert_route_service)
                .delete(handlers::delete_route_service),
        )
        // Certificates nested SNIs — 证书嵌套 SNI 路由
        .route(
            "/certificates/{cert_id_or_name}/snis",
            get(handlers::list_certificate_snis).post(handlers::create_certificate_sni),
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
        // AI Providers
        .route("/ai-providers", get(handlers::ai_providers::list).post(handlers::ai_providers::create))
        .route(
            "/ai-providers/{id_or_name}",
            get(handlers::ai_providers::get_one)
                .patch(handlers::ai_providers::update)
                .put(handlers::ai_providers::upsert)
                .delete(handlers::ai_providers::delete_one),
        )
        .route("/ai-providers/{id}/ai-models", get(handlers::ai_providers::list_models))
        // AI Models
        .route("/ai-models", get(handlers::ai_models::list).post(handlers::ai_models::create))
        .route(
            "/ai-models/{id}",
            get(handlers::ai_models::get_one)
                .patch(handlers::ai_models::update)
                .put(handlers::ai_models::upsert)
                .delete(handlers::ai_models::delete_one),
        )
        .route("/ai-model-groups", get(handlers::ai_models::list_groups))
        // AI Virtual Keys
        .route("/ai-virtual-keys", get(handlers::ai_virtual_keys::list).post(handlers::ai_virtual_keys::create))
        .route(
            "/ai-virtual-keys/{id_or_name}",
            get(handlers::ai_virtual_keys::get_one)
                .patch(handlers::ai_virtual_keys::update)
                .delete(handlers::ai_virtual_keys::delete_one),
        )
        .route("/ai-virtual-keys/{id}/rotate", axum::routing::post(handlers::ai_virtual_keys::rotate))
        // Clustering — 集群端点
        .route("/clustering/data-planes", get(handlers::clustering::list_data_planes))
        .route("/clustering/status", get(handlers::clustering::clustering_status))
        // Cache management — 缓存管理（任务 16.3）
        .route("/cache", axum::routing::delete(handlers::cache::purge_cache))
        .route(
            "/cache/{key}",
            get(handlers::cache::get_cache_entry).delete(handlers::cache::delete_cache_entry),
        )
        // Debug — runtime log level control — 运行时日志级别控制（任务 16.4）
        .route("/debug/node/log-level", get(handlers::debug::get_log_level))
        .route(
            "/debug/node/log-level/{level}",
            put(handlers::debug::set_log_level),
        )
        // Timers — Kong-compatible timer stats — 计时器统计（任务 16.5）
        .route("/timers", get(handlers::timers::get_timers))
        .fallback(admin_fallback_with_trailing_slash)
        // Return JSON body for 405 Method Not Allowed — 405 方法不允许时返回 JSON 响应体
        .method_not_allowed_fallback(method_not_allowed_handler)
        // Issue 4: OPTIONS requests return 204 (Kong-compatible) — OPTIONS 请求返回 204（兼容 Kong）
        .layer(middleware::from_fn(options_middleware))
        // Admin headers — Server + Admin 延迟响应头（最外层，确保所有请求都包含此头）
        .layer(middleware::from_fn_with_state(state.clone(), admin_headers_middleware))
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

/// Admin API 404 fallback with trailing slash normalization — 带尾部斜杠归一化的 404 fallback
/// If path ends with '/', retry without it — 如果路径以 '/' 结尾，去掉后重试
async fn admin_fallback_with_trailing_slash(
    State(state): State<AdminState>,
    mut req: axum::extract::Request,
) -> axum::response::Response {
    let path = req.uri().path().to_string();
    if path.len() > 1 && path.ends_with('/') {
        let trimmed = path.trim_end_matches('/');
        let new_uri = if let Some(q) = req.uri().query() {
            format!("{}?{}", trimmed, q)
        } else {
            trimmed.to_string()
        };
        if let Ok(uri) = new_uri.parse::<axum::http::Uri>() {
            *req.uri_mut() = uri;
            // Re-route through the admin router — 通过 admin router 重新路由
            let router = build_admin_router(state);
            let mut svc = router.into_service();
            return tower::Service::<axum::extract::Request>::call(&mut svc, req).await.unwrap_or_else(|e| {
                match e {}
            });
        }
    }
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "message": "Not found",
            "name": "not found",
            "code": 3,
        })),
    ).into_response()
}

/// Build the Status API router — 构建 Status API 路由
pub fn build_status_router(state: AdminState) -> Router {
    use handlers::*;

    Router::new()
        .route("/status", get(status_info))
        .route("/status/ready", get(status_ready))
        .route("/metrics", get(status_metrics))
        // admin-api-method plugin test endpoint — admin-api-method 测试插件端点
        .route("/hello", get(status_hello))
        .with_state(state)
}

/// GET /hello — test endpoint for admin-api-method plugin compatibility — 测试端点，用于 admin-api-method 插件兼容性
async fn status_hello() -> impl axum::response::IntoResponse {
    Json(json!({ "hello": "from status api" }))
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

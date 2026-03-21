//! Admin API request handlers — Admin API 请求处理器
//!
//! Implements Kong-compatible REST API endpoints: — 实现 Kong 兼容的 REST API 端点:
//! - Generic CRUD endpoints (generic) — 通用 CRUD 端点（泛型）
//! - Nested endpoints (e.g. /services/{id}/routes) — 嵌套端点（如 /services/{id}/routes）
//! - Special endpoints (/, /status) — 特殊端点（/, /status）

pub mod schemas;
pub use schemas::*;

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use kong_core::error::KongError;
use kong_core::models::*;
use kong_core::traits::{Dao, Entity, Page, PageParams, PrimaryKey};

use crate::extractors::FlexibleBody;
use crate::AdminState;

// ============ Cache refresh — 缓存刷新 ============

impl AdminState {
    /// Asynchronously refresh KongProxy in-memory cache after Admin API write operations — Admin API 写操作后异步刷新 KongProxy 内存缓存
    pub async fn refresh_proxy_cache(&self, entity_type: &str) {
        let all_params = kong_core::traits::PageParams {
            size: 10000,
            offset: None,
            tags: None,
        };

        match entity_type {
            "services" => match self.services.page(&all_params).await {
                Ok(page) => self.proxy.update_services(page.data),
                Err(e) => tracing::error!("刷新 services 缓存失败: {}", e),
            },
            "routes" => {
                match self.routes.page(&all_params).await {
                    Ok(page) => {
                        self.proxy.update_routes(&page.data);
                        // Sync update Stream router (L4 routing table) — 同步更新 Stream 路由器（L4 路由表）
                        if let Some(ref sr) = self.stream_router {
                            if let Ok(mut router) = sr.write() {
                                router.rebuild(&page.data);
                                tracing::debug!(
                                    "Stream 路由表已刷新: {} 条路由",
                                    router.route_count()
                                );
                            }
                        }
                    }
                    Err(e) => tracing::error!("刷新 routes 缓存失败: {}", e),
                }
            }
            "plugins" => match self.plugins.page(&all_params).await {
                Ok(page) => self.proxy.update_plugins(page.data),
                Err(e) => tracing::error!("刷新 plugins 缓存失败: {}", e),
            },
            "upstreams" | "targets" => {
                let upstreams = self.upstreams.page(&all_params).await;
                let targets = self.targets.page(&all_params).await;
                match (upstreams, targets) {
                    (Ok(u), Ok(t)) => self.proxy.update_upstreams(u.data, t.data),
                    (Err(e), _) | (_, Err(e)) => {
                        tracing::error!("刷新 upstreams 缓存失败: {}", e);
                    }
                }
            }
            "certificates" | "snis" => {
                let certs = self.certificates.page(&all_params).await;
                let snis = self.snis.page(&all_params).await;
                match (certs, snis) {
                    (Ok(c), Ok(s)) => self.proxy.cert_manager.load_certificates(&c.data, &s.data),
                    (Err(e), _) | (_, Err(e)) => {
                        tracing::error!("刷新 certificates 缓存失败: {}", e);
                    }
                }
            }
            "ca_certificates" => match self.ca_certificates.page(&all_params).await {
                Ok(page) => self.proxy.update_ca_certificates(page.data),
                Err(e) => tracing::error!("刷新 ca_certificates 缓存失败: {}", e),
            },
            _ => {} // consumers / vaults etc. are not directly used in proxy flow — consumers / vaults 等代理流程不直接使用
        }
    }
}

// ============ Query parameters — 查询参数 ============

#[derive(Debug, Deserialize)]
pub struct ListParams {
    pub size: Option<usize>,
    pub offset: Option<String>,
    pub tags: Option<String>,
}

impl ListParams {
    fn to_page_params(&self) -> PageParams {
        PageParams {
            size: self.size.unwrap_or(100).min(1000),
            offset: self.offset.clone(),
            tags: self
                .tags
                .as_ref()
                .map(|t| t.split(',').map(|s| s.trim().to_string()).collect()),
        }
    }
}

// ============ Error response — 错误响应 ============

/// Kong-compatible error response format — Kong 兼容的错误响应格式
#[allow(dead_code)]
fn error_response(err: KongError) -> Response {
    let status =
        StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = json!({
        "message": err.to_string(),
        "name": err.error_name(),
        "code": err.error_code(),
    });
    (status, Json(body)).into_response()
}

// ============ Special endpoints — 特殊端点 ============

/// Serialize ListenAddr list to Kong-compatible string array — 将 ListenAddr 列表序列化为 Kong 兼容的字符串数组
fn listen_addrs_to_strings(addrs: &[kong_config::ListenAddr]) -> Vec<String> {
    addrs
        .iter()
        .map(|a| {
            let mut s = format!("{}:{}", a.ip, a.port);
            if a.ssl {
                s.push_str(" ssl");
            }
            if a.http2 {
                s.push_str(" http2");
            }
            if a.reuseport {
                s.push_str(" reuseport");
            }
            if a.proxy_protocol {
                s.push_str(" proxy_protocol");
            }
            if let Some(bl) = a.backlog {
                s.push_str(&format!(" backlog={}", bl));
            }
            s
        })
        .collect()
}

/// Kong-compatible version string — Kong 兼容版本号
pub const KONG_VERSION: &str = "3.10.0";

/// GET / — Node info (Kong-compatible) — GET / — 节点信息（兼容 Kong）
pub async fn root_info(State(state): State<AdminState>) -> impl IntoResponse {
    let config = &state.config;
    let hostname = gethostname::gethostname().to_string_lossy().to_string();
    let mut available_on_server = serde_json::Map::new();
    for name in state.proxy.plugin_registry.registered_names() {
        available_on_server.insert(name, json!(true));
    }

    // Convert listen addresses to the [{port, ssl}] format expected by frontend — 将监听地址转为前端期望的 [{port, ssl}] 格式
    let to_listeners = |addrs: &[kong_config::ListenAddr]| -> Vec<Value> {
        addrs
            .iter()
            .map(|a| json!({"port": a.port, "ssl": a.ssl}))
            .collect()
    };

    // Return Kong-compatible version and Server header — 返回 Kong 兼容的版本号和 Server 响应头
    let body = Json(json!({
        "version": KONG_VERSION,
        "edition": "community",
        "lua_version": "LuaJIT 2.1.0-beta3",
        "tagline": "Welcome to kong",
        "hostname": hostname,
        "node_id": state.node_id.to_string(),
        "configuration": {
            "database": if config.database == "off" { "off" } else { "postgres" },
            "router_flavor": &config.router_flavor,
            "role": &config.role,
            "proxy_listen": listen_addrs_to_strings(&config.proxy_listen),
            "admin_listen": listen_addrs_to_strings(&config.admin_listen),
            "admin_gui_listen": listen_addrs_to_strings(&config.admin_gui_listen),
            "admin_gui_url": &config.admin_gui_url,
            "status_listen": listen_addrs_to_strings(&config.status_listen),
            "proxy_access_log": &config.proxy_access_log,
            "proxy_error_log": &config.proxy_error_log,
            "admin_access_log": &config.admin_access_log,
            "admin_error_log": &config.admin_error_log,
            "proxy_stream_access_log": &config.proxy_stream_access_log,
            "proxy_stream_error_log": &config.proxy_stream_error_log,
            "prefix": &config.prefix,
            "log_level": &config.log_level,
            "plugins": &config.plugins,
            "pg_host": &config.pg_host,
            "pg_port": config.pg_port,
            "pg_database": &config.pg_database,
            "pg_user": &config.pg_user,
            "pg_ssl": config.pg_ssl,
            "pg_ssl_verify": config.pg_ssl_verify,
            "pg_timeout": config.pg_timeout,
            "ssl_cipher_suite": &config.ssl_cipher_suite,
            "ssl_protocols": &config.ssl_protocols,
            "dns_resolver": &config.dns_resolver,
            "worker_consistency": &config.worker_consistency,
            "worker_state_update_frequency": config.worker_state_update_frequency,
            "db_update_frequency": config.db_update_frequency,
            "db_cache_ttl": config.db_cache_ttl,
            "db_resurrect_ttl": config.db_resurrect_ttl,
            "nginx_worker_processes": &config.nginx_worker_processes,
            "upstream_keepalive_pool_size": config.upstream_keepalive_pool_size,
            "upstream_keepalive_max_requests": config.upstream_keepalive_max_requests,
            "upstream_keepalive_idle_timeout": config.upstream_keepalive_idle_timeout,
            "mem_cache_size": &config.mem_cache_size,
            "error_default_type": &config.error_default_type,
            "headers": &config.headers,
            "trusted_ips": &config.trusted_ips,
            "real_ip_header": &config.real_ip_header,
            "proxy_listeners": to_listeners(&config.proxy_listen),
            "admin_gui_listeners": to_listeners(&config.admin_gui_listen),
        },
        "plugins": {
            "available_on_server": available_on_server,
            "enabled_in_cluster": [],
        },
        "timers": {
            "running": 0,
            "pending": 0,
        },
        "pids": {
            "master": std::process::id(),
            "workers": [std::process::id()],
        },
    }));

    // Add Server header for Kong compatibility — 添加 Kong 兼容的 Server 响应头
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::SERVER,
        HeaderValue::from_static("kong/3.10.0"),
    );
    response
}

/// GET /plugins/enabled — List registered plugins on this node. — GET /plugins/enabled — 返回当前节点已注册插件。
pub async fn list_enabled_plugins(State(state): State<AdminState>) -> impl IntoResponse {
    let mut enabled_plugins = state.proxy.plugin_registry.registered_names();
    enabled_plugins.sort();

    Json(json!({
        "enabled_plugins": enabled_plugins,
    }))
}

/// GET /status — Service status — GET /status — 服务状态
pub async fn status_info(State(_state): State<AdminState>) -> impl IntoResponse {
    Json(json!({
        "server": {
            "connections_accepted": 0,
            "connections_active": 0,
            "connections_handled": 0,
            "connections_reading": 0,
            "connections_writing": 0,
            "connections_waiting": 0,
            "total_requests": 0,
        },
        "database": {
            "reachable": true,
        },
        "memory": {
            "lua_shared_dicts": {},
            "workers_lua_vms": [],
        },
        "configuration_hash": "00000000000000000000000000000000",
    }))
}

/// GET /metrics — Prometheus metrics from the status port — GET /metrics — 从状态端口暴露的 Prometheus 指标
pub async fn status_metrics(State(state): State<AdminState>) -> Response {
    let params = PageParams {
        size: 1000,
        offset: None,
        tags: None,
    };
    let plugin_page = match state.plugins.page(&params).await {
        Ok(page) => page,
        Err(err) => return error_response(err),
    };

    let prometheus_configs = plugin_page
        .data
        .into_iter()
        .filter(|plugin| plugin.enabled && plugin.name == "prometheus")
        .map(|plugin| plugin.config)
        .collect::<Vec<_>>();

    if prometheus_configs.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "message": "prometheus plugin is not enabled",
                "name": "not found",
                "code": 3,
            })),
        )
            .into_response();
    }

    match kong_lua_bridge::metrics::collect_prometheus_metrics(&state.config, &prometheus_configs) {
        Ok(metrics) => {
            let mut response = metrics.into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            response
        }
        Err(err) => error_response(err).into_response(),
    }
}

// ============ Generic CRUD endpoints — 通用 CRUD 端点 ============

// ============ Generic CRUD helpers — 通用 CRUD 辅助 ============

/// Build Kong-compatible paginated response — 构建 Kong 兼容的分页响应
/// Always includes `next` (null when no more pages), only includes `offset` when present —
/// 始终包含 `next`（无更多页时为 null），仅在存在时包含 `offset`
fn build_page_response<T: Serialize>(page: &Page<T>) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("data".to_string(), json!(page.data));
    // Always include next (null when no more pages) — 始终包含 next（无更多页时为 null）
    body.insert("next".to_string(), match &page.next {
        Some(n) => json!(n),
        None => Value::Null,
    });
    // Only include offset when present — 仅在存在时包含 offset
    if let Some(ref offset) = page.offset {
        body.insert("offset".to_string(), json!(offset));
    }
    Value::Object(body)
}

/// Generic list/query/create/update/delete logic — 通用的列表/查询/创建/更新/删除逻辑
/// Due to Rust generics limitations (cannot select DAO by type at runtime), — 因 Rust 泛型限制（无法在运行时根据类型选择 DAO）,
/// uses concrete type handlers simplified via macros — 使用具体类型的 handler 通过宏简化注册

/// Generic list handler — 通用列表处理
async fn do_list<T: Entity + Serialize + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    params: &ListParams,
) -> (StatusCode, Json<Value>) {
    match dao.page(&params.to_page_params()).await {
        Ok(page) => {
            (StatusCode::OK, Json(build_page_response(&page)))
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

/// Generic get handler — 通用查询处理
async fn do_get<T: Entity + Serialize + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    id_or_name: &str,
) -> (StatusCode, Json<Value>) {
    let pk = PrimaryKey::from_str_or_uuid(id_or_name);
    match dao.select(&pk).await {
        Ok(Some(entity)) => {
            let body = serde_json::to_value(&entity).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "message": format!("{} not found", T::table_name()),
                "name": "not found",
                "code": 3,
            })),
        ),
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

/// Expand `url` shorthand field into protocol/host/port/path — 将 `url` 快捷字段展开为 protocol/host/port/path
fn expand_url_shorthand(body: &Value) -> Result<Value, (StatusCode, Json<Value>)> {
    let mut body = body.clone();
    if let Some(obj) = body.as_object_mut() {
        if let Some(url_val) = obj.remove("url") {
            if let Some(url_str) = url_val.as_str() {
                if url_str.is_empty() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": "schema violation (url: missing host in url)",
                            "name": "schema violation",
                            "code": 2,
                        })),
                    ));
                }
                let parsed = url::Url::parse(url_str).map_err(|_| (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": "schema violation (url: missing host in url)",
                        "name": "schema violation",
                        "code": 2,
                    })),
                ))?;
                if parsed.host_str().map_or(true, |h| h.is_empty()) {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": "schema violation (url: missing host in url)",
                            "name": "schema violation",
                            "code": 2,
                        })),
                    ));
                }
                if !obj.contains_key("protocol") {
                    obj.insert("protocol".to_string(), json!(parsed.scheme()));
                }
                if !obj.contains_key("host") {
                    if let Some(host) = parsed.host_str() {
                        obj.insert("host".to_string(), json!(host));
                    }
                }
                if !obj.contains_key("port") {
                    if let Some(port) = parsed.port_or_known_default() {
                        obj.insert("port".to_string(), json!(port));
                    }
                }
                // Only set path if the original URL explicitly includes a path component — 仅当原始 URL 明确包含路径部分时才设置 path
                // e.g. "http://example.com" → no path, "http://example.com/" → path="/", "http://example.com/foo" → path="/foo"
                let has_explicit_path = {
                    let after_scheme = url_str.find("://").map(|i| i + 3).unwrap_or(0);
                    url_str[after_scheme..].find('/').is_some()
                };
                if has_explicit_path && !obj.contains_key("path") {
                    obj.insert("path".to_string(), json!(parsed.path()));
                }
            }
        }
    }
    Ok(body)
}

/// Generic create handler — 通用创建处理
async fn do_create<T: Entity + Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    body: Value,
) -> (StatusCode, Json<Value>) {
    // Auto-inject id and timestamps (Kong-compatible: these fields are optional on create) — 自动注入 id 和时间戳（Kong 兼容：创建时这些字段可选）
    let mut body = body;
    if let Some(obj) = body.as_object_mut() {
        if !obj.contains_key("id") {
            obj.insert("id".to_string(), json!(uuid::Uuid::new_v4()));
        }
        let now = chrono::Utc::now().timestamp();
        if !obj.contains_key("created_at") {
            obj.insert("created_at".to_string(), json!(now));
        }
        if !obj.contains_key("updated_at") {
            obj.insert("updated_at".to_string(), json!(now));
        }
        // Kong-compatible: url field is a shorthand for protocol + host + port + path — Kong 兼容：url 字段是 protocol + host + port + path 的快捷方式
        if let Some(url_val) = obj.remove("url") {
            if let Some(url_str) = url_val.as_str() {
                // Validate URL is not empty or invalid — 验证 URL 不为空或无效
                if url_str.is_empty() {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": "schema violation (url: missing host in url)",
                            "name": "schema violation",
                            "code": 2,
                        })),
                    );
                }
                match url::Url::parse(url_str) {
                    Ok(parsed) => {
                        // Validate host is not empty — 验证 host 不为空
                        if parsed.host_str().map_or(true, |h| h.is_empty()) {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(json!({
                                    "message": "schema violation (url: missing host in url)",
                                    "name": "schema violation",
                                    "code": 2,
                                })),
                            );
                        }
                        if !obj.contains_key("protocol") {
                            obj.insert("protocol".to_string(), json!(parsed.scheme()));
                        }
                        if !obj.contains_key("host") {
                            if let Some(host) = parsed.host_str() {
                                obj.insert("host".to_string(), json!(host));
                            }
                        }
                        if !obj.contains_key("port") {
                            if let Some(port) = parsed.port_or_known_default() {
                                obj.insert("port".to_string(), json!(port));
                            }
                        }
                        // Only set path if the original URL explicitly includes a path component — 仅当原始 URL 明确包含路径部分时才设置 path
                        let has_explicit_path = {
                            let after_scheme = url_str.find("://").map(|i| i + 3).unwrap_or(0);
                            url_str[after_scheme..].find('/').is_some()
                        };
                        if has_explicit_path && !obj.contains_key("path") {
                            obj.insert("path".to_string(), json!(parsed.path()));
                        }
                    }
                    Err(_) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "message": "schema violation (url: missing host in url)",
                                "name": "schema violation",
                                "code": 2,
                            })),
                        );
                    }
                }
            }
        }
    }

    // Entity-specific validation before deserialization — 实体类型特定的前置验证
    if let Some(obj) = body.as_object() {
        // Issue 1: Service.host is required and must be non-empty — Service.host 必填且不能为空
        if T::table_name() == "services" {
            let host_missing = match obj.get("host") {
                None => true,
                Some(Value::Null) => true,
                Some(h) => h.as_str().map_or(false, |s| s.is_empty()),
            };
            if host_missing {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": "schema violation (host: required field missing)",
                        "name": "schema violation",
                        "code": 2,
                    })),
                );
            }
        }

        // Issue 2: Consumer requires at least one of username or custom_id — Consumer 至少需要 username 或 custom_id 之一
        if T::table_name() == "consumers" {
            let has_username = obj
                .get("username")
                .and_then(|v| v.as_str())
                .map_or(false, |s| !s.is_empty());
            let has_custom_id = obj
                .get("custom_id")
                .and_then(|v| v.as_str())
                .map_or(false, |s| !s.is_empty());
            if !has_username && !has_custom_id {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": "schema violation (at least one of these fields must be non-empty: 'custom_id', 'username')",
                        "name": "schema violation",
                        "code": 2,
                    })),
                );
            }
        }
    }

    let entity: T = match serde_json::from_value(body) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": format!("schema violation: {}", e),
                    "name": "schema violation",
                    "code": 2,
                })),
            );
        }
    };

    match dao.insert(&entity).await {
        Ok(created) => {
            let body = serde_json::to_value(&created).unwrap_or(json!(null));
            (StatusCode::CREATED, Json(body))
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (
                status,
                Json(json!({
                    "message": e.to_string(),
                    "name": e.error_name(),
                    "code": e.error_code(),
                })),
            )
        }
    }
}

/// Generic update handler — 通用更新处理
async fn do_update<T: Entity + Serialize + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    id_or_name: &str,
    body: &Value,
) -> (StatusCode, Json<Value>) {
    // Parse url shorthand for Service updates — Service 更新时解析 url 快捷方式
    let body = match expand_url_shorthand(body) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let pk = PrimaryKey::from_str_or_uuid(id_or_name);
    match dao.update(&pk, &body).await {
        Ok(updated) => {
            let body = serde_json::to_value(&updated).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (
                status,
                Json(json!({
                    "message": e.to_string(),
                    "name": e.error_name(),
                    "code": e.error_code(),
                })),
            )
        }
    }
}

/// Generic upsert handler — 通用 upsert 处理
async fn do_upsert<T: Entity + Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    id_or_name: &str,
    body: Value,
) -> (StatusCode, Json<Value>) {
    // Parse url shorthand + inject timestamps — 解析 url 快捷方式 + 注入时间戳
    let mut body = match expand_url_shorthand(&body) {
        Ok(b) => b,
        Err(e) => return e,
    };
    if let Some(obj) = body.as_object_mut() {
        let now = chrono::Utc::now().timestamp();
        if !obj.contains_key("created_at") {
            obj.insert("created_at".to_string(), json!(now));
        }
        if !obj.contains_key("updated_at") {
            obj.insert("updated_at".to_string(), json!(now));
        }
    }
    let entity: T = match serde_json::from_value(body) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": format!("schema violation: {}", e),
                    "name": "schema violation",
                    "code": 2,
                })),
            );
        }
    };

    let pk = PrimaryKey::from_str_or_uuid(id_or_name);
    match dao.upsert(&pk, &entity).await {
        Ok(result) => {
            let body = serde_json::to_value(&result).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (
                status,
                Json(json!({
                    "message": e.to_string(),
                    "name": e.error_name(),
                    "code": e.error_code(),
                })),
            )
        }
    }
}

/// Generic delete handler — 通用删除处理
/// Kong-compatible: DELETE is idempotent, returns 204 even if not found — Kong 兼容：DELETE 幂等，即使不存在也返回 204
async fn do_delete<T: Entity + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    id_or_name: &str,
) -> axum::response::Response {
    let pk = PrimaryKey::from_str_or_uuid(id_or_name);
    match dao.delete(&pk).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            // Kong-compatible: DELETE returns 204 even if entity not found — Kong 兼容：即使实体不存在也返回 204
            if e.status_code() == 404 {
                return StatusCode::NO_CONTENT.into_response();
            }
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let body = json!({
                "message": e.to_string(),
                "name": e.error_name(),
                "code": e.error_code(),
            });
            (status, Json(body)).into_response()
        }
    }
}

// ============ Macro-generated concrete type handlers — 宏生成具体类型的 handler ============

/// Generate concrete CRUD handlers for each entity type — 为每个实体类型生成具体的 CRUD handler
macro_rules! entity_handlers {
    ($entity:ty, $dao_field:ident, $entity_name:expr, $list:ident, $get:ident, $create:ident, $update:ident, $upsert:ident, $del:ident) => {
        pub async fn $list(
            State(state): State<AdminState>,
            Query(params): Query<ListParams>,
        ) -> impl IntoResponse {
            do_list::<$entity>(&state.$dao_field, &params).await
        }

        pub async fn $get(
            State(state): State<AdminState>,
            Path(id_or_name): Path<String>,
        ) -> impl IntoResponse {
            do_get::<$entity>(&state.$dao_field, &id_or_name).await
        }

        pub async fn $create(
            State(state): State<AdminState>,
            FlexibleBody(body): FlexibleBody,
        ) -> impl IntoResponse {
            let result = do_create::<$entity>(&state.$dao_field, body).await;
            if result.0.is_success() && !$entity_name.is_empty() {
                let _ = state.refresh_tx.send($entity_name);
            }
            result
        }

        pub async fn $update(
            State(state): State<AdminState>,
            Path(id_or_name): Path<String>,
            FlexibleBody(body): FlexibleBody,
        ) -> impl IntoResponse {
            let result = do_update::<$entity>(&state.$dao_field, &id_or_name, &body).await;
            if result.0.is_success() && !$entity_name.is_empty() {
                let _ = state.refresh_tx.send($entity_name);
            }
            result
        }

        pub async fn $upsert(
            State(state): State<AdminState>,
            Path(id_or_name): Path<String>,
            FlexibleBody(body): FlexibleBody,
        ) -> impl IntoResponse {
            let result = do_upsert::<$entity>(&state.$dao_field, &id_or_name, body).await;
            if result.0.is_success() && !$entity_name.is_empty() {
                let _ = state.refresh_tx.send($entity_name);
            }
            result
        }

        pub async fn $del(
            State(state): State<AdminState>,
            Path(id_or_name): Path<String>,
        ) -> impl IntoResponse {
            let result = do_delete::<$entity>(&state.$dao_field, &id_or_name).await;
            if !$entity_name.is_empty() {
                let _ = state.refresh_tx.send($entity_name);
            }
            result
        }
    };
}

// Generate handlers for each entity type — 为每个实体类型生成 handler
entity_handlers!(
    Service,
    services,
    "services",
    list_services,
    get_service,
    create_service,
    update_service,
    upsert_service,
    delete_service
);
entity_handlers!(
    Route,
    routes,
    "routes",
    list_routes,
    get_route,
    create_route,
    update_route,
    upsert_route,
    delete_route
);
entity_handlers!(
    Consumer,
    consumers,
    "",
    list_consumers,
    get_consumer,
    create_consumer,
    update_consumer,
    upsert_consumer,
    delete_consumer
);
entity_handlers!(
    Plugin,
    plugins,
    "plugins",
    list_plugins,
    get_plugin,
    create_plugin,
    update_plugin,
    upsert_plugin,
    delete_plugin
);
entity_handlers!(
    Upstream,
    upstreams,
    "upstreams",
    list_upstreams,
    get_upstream,
    create_upstream,
    update_upstream,
    upsert_upstream,
    delete_upstream
);
entity_handlers!(
    Certificate,
    certificates,
    "certificates",
    list_certificates,
    get_certificate,
    create_certificate,
    update_certificate,
    upsert_certificate,
    delete_certificate
);
entity_handlers!(
    Sni, snis, "snis", list_snis, get_sni, create_sni, update_sni, upsert_sni, delete_sni
);
entity_handlers!(
    CaCertificate,
    ca_certificates,
    "ca_certificates",
    list_ca_certificates,
    get_ca_certificate,
    create_ca_certificate,
    update_ca_certificate,
    upsert_ca_certificate,
    delete_ca_certificate
);
entity_handlers!(
    Vault,
    vaults,
    "",
    list_vaults,
    get_vault,
    create_vault,
    update_vault,
    upsert_vault,
    delete_vault
);

// ============ Nested endpoints — 嵌套端点 ============

/// GET /services/:service_id/routes
pub async fn list_nested_routes(
    State(state): State<AdminState>,
    Path(service_id_or_name): Path<String>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    // Resolve service ID first — 先解析 service ID
    let service_pk = PrimaryKey::from_str_or_uuid(&service_id_or_name);
    let service = match state.services.select(&service_pk).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "service not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    match state
        .routes
        .select_by_foreign_key("service", &service.id, &params.to_page_params())
        .await
    {
        Ok(page) => (
            StatusCode::OK,
            Json(build_page_response(&page)),
        ),
        Err(e) => (
            StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"message": e.to_string()})),
        ),
    }
}

/// GET /services/:service_id/plugins
pub async fn list_service_plugins(
    State(state): State<AdminState>,
    Path(service_id_or_name): Path<String>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let service_pk = PrimaryKey::from_str_or_uuid(&service_id_or_name);
    let service = match state.services.select(&service_pk).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "service not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };
    match state
        .plugins
        .select_by_foreign_key("service", &service.id, &params.to_page_params())
        .await
    {
        Ok(page) => (
            StatusCode::OK,
            Json(build_page_response(&page)),
        ),
        Err(e) => (
            StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"message": e.to_string()})),
        ),
    }
}

async fn create_scoped_plugin(
    state: &AdminState,
    mut body: Value,
    scope_field: &str,
    scope_id: uuid::Uuid,
) -> (StatusCode, Json<Value>) {
    let Some(obj) = body.as_object_mut() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "schema violation: expected JSON object",
                "name": "schema violation",
                "code": 2,
            })),
        );
    };

    obj.insert(scope_field.to_string(), json!(ForeignKey::new(scope_id)));
    do_create::<Plugin>(&state.plugins, body).await
}

fn plugin_scope_matches(plugin: &Plugin, scope_field: &str, scope_id: uuid::Uuid) -> bool {
    match scope_field {
        "service" => plugin.service.as_ref().map(|fk| fk.id) == Some(scope_id),
        "route" => plugin.route.as_ref().map(|fk| fk.id) == Some(scope_id),
        "consumer" => plugin.consumer.as_ref().map(|fk| fk.id) == Some(scope_id),
        _ => false,
    }
}

async fn get_scoped_plugin(
    dao: &Arc<dyn Dao<Plugin>>,
    plugin_id_or_name: &str,
    scope_field: &str,
    scope_id: uuid::Uuid,
) -> (StatusCode, Json<Value>) {
    let pk = PrimaryKey::from_str_or_uuid(plugin_id_or_name);
    match dao.select(&pk).await {
        Ok(Some(plugin)) if plugin_scope_matches(&plugin, scope_field, scope_id) => (
            StatusCode::OK,
            Json(serde_json::to_value(plugin).unwrap_or(json!(null))),
        ),
        Ok(Some(_)) | Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "message": "plugins not found",
                "name": "not found",
                "code": 3,
            })),
        ),
        Err(e) => (
            StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"message": e.to_string()})),
        ),
    }
}

async fn update_scoped_plugin(
    dao: &Arc<dyn Dao<Plugin>>,
    plugin_id_or_name: &str,
    mut body: Value,
    scope_field: &str,
    scope_id: uuid::Uuid,
    upsert: bool,
) -> (StatusCode, Json<Value>) {
    let existing = get_scoped_plugin(dao, plugin_id_or_name, scope_field, scope_id).await;
    if existing.0 == StatusCode::NOT_FOUND {
        return existing;
    }

    if let Some(obj) = body.as_object_mut() {
        obj.insert(scope_field.to_string(), json!(ForeignKey::new(scope_id)));
    }

    if upsert {
        do_upsert::<Plugin>(dao, plugin_id_or_name, body).await
    } else {
        do_update::<Plugin>(dao, plugin_id_or_name, &body).await
    }
}

async fn delete_scoped_plugin(
    dao: &Arc<dyn Dao<Plugin>>,
    plugin_id_or_name: &str,
    scope_field: &str,
    scope_id: uuid::Uuid,
) -> axum::response::Response {
    let existing = get_scoped_plugin(dao, plugin_id_or_name, scope_field, scope_id).await;
    if existing.0 == StatusCode::NOT_FOUND {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "message": "plugins not found",
                "name": "not found",
                "code": 3,
            })),
        )
            .into_response();
    }

    do_delete::<Plugin>(dao, plugin_id_or_name).await
}

/// POST /services/:service_id/plugins
pub async fn create_service_plugin(
    State(state): State<AdminState>,
    Path(service_id_or_name): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let service_pk = PrimaryKey::from_str_or_uuid(&service_id_or_name);
    let service = match state.services.select(&service_pk).await {
        Ok(Some(service)) => service,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "service not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    let result = create_scoped_plugin(&state, body, "service", service.id).await;
    let _ = state.refresh_tx.send("plugins");
    result
}

/// GET /services/:service_id/plugins/:plugin_id
pub async fn get_service_plugin(
    State(state): State<AdminState>,
    Path((service_id_or_name, plugin_id_or_name)): Path<(String, String)>,
) -> impl IntoResponse {
    let service_pk = PrimaryKey::from_str_or_uuid(&service_id_or_name);
    let service = match state.services.select(&service_pk).await {
        Ok(Some(service)) => service,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "service not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    get_scoped_plugin(&state.plugins, &plugin_id_or_name, "service", service.id).await
}

/// PATCH /services/:service_id/plugins/:plugin_id
pub async fn update_service_plugin(
    State(state): State<AdminState>,
    Path((service_id_or_name, plugin_id_or_name)): Path<(String, String)>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let service_pk = PrimaryKey::from_str_or_uuid(&service_id_or_name);
    let service = match state.services.select(&service_pk).await {
        Ok(Some(service)) => service,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "service not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    let result = update_scoped_plugin(
        &state.plugins,
        &plugin_id_or_name,
        body,
        "service",
        service.id,
        false,
    )
    .await;
    let _ = state.refresh_tx.send("plugins");
    result
}

/// PUT /services/:service_id/plugins/:plugin_id
pub async fn upsert_service_plugin(
    State(state): State<AdminState>,
    Path((service_id_or_name, plugin_id_or_name)): Path<(String, String)>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let service_pk = PrimaryKey::from_str_or_uuid(&service_id_or_name);
    let service = match state.services.select(&service_pk).await {
        Ok(Some(service)) => service,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "service not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    let result = update_scoped_plugin(
        &state.plugins,
        &plugin_id_or_name,
        body,
        "service",
        service.id,
        true,
    )
    .await;
    let _ = state.refresh_tx.send("plugins");
    result
}

/// DELETE /services/:service_id/plugins/:plugin_id
pub async fn delete_service_plugin(
    State(state): State<AdminState>,
    Path((service_id_or_name, plugin_id_or_name)): Path<(String, String)>,
) -> impl IntoResponse {
    let service_pk = PrimaryKey::from_str_or_uuid(&service_id_or_name);
    let service = match state.services.select(&service_pk).await {
        Ok(Some(service)) => service,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "service not found"})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            )
                .into_response();
        }
    };

    let _ = state.refresh_tx.send("plugins");
    delete_scoped_plugin(&state.plugins, &plugin_id_or_name, "service", service.id).await
}

/// GET /routes/:route_id/plugins
pub async fn list_route_plugins(
    State(state): State<AdminState>,
    Path(route_id_or_name): Path<String>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let route_pk = PrimaryKey::from_str_or_uuid(&route_id_or_name);
    let route = match state.routes.select(&route_pk).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "route not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };
    match state
        .plugins
        .select_by_foreign_key("route", &route.id, &params.to_page_params())
        .await
    {
        Ok(page) => (
            StatusCode::OK,
            Json(build_page_response(&page)),
        ),
        Err(e) => (
            StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"message": e.to_string()})),
        ),
    }
}

/// POST /routes/:route_id/plugins
pub async fn create_route_plugin(
    State(state): State<AdminState>,
    Path(route_id_or_name): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let route_pk = PrimaryKey::from_str_or_uuid(&route_id_or_name);
    let route = match state.routes.select(&route_pk).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "route not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    let result = create_scoped_plugin(&state, body, "route", route.id).await;
    let _ = state.refresh_tx.send("plugins");
    result
}

pub async fn get_route_plugin(
    State(state): State<AdminState>,
    Path((route_id_or_name, plugin_id_or_name)): Path<(String, String)>,
) -> impl IntoResponse {
    let route_pk = PrimaryKey::from_str_or_uuid(&route_id_or_name);
    let route = match state.routes.select(&route_pk).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "route not found"})),
            )
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            )
        }
    };
    get_scoped_plugin(&state.plugins, &plugin_id_or_name, "route", route.id).await
}

pub async fn update_route_plugin(
    State(state): State<AdminState>,
    Path((route_id_or_name, plugin_id_or_name)): Path<(String, String)>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let route_pk = PrimaryKey::from_str_or_uuid(&route_id_or_name);
    let route = match state.routes.select(&route_pk).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "route not found"})),
            )
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            )
        }
    };
    let result = update_scoped_plugin(
        &state.plugins,
        &plugin_id_or_name,
        body,
        "route",
        route.id,
        false,
    )
    .await;
    let _ = state.refresh_tx.send("plugins");
    result
}

pub async fn upsert_route_plugin(
    State(state): State<AdminState>,
    Path((route_id_or_name, plugin_id_or_name)): Path<(String, String)>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let route_pk = PrimaryKey::from_str_or_uuid(&route_id_or_name);
    let route = match state.routes.select(&route_pk).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "route not found"})),
            )
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            )
        }
    };
    let result = update_scoped_plugin(
        &state.plugins,
        &plugin_id_or_name,
        body,
        "route",
        route.id,
        true,
    )
    .await;
    let _ = state.refresh_tx.send("plugins");
    result
}

pub async fn delete_route_plugin(
    State(state): State<AdminState>,
    Path((route_id_or_name, plugin_id_or_name)): Path<(String, String)>,
) -> impl IntoResponse {
    let route_pk = PrimaryKey::from_str_or_uuid(&route_id_or_name);
    let route = match state.routes.select(&route_pk).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "route not found"})),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            )
                .into_response()
        }
    };
    let _ = state.refresh_tx.send("plugins");
    delete_scoped_plugin(&state.plugins, &plugin_id_or_name, "route", route.id).await
}

/// GET /consumers/:consumer_id/plugins
pub async fn list_consumer_plugins(
    State(state): State<AdminState>,
    Path(consumer_id_or_name): Path<String>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let consumer_pk = PrimaryKey::from_str_or_uuid(&consumer_id_or_name);
    let consumer = match state.consumers.select(&consumer_pk).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "consumer not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };
    match state
        .plugins
        .select_by_foreign_key("consumer", &consumer.id, &params.to_page_params())
        .await
    {
        Ok(page) => (
            StatusCode::OK,
            Json(build_page_response(&page)),
        ),
        Err(e) => (
            StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"message": e.to_string()})),
        ),
    }
}

/// POST /consumers/:consumer_id/plugins
pub async fn create_consumer_plugin(
    State(state): State<AdminState>,
    Path(consumer_id_or_name): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let consumer_pk = PrimaryKey::from_str_or_uuid(&consumer_id_or_name);
    let consumer = match state.consumers.select(&consumer_pk).await {
        Ok(Some(consumer)) => consumer,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "consumer not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    let result = create_scoped_plugin(&state, body, "consumer", consumer.id).await;
    let _ = state.refresh_tx.send("plugins");
    result
}

pub async fn get_consumer_plugin(
    State(state): State<AdminState>,
    Path((consumer_id_or_name, plugin_id_or_name)): Path<(String, String)>,
) -> impl IntoResponse {
    let consumer_pk = PrimaryKey::from_str_or_uuid(&consumer_id_or_name);
    let consumer = match state.consumers.select(&consumer_pk).await {
        Ok(Some(consumer)) => consumer,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "consumer not found"})),
            )
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            )
        }
    };
    get_scoped_plugin(&state.plugins, &plugin_id_or_name, "consumer", consumer.id).await
}

pub async fn update_consumer_plugin(
    State(state): State<AdminState>,
    Path((consumer_id_or_name, plugin_id_or_name)): Path<(String, String)>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let consumer_pk = PrimaryKey::from_str_or_uuid(&consumer_id_or_name);
    let consumer = match state.consumers.select(&consumer_pk).await {
        Ok(Some(consumer)) => consumer,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "consumer not found"})),
            )
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            )
        }
    };
    let result = update_scoped_plugin(
        &state.plugins,
        &plugin_id_or_name,
        body,
        "consumer",
        consumer.id,
        false,
    )
    .await;
    let _ = state.refresh_tx.send("plugins");
    result
}

pub async fn upsert_consumer_plugin(
    State(state): State<AdminState>,
    Path((consumer_id_or_name, plugin_id_or_name)): Path<(String, String)>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let consumer_pk = PrimaryKey::from_str_or_uuid(&consumer_id_or_name);
    let consumer = match state.consumers.select(&consumer_pk).await {
        Ok(Some(consumer)) => consumer,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "consumer not found"})),
            )
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            )
        }
    };
    let result = update_scoped_plugin(
        &state.plugins,
        &plugin_id_or_name,
        body,
        "consumer",
        consumer.id,
        true,
    )
    .await;
    let _ = state.refresh_tx.send("plugins");
    result
}

pub async fn delete_consumer_plugin(
    State(state): State<AdminState>,
    Path((consumer_id_or_name, plugin_id_or_name)): Path<(String, String)>,
) -> impl IntoResponse {
    let consumer_pk = PrimaryKey::from_str_or_uuid(&consumer_id_or_name);
    let consumer = match state.consumers.select(&consumer_pk).await {
        Ok(Some(consumer)) => consumer,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "consumer not found"})),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            )
                .into_response()
        }
    };
    let _ = state.refresh_tx.send("plugins");
    delete_scoped_plugin(&state.plugins, &plugin_id_or_name, "consumer", consumer.id).await
}

/// POST /services/:service_id/routes
pub async fn create_nested_route(
    State(state): State<AdminState>,
    Path(service_id_or_name): Path<String>,
    FlexibleBody(mut body): FlexibleBody,
) -> impl IntoResponse {
    // Resolve service — 解析 service
    let service_pk = PrimaryKey::from_str_or_uuid(&service_id_or_name);
    let service = match state.services.select(&service_pk).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "service not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    // Inject service FK — 注入 service FK
    if let Some(obj) = body.as_object_mut() {
        obj.insert("service".to_string(), json!({"id": service.id.to_string()}));
    }

    let result = do_create::<Route>(&state.routes, body).await;
    if result.0.is_success() {
        let _ = state.refresh_tx.send("routes");
    }
    result
}

/// GET /upstreams/:upstream_id/targets
pub async fn list_nested_targets(
    State(state): State<AdminState>,
    Path(upstream_id_or_name): Path<String>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let upstream_pk = PrimaryKey::from_str_or_uuid(&upstream_id_or_name);
    let upstream = match state.upstreams.select(&upstream_pk).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "upstream not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    match state
        .targets
        .select_by_foreign_key("upstream", &upstream.id, &params.to_page_params())
        .await
    {
        Ok(page) => (
            StatusCode::OK,
            Json(build_page_response(&page)),
        ),
        Err(e) => (
            StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"message": e.to_string()})),
        ),
    }
}

/// POST /upstreams/:upstream_id/targets
pub async fn create_nested_target(
    State(state): State<AdminState>,
    Path(upstream_id_or_name): Path<String>,
    FlexibleBody(mut body): FlexibleBody,
) -> impl IntoResponse {
    // Resolve upstream — 解析 upstream
    let upstream_pk = PrimaryKey::from_str_or_uuid(&upstream_id_or_name);
    let upstream = match state.upstreams.select(&upstream_pk).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "upstream not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    // Inject upstream FK — 注入 upstream FK
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "upstream".to_string(),
            json!({"id": upstream.id.to_string()}),
        );
    }

    let target: Target = match serde_json::from_value(body) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"message": format!("schema violation: {}", e)})),
            );
        }
    };

    match state.targets.insert(&target).await {
        Ok(created) => {
            let _ = state.refresh_tx.send("targets");
            let body = serde_json::to_value(&created).unwrap_or(json!(null));
            (StatusCode::CREATED, Json(body))
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

/// GET /upstreams/:upstream_id/targets/:id
pub async fn get_nested_target(
    State(state): State<AdminState>,
    Path((upstream_id_or_name, target_id_or_name)): Path<(String, String)>,
) -> impl IntoResponse {
    // Verify upstream exists — 验证 upstream 存在
    let upstream_pk = PrimaryKey::from_str_or_uuid(&upstream_id_or_name);
    if let Ok(None) = state.upstreams.select(&upstream_pk).await {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"message": "upstream not found"})),
        );
    }

    let pk = PrimaryKey::from_str_or_uuid(&target_id_or_name);
    match state.targets.select(&pk).await {
        Ok(Some(target)) => {
            let body = serde_json::to_value(&target).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"message": "target not found"})),
        ),
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

/// PATCH /upstreams/:upstream_id/targets/:id
pub async fn update_nested_target(
    State(state): State<AdminState>,
    Path((_upstream_id_or_name, target_id_or_name)): Path<(String, String)>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let pk = PrimaryKey::from_str_or_uuid(&target_id_or_name);
    match state.targets.update(&pk, &body).await {
        Ok(updated) => {
            let _ = state.refresh_tx.send("targets");
            let body = serde_json::to_value(&updated).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

/// DELETE /upstreams/:upstream_id/targets/:id
pub async fn delete_nested_target(
    State(state): State<AdminState>,
    Path((_upstream_id_or_name, target_id_or_name)): Path<(String, String)>,
) -> impl IntoResponse {
    let pk = PrimaryKey::from_str_or_uuid(&target_id_or_name);
    match state.targets.delete(&pk).await {
        Ok(_) => {
            let _ = state.refresh_tx.send("targets");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let body = json!({"message": e.to_string()});
            (status, Json(body)).into_response()
        }
    }
}

// ============ Tags API — 标签 API ============

/// Collect tags from a single entity table and append to results — 从单个实体表收集标签并追加到结果
async fn collect_tags_from<T: Entity + Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    entity_name: &str,
    tag_filter: Option<&str>,
    results: &mut Vec<Value>,
) {
    let params = PageParams {
        size: 10000,
        offset: None,
        tags: None,
    };
    if let Ok(page) = dao.page(&params).await {
        for entity in &page.data {
            if let Some(tags) = entity.tags() {
                for tag in tags {
                    // If filtering by tag, skip non-matching — 如果按标签过滤，跳过不匹配的
                    if let Some(filter) = tag_filter {
                        if tag != filter {
                            continue;
                        }
                    }
                    results.push(json!({
                        "entity_name": entity_name,
                        "entity_id": entity.id().to_string(),
                        "tag": tag,
                    }));
                }
            }
        }
    }
}

/// GET /tags — List all tags across all entity types — GET /tags — 列出所有实体类型的全部标签
pub async fn list_all_tags(State(state): State<AdminState>) -> impl IntoResponse {
    let mut results = Vec::new();

    collect_tags_from(&state.services, "services", None, &mut results).await;
    collect_tags_from(&state.routes, "routes", None, &mut results).await;
    collect_tags_from(&state.consumers, "consumers", None, &mut results).await;
    collect_tags_from(&state.plugins, "plugins", None, &mut results).await;
    collect_tags_from(&state.upstreams, "upstreams", None, &mut results).await;
    collect_tags_from(&state.targets, "targets", None, &mut results).await;
    collect_tags_from(&state.certificates, "certificates", None, &mut results).await;
    collect_tags_from(&state.snis, "snis", None, &mut results).await;
    collect_tags_from(&state.ca_certificates, "ca_certificates", None, &mut results).await;
    collect_tags_from(&state.vaults, "vaults", None, &mut results).await;

    Json(json!({
        "data": results,
        "offset": null,
        "next": null,
    }))
}

/// GET /tags/:tag — List entities filtered by a specific tag — GET /tags/:tag — 按指定标签过滤实体
pub async fn list_by_tag(
    State(state): State<AdminState>,
    Path(tag): Path<String>,
) -> impl IntoResponse {
    let mut results = Vec::new();
    let tag_ref = tag.as_str();

    collect_tags_from(&state.services, "services", Some(tag_ref), &mut results).await;
    collect_tags_from(&state.routes, "routes", Some(tag_ref), &mut results).await;
    collect_tags_from(&state.consumers, "consumers", Some(tag_ref), &mut results).await;
    collect_tags_from(&state.plugins, "plugins", Some(tag_ref), &mut results).await;
    collect_tags_from(&state.upstreams, "upstreams", Some(tag_ref), &mut results).await;
    collect_tags_from(&state.targets, "targets", Some(tag_ref), &mut results).await;
    collect_tags_from(&state.certificates, "certificates", Some(tag_ref), &mut results).await;
    collect_tags_from(&state.snis, "snis", Some(tag_ref), &mut results).await;
    collect_tags_from(&state.ca_certificates, "ca_certificates", Some(tag_ref), &mut results).await;
    collect_tags_from(&state.vaults, "vaults", Some(tag_ref), &mut results).await;

    Json(json!({
        "data": results,
        "offset": null,
        "next": null,
    }))
}

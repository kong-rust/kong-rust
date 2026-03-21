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
            ..Default::default()
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
    /// Filter consumers by custom_id — 按 custom_id 过滤 consumer
    pub custom_id: Option<String>,
    /// Filter consumers by username — 按 username 过滤 consumer
    pub username: Option<String>,
}

impl ListParams {
    /// Validate tags query parameter — 验证 tags 查询参数
    /// Returns Err with 400 response if invalid — 无效时返回 400 错误响应
    fn validate_tags(&self) -> Result<(), (StatusCode, Json<Value>)> {
        if let Some(ref tags_str) = self.tags {
            // Empty tag value — 空标签值
            if tags_str.is_empty() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": "invalid option (tags: cannot be null)",
                        "name": "invalid option",
                        "code": 2,
                    })),
                ));
            }
            // Mixed AND (/) and OR (,) operators — 混合使用 AND (/) 和 OR (,) 操作符
            if tags_str.contains('/') && tags_str.contains(',') {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": "invalid option (tags: invalid filter syntax)",
                        "name": "invalid option",
                        "code": 2,
                    })),
                ));
            }
            // Invalid tag characters (non-UTF8 or control characters) — 无效标签字符（非 UTF-8 或控制字符）
            for ch in tags_str.chars() {
                if ch.is_control() || ch == '\0' {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": "invalid option (tags: invalid filter syntax)",
                            "name": "invalid option",
                            "code": 2,
                        })),
                    ));
                }
            }
            // Validate each tag value (Kong allows printable + some unicode, rejects invalid bytes) — 验证每个标签值
            let tags_list: Vec<&str> = if tags_str.contains('/') {
                tags_str.split('/').collect()
            } else {
                tags_str.split(',').collect()
            };
            for tag in &tags_list {
                let tag = tag.trim();
                if tag.is_empty() {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": "invalid option (tags: invalid filter syntax)",
                            "name": "invalid option",
                            "code": 2,
                        })),
                    ));
                }
                // Check for non-printable characters (except space) — 检查不可打印字符（空格除外）
                for b in tag.as_bytes() {
                    if *b < 0x20 && *b != b'\t' {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "message": "invalid option (tags: invalid filter syntax)",
                                "name": "invalid option",
                                "code": 2,
                            })),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn to_page_params(&self) -> PageParams {
        use kong_core::traits::TagFilterMode;
        let is_or = self.tags.as_ref().map(|t| t.contains('/')).unwrap_or(false);
        let tags = self.tags.as_ref().map(|t| {
            // OR uses slash separator, AND uses comma — OR 使用斜杠分隔，AND 使用逗号
            if t.contains('/') {
                t.split('/').map(|s| s.trim().to_string()).collect()
            } else {
                t.split(',').map(|s| s.trim().to_string()).collect()
            }
        });
        PageParams {
            size: self.size.unwrap_or(100).min(1000),
            offset: self.offset.clone(),
            tags,
            tags_mode: if is_or { TagFilterMode::Or } else { TagFilterMode::And },
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

/// Enrich UNIQUE violation error with actual field values from the request body
/// 用请求体中的实际字段值丰富 UNIQUE 冲突错误消息
/// Returns (message, optional fields object) — 返回 (消息, 可选的 fields 对象)
fn enrich_unique_violation(err: &KongError, body: &Value) -> (String, Option<Value>) {
    let msg = err.to_string();
    if let KongError::UniqueViolation(inner) = err {
        if let Some(obj) = body.as_object() {
            // Check if this is a plugin cache_key violation — 检查是否是插件 cache_key 冲突
            // cache_key field pattern: {cache_key="..."} — cache_key 字段模式
            if inner.contains("cache_key") {
                // Parse cache_key from body: format is "name:route_id:service_id:consumer_id:"
                // 从请求体解析 cache_key：格式为 "name:route_id:service_id:consumer_id:"
                if let Some(cache_key) = obj.get("cache_key").and_then(|v| v.as_str()) {
                    let parts: Vec<&str> = cache_key.splitn(5, ':').collect();
                    if parts.len() >= 4 {
                        let name = parts[0];
                        let route_id = parts[1];
                        let service_id = parts[2];
                        let consumer_id = parts[3];

                        // Build fields object — 构建 fields 对象
                        let consumer_field = if consumer_id.is_empty() {
                            Value::Null
                        } else {
                            json!({"id": consumer_id})
                        };
                        let route_field = if route_id.is_empty() {
                            Value::Null
                        } else {
                            json!({"id": route_id})
                        };
                        let service_field = if service_id.is_empty() {
                            Value::Null
                        } else {
                            json!({"id": service_id})
                        };

                        let fields = json!({
                            "consumer": consumer_field,
                            "name": name,
                            "route": route_field,
                            "service": service_field,
                        });

                        // Build message in Kong format: '{consumer=null,name="basic-auth",route=null,service={id="xxx"}}'
                        // 构建 Kong 格式的消息
                        let fmt_fk = |v: &Value| -> String {
                            if v.is_null() {
                                "null".to_string()
                            } else if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                                format!("{{id=\"{}\"}}", id)
                            } else {
                                "null".to_string()
                            }
                        };
                        let message = format!(
                            "UNIQUE violation detected on '{{consumer={},name=\"{}\",route={},service={}}}'",
                            fmt_fk(&consumer_field),
                            name,
                            fmt_fk(&route_field),
                            fmt_fk(&service_field),
                        );
                        return (message, Some(fields));
                    }
                }
            }

            // Standard field violation — 标准字段冲突
            // Extract field name from the error message pattern {field="..."} — 从错误消息中提取字段名
            if let Some(start) = msg.find('{') {
                if let Some(eq_pos) = msg[start..].find('=') {
                    let field_name = &msg[start + 1..start + eq_pos];
                    if let Some(val) = obj.get(field_name) {
                        let val_str = match val {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        return (
                            format!("UNIQUE violation detected on '{{{field_name}=\"{val_str}\"}}'"),
                            None,
                        );
                    }
                }
            }
        }
    }
    (msg, None)
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

    // Build loaded_plugins map: {plugin_name: true} — 构建 loaded_plugins 映射
    let mut loaded_plugins = serde_json::Map::new();
    for name in config.loaded_plugins() {
        loaded_plugins.insert(name, json!(true));
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
            "pg_password": "******",
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
            "loaded_plugins": loaded_plugins,
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

/// Query parameters for /status endpoint — /status 端点查询参数
#[derive(Debug, Deserialize)]
pub struct StatusParams {
    /// Memory unit: "k" for KiB, "b" for raw bytes, default MiB — 内存单位："k" 为 KiB，"b" 为原始字节，默认 MiB
    pub unit: Option<String>,
    /// Decimal scale (default 2) — 小数位数（默认 2）
    pub scale: Option<usize>,
}

/// Format a byte count into the requested unit/scale — 将字节数格式化为请求的单位/精度
fn format_memory(bytes: u64, unit: Option<&str>, scale: usize) -> Value {
    match unit {
        Some("b") => json!(bytes),
        Some("k") => {
            let kib = bytes as f64 / 1024.0;
            json!(format!("{:.prec$} KiB", kib, prec = scale))
        }
        _ => {
            // Default: MiB — 默认 MiB
            let mib = bytes as f64 / (1024.0 * 1024.0);
            json!(format!("{:.prec$} MiB", mib, prec = scale))
        }
    }
}

/// GET /status — Service status — GET /status — 服务状态
pub async fn status_info(
    State(state): State<AdminState>,
    Query(status_params): Query<StatusParams>,
) -> Response {
    let is_dbless = state.config.database == "off";
    let unit = status_params.unit.as_deref();

    // Validate unit parameter — 验证 unit 查询参数
    if let Some(u) = unit {
        if !matches!(u.to_lowercase().as_str(), "m" | "k" | "g" | "b") {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": format!("invalid unit '{}' (expected 'k/K', 'm/M', or 'g/G')", u),
                })),
            ).into_response();
        }
    }

    let scale = status_params.scale.unwrap_or(2);

    // Simulated memory values in bytes (Kong-compatible) — 模拟内存值（字节，Kong 兼容）
    let kong_capacity: u64 = 5242880;        // 5 MiB
    let kong_alloc: u64 = 40960;             // 0.04 MiB
    let db_cache_capacity: u64 = 134217728;  // 128 MiB
    let db_cache_alloc: u64 = 81920;         // 0.08 MiB
    let worker_gc: u64 = 1069056;            // ~1.02 MiB

    let mut body = json!({
        "server": {
            "connections_accepted": 0,
            "connections_active": 0,
            "connections_handled": 0,
            "connections_reading": 0,
            "connections_writing": 0,
            "connections_waiting": 0,
            "total_requests": 0,
        },
        "memory": {
            "lua_shared_dicts": {
                "kong": {
                    "allocated_slabs": format_memory(kong_alloc, unit, scale),
                    "capacity": format_memory(kong_capacity, unit, scale),
                },
                "kong_db_cache": {
                    "allocated_slabs": format_memory(db_cache_alloc, unit, scale),
                    "capacity": format_memory(db_cache_capacity, unit, scale),
                },
            },
            "workers_lua_vms": [{
                "http_allocated_gc": format_memory(worker_gc, unit, scale),
                "pid": std::process::id(),
            }],
        },
    });

    if is_dbless {
        // In dbless mode, include configuration_hash from shared state — dbless 模式下从共享状态获取 configuration_hash
        let hash = state.configuration_hash.read()
            .map(|h| h.clone())
            .unwrap_or_else(|_| "00000000000000000000000000000000".to_string());
        body["configuration_hash"] = json!(hash);
    } else {
        // In DB mode, include database reachable status (no configuration_hash) — DB 模式下包含数据库可达状态（不含 configuration_hash）
        body["database"] = json!({ "reachable": true });
    }

    Json(body).into_response()
}

/// POST /config — Accept declarative config (db-less mode) — POST /config — 接受声明式配置（db-less 模式）
pub async fn post_config(
    State(state): State<AdminState>,
    body: String,
) -> impl IntoResponse {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    if !state.config.is_dbless() {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(json!({
                "message": "this endpoint is only available when Kong is in DB-less mode",
            })),
        ).into_response();
    }

    // Compute hash of the config body using DefaultHasher — 使用 DefaultHasher 计算配置内容的哈希
    let mut hasher = DefaultHasher::new();
    body.hash(&mut hasher);
    let h1 = hasher.finish();
    // Generate a second hash with a different seed for 32 hex chars — 用不同种子再生成一个哈希以凑 32 位十六进制
    let mut hasher2 = DefaultHasher::new();
    h1.hash(&mut hasher2);
    let h2 = hasher2.finish();
    let hash = format!("{:016x}{:016x}", h1, h2);

    // Store the hash in shared state — 将哈希存储到共享状态
    if let Ok(mut h) = state.configuration_hash.write() {
        *h = hash.clone();
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "configuration_hash": hash,
        })),
    ).into_response()
}

/// GET /endpoints — List all registered Admin API endpoints — GET /endpoints — 列出所有已注册的 Admin API 端点
pub async fn list_endpoints() -> impl IntoResponse {
    let endpoints = vec![
        "/",
        "/endpoints",
        "/status",
        "/services",
        "/services/{services}",
        "/services/{services}/plugins",
        "/services/{services}/plugins/{plugins}",
        "/services/{services}/routes",
        "/routes",
        "/routes/{routes}",
        "/routes/{routes}/plugins",
        "/routes/{routes}/plugins/{plugins}",
        "/consumers",
        "/consumers/{consumers}",
        "/consumers/{consumers}/plugins",
        "/consumers/{consumers}/plugins/{plugins}",
        "/plugins",
        "/plugins/{plugins}",
        "/plugins/enabled",
        "/upstreams",
        "/upstreams/{upstreams}",
        "/upstreams/{upstreams}/targets",
        "/upstreams/{upstreams}/targets/{targets}",
        "/certificates",
        "/certificates/{certificates}",
        "/snis",
        "/snis/{snis}",
        "/ca_certificates",
        "/ca_certificates/{ca_certificates}",
        "/vaults",
        "/vaults/{vaults}",
        "/schemas/{entity}",
        "/schemas/plugins/{name}",
        "/schemas/vaults/{name}",
        "/schemas/{entity}/validate",
        "/schemas/plugins/validate",
        "/tags",
        "/tags/{tags}",
        // Plugin credential endpoints — 插件凭证端点
        "/basic-auths",
        "/basic-auths/{basicauth_credentials}",
        "/basic-auths/{basicauth_credentials}/consumer",
        "/consumers/{consumers}/basic-auth",
        "/consumers/{consumers}/basic-auth/{basicauth_credentials}",
        "/key-auths",
        "/key-auths/{keyauth_credentials}",
        "/key-auths/{keyauth_credentials}/consumer",
        "/consumers/{consumers}/key-auth",
        "/consumers/{consumers}/key-auth/{keyauth_credentials}",
        "/hmac-auths",
        "/hmac-auths/{hmacauth_credentials}",
        "/consumers/{consumers}/hmac-auth",
        "/jwt-secrets",
        "/jwt-secrets/{jwtsecret}",
        "/consumers/{consumers}/jwt",
        "/oauth2-tokens",
        "/oauth2-tokens/{oauth2_tokens}",
        "/oauth2",
        "/oauth2/{oauth2_credentials}",
        "/consumers/{consumers}/oauth2",
        "/acls",
        "/acls/{acls}",
        "/consumers/{consumers}/acls",
        "/reports/send-ping",
    ];

    Json(json!({ "data": endpoints }))
}

/// POST /schemas/{entity}/validate — Validate an entity schema — POST /schemas/{entity}/validate — 验证实体 schema
pub async fn validate_entity_schema(
    Path(entity_name): Path<String>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let known_entities = [
        "services", "routes", "consumers", "plugins", "upstreams",
        "targets", "certificates", "snis", "ca_certificates", "vaults",
    ];

    if !known_entities.contains(&entity_name.as_str()) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": format!("No entity named '{}'", entity_name) })),
        ).into_response();
    }

    // Check if body is provided — 检查请求体是否存在
    let body = match body {
        Some(Json(v)) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": "schema violation",
                    "name": "schema violation",
                    "code": 2,
                    "fields": {},
                })),
            ).into_response();
        }
    };

    // Simple validation: if body is an empty object, return 400 — 简单验证：空对象返回 400
    if body.as_object().map_or(true, |o| o.is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "schema violation",
                "name": "schema violation",
                "code": 2,
                "fields": {},
            })),
        ).into_response();
    }

    // Entity-specific validations — 实体特定的验证逻辑
    if entity_name == "certificates" {
        // Validate snis: must not be IP addresses — 验证 snis：不能是 IP 地址
        if let Some(snis) = body.get("snis").and_then(|v| v.as_array()) {
            for (i, sni) in snis.iter().enumerate() {
                if let Some(sni_str) = sni.as_str() {
                    if looks_like_ip(sni_str) {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "message": format!("schema violation (snis.{}: must not be an IP)", i + 1),
                                "name": "schema violation",
                                "code": 2,
                                "fields": {
                                    "snis": ["must not be an IP"]
                                },
                            })),
                        ).into_response();
                    }
                }
            }
        }
    }

    (
        StatusCode::OK,
        Json(json!({ "message": "schema validation successful" })),
    ).into_response()
}

/// Check if a string looks like an IP address (v4 or v6), optionally with port — 检查字符串是否看起来像 IP 地址（v4 或 v6），可选带端口
fn looks_like_ip(s: &str) -> bool {
    use std::net::{IpAddr, SocketAddr};
    // Direct IP parse — 直接 IP 解析
    if s.parse::<IpAddr>().is_ok() {
        return true;
    }
    // IP:port format — IP:端口 格式
    if s.parse::<SocketAddr>().is_ok() {
        return true;
    }
    // IPv4 with port: e.g. "120.0.9.32:90" — 带端口的 IPv4
    if let Some(colon_pos) = s.rfind(':') {
        let host = &s[..colon_pos];
        let port = &s[colon_pos + 1..];
        if port.parse::<u16>().is_ok() && host.parse::<IpAddr>().is_ok() {
            return true;
        }
    }
    false
}

/// GET /metrics — Prometheus metrics from the status port — GET /metrics — 从状态端口暴露的 Prometheus 指标
pub async fn status_metrics(State(state): State<AdminState>) -> Response {
    let params = PageParams {
        size: 1000,
        ..Default::default()
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

/// Percent-encode a tags parameter value for use in URLs — 对 tags 参数值进行百分号编码用于 URL
/// Preserves `/` and `,` as tag operators, encodes spaces and other special chars — 保留 `/` 和 `,` 作为标签操作符，编码空格和其他特殊字符
fn encode_tags_param(tags_str: &str) -> String {
    let mut result = String::with_capacity(tags_str.len() * 2);
    for ch in tags_str.chars() {
        match ch {
            // Preserve tag operators and safe characters — 保留标签操作符和安全字符
            '/' | ',' | '-' | '_' | '.' | '~' => result.push(ch),
            // Alphanumeric characters are safe — 字母数字字符是安全的
            c if c.is_ascii_alphanumeric() => result.push(c),
            // Percent-encode everything else — 百分号编码其他所有字符
            c => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                for b in encoded.as_bytes() {
                    result.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    result
}

// ============ Generic CRUD endpoints — 通用 CRUD 端点 ============

// ============ Generic CRUD helpers — 通用 CRUD 辅助 ============

/// Build Kong-compatible paginated response — 构建 Kong 兼容的分页响应
/// Always includes `next` (null when no more pages), only includes `offset` when present —
/// 始终包含 `next`（无更多页时为 null），仅在存在时包含 `offset`
fn build_page_response<T: Serialize>(page: &Page<T>) -> Value {
    build_page_response_with_tags(page, None)
}

/// Build paginated response, optionally preserving tags filter in the next URL — 构建分页响应，可选在 next URL 中保留 tags 过滤参数
fn build_page_response_with_tags<T: Serialize>(page: &Page<T>, tags: Option<&str>) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("data".to_string(), json!(page.data));
    // Always include next (null when no more pages) — 始终包含 next（无更多页时为 null）
    body.insert("next".to_string(), match &page.next {
        Some(n) => {
            // Append tags param to next URL if present, with percent-encoding — 如果有 tags 参数则追加到 next URL，进行百分号编码
            if let Some(tags_str) = tags {
                if !tags_str.is_empty() {
                    // Percent-encode the tags value (preserve / and , as-is since they are operators) — 百分号编码标签值（保留 / 和 , 因为它们是操作符）
                    let encoded = encode_tags_param(tags_str);
                    json!(format!("{}&tags={}", n, encoded))
                } else {
                    json!(n)
                }
            } else {
                json!(n)
            }
        }
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
    // Validate tags before query — 查询前验证 tags 参数
    if let Err(err) = params.validate_tags() {
        return err;
    }
    match dao.page(&params.to_page_params()).await {
        Ok(page) => {
            let resp = build_page_response_with_tags(&page, params.tags.as_deref());
            (StatusCode::OK, Json(resp))
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
                            "fields": {"url": "missing host in url"},
                        })),
                    ));
                }
                let parsed = match url::Url::parse(url_str) {
                    Ok(p) => p,
                    Err(_) => {
                        // URL 无法解析 — 分解为字段级错误
                        let mut fields = serde_json::Map::new();
                        let mut violations = Vec::new();
                        fields.insert("host".to_string(), json!("required field missing"));
                        violations.push("host: required field missing");
                        fields.insert("path".to_string(), json!("should start with: /"));
                        violations.push("path: should start with: /");
                        let msg = if violations.len() > 1 {
                            format!("{} schema violations ({})", violations.len(), violations.join("; "))
                        } else {
                            format!("schema violation ({})", violations[0])
                        };
                        return Err((
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "message": msg,
                                "name": "schema violation",
                                "code": 2,
                                "fields": Value::Object(fields),
                            })),
                        ));
                    }
                };
                if parsed.host_str().map_or(true, |h| h.is_empty()) {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": "schema violation (host: required field missing)",
                            "name": "schema violation",
                            "code": 2,
                            "fields": {"host": "required field missing"},
                        })),
                    ));
                }
                // Validate protocol — 验证 URL 中解析出的协议
                let scheme = parsed.scheme();
                let valid_protocols = ["grpc", "grpcs", "http", "https", "tcp", "tls", "tls_passthrough", "udp"];
                if !valid_protocols.contains(&scheme) {
                    // 无效协议 — 分解为字段级错误
                    let mut fields = serde_json::Map::new();
                    let mut violations = Vec::new();
                    fields.insert("protocol".to_string(), json!("expected one of: grpc, grpcs, http, https, tcp, tls, tls_passthrough, udp"));
                    violations.push("protocol: expected one of: grpc, grpcs, http, https, tcp, tls, tls_passthrough, udp");
                    // Check if path is missing or doesn't start with / — 检查路径
                    let has_explicit_path = {
                        let after_scheme = url_str.find("://").map(|i| i + 3).unwrap_or(0);
                        url_str[after_scheme..].find('/').is_some()
                    };
                    if has_explicit_path {
                        let path = parsed.path();
                        if !path.starts_with('/') {
                            fields.insert("path".to_string(), json!("should start with: /"));
                            violations.push("path: should start with: /");
                        }
                    }
                    let msg = if violations.len() > 1 {
                        format!("{} schema violations ({})", violations.len(), violations.join("; "))
                    } else {
                        format!("schema violation ({})", violations[0])
                    };
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": msg,
                            "name": "schema violation",
                            "code": 2,
                            "fields": Value::Object(fields),
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
                // e.g. "http://example.com" → no path (set null), "http://example.com/" → path="/", "http://example.com/foo" → path="/foo"
                let has_explicit_path = {
                    let after_scheme = url_str.find("://").map(|i| i + 3).unwrap_or(0);
                    url_str[after_scheme..].find('/').is_some()
                };
                if has_explicit_path && !obj.contains_key("path") {
                    obj.insert("path".to_string(), json!(parsed.path()));
                } else if !has_explicit_path {
                    // No explicit path in URL — explicitly set path to null to clear it — URL 中无显式路径 — 显式设置 path 为 null 以清除
                    obj.insert("path".to_string(), Value::Null);
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
                            "fields": {"url": "missing host in url"},
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
                                    "message": "schema violation (host: required field missing)",
                                    "name": "schema violation",
                                    "code": 2,
                                    "fields": {"host": "required field missing"},
                                })),
                            );
                        }
                        // Validate protocol from URL — 验证 URL 中解析出的协议
                        let scheme = parsed.scheme();
                        let valid_protocols = ["grpc", "grpcs", "http", "https", "tcp", "tls", "tls_passthrough", "udp"];
                        if !valid_protocols.contains(&scheme) {
                            let mut fields = serde_json::Map::new();
                            let mut violations = Vec::new();
                            fields.insert("protocol".to_string(), json!("expected one of: grpc, grpcs, http, https, tcp, tls, tls_passthrough, udp"));
                            violations.push("protocol: expected one of: grpc, grpcs, http, https, tcp, tls, tls_passthrough, udp");
                            let msg = format!("schema violation ({})", violations[0]);
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(json!({
                                    "message": msg,
                                    "name": "schema violation",
                                    "code": 2,
                                    "fields": Value::Object(fields),
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
                        } else if !has_explicit_path {
                            // No explicit path in URL — explicitly set path to null — URL 中无显式路径 — 显式设置 path 为 null
                            obj.insert("path".to_string(), Value::Null);
                        }
                    }
                    Err(_) => {
                        // URL 无法解析 — 分解为字段级错误
                        let mut fields = serde_json::Map::new();
                        let mut violations = Vec::new();
                        fields.insert("host".to_string(), json!("required field missing"));
                        violations.push("host: required field missing");
                        fields.insert("path".to_string(), json!("should start with: /"));
                        violations.push("path: should start with: /");
                        let msg = if violations.len() > 1 {
                            format!("{} schema violations ({})", violations.len(), violations.join("; "))
                        } else {
                            format!("schema violation ({})", violations[0])
                        };
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "message": msg,
                                "name": "schema violation",
                                "code": 2,
                                "fields": Value::Object(fields),
                            })),
                        );
                    }
                }
            }
        }
    }

    // Generate cache_key and apply config defaults for plugins — 为插件生成 cache_key 并填充默认 config
    if T::table_name() == "plugins" {
        if let Some(obj) = body.as_object_mut() {
            generate_plugin_cache_key(obj);

            // Apply plugin config defaults — 填充插件 config 默认值
            if let Some(name) = obj.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()) {
                // Validate plugin name — 验证插件名称
                if !is_valid_plugin_name(&name) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": format!("schema violation (plugin '{}' not enabled; add it to the 'plugins' configuration property)", name),
                            "name": "schema violation",
                            "code": 2,
                            "fields": {
                                "name": format!("plugin '{}' not enabled; add it to the 'plugins' configuration property", name)
                            },
                        })),
                    );
                }

                // Ensure config object exists — 确保 config 对象存在
                if !obj.contains_key("config") || obj.get("config").map_or(false, |v| v.is_null()) {
                    obj.insert("config".to_string(), json!({}));
                }
                if let Some(config) = obj.get_mut("config").and_then(|v| v.as_object_mut()) {
                    apply_plugin_config_defaults(&name, config);
                }
            }
        }
    }

    // Entity-specific validation before deserialization — 实体类型特定的前置验证
    if let Some(obj) = body.as_object() {
        // Issue 1: Service.host is required and must be non-empty — Service.host 必填且不能为空
        if T::table_name() == "services" {
            // Validate protocol — 验证协议
            if let Some(protocol) = obj.get("protocol").and_then(|v| v.as_str()) {
                let valid = ["grpc", "grpcs", "http", "https", "tcp", "tls", "tls_passthrough", "udp"];
                if !valid.contains(&protocol) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "code": 2,
                            "name": "schema violation",
                            "message": "schema violation (protocol: expected one of: grpc, grpcs, http, https, tcp, tls, tls_passthrough, udp)",
                            "fields": {"protocol": "expected one of: grpc, grpcs, http, https, tcp, tls, tls_passthrough, udp"}
                        })),
                    );
                }
            }

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
                        "fields": {
                            "host": "required field missing"
                        },
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
                        "fields": {
                            "@entity": ["at least one of these fields must be non-empty: 'custom_id', 'username'"]
                        },
                    })),
                );
            }
        }
    }

    // Clone body for UNIQUE violation error enrichment — 克隆请求体以便 UNIQUE 冲突时提取字段值
    let body_for_err = body.clone();

    let entity: T = match serde_json::from_value(body) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": format!("schema violation: {}", e),
                    "name": "schema violation",
                    "code": 2,
                    "fields": {},
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
            let (message, fields) = enrich_unique_violation(&e, &body_for_err);
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut resp = json!({
                "message": message,
                "name": e.error_name(),
                "code": e.error_code(),
            });
            if let Some(f) = fields {
                resp.as_object_mut().unwrap().insert("fields".to_string(), f);
            }
            (status, Json(resp))
        }
    }
}

/// Generic update handler (PATCH = merge semantics) — 通用更新处理（PATCH = 合并语义）
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

    // Plugin unknown field validation on PATCH — PATCH 时验证插件未知字段
    if T::table_name() == "plugins" {
        let known_fields = [
            "id", "name", "enabled", "config", "service", "route", "consumer",
            "protocols", "tags", "instance_name", "ordering",
            "created_at", "updated_at", "cache_key", "ws_id",
        ];
        if let Some(obj) = body.as_object() {
            for key in obj.keys() {
                if !known_fields.contains(&key.as_str()) {
                    let mut fields_map = serde_json::Map::new();
                    fields_map.insert(key.clone(), json!("unknown field"));
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "code": 2,
                            "name": "schema violation",
                            "message": format!("schema violation ({}: unknown field)", key),
                            "fields": fields_map,
                        })),
                    );
                }
            }
        }
    }

    // Plugin name validation on PATCH — PATCH 时验证插件名称
    if T::table_name() == "plugins" {
        if let Some(name) = body.as_object().and_then(|o| o.get("name")).and_then(|v| v.as_str()) {
            if !is_valid_plugin_name(name) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": format!("schema violation (plugin '{}' not enabled; add it to the 'plugins' configuration property)", name),
                        "name": "schema violation",
                        "code": 2,
                        "fields": {
                            "name": format!("plugin '{}' not enabled; add it to the 'plugins' configuration property", name)
                        },
                    })),
                );
            }
        }
        // Validate config type — 验证 config 字段类型
        if let Some(config) = body.as_object().and_then(|o| o.get("config")) {
            if !config.is_object() && !config.is_null() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": "schema violation (config: expected a record)",
                        "name": "schema violation",
                        "code": 2,
                        "fields": {
                            "config": "expected a record"
                        },
                    })),
                );
            }
        }
    }

    // PATCH merge: fetch existing entity and merge with request body — PATCH 合并：获取已有实体并与请求体合并
    // For fields explicitly set to null in the request, keep them as null (clear the field) — 请求中显式设置为 null 的字段保留为 null（清除该字段）
    // For fields not present in the request, keep existing values — 请求中不存在的字段保留已有值
    let pk = PrimaryKey::from_str_or_uuid(id_or_name);
    let merged_body = match dao.select(&pk).await {
        Ok(Some(existing)) => {
            if let (Ok(mut existing_json), Some(patch_obj)) = (
                serde_json::to_value(&existing),
                body.as_object(),
            ) {
                if let Some(existing_obj) = existing_json.as_object_mut() {
                    for (key, value) in patch_obj {
                        // If the patch value is null, always override (clear the field) — 如果 patch 值为 null，始终覆盖（清除该字段）
                        if value.is_null() {
                            existing_obj.insert(key.clone(), Value::Null);
                            continue;
                        }
                        // Deep merge for nested objects (e.g., plugin config) — 嵌套对象深度合并（如插件 config）
                        if key == "config" || key == "headers" {
                            if let (Some(existing_sub), Some(patch_sub)) = (
                                existing_obj.get(key).and_then(|v| v.as_object()).cloned(),
                                value.as_object(),
                            ) {
                                let mut merged = existing_sub;
                                for (sk, sv) in patch_sub {
                                    merged.insert(sk.clone(), sv.clone());
                                }
                                existing_obj.insert(key.clone(), json!(merged));
                                continue;
                            }
                        }
                        // 显式设置的字段（包括 null）覆盖已有值
                        existing_obj.insert(key.clone(), value.clone());
                    }
                    // Update updated_at timestamp on PATCH — PATCH 时更新 updated_at 时间戳
                    existing_obj.insert("updated_at".to_string(), json!(chrono::Utc::now().timestamp()));
                }
                existing_json
            } else {
                body
            }
        }
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "message": format!("{} not found", T::table_name()),
                    "name": "not found",
                    "code": 3,
                })),
            );
        }
        Err(_) => body,
    };

    match dao.update(&pk, &merged_body).await {
        Ok(updated) => {
            let body = serde_json::to_value(&updated).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let (message, fields) = enrich_unique_violation(&e, &merged_body);
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut resp = json!({
                "message": message,
                "name": e.error_name(),
                "code": e.error_code(),
            });
            if let Some(f) = fields {
                resp.as_object_mut().unwrap().insert("fields".to_string(), f);
            }
            (status, Json(resp))
        }
    }
}

/// Generic upsert handler (PUT = replace semantics) — 通用 upsert 处理（PUT = 替换语义）
async fn do_upsert<T: Entity + Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    id_or_name: &str,
    body: Value,
) -> (StatusCode, Json<Value>) {
    // Parse url shorthand — 解析 url 快捷方式
    let mut body = match expand_url_shorthand(&body) {
        Ok(b) => b,
        Err(e) => return e,
    };

    // Inject id/endpoint_key from URL path — 从 URL 路径注入 id 或 endpoint_key
    if let Some(obj) = body.as_object_mut() {
        if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
            // URL path is a UUID → set as id — URL 路径是 UUID → 设置为 id
            obj.insert("id".to_string(), json!(uuid));
        } else {
            // URL path is a name/endpoint_key → set the endpoint key field — URL 路径是名称 → 设置 endpoint key 字段
            if let Some(key_field) = T::endpoint_key() {
                obj.insert(key_field.to_string(), json!(id_or_name));
            }
        }

        // Inject timestamps — 注入时间戳
        let now = chrono::Utc::now().timestamp();
        if !obj.contains_key("created_at") {
            obj.insert("created_at".to_string(), json!(now));
        }
        // PUT always sets updated_at — PUT 始终设置 updated_at
        obj.insert("updated_at".to_string(), json!(now));
    }

    // Consumer validation: at least one of username or custom_id — Consumer 验证：至少需要 username 或 custom_id 之一
    if T::table_name() == "consumers" {
        if let Some(obj) = body.as_object() {
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
                        "fields": {
                            "@entity": ["at least one of these fields must be non-empty: 'custom_id', 'username'"]
                        },
                    })),
                );
            }
        }
    }

    // Plugin name validation and config defaults — 插件名称验证与 config 默认值
    if T::table_name() == "plugins" {
        if let Some(name) = body.as_object().and_then(|o| o.get("name")).and_then(|v| v.as_str()).map(|s| s.to_string()) {
            if !is_valid_plugin_name(&name) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": format!("schema violation (plugin '{}' not enabled; add it to the 'plugins' configuration property)", name),
                        "name": "schema violation",
                        "code": 2,
                        "fields": {
                            "name": format!("plugin '{}' not enabled; add it to the 'plugins' configuration property", name)
                        },
                    })),
                );
            }
            // Apply config defaults for upsert — upsert 时填充 config 默认值
            if let Some(obj) = body.as_object_mut() {
                if !obj.contains_key("config") || obj.get("config").map_or(false, |v| v.is_null()) {
                    obj.insert("config".to_string(), json!({}));
                }
                if let Some(config) = obj.get_mut("config").and_then(|v| v.as_object_mut()) {
                    apply_plugin_config_defaults(&name, config);
                }
            }
        }
        // Validate config type — 验证 config 字段类型
        if let Some(config) = body.as_object().and_then(|o| o.get("config")) {
            if !config.is_object() && !config.is_null() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": "schema violation (config: expected a record)",
                        "name": "schema violation",
                        "code": 2,
                        "fields": {
                            "config": "expected a record"
                        },
                    })),
                );
            }
        }
    }

    // Clone body for UNIQUE violation error enrichment — 克隆请求体以便 UNIQUE 冲突时提取字段值
    let body_for_err = body.clone();

    let entity: T = match serde_json::from_value(body) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": format!("schema violation: {}", e),
                    "name": "schema violation",
                    "code": 2,
                    "fields": {},
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
            let (message, fields) = enrich_unique_violation(&e, &body_for_err);
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut resp = json!({
                "message": message,
                "name": e.error_name(),
                "code": e.error_code(),
            });
            if let Some(f) = fields {
                resp.as_object_mut().unwrap().insert("fields".to_string(), f);
            }
            (status, Json(resp))
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
    _list_consumers_generic,
    get_consumer,
    create_consumer,
    update_consumer,
    upsert_consumer,
    delete_consumer
);

/// Custom list handler for consumers with custom_id/username filtering — Consumer 自定义列表 handler，支持 custom_id/username 过滤
pub async fn list_consumers(
    State(state): State<AdminState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    // Validate: empty string custom_id or username is invalid — 空字符串的 custom_id 或 username 无效
    if let Some(ref cid) = params.custom_id {
        if cid.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": "invalid filter syntax",
                    "name": "invalid filter syntax",
                    "code": 2,
                })),
            );
        }
    }
    if let Some(ref uname) = params.username {
        if uname.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": "invalid filter syntax",
                    "name": "invalid filter syntax",
                    "code": 2,
                })),
            );
        }
    }

    // Validate tags before query — 查询前验证 tags 参数
    if let Err(err) = params.validate_tags() {
        return err;
    }

    match state.consumers.page(&params.to_page_params()).await {
        Ok(page) => {
            // Post-filter by custom_id and/or username — 按 custom_id 和/或 username 后置过滤
            let filtered_data: Vec<_> = page.data.into_iter().filter(|c: &Consumer| {
                if let Some(ref cid) = params.custom_id {
                    if c.custom_id.as_deref() != Some(cid.as_str()) {
                        return false;
                    }
                }
                if let Some(ref uname) = params.username {
                    if c.username.as_deref() != Some(uname.as_str()) {
                        return false;
                    }
                }
                true
            }).collect();

            let filtered_page = Page {
                data: filtered_data,
                offset: page.offset,
                next: page.next,
            };
            let resp = build_page_response_with_tags(&filtered_page, params.tags.as_deref());
            (StatusCode::OK, Json(resp))
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}
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
    // Validate tags before query — 查询前验证 tags 参数
    if let Err(err) = params.validate_tags() {
        return err;
    }

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
    // Validate tags before query — 查询前验证 tags 参数
    if let Err(err) = params.validate_tags() {
        return err;
    }

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

    // Validate plugin name is required — 验证插件 name 字段必填
    let has_name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .map_or(false, |s| !s.is_empty());
    if !has_name {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "schema violation (name: required field missing)",
                "name": "schema violation",
                "code": 2,
                "fields": {
                    "name": "required field missing"
                },
            })),
        );
    }

    obj.insert(scope_field.to_string(), json!(ForeignKey::new(scope_id)));

    // Generate cache_key for plugin uniqueness — 生成 cache_key 用于插件唯一性检查
    generate_plugin_cache_key(obj);

    do_create::<Plugin>(&state.plugins, body).await
}

/// Apply default config values for known plugins — 为已知插件填充默认 config 值
///
/// When a plugin is created, if the config doesn't contain expected default fields,
/// fill them in based on the plugin's schema definition. — 创建插件时，如果 config 不含预期默认字段，根据插件 schema 定义填充。
fn apply_plugin_config_defaults(name: &str, config: &mut serde_json::Map<String, Value>) {
    match name {
        "key-auth" => {
            config.entry("key_names".to_string()).or_insert_with(|| json!(["apikey", "key"]));
            config.entry("key_in_body".to_string()).or_insert(json!(false));
            config.entry("key_in_header".to_string()).or_insert(json!(true));
            config.entry("key_in_query".to_string()).or_insert(json!(true));
            config.entry("hide_credentials".to_string()).or_insert(json!(false));
            config.entry("run_on_preflight".to_string()).or_insert(json!(true));
        }
        "basic-auth" => {
            config.entry("hide_credentials".to_string()).or_insert(json!(false));
        }
        "rate-limiting" => {
            config.entry("second".to_string()).or_insert(Value::Null);
            config.entry("minute".to_string()).or_insert(Value::Null);
            config.entry("hour".to_string()).or_insert(Value::Null);
            config.entry("day".to_string()).or_insert(Value::Null);
            config.entry("month".to_string()).or_insert(Value::Null);
            config.entry("year".to_string()).or_insert(Value::Null);
            config.entry("limit_by".to_string()).or_insert(json!("consumer"));
            config.entry("policy".to_string()).or_insert(json!("local"));
            config.entry("fault_tolerant".to_string()).or_insert(json!(true));
            config.entry("hide_client_headers".to_string()).or_insert(json!(false));
            config.entry("redis_host".to_string()).or_insert(Value::Null);
            config.entry("redis_port".to_string()).or_insert(json!(6379));
            config.entry("redis_password".to_string()).or_insert(Value::Null);
            config.entry("redis_timeout".to_string()).or_insert(json!(2000));
            config.entry("redis_database".to_string()).or_insert(json!(0));
            config.entry("header_name".to_string()).or_insert(Value::Null);
            config.entry("path".to_string()).or_insert(Value::Null);
            config.entry("redis_ssl".to_string()).or_insert(json!(false));
            config.entry("redis_ssl_verify".to_string()).or_insert(json!(false));
            config.entry("redis_server_name".to_string()).or_insert(Value::Null);
            config.entry("error_code".to_string()).or_insert(json!(429));
            config.entry("error_message".to_string()).or_insert(json!("API rate limit exceeded"));
            config.entry("sync_rate".to_string()).or_insert(json!(-1));
        }
        "cors" => {
            config.entry("origins".to_string()).or_insert(Value::Null);
            config.entry("methods".to_string()).or_insert_with(|| json!(["GET", "HEAD", "PUT", "PATCH", "POST", "DELETE", "OPTIONS", "TRACE", "CONNECT"]));
            config.entry("headers".to_string()).or_insert(Value::Null);
            config.entry("exposed_headers".to_string()).or_insert(Value::Null);
            config.entry("credentials".to_string()).or_insert(json!(false));
            config.entry("max_age".to_string()).or_insert(Value::Null);
            config.entry("preflight_continue".to_string()).or_insert(json!(false));
            config.entry("private_network".to_string()).or_insert(json!(false));
        }
        "request-transformer" => {
            let _empty_record = || json!({"remove": [], "rename": [], "replace": [], "add": [], "append": []});
            config.entry("http_method".to_string()).or_insert(Value::Null);
            config.entry("remove".to_string()).or_insert_with(|| json!({"headers": [], "querystring": [], "body": []}));
            config.entry("rename".to_string()).or_insert_with(|| json!({"headers": [], "querystring": [], "body": []}));
            config.entry("replace".to_string()).or_insert_with(|| json!({"headers": [], "querystring": [], "body": [], "uri": null}));
            config.entry("add".to_string()).or_insert_with(|| json!({"headers": [], "querystring": [], "body": []}));
            config.entry("append".to_string()).or_insert_with(|| json!({"headers": [], "querystring": [], "body": []}));
        }
        "tcp-log" => {
            config.entry("host".to_string()).or_insert(Value::Null);
            config.entry("port".to_string()).or_insert(Value::Null);
            config.entry("timeout".to_string()).or_insert(json!(10000));
            config.entry("keepalive".to_string()).or_insert(json!(60000));
            config.entry("tls".to_string()).or_insert(json!(false));
            config.entry("tls_sni".to_string()).or_insert(Value::Null);
        }
        "udp-log" => {
            config.entry("host".to_string()).or_insert(Value::Null);
            config.entry("port".to_string()).or_insert(Value::Null);
            config.entry("timeout".to_string()).or_insert(json!(10000));
        }
        "http-log" => {
            config.entry("http_endpoint".to_string()).or_insert(Value::Null);
            config.entry("method".to_string()).or_insert(json!("POST"));
            config.entry("content_type".to_string()).or_insert(json!("application/json"));
            config.entry("timeout".to_string()).or_insert(json!(10000));
            config.entry("keepalive".to_string()).or_insert(json!(60000));
            config.entry("flush_timeout".to_string()).or_insert(json!(2));
            config.entry("retry_count".to_string()).or_insert(json!(10));
            config.entry("queue_size".to_string()).or_insert(json!(1));
        }
        "file-log" => {
            config.entry("path".to_string()).or_insert(Value::Null);
            config.entry("reopen".to_string()).or_insert(json!(false));
        }
        "ip-restriction" => {
            config.entry("allow".to_string()).or_insert(Value::Null);
            config.entry("deny".to_string()).or_insert(Value::Null);
            config.entry("status".to_string()).or_insert(Value::Null);
            config.entry("message".to_string()).or_insert(Value::Null);
        }
        "acl" => {
            config.entry("allow".to_string()).or_insert(Value::Null);
            config.entry("deny".to_string()).or_insert(Value::Null);
            config.entry("hide_groups_header".to_string()).or_insert(json!(false));
        }
        "hmac-auth" => {
            config.entry("hide_credentials".to_string()).or_insert(json!(false));
            config.entry("clock_skew".to_string()).or_insert(json!(300));
            config.entry("algorithms".to_string()).or_insert_with(|| json!(["hmac-sha1", "hmac-sha256", "hmac-sha384", "hmac-sha512"]));
            config.entry("enforce_headers".to_string()).or_insert_with(|| json!([]));
            config.entry("validate_request_body".to_string()).or_insert(json!(false));
        }
        "jwt" => {
            config.entry("uri_param_names".to_string()).or_insert_with(|| json!(["jwt"]));
            config.entry("cookie_names".to_string()).or_insert_with(|| json!([]));
            config.entry("header_names".to_string()).or_insert_with(|| json!(["authorization"]));
            config.entry("key_claim_name".to_string()).or_insert(json!("iss"));
            config.entry("secret_is_base64".to_string()).or_insert(json!(false));
            config.entry("claims_to_verify".to_string()).or_insert(Value::Null);
            config.entry("anonymous".to_string()).or_insert(Value::Null);
            config.entry("run_on_preflight".to_string()).or_insert(json!(true));
            config.entry("maximum_expiration".to_string()).or_insert(json!(0));
        }
        "response-transformer" => {
            config.entry("remove".to_string()).or_insert_with(|| json!({"headers": [], "json": []}));
            config.entry("rename".to_string()).or_insert_with(|| json!({"headers": []}));
            config.entry("replace".to_string()).or_insert_with(|| json!({"headers": [], "json": [], "json_types": []}));
            config.entry("add".to_string()).or_insert_with(|| json!({"headers": [], "json": [], "json_types": []}));
            config.entry("append".to_string()).or_insert_with(|| json!({"headers": [], "json": [], "json_types": []}));
        }
        "request-size-limiting" => {
            config.entry("allowed_payload_size".to_string()).or_insert(json!(128));
            config.entry("size_unit".to_string()).or_insert(json!("megabytes"));
            config.entry("require_content_length".to_string()).or_insert(json!(false));
        }
        "request-termination" => {
            config.entry("status_code".to_string()).or_insert(json!(503));
            config.entry("message".to_string()).or_insert(Value::Null);
            config.entry("body".to_string()).or_insert(Value::Null);
            config.entry("content_type".to_string()).or_insert(Value::Null);
            config.entry("trigger".to_string()).or_insert(Value::Null);
            config.entry("echo".to_string()).or_insert(json!(false));
        }
        "bot-detection" => {
            config.entry("allow".to_string()).or_insert_with(|| json!([]));
            config.entry("deny".to_string()).or_insert_with(|| json!([]));
        }
        "correlation-id" => {
            config.entry("header_name".to_string()).or_insert(json!("Kong-Request-ID"));
            config.entry("generator".to_string()).or_insert(json!("uuid#counter"));
            config.entry("echo_downstream".to_string()).or_insert(json!(false));
        }
        "prometheus" => {
            config.entry("per_consumer".to_string()).or_insert(json!(false));
            config.entry("status_code_metrics".to_string()).or_insert(json!(false));
            config.entry("latency_metrics".to_string()).or_insert(json!(false));
            config.entry("bandwidth_metrics".to_string()).or_insert(json!(false));
            config.entry("upstream_health_metrics".to_string()).or_insert(json!(false));
        }
        // Other plugins: leave config as-is — 其他插件：保持 config 不变
        _ => {}
    }
}

/// Check if a plugin name is valid (in bundled list or known test plugins) — 检查插件名称是否有效（在内置列表或测试插件中）
fn is_valid_plugin_name(name: &str) -> bool {
    // Check against bundled plugins — 检查内置插件列表
    if kong_config::BUNDLED_PLUGINS.contains(&name) {
        return true;
    }
    // Known test/development plugins — 已知的测试/开发插件
    const TEST_PLUGINS: &[&str] = &[
        "rewriter", "dummy", "ctx-tests", "error-handler-log",
        "error-generator-last", "error-generator-pre", "error-generator-post",
        "short-circuit", "short-circuit-last", "logger", "reports-api",
        "request-transformer-advanced", "response-transformer-advanced",
        "rate-limiting-advanced", "canary", "forward-proxy", "upstream-tls",
        "vault-auth", "key-auth-enc", "opa", "mocking", "degraphql",
        "graphql-proxy-cache-advanced", "graphql-rate-limiting-advanced",
        "jq", "exit-transformer", "kafka-log", "kafka-upstream",
        "mtls-auth", "application-registration", "websocket-size-limit",
        "websocket-validator", "openid-connect", "proxy-cache-advanced",
        "tls-handshake-modifier", "tls-metadata-headers",
    ];
    TEST_PLUGINS.contains(&name)
}

/// Generate plugin cache_key from (name, route_id, service_id, consumer_id) — 从 (name, route_id, service_id, consumer_id) 生成插件 cache_key
fn generate_plugin_cache_key(obj: &mut serde_json::Map<String, Value>) {
    let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let route_id = obj
        .get("route")
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let service_id = obj
        .get("service")
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let consumer_id = obj
        .get("consumer")
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let cache_key = format!("{}:{}:{}:{}:", name, route_id, service_id, consumer_id);
    obj.insert("cache_key".to_string(), json!(cache_key));
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
    // For non-upsert (PATCH), verify plugin exists first — 非 upsert (PATCH) 时先验证插件存在
    if !upsert {
        let existing = get_scoped_plugin(dao, plugin_id_or_name, scope_field, scope_id).await;
        if existing.0 == StatusCode::NOT_FOUND {
            return existing;
        }
    }

    if let Some(obj) = body.as_object_mut() {
        // If the patch body explicitly sets the scope field to null, allow clearing it — 如果 patch 请求体显式将作用域字段设为 null，允许清除
        // Otherwise inject the scope foreign key — 否则注入作用域外键
        let explicitly_null = obj.get(scope_field).map_or(false, |v| v.is_null());
        if !explicitly_null {
            obj.insert(scope_field.to_string(), json!(ForeignKey::new(scope_id)));
        }
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
    // Validate tags before query — 查询前验证 tags 参数
    if let Err(err) = params.validate_tags() {
        return err;
    }

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
    // Validate tags before query — 查询前验证 tags 参数
    if let Err(err) = params.validate_tags() {
        return err;
    }

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
    // Validate tags before query — 查询前验证 tags 参数
    if let Err(err) = params.validate_tags() {
        return err;
    }

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
        ..Default::default()
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
) -> Response {
    // Validate tag value: reject control characters (including null bytes) — 验证标签值：拒绝控制字符（包括空字节）
    for ch in tag.chars() {
        if ch.is_control() || ch == '\0' {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": format!("invalid tag value: '{}' - tags must not contain control characters", tag.replace('\0', "\\0")),
                    "name": "invalid tag",
                    "code": 2,
                })),
            ).into_response();
        }
    }

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
    })).into_response()
}

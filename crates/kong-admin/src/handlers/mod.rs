//! Admin API request handlers — Admin API 请求处理器
//!
//! Implements Kong-compatible REST API endpoints: — 实现 Kong 兼容的 REST API 端点:
//! - Generic CRUD endpoints (generic) — 通用 CRUD 端点（泛型）
//! - Nested endpoints (e.g. /services/{id}/routes) — 嵌套端点（如 /services/{id}/routes）
//! - Special endpoints (/, /status) — 特殊端点（/, /status）

pub mod schemas;
pub mod ai_providers;
pub mod ai_models;
pub mod ai_virtual_keys;
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
    pub size: Option<String>,
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
            size: self.size.as_ref().and_then(|s| s.parse::<usize>().ok()).unwrap_or(100).min(1000),
            offset: self.offset.clone(),
            tags,
            tags_mode: if is_or { TagFilterMode::Or } else { TagFilterMode::And },
            filters: Vec::new(),
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

/// Validate integer fields and required fields in request body — 验证请求体中的整数字段和必填字段
fn validate_integer_fields(table_name: &str, body: &Value) -> Option<(StatusCode, Json<Value>)> {
    validate_integer_fields_ext(table_name, body, false)
}

fn validate_integer_fields_create(table_name: &str, body: &Value) -> Option<(StatusCode, Json<Value>)> {
    validate_integer_fields_ext(table_name, body, true)
}

fn validate_integer_fields_ext(table_name: &str, body: &Value, check_required: bool) -> Option<(StatusCode, Json<Value>)> {
    let int_fields: &[&str] = match table_name {
        "routes" => &["regex_priority", "https_redirect_status_code", "priority"],
        "services" => &["port", "retries", "connect_timeout", "write_timeout", "read_timeout", "tls_verify_depth"],
        "upstreams" => &["slots"],
        "targets" => &["weight"],
        _ => return None,
    };
    if let Some(obj) = body.as_object() {
        let mut violations = Vec::new();
        let mut fields = serde_json::Map::new();
        for &field in int_fields {
            if let Some(val) = obj.get(field) {
                if val.is_string() {
                    violations.push(format!("{}: expected an integer", field));
                    fields.insert(field.to_string(), json!("expected an integer"));
                }
            }
        }
        // Also check required fields for service on create/upsert — 创建/upsert 时也检查 service 必填字段
        if table_name == "services" && check_required {
            let host_missing = match obj.get("host") {
                None => true,
                Some(Value::Null) => true,
                Some(h) => h.as_str().map_or(false, |s| s.is_empty()),
            };
            if host_missing && !violations.is_empty() {
                violations.push("host: required field missing".to_string());
                fields.insert("host".to_string(), json!("required field missing"));
            }
        }
        if !violations.is_empty() {
            let msg = if violations.len() == 1 {
                format!("schema violation ({})", violations[0])
            } else {
                format!("{} schema violations ({})", violations.len(), violations.join("; "))
            };
            return Some((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": msg,
                    "name": "schema violation",
                    "code": 2,
                    "fields": Value::Object(fields),
                })),
            ));
        }
    }
    None
}

/// Convert empty JSON objects {} to empty arrays [] for known array fields — 将已知数组字段的空 JSON 对象转为空数组
fn normalize_empty_objects_to_arrays(body: &Value) -> Value {
    let known_array_fields = [
        "protocols", "methods", "hosts", "paths", "snis",
        "sources", "destinations", "ca_certificates", "tags",
    ];
    let mut body = body.clone();
    if let Some(obj) = body.as_object_mut() {
        for &field in &known_array_fields {
            if let Some(val) = obj.get(field) {
                if let Some(map) = val.as_object() {
                    if map.is_empty() {
                        obj.insert(field.to_string(), json!([]));
                    }
                }
            }
        }
    }
    body
}

/// Enrich UNIQUE violation error with actual field values from the request body
/// 用请求体中的实际字段值丰富 UNIQUE 冲突错误消息
/// Returns (message, optional fields object) — 返回 (消息, 可选的 fields 对象)
/// Validate upstream name: must be valid hostname, not IP — 验证 upstream 名称：有效主机名，不能是 IP
fn validate_upstream_name(name: &str) -> Option<(StatusCode, Json<Value>)> {
    if name.parse::<std::net::IpAddr>().is_ok() {
        return Some((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": format!("Invalid name ('{}'); no ip addresses allowed", name),
                "name": "schema violation",
                "code": 2,
                "fields": { "name": format!("Invalid name ('{}'); no ip addresses allowed", name) },
            })),
        ));
    }
    let is_valid = !name.is_empty() && !name.contains(' ')
        && name.split('.').all(|label| {
            !label.is_empty() && label.len() <= 63
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
    if !is_valid {
        return Some((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": format!("schema violation (name: Invalid name ('{}'); must be a valid hostname)", name),
                "name": "schema violation",
                "code": 2,
                "fields": { "name": format!("Invalid name ('{}'); must be a valid hostname", name) },
            })),
        ));
    }
    None
}


/// Validate upstream hash_on / hash_fallback related fields
/// 验证 upstream 的 hash_on / hash_fallback 相关字段
fn validate_upstream_hash_fields(obj: &serde_json::Map<String, Value>) -> Option<(StatusCode, Json<Value>)> {
    let valid_hash_on = ["none", "consumer", "ip", "header", "cookie", "path", "query_arg", "uri_capture"];
    let valid_hash_fallback = ["none", "ip", "header", "cookie", "path", "query_arg", "uri_capture"];
    let schema_err = |fields: Value| -> (StatusCode, Json<Value>) {
        let fields_str = fields.as_object()
            .map(|o| o.iter().map(|(k, v)| format!("{}: {}", k, v.as_str().unwrap_or(&v.to_string()))).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        (StatusCode::BAD_REQUEST, Json(json!({
            "message": format!("schema violation ({})", fields_str),
            "name": "schema violation",
            "code": 2,
            "fields": fields,
        })))
    };
    let get_str = |key: &str| -> Option<&str> { obj.get(key).and_then(|v| v.as_str()) };
    if let Some(hash_on) = get_str("hash_on") {
        if !valid_hash_on.contains(&hash_on) {
            return Some(schema_err(json!({"hash_on": "expected one of: none, consumer, ip, header, cookie, path, query_arg, uri_capture"})));
        }
        if hash_on == "header" {
            match get_str("hash_on_header") {
                None | Some("") => {
                    return Some(schema_err(json!({"@entity": ["failed conditional validation given value of field 'hash_on'"], "hash_on_header": "required field missing"})));
                }
                Some(h) => {
                    if !h.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                        return Some(schema_err(json!({"hash_on_header": format!("bad header name '{}', allowed characters are A-Z, a-z, 0-9, '_', and '-'", h)})));
                    }
                }
            }
        }
        if hash_on == "cookie" {
            if let Some(cookie) = get_str("hash_on_cookie") {
                if !is_valid_cookie_name(cookie) {
                    return Some(schema_err(json!({"hash_on_cookie": r#"contains one or more invalid characters. ASCII control characters (0-31;127), space, tab and the characters ()<>@,;:\"/?={}[] are not allowed."#})));
                }
            }
            if let Some(path) = get_str("hash_on_cookie_path") {
                if !path.starts_with('/') {
                    return Some(schema_err(json!({"hash_on_cookie_path": "should start with: /"})));
                }
            }
        }
        if let Some(hash_fallback) = get_str("hash_fallback") {
            if hash_on == "cookie" && hash_fallback != "none" {
                return Some(schema_err(json!({"@entity": ["failed conditional validation given value of field 'hash_on'"], "hash_fallback": "expected one of: none"})));
            }
            if !valid_hash_fallback.contains(&hash_fallback) && hash_fallback != "consumer" {
                return Some(schema_err(json!({"@entity": ["failed conditional validation given value of field 'hash_on'"], "hash_fallback": "expected one of: none, ip, header, cookie, path, query_arg, uri_capture"})));
            }
            if hash_on == "consumer" && hash_fallback == "consumer" {
                return Some(schema_err(json!({"@entity": ["failed conditional validation given value of field 'hash_on'"], "hash_fallback": "expected one of: none, ip, header, cookie, path, query_arg, uri_capture"})));
            }
            if hash_fallback == "header" {
                match get_str("hash_fallback_header") {
                    None | Some("") => {
                        return Some(schema_err(json!({"@entity": ["failed conditional validation given value of field 'hash_fallback'"], "hash_fallback_header": "required field missing"})));
                    }
                    Some(h) => {
                        if !h.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                            return Some(schema_err(json!({"hash_fallback_header": format!("bad header name '{}', allowed characters are A-Z, a-z, 0-9, '_', and '-'", h)})));
                        }
                    }
                }
            }
            if hash_on == "header" && hash_fallback == "header" {
                if let (Some(h1), Some(h2)) = (get_str("hash_on_header"), get_str("hash_fallback_header")) {
                    if h1 == h2 {
                        return Some(schema_err(json!({"@entity": ["values of these fields must be distinct: 'hash_on_header', 'hash_fallback_header'"]})));
                    }
                }
            }
            if hash_fallback == "cookie" {
                if let Some(cookie) = get_str("hash_on_cookie") {
                    if !is_valid_cookie_name(cookie) {
                        return Some(schema_err(json!({"hash_on_cookie": r#"contains one or more invalid characters. ASCII control characters (0-31;127), space, tab and the characters ()<>@,;:\"/?={}[] are not allowed."#})));
                    }
                }
                if let Some(path) = get_str("hash_on_cookie_path") {
                    if !path.starts_with('/') {
                        return Some(schema_err(json!({"hash_on_cookie_path": "should start with: /"})));
                    }
                }
            }
        }
    }
    None
}

fn is_valid_cookie_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    for c in name.chars() {
        let b = c as u32;
        if b <= 31 || b == 127 {
            return false;
        }
        if matches!(c, '(' | ')' | '<' | '>' | '@' | ',' | ';' | ':' | '\\' | '"' | '/' | '?' | '=' | '{' | '}' | '[' | ']' | ' ' | '\t') {
            return false;
        }
    }
    true
}

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
                        let fields = json!({ field_name: val_str });
                        return (
                            format!("UNIQUE violation detected on '{{{field_name}=\"{val_str}\"}}'"),
                            Some(fields),
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

    // Server header is now handled by admin_headers_middleware — Server 头现在由 admin_headers_middleware 中间件处理
    body.into_response()
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

    // Include configuration_hash if set (dbless mode or after POST /config) — 如果已设置则包含 configuration_hash
    let hash = state.configuration_hash.read()
        .map(|h| h.clone())
        .unwrap_or_default();
    if !hash.is_empty() {
        body["configuration_hash"] = json!(hash);
    }

    if !is_dbless {
        // In DB mode, include database reachable status — DB 模式下包含数据库可达状态
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

    // Parse the config body as JSON — 将配置内容解析为 JSON
    let config: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": format!("failed to parse declarative config: {}", e),
                })),
            ).into_response();
        }
    };

    // In db-less mode, load the config into the in-memory store — db-less 模式下将配置加载到内存存储
    if let Some(ref store) = state.dbless_store {
        if let Err(e) = store.load_from_json(&config) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": format!("failed to load declarative config: {}", e),
                })),
            ).into_response();
        }
        // Trigger proxy cache refresh — 触发代理缓存刷新
        let _ = state.refresh_tx.send("config");
    }

    // Compute hash of the config body — 计算配置内容的哈希
    let mut hasher = DefaultHasher::new();
    body.hash(&mut hasher);
    let h1 = hasher.finish();
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
        // AI Gateway endpoints — AI 网关端点
        "/ai-providers",
        "/ai-providers/{ai_providers}",
        "/ai-providers/{ai_providers}/ai-models",
        "/ai-models",
        "/ai-models/{ai_models}",
        "/ai-model-groups",
        "/ai-virtual-keys",
        "/ai-virtual-keys/{ai_virtual_keys}",
        "/ai-virtual-keys/{ai_virtual_keys}/rotate",
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
pub(crate) fn build_page_response<T: Serialize>(page: &Page<T>) -> Value {
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
pub(crate) async fn do_list<T: Entity + Serialize + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    params: &ListParams,
) -> (StatusCode, Json<Value>) {
    // Validate size parameter — 验证 size 参数
    if let Some(ref s) = params.size {
        if s.parse::<usize>().is_err() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "code": 9,
                    "name": "invalid size",
                    "message": "size must be a number",
                })),
            );
        }
    }
    // Validate offset parameter — 验证 offset 参数
    if let Some(ref offset) = params.offset {
        // base64 strings must have length >= 4 and valid chars — base64 字符串长度须 >= 4 且字符有效
        let valid_b64 = offset.len() >= 4
            && offset.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
            && offset.trim_end_matches('=').len() >= 2;
        if !valid_b64 {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "code": 7,
                    "name": "invalid offset",
                    "message": format!("'{}' is not a valid offset: bad base64 encoding", offset),
                })),
            );
        }
    }
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
pub(crate) async fn do_get<T: Entity + Serialize + Send + Sync + 'static>(
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
pub(crate) async fn do_create<T: Entity + Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    body: Value,
) -> (StatusCode, Json<Value>) {
    // Normalize empty objects and validate integer fields — 归一化空对象并验证整数字段
    let mut body = normalize_empty_objects_to_arrays(&body);
    if let Some(err) = validate_integer_fields(T::table_name(), &body) {
        return err;
    }
    // Auto-inject id and timestamps (Kong-compatible: these fields are optional on create) — 自动注入 id 和时间戳（Kong 兼容：创建时这些字段可选）
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

                // Validate plugin config (transformer colon checks, null array checks, etc.) — 验证插件 config（转换器冒号检查、null 数组检查等）
                if let Some(config) = obj.get("config") {
                    if let Some(err) = validate_transformer_plugin_config(&name, config) {
                        return err;
                    }
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

        // Upstream.name is required — Upstream.name 必填
        if T::table_name() == "upstreams" {
            let name_missing = match obj.get("name") {
                None => true,
                Some(Value::Null) => true,
                Some(n) => n.as_str().map_or(false, |s| s.is_empty()),
            };
            if name_missing {
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
            // Validate upstream name format — 验证上游名称格式
            if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
                if let Some(err) = validate_upstream_name(name) {
                    return err;
                }
            }
            // Validate upstream slots range [10, 65536] — 验证 slots 范围
            if let Some(slots_val) = obj.get("slots") {
                let slots_invalid = if let Some(n) = slots_val.as_i64() {
                    n < 10 || n > 65536
                } else if let Some(n) = slots_val.as_f64() {
                    (n as i64) < 10 || (n as i64) > 65536
                } else {
                    false
                };
                if slots_invalid {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": "schema violation (slots: value should be between 10 and 65536)",
                            "name": "schema violation",
                            "code": 2,
                            "fields": {
                                "slots": "value should be between 10 and 65536"
                            },
                        })),
                    );
                }
            }
            // Validate hash_on / hash_fallback fields — 验证 hash 相关字段
            if let Some(err) = validate_upstream_hash_fields(obj) {
                return err;
            }
        }

        // Route must have at least one of methods/hosts/headers/paths/snis — Route 至少需要一个路由匹配字段
        if T::table_name() == "routes" {
            if let Some(obj) = body.as_object_mut() {
                let has_field = |name: &str| -> bool {
                    match obj.get(name) {
                        Some(Value::Array(a)) => !a.is_empty(),
                        Some(Value::Object(o)) => !o.is_empty(),
                        Some(Value::Null) | None => false,
                        Some(_) => true,
                    }
                };
                let has_routing_field = has_field("methods") || has_field("hosts") || has_field("headers")
                    || has_field("paths") || has_field("snis") || has_field("sources") || has_field("destinations");
                // Determine protocols — 确定协议
                let protocols = obj.get("protocols").and_then(|v| v.as_array());
                let is_grpc = protocols.map_or(false, |arr| {
                    arr.iter().any(|p| p.as_str().map_or(false, |s| s == "grpc" || s == "grpcs"))
                });

                // Validate protocol values and service.protocol — 验证协议值和 service.protocol
                {
                    let valid_protos = ["grpc", "grpcs", "http", "https", "tcp", "tls", "tls_passthrough", "udp"];
                    let mut proto_violations = Vec::new();
                    let mut proto_fields = serde_json::Map::new();

                    if let Some(Value::Array(protos)) = obj.get("protocols") {
                        for (i, p) in protos.iter().enumerate() {
                            if let Some(ps) = p.as_str() {
                                if !valid_protos.contains(&ps) {
                                    proto_violations.push(format!("protocols.{}: expected one of: grpc, grpcs, http, https, tcp, tls, tls_passthrough, udp", i + 1));
                                    proto_fields.insert("protocols".to_string(), json!(["expected one of: grpc, grpcs, http, https, tcp, tls, tls_passthrough, udp"]));
                                    break; // only report first bad protocol — 只报告第一个坏协议
                                }
                            }
                        }
                    }

                    // Also check service.protocol — 也检查 service.protocol
                    if let Some(svc) = obj.get("service").and_then(|v| v.as_object()) {
                        if let Some(proto) = svc.get("protocol").and_then(|v| v.as_str()) {
                            if !valid_protos.contains(&proto) {
                                proto_violations.push("service.protocol: expected one of: grpc, grpcs, http, https, tcp, tls, tls_passthrough, udp".to_string());
                                let mut svc_fields = serde_json::Map::new();
                                svc_fields.insert("protocol".to_string(), json!("expected one of: grpc, grpcs, http, https, tcp, tls, tls_passthrough, udp"));
                                proto_fields.insert("service".to_string(), Value::Object(svc_fields));
                            }
                        }
                    }

                    if !proto_violations.is_empty() {
                        let msg = if proto_violations.len() == 1 {
                            format!("schema violation ({})", proto_violations[0])
                        } else {
                            format!("{} schema violations ({})", proto_violations.len(), proto_violations.join("; "))
                        };
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "message": msg,
                                "name": "schema violation",
                                "code": 2,
                                "fields": Value::Object(proto_fields),
                            })),
                        );
                    }
                }

                if !has_routing_field {
                    // Kong shows the most restrictive (TLS/secure) protocol — Kong 只显示最受限的协议
                    let proto_display = if let Some(arr) = protocols {
                        let pl: Vec<&str> = arr.iter().filter_map(|p| p.as_str()).collect();
                        if pl.contains(&"grpcs") { "grpcs".to_string() }
                        else if pl.contains(&"https") { "https".to_string() }
                        else if pl.contains(&"tls") { "tls".to_string() }
                        else { pl.join("', '") }
                    } else {
                        "https".to_string()
                    };
                    let rf = if is_grpc { "'hosts', 'headers', 'paths', 'snis'" }
                             else { "'methods', 'hosts', 'headers', 'paths', 'snis'" };
                    let emsg = format!("must set one of {} when 'protocols' is '{}'", rf, proto_display);
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": format!("schema violation ({})", emsg),
                            "name": "schema violation",
                            "code": 2,
                            "fields": { "@entity": [emsg] },
                        })),
                    );
                }

                // gRPC routes: validate constraints — gRPC 路由约束验证
                if is_grpc {
                    if obj.get("strip_path").and_then(|v| v.as_bool()).unwrap_or(false) {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "message": "schema violation (strip_path: cannot set 'strip_path' when 'protocols' is 'grpc' or 'grpcs')",
                                "name": "schema violation",
                                "code": 2,
                                "fields": { "strip_path": "cannot set 'strip_path' when 'protocols' is 'grpc' or 'grpcs'" },
                            })),
                        );
                    }
                    if let Some(Value::Array(arr)) = obj.get("methods") {
                        if !arr.is_empty() {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(json!({
                                    "message": "schema violation (methods: cannot set 'methods' when 'protocols' is 'grpc' or 'grpcs')",
                                    "name": "schema violation",
                                    "code": 2,
                                    "fields": { "methods": "cannot set 'methods' when 'protocols' is 'grpc' or 'grpcs'" },
                                })),
                            );
                        }
                    }
                    obj.insert("strip_path".to_string(), json!(false));
                }
            }
        }
    }

    // Route: validate service has id or name — 路由：验证 service 有 id 或 name
    if T::table_name() == "routes" {
        if let Some(svc) = body.as_object().and_then(|o| o.get("service")).and_then(|v| v.as_object()) {
            if !svc.contains_key("id") && !svc.contains_key("name") {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": "schema violation (service.id: missing primary key)",
                        "name": "schema violation",
                        "code": 2,
                        "fields": { "service": { "id": "missing primary key" } },
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
pub(crate) async fn do_update<T: Entity + Serialize + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    id_or_name: &str,
    body: &Value,
) -> (StatusCode, Json<Value>) {
    // Parse url shorthand for Service updates — Service 更新时解析 url 快捷方式
    let body = match expand_url_shorthand(body) {
        Ok(b) => b,
        Err(e) => return e,
    };

    // Normalize empty objects and validate integer fields — 归一化空对象并验证整数字段
    let body = normalize_empty_objects_to_arrays(&body);
    if let Some(err) = validate_integer_fields(T::table_name(), &body) {
        return err;
    }

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
pub(crate) async fn do_upsert<T: Entity + Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    id_or_name: &str,
    body: Value,
) -> (StatusCode, Json<Value>) {
    // Parse url shorthand — 解析 url 快捷方式
    let mut body = match expand_url_shorthand(&body) {
        Ok(b) => b,
        Err(e) => return e,
    };

    // Validate integer fields (with required field check for create/upsert) — 验证整数字段（含必填字段检查）
    if let Some(err) = validate_integer_fields_create(T::table_name(), &body) {
        return err;
    }

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

                // Validate plugin config for upsert — upsert 时验证插件 config
                if let Some(config) = obj.get("config") {
                    if let Some(err) = validate_transformer_plugin_config(&name, config) {
                        return err;
                    }
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

    // Upstream.name is required for upsert — upsert 时 Upstream.name 必填
    if T::table_name() == "upstreams" {
        if let Some(obj) = body.as_object() {
            let name_missing = match obj.get("name") {
                None => true,
                Some(Value::Null) => true,
                Some(n) => n.as_str().map_or(false, |s| s.is_empty()),
            };
            if name_missing {
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
            // Validate upstream name format — 验证上游名称格式
            if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
                if let Some(err) = validate_upstream_name(name) {
                    return err;
                }
            }
            // Validate upstream slots range [10, 65536] — 验证 slots 范围
            if let Some(slots_val) = obj.get("slots") {
                let slots_invalid = if let Some(n) = slots_val.as_i64() {
                    n < 10 || n > 65536
                } else if let Some(n) = slots_val.as_f64() {
                    (n as i64) < 10 || (n as i64) > 65536
                } else {
                    false
                };
                if slots_invalid {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "message": "schema violation (slots: value should be between 10 and 65536)",
                            "name": "schema violation",
                            "code": 2,
                            "fields": {
                                "slots": "value should be between 10 and 65536"
                            },
                        })),
                    );
                }
            }
            // Validate hash_on / hash_fallback fields — 验证 hash 相关字段
            if let Some(err) = validate_upstream_hash_fields(obj) {
                return err;
            }
        }
    }

    // Route must have at least one routing field for upsert — upsert 时 Route 也需要至少一个路由匹配字段
    if T::table_name() == "routes" {
        if let Some(obj) = body.as_object_mut() {
            let has_field = |name: &str| -> bool {
                match obj.get(name) {
                    Some(Value::Array(a)) => !a.is_empty(),
                    Some(Value::Object(o)) => !o.is_empty(),
                    Some(Value::Null) | None => false,
                    Some(_) => true,
                }
            };
            let has_routing = has_field("methods") || has_field("hosts") || has_field("headers")
                || has_field("paths") || has_field("snis") || has_field("sources") || has_field("destinations");

            let protocols = obj.get("protocols").and_then(|v| v.as_array());
            let is_grpc = protocols.map_or(false, |arr| {
                arr.iter().any(|p| p.as_str().map_or(false, |s| s == "grpc" || s == "grpcs"))
            });

            // Validate protocol values in upsert — upsert 中验证协议值
            if let Some(Value::Array(protos)) = obj.get("protocols") {
                let valid_protos = ["grpc", "grpcs", "http", "https", "tcp", "tls", "tls_passthrough", "udp"];
                for (i, p) in protos.iter().enumerate() {
                    if let Some(ps) = p.as_str() {
                        if !valid_protos.contains(&ps) {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(json!({
                                    "message": format!("schema violation (protocols.{}: expected one of: grpc, grpcs, http, https, tcp, tls, tls_passthrough, udp)", i + 1),
                                    "name": "schema violation",
                                    "code": 2,
                                    "fields": { "protocols": ["expected one of: grpc, grpcs, http, https, tcp, tls, tls_passthrough, udp"] },
                                })),
                            );
                        }
                    }
                }
            }

            if !has_routing {
                let proto_display = if let Some(arr) = protocols {
                    let pl: Vec<&str> = arr.iter().filter_map(|p| p.as_str()).collect();
                    if pl.contains(&"grpcs") { "grpcs".to_string() }
                    else if pl.contains(&"https") { "https".to_string() }
                    else if pl.contains(&"tls") { "tls".to_string() }
                    else { pl.join("', '") }
                } else {
                    "https".to_string()
                };
                let rf = if is_grpc { "'hosts', 'headers', 'paths', 'snis'" }
                         else { "'methods', 'hosts', 'headers', 'paths', 'snis'" };
                let emsg = format!("must set one of {} when 'protocols' is '{}'", rf, proto_display);
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": format!("schema violation ({})", emsg),
                        "name": "schema violation",
                        "code": 2,
                        "fields": { "@entity": [emsg] },
                    })),
                );
            }
            // gRPC routes: strip_path forced to false — gRPC 路由：strip_path 强制为 false
            if is_grpc {
                obj.insert("strip_path".to_string(), json!(false));
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
pub(crate) async fn do_delete<T: Entity + Send + Sync + 'static>(
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
// Route handlers (custom, with service name resolution) — 路由处理器（自定义，支持 service name 解析）
pub async fn list_routes(
    State(state): State<AdminState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    do_list::<Route>(&state.routes, &params).await
}

pub async fn get_route(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    do_get::<Route>(&state.routes, &id_or_name).await
}

pub async fn create_route(
    State(state): State<AdminState>,
    FlexibleBody(mut body): FlexibleBody,
) -> impl IntoResponse {
    if let Err(e) = resolve_service_name_ref(&state, &mut body).await {
        return e;
    }
    let result = do_create::<Route>(&state.routes, body).await;
    if result.0.is_success() {
        let _ = state.refresh_tx.send("routes");
    }
    result
}

pub async fn update_route(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let result = do_update::<Route>(&state.routes, &id_or_name, &body).await;
    if result.0.is_success() {
        let _ = state.refresh_tx.send("routes");
    }
    result
}

pub async fn upsert_route(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
    FlexibleBody(mut body): FlexibleBody,
) -> impl IntoResponse {
    if let Err(e) = resolve_service_name_ref(&state, &mut body).await {
        return e;
    }
    let result = do_upsert::<Route>(&state.routes, &id_or_name, body).await;
    if result.0.is_success() {
        let _ = state.refresh_tx.send("routes");
    }
    result
}

pub async fn delete_route(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    let result = do_delete::<Route>(&state.routes, &id_or_name).await;
    let _ = state.refresh_tx.send("routes");
    result
}

/// Resolve service.name reference to service.id — 将 service.name 引用解析为 service.id
async fn resolve_service_name_ref(state: &AdminState, body: &mut Value) -> Result<(), (StatusCode, Json<Value>)> {
    if let Some(svc_obj) = body.as_object_mut().and_then(|o| o.get_mut("service")).and_then(|v| v.as_object_mut()) {
        if !svc_obj.contains_key("id") && svc_obj.contains_key("name") {
            let name_val = svc_obj.get("name").cloned();
            let name_str = name_val.as_ref().and_then(|v| v.as_str()).filter(|s| !s.is_empty());
            if name_str.is_none() {
                // name is null/empty → missing primary key — name 为 null/空 → 缺少主键
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": "schema violation (service.id: missing primary key)",
                        "name": "schema violation",
                        "code": 2,
                        "fields": { "service": { "id": "missing primary key" } },
                    })),
                ));
            }
            if let Some(name) = name_str.map(|s| s.to_string()) {
                let pk = PrimaryKey::EndpointKey(name.clone());
                match state.services.select(&pk).await {
                    Ok(Some(svc)) => {
                        let svc_json = serde_json::to_value(&svc).unwrap_or(json!(null));
                        if let Some(id) = svc_json.get("id").and_then(|v| v.as_str()) {
                            svc_obj.remove("name");
                            svc_obj.insert("id".to_string(), json!(id));
                        }
                    }
                    _ => {
                        let fk_msg = String::from("the foreign key cannot be resolved with '{name=\"") + &name + "\"}' for an existing 'services' entity";
                        return Err((
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "message": format!("foreign key unresolved (service.name: {})", fk_msg),
                                "name": "foreign keys unresolved",
                                "code": 13,
                                "fields": { "service": { "name": fk_msg } },
                            })),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}
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

    // Push custom_id/username filters to the DAO layer — 将 custom_id/username 过滤下推到 DAO 层
    let mut page_params = params.to_page_params();
    if let Some(ref cid) = params.custom_id {
        page_params.filters.push(("custom_id".to_string(), cid.clone()));
    }
    if let Some(ref uname) = params.username {
        page_params.filters.push(("username".to_string(), uname.clone()));
    }

    match state.consumers.page(&page_params).await {
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
// ============ Certificate handlers (custom, with SNI embedding) — 证书处理器（自定义，嵌入 SNI） ============

/// Percent-decode PEM fields (cert, key, cert_alt, key_alt) that may be URL-encoded from form-urlencoded bodies
/// 对可能来自 form-urlencoded 的 PEM 字段进行百分号解码
fn percent_decode_pem_fields(body: &mut Value) {
    let pem_fields = ["cert", "key", "cert_alt", "key_alt"];
    if let Some(obj) = body.as_object_mut() {
        for field in &pem_fields {
            if let Some(val) = obj.get_mut(*field) {
                if let Some(s) = val.as_str() {
                    if s.contains('%') {
                        if let Ok(decoded) = url::form_urlencoded::parse(s.as_bytes())
                            .into_owned()
                            .next()
                            .map(|(k, _)| k)
                            .ok_or(()) {
                            // form_urlencoded::parse treats the whole string as key if no = — 如果没有 = 号则整个字符串作为 key
                            *val = Value::String(decoded);
                        } else {
                            // Fallback: manual percent decode — 后备：手动百分号解码
                            let decoded = percent_decode_str(s);
                            *val = Value::String(decoded);
                        }
                    }
                }
            }
        }
    }
}

fn percent_decode_str(s: &str) -> String {
    // Proper percent decoding — 百分号解码
    let mut result = Vec::new();
    let input = s.as_bytes();
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' && i + 2 < input.len() {
            let hi = hex_val(input[i + 1]);
            let lo = hex_val(input[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                result.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        result.push(input[i]);
        i += 1;
    }
    String::from_utf8(result).unwrap_or_else(|_| s.to_string())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Fetch SNI names associated with a certificate — 获取关联证书的 SNI 名称列表
async fn fetch_sni_names_for_cert(snis_dao: &Arc<dyn Dao<Sni>>, cert_id: &uuid::Uuid) -> Vec<String> {
    let all_params = PageParams { size: 10000, ..Default::default() };
    match snis_dao.select_by_foreign_key("certificate", cert_id, &all_params).await {
        Ok(page) => page.data.into_iter().map(|s| s.name).collect(),
        Err(_) => vec![],
    }
}

/// Embed snis array into a certificate JSON value (sorted alphabetically) — 将 snis 数组嵌入证书 JSON 值（按字母排序）
async fn embed_snis_in_cert(snis_dao: &Arc<dyn Dao<Sni>>, cert_json: &mut Value) {
    if let Some(id_str) = cert_json.get("id").and_then(|v| v.as_str()) {
        if let Ok(cert_id) = uuid::Uuid::parse_str(id_str) {
            let mut sni_names = fetch_sni_names_for_cert(snis_dao, &cert_id).await;
            sni_names.sort();
            cert_json.as_object_mut().unwrap().insert("snis".to_string(), json!(sni_names));
        }
    }
}

/// Validate SNI name: wildcards must be only at start or end, not both — 验证 SNI 名称通配符
fn validate_sni_name(name: &str) -> Result<(), String> {
    let wildcard_count = name.matches('*').count();
    if wildcard_count > 1 {
        return Err("only one wildcard must be specified".to_string());
    }
    if wildcard_count == 1 {
        if !name.starts_with("*.") && !name.ends_with(".*") {
            return Err("wildcard must be leftmost or rightmost character".to_string());
        }
    }
    Ok(())
}

/// Validate SNI names: check for duplicates, wildcard format, and pre-existing SNIs — 验证 SNI 名称
async fn validate_sni_names_for_create(
    snis_dao: &Arc<dyn Dao<Sni>>,
    sni_names: &[String],
    exclude_cert_id: Option<uuid::Uuid>,
) -> Result<(), (StatusCode, Json<Value>)> {
    let mut seen = std::collections::HashSet::new();
    for name in sni_names {
        if !seen.insert(name.as_str()) {
            return Err((StatusCode::BAD_REQUEST, Json(json!({
                "message": format!("schema violation (snis: {} is duplicated)", name),
                "name": "schema violation", "code": 2,
                "fields": { "snis": format!("{} is duplicated", name) },
            }))));
        }
        if let Err(msg) = validate_sni_name(name) {
            return Err((StatusCode::BAD_REQUEST, Json(json!({
                "message": format!("schema violation (name: {})", msg),
                "name": "schema violation", "code": 2,
                "fields": { "name": msg },
            }))));
        }
    }
    for name in sni_names {
        let sni_pk = PrimaryKey::EndpointKey(name.clone());
        if let Ok(Some(existing_sni)) = snis_dao.select(&sni_pk).await {
            let is_same_cert = exclude_cert_id.map_or(false, |id| existing_sni.certificate.id == id);
            if !is_same_cert {
                let cert_id_str = existing_sni.certificate.id.to_string();
                return Err((StatusCode::BAD_REQUEST, Json(json!({
                    "message": format!("schema violation (snis: {} already associated with existing certificate '{}')", name, cert_id_str),
                    "name": "schema violation", "code": 2,
                    "fields": { "snis": format!("{} already associated with existing certificate '{}'", name, cert_id_str) },
                }))));
            }
        }
    }
    Ok(())
}

/// Validate certificate PEM: cert/key match, cert_alt/key_alt pairing, non-distinct certs — 验证证书 PEM
fn validate_certificate_pem(body: &Value) -> Result<(), (StatusCode, Json<Value>)> {
    use openssl::x509::X509;
    use openssl::pkey::PKey;

    let cert_str = body.get("cert").and_then(|v| v.as_str()).unwrap_or("");
    let key_str = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let cert_alt_str = body.get("cert_alt").and_then(|v| v.as_str());
    let key_alt_str = body.get("key_alt").and_then(|v| v.as_str());

    if cert_str.is_empty() || key_str.is_empty() {
        return Ok(());
    }

    let cert = X509::from_pem(cert_str.as_bytes()).map_err(|_| cert_schema_violation("certificate is not valid PEM format"))?;
    let key = PKey::private_key_from_pem(key_str.as_bytes()).map_err(|_| cert_schema_violation("key is not valid PEM format"))?;

    if cert.public_key().ok().and_then(|pk| pk.public_eq(&key).then_some(())).is_none() {
        return Err(cert_schema_violation("certificate does not match key"));
    }

    let has_cert_alt = cert_alt_str.map_or(false, |s| !s.is_empty()) && !body.get("cert_alt").map_or(false, |v| v.is_null());
    let has_key_alt = key_alt_str.map_or(false, |s| !s.is_empty()) && !body.get("key_alt").map_or(false, |v| v.is_null());

    if has_cert_alt != has_key_alt {
        return Err(cert_schema_violation("all or none of these fields must be set: 'cert_alt', 'key_alt'"));
    }

    if has_cert_alt && has_key_alt {
        let cert_alt_pem = cert_alt_str.unwrap();
        let key_alt_pem = key_alt_str.unwrap();

        let cert_alt = X509::from_pem(cert_alt_pem.as_bytes()).map_err(|_| cert_schema_violation("alternative certificate is not valid PEM format"))?;
        let key_alt = PKey::private_key_from_pem(key_alt_pem.as_bytes()).map_err(|_| cert_schema_violation("alternative key is not valid PEM format"))?;

        if cert_alt.public_key().ok().and_then(|pk| pk.public_eq(&key_alt).then_some(())).is_none() {
            return Err(cert_schema_violation("alternative certificate does not match key"));
        }

        let cert_key_type = cert.public_key().ok().map(|pk| pk.id());
        let cert_alt_key_type = cert_alt.public_key().ok().map(|pk| pk.id());
        if cert_key_type == cert_alt_key_type {
            return Err(cert_schema_violation("certificate and alternative certificate need to have different type (e.g. RSA and ECDSA), the provided certificates were both of the same type"));
        }
    }

    Ok(())
}

fn cert_schema_violation(msg: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({
        "message": format!("schema violation ({})", msg),
        "name": "schema violation", "code": 2,
        "fields": { "@entity": [msg] },
    })))
}

/// Extract snis field from request body, returning (cleaned body, snis list) — 从请求体提取 snis 字段
fn extract_snis_from_body(body: &mut Value) -> Option<Vec<String>> {
    if let Some(obj) = body.as_object_mut() {
        if let Some(snis_val) = obj.remove("snis") {
            if snis_val.is_null() {
                return Some(vec![]);
            }
            if let Some(arr) = snis_val.as_array() {
                let names: Vec<String> = arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                return Some(names);
            }
        }
    }
    None
}

/// Create SNI records for a certificate — 为证书创建 SNI 记录
async fn create_snis_for_cert(
    snis_dao: &Arc<dyn Dao<Sni>>,
    cert_id: uuid::Uuid,
    sni_names: &[String],
) -> Result<(), (StatusCode, Json<Value>)> {
    let now = chrono::Utc::now().timestamp();
    for name in sni_names {
        let sni = Sni {
            id: uuid::Uuid::new_v4(),
            name: name.clone(),
            created_at: now,
            updated_at: now,
            tags: None,
            certificate: ForeignKey::new(cert_id),
            ws_id: None,
        };
        if let Err(e) = snis_dao.insert(&sni).await {
            return Err((
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({ "message": e.to_string(), "name": e.error_name(), "code": e.error_code() })),
            ));
        }
    }
    Ok(())
}

/// Replace all SNIs for a certificate (delete existing, create new) — 替换证书的所有 SNI（删除已有，创建新的）
async fn replace_snis_for_cert(
    snis_dao: &Arc<dyn Dao<Sni>>,
    cert_id: uuid::Uuid,
    sni_names: &[String],
) -> Result<(), (StatusCode, Json<Value>)> {
    let existing = fetch_sni_names_for_cert(snis_dao, &cert_id).await;
    for name in &existing {
        let pk = PrimaryKey::EndpointKey(name.clone());
        let _ = snis_dao.delete(&pk).await;
    }
    create_snis_for_cert(snis_dao, cert_id, sni_names).await
}

/// GET /certificates — list all certificates with embedded snis — 列出所有证书（嵌入 snis）
pub async fn list_certificates(
    State(state): State<AdminState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    if let Err(err) = params.validate_tags() {
        return err;
    }
    match state.certificates.page(&params.to_page_params()).await {
        Ok(page) => {
            let mut resp = build_page_response_with_tags(&page, params.tags.as_deref());
            if let Some(data_arr) = resp.get_mut("data").and_then(|v| v.as_array_mut()) {
                for cert_json in data_arr.iter_mut() {
                    embed_snis_in_cert(&state.snis, cert_json).await;
                }
            }
            (StatusCode::OK, Json(resp))
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

/// GET /certificates/{id_or_sni} — get certificate by ID, UUID, or SNI name — 按 ID、UUID 或 SNI 名称获取证书
pub async fn get_certificate(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    let pk = PrimaryKey::from_str_or_uuid(&id_or_name);
    let cert_opt = match &pk {
        PrimaryKey::Id(_) => {
            match state.certificates.select(&pk).await {
                Ok(c) => c,
                Err(e) => {
                    let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                    return (status, Json(json!({"message": e.to_string()})));
                }
            }
        }
        PrimaryKey::EndpointKey(_) => None,
    };

    let cert = if let Some(c) = cert_opt {
        c
    } else {
        let sni_pk = PrimaryKey::EndpointKey(id_or_name.clone());
        match state.snis.select(&sni_pk).await {
            Ok(Some(sni)) => {
                let cert_pk = PrimaryKey::Id(sni.certificate.id);
                match state.certificates.select(&cert_pk).await {
                    Ok(Some(c)) => c,
                    Ok(None) | Err(_) => {
                        return (StatusCode::NOT_FOUND, Json(json!({"message": "certificates not found", "name": "not found", "code": 3})));
                    }
                }
            }
            Ok(None) | Err(_) => {
                return (StatusCode::NOT_FOUND, Json(json!({"message": "certificates not found", "name": "not found", "code": 3})));
            }
        }
    };

    let mut body = serde_json::to_value(&cert).unwrap_or(json!(null));
    embed_snis_in_cert(&state.snis, &mut body).await;
    (StatusCode::OK, Json(body))
}

/// POST /certificates — create certificate with optional snis — 创建证书（可选带 snis）
pub async fn create_certificate(
    State(state): State<AdminState>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let mut body = body;
    percent_decode_pem_fields(&mut body);
    let sni_names = extract_snis_from_body(&mut body);

    // Validate certificate PEM — 验证证书 PEM
    if let Err(err) = validate_certificate_pem(&body) {
        return err;
    }
    // Pre-validate SNI names — 预验证 SNI 名称
    if let Some(ref names) = sni_names {
        if let Err(err) = validate_sni_names_for_create(&state.snis, names, None).await {
            return err;
        }
    }

    let result = do_create::<Certificate>(&state.certificates, body).await;
    if result.0.is_success() {
        if let Some(ref names) = sni_names {
            if !names.is_empty() {
                if let Some(cert_id_str) = result.1.get("id").and_then(|v| v.as_str()) {
                    if let Ok(cert_id) = uuid::Uuid::parse_str(cert_id_str) {
                        if let Err(err) = create_snis_for_cert(&state.snis, cert_id, names).await {
                            return err;
                        }
                    }
                }
            }
        }
        let _ = state.refresh_tx.send("certificates");
        let _ = state.refresh_tx.send("snis");

        let mut cert_json = result.1.0;
        embed_snis_in_cert(&state.snis, &mut cert_json).await;
        (StatusCode::CREATED, Json(cert_json))
    } else {
        (result.0, Json(result.1.0))
    }
}

/// PATCH /certificates/{id_or_sni} — update certificate with optional snis replacement — 更新证书（可选替换 snis）
pub async fn update_certificate(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let mut body = body;
    percent_decode_pem_fields(&mut body);
    let sni_names = extract_snis_from_body(&mut body);

    // Pre-validate SNI names for PATCH — PATCH 时预验证 SNI 名称
    let cert_id_or_name = resolve_cert_id_or_name(&state, &id_or_name).await;
    let resolved = match cert_id_or_name {
        Some(id) => id,
        None => {
            return (StatusCode::NOT_FOUND, Json(json!({ "message": "certificates not found", "name": "not found", "code": 3 })));
        }
    };

    let exclude_cert_id = uuid::Uuid::parse_str(&resolved).ok();
    if let Some(ref names) = sni_names {
        if let Err(err) = validate_sni_names_for_create(&state.snis, names, exclude_cert_id).await {
            return err;
        }
    }

    let result = do_update::<Certificate>(&state.certificates, &resolved, &body).await;
    if result.0.is_success() {
        if let Some(ref names) = sni_names {
            if let Some(cert_id_str) = result.1.get("id").and_then(|v| v.as_str()) {
                if let Ok(cert_id) = uuid::Uuid::parse_str(cert_id_str) {
                    if let Err(err) = replace_snis_for_cert(&state.snis, cert_id, names).await {
                        return err;
                    }
                }
            }
        }
        let _ = state.refresh_tx.send("certificates");
        let _ = state.refresh_tx.send("snis");

        let mut cert_json = result.1.0;
        embed_snis_in_cert(&state.snis, &mut cert_json).await;
        (StatusCode::OK, Json(cert_json))
    } else {
        (result.0, Json(result.1.0))
    }
}

/// PUT /certificates/{id_or_sni} — upsert certificate with optional snis replacement — upsert 证书（可选替换 snis）
pub async fn upsert_certificate(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let mut body = body;
    let sni_names = extract_snis_from_body(&mut body);

    // Validate required fields for PUT — 验证 PUT 必填字段
    {
        let mut violations = Vec::new();
        let mut fields = serde_json::Map::new();
        let cert_val = body.get("cert").and_then(|v| v.as_str()).unwrap_or("");
        let key_val = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
        if cert_val.is_empty() {
            violations.push("cert: required field missing");
            fields.insert("cert".to_string(), json!("required field missing"));
        }
        if key_val.is_empty() {
            violations.push("key: required field missing");
            fields.insert("key".to_string(), json!("required field missing"));
        }
        if !violations.is_empty() {
            let msg = if violations.len() > 1 {
                format!("{} schema violations ({})", violations.len(), violations.join("; "))
            } else {
                format!("schema violation ({})", violations[0])
            };
            return (StatusCode::BAD_REQUEST, Json(json!({"message": msg, "name": "schema violation", "code": 2, "fields": Value::Object(fields)})));
        }
    }

    // Validate certificate PEM — 验证证书 PEM
    if let Err(err) = validate_certificate_pem(&body) {
        return err;
    }

    // Check if the URL path is an SNI name (not UUID) — 检查 URL 路径是否为 SNI 名称
    let url_is_sni_name = uuid::Uuid::parse_str(&id_or_name).is_err();

    // Build combined SNI list: URL SNI + body snis (deduplicated) — 构建合并 SNI 列表
    let mut combined_snis: Vec<String> = Vec::new();
    if url_is_sni_name {
        combined_snis.push(id_or_name.clone());
    }
    if let Some(ref names) = sni_names {
        for name in names {
            if !combined_snis.contains(name) {
                combined_snis.push(name.clone());
            }
        }
    }

    let resolved = resolve_cert_id_or_name(&state, &id_or_name).await
        .unwrap_or_else(|| id_or_name.clone());

    let result = do_upsert::<Certificate>(&state.certificates, &resolved, body).await;
    if result.0.is_success() {
        if let Some(cert_id_str) = result.1.get("id").and_then(|v| v.as_str()) {
            if let Ok(cert_id) = uuid::Uuid::parse_str(cert_id_str) {
                // PUT always replaces SNIs (full replacement semantics) — PUT 始终替换 SNI（全量替换语义）
                if let Err(err) = replace_snis_for_cert(&state.snis, cert_id, &combined_snis).await {
                    return err;
                }
            }
        }
        let _ = state.refresh_tx.send("certificates");
        let _ = state.refresh_tx.send("snis");

        let mut cert_json = result.1.0;
        embed_snis_in_cert(&state.snis, &mut cert_json).await;
        (StatusCode::OK, Json(cert_json))
    } else {
        (result.0, Json(result.1.0))
    }
}

/// DELETE /certificates/{id_or_sni} — delete certificate (SNIs cascade-deleted) — 删除证书（SNI 级联删除）
pub async fn delete_certificate(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    let resolved = resolve_cert_id_or_name(&state, &id_or_name).await
        .unwrap_or_else(|| id_or_name.clone());

    // Delete associated SNIs first (DB CASCADE as backup) — 先删 SNI（DB CASCADE 作为后备）
    if let Ok(cert_id) = uuid::Uuid::parse_str(&resolved) {
        let existing_snis = fetch_sni_names_for_cert(&state.snis, &cert_id).await;
        for name in &existing_snis {
            let pk = PrimaryKey::EndpointKey(name.clone());
            let _ = state.snis.delete(&pk).await;
        }
    }

    let result = do_delete::<Certificate>(&state.certificates, &resolved).await;
    let _ = state.refresh_tx.send("certificates");
    let _ = state.refresh_tx.send("snis");
    result
}

/// GET /certificates/{cert_id_or_name}/snis — list SNIs for a certificate — 列出证书关联的 SNI
pub async fn list_certificate_snis(
    State(state): State<AdminState>,
    Path(cert_id_or_name): Path<String>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let resolved = resolve_cert_id_or_name(&state, &cert_id_or_name).await;
    let cert_id = match resolved.and_then(|s| uuid::Uuid::parse_str(&s).ok()) {
        Some(id) => id,
        None => return (StatusCode::NOT_FOUND, Json(json!({"message": "Not found"}))),
    };
    let page_params = params.to_page_params();
    match state.snis.select_by_foreign_key("certificate", &cert_id, &page_params).await {
        Ok(page) => {
            let resp = build_page_response_with_tags(&page, params.tags.as_deref());
            (StatusCode::OK, Json(resp))
        }
        Err(e) => (StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"message": e.to_string()}))),
    }
}

/// POST /certificates/{cert_id_or_name}/snis — create SNI for a certificate — 为证书创建 SNI
pub async fn create_certificate_sni(
    State(state): State<AdminState>,
    Path(cert_id_or_name): Path<String>,
    body: crate::extractors::FlexibleBody,
) -> impl IntoResponse {
    let resolved = resolve_cert_id_or_name(&state, &cert_id_or_name).await;
    let cert_id = match resolved.and_then(|s| uuid::Uuid::parse_str(&s).ok()) {
        Some(id) => id,
        None => return (StatusCode::NOT_FOUND, Json(json!({"message": "Not found"}))),
    };
    // Verify the certificate actually exists — 验证证书是否确实存在
    let cert_pk = PrimaryKey::Id(cert_id);
    match state.certificates.select(&cert_pk).await {
        Ok(Some(_)) => {}
        _ => return (StatusCode::NOT_FOUND, Json(json!({"message": "Not found"}))),
    }
    let mut body = body.0;
    // Force certificate FK to the path cert — 强制 certificate 外键指向路径中的证书
    if let Some(obj) = body.as_object_mut() {
        obj.insert("certificate".to_string(), json!({"id": cert_id.to_string()}));
    }
    let result = do_create::<Sni>(&state.snis, body).await;
    let _ = state.refresh_tx.send("snis");
    result
}

// ============ Route ↔ Service nested endpoints — 路由 ↔ 服务嵌套端点 ============

/// GET /routes/{route_id_or_name}/service — get the service associated with a route — 获取路由关联的服务
pub async fn get_route_service(
    State(state): State<AdminState>,
    Path(route_id_or_name): Path<String>,
) -> impl IntoResponse {
    let pk = PrimaryKey::from_str_or_uuid(&route_id_or_name);
    let route: Route = match state.routes.select(&pk).await {
        Ok(Some(r)) => r,
        _ => return (StatusCode::NOT_FOUND, Json(json!({"message": "Not found"}))),
    };
    let service_id = match &route.service {
        Some(fk) => fk.id.to_string(),
        None => return (StatusCode::NOT_FOUND, Json(json!({"message": "Not found"}))),
    };
    match state.services.select(&PrimaryKey::Id(uuid::Uuid::parse_str(&service_id).unwrap())).await {
        Ok(Some(svc)) => (StatusCode::OK, Json(serde_json::to_value(&svc).unwrap_or(json!(null)))),
        _ => (StatusCode::NOT_FOUND, Json(json!({"message": "Not found"}))),
    }
}

/// PATCH /routes/{route_id_or_name}/service — update the service associated with a route — 更新路由关联的服务
pub async fn update_route_service(
    State(state): State<AdminState>,
    Path(route_id_or_name): Path<String>,
    body: crate::extractors::FlexibleBody,
) -> impl IntoResponse {
    let pk = PrimaryKey::from_str_or_uuid(&route_id_or_name);
    let route: Route = match state.routes.select(&pk).await {
        Ok(Some(r)) => r,
        _ => return (StatusCode::NOT_FOUND, Json(json!({"message": "Not found"}))),
    };
    let service_id = match &route.service {
        Some(fk) => fk.id.to_string(),
        None => return (StatusCode::NOT_FOUND, Json(json!({"message": "Not found"}))),
    };
    let result = do_update::<Service>(&state.services, &service_id, &body.0).await;
    let _ = state.refresh_tx.send("services");
    result
}

/// PUT /routes/{route_id_or_name}/service — create or replace the service associated with a route — 创建或替换路由关联的服务
pub async fn upsert_route_service(
    State(state): State<AdminState>,
    Path(route_id_or_name): Path<String>,
    body: crate::extractors::FlexibleBody,
) -> impl IntoResponse {
    let pk = PrimaryKey::from_str_or_uuid(&route_id_or_name);
    let route: Route = match state.routes.select(&pk).await {
        Ok(Some(r)) => r,
        _ => return (StatusCode::NOT_FOUND, Json(json!({"message": "Not found"}))),
    };
    let body = body.0;
    if let Some(fk) = &route.service {
        let result = do_upsert::<Service>(&state.services, &fk.id.to_string(), body).await;
        let _ = state.refresh_tx.send("services");
        result
    } else {
        let result = do_create::<Service>(&state.services, body.clone()).await;
        let _ = state.refresh_tx.send("services");
        if let (StatusCode::CREATED, Json(ref svc_json)) = result {
            if let Some(svc_id) = svc_json.get("id").and_then(|v| v.as_str()) {
                let update_body = json!({"service": {"id": svc_id}});
                let _ = do_update::<Route>(&state.routes, &route_id_or_name, &update_body).await;
                let _ = state.refresh_tx.send("routes");
            }
        }
        result
    }
}

/// DELETE /routes/{route_id_or_name}/service — route exists→405, not found→404
pub async fn delete_route_service(
    State(state): State<AdminState>,
    Path(route_id_or_name): Path<String>,
) -> impl IntoResponse {
    let pk = PrimaryKey::from_str_or_uuid(&route_id_or_name);
    match state.routes.select(&pk).await {
        Ok(Some(_)) => (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(json!({"message": "Method not allowed"})),
        )
            .into_response(),
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({"message": "Not found"})),
        )
            .into_response(),
    }
}

/// Resolve a certificate identifier that may be a UUID or an SNI name — 解析可能是 UUID 或 SNI 名称的证书标识符
/// Returns the UUID string if resolved, None if not found — 如果解析成功返回 UUID 字符串，未找到返回 None
async fn resolve_cert_id_or_name(state: &AdminState, id_or_name: &str) -> Option<String> {
    // If it's a valid UUID, return as-is — 如果是有效 UUID，直接返回
    if uuid::Uuid::parse_str(id_or_name).is_ok() {
        return Some(id_or_name.to_string());
    }
    // Try SNI name lookup — 尝试 SNI 名称查找
    let sni_pk = PrimaryKey::EndpointKey(id_or_name.to_string());
    if let Ok(Some(sni)) = state.snis.select(&sni_pk).await {
        return Some(sni.certificate.id.to_string());
    }
    None
}
// Custom SNI handlers with wildcard validation — 自定义 SNI handler 含通配符验证
pub async fn list_snis(State(state): State<AdminState>, Query(params): Query<ListParams>) -> impl IntoResponse {
    do_list::<Sni>(&state.snis, &params).await
}
pub async fn get_sni(State(state): State<AdminState>, Path(id_or_name): Path<String>) -> impl IntoResponse {
    do_get::<Sni>(&state.snis, &id_or_name).await
}
pub async fn create_sni(State(state): State<AdminState>, FlexibleBody(body): FlexibleBody) -> impl IntoResponse {
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        if let Err(msg) = validate_sni_name(name) {
            return (StatusCode::BAD_REQUEST, Json(json!({"message": format!("schema violation (name: {})", msg), "name": "schema violation", "code": 2, "fields": {"name": msg}})));
        }
    }
    let result = do_create::<Sni>(&state.snis, body).await;
    if result.0.is_success() { let _ = state.refresh_tx.send("snis"); }
    result
}
pub async fn update_sni(State(state): State<AdminState>, Path(id_or_name): Path<String>, FlexibleBody(body): FlexibleBody) -> impl IntoResponse {
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        if let Err(msg) = validate_sni_name(name) {
            return (StatusCode::BAD_REQUEST, Json(json!({"message": format!("schema violation (name: {})", msg), "name": "schema violation", "code": 2, "fields": {"name": msg}})));
        }
    }
    let result = do_update::<Sni>(&state.snis, &id_or_name, &body).await;
    if result.0.is_success() { let _ = state.refresh_tx.send("snis"); }
    result
}
pub async fn upsert_sni(State(state): State<AdminState>, Path(id_or_name): Path<String>, FlexibleBody(body): FlexibleBody) -> impl IntoResponse {
    // Validate required fields for PUT SNI — PUT SNI 必填字段验证
    {
        let mut violations = Vec::new();
        let mut fields = serde_json::Map::new();
        let has_cert = body.get("certificate").and_then(|v| v.as_object()).and_then(|o| o.get("id")).is_some();
        let has_name = body.get("name").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty());
        if !has_cert { violations.push("certificate: required field missing"); fields.insert("certificate".to_string(), json!("required field missing")); }
        if !has_name { violations.push("name: required field missing"); fields.insert("name".to_string(), json!("required field missing")); }
        if !violations.is_empty() {
            let msg = if violations.len() > 1 { format!("{} schema violations ({})", violations.len(), violations.join("; ")) } else { format!("schema violation ({})", violations[0]) };
            return (StatusCode::BAD_REQUEST, Json(json!({"message": msg, "name": "schema violation", "code": 2, "fields": Value::Object(fields)})));
        }
    }
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        if let Err(msg) = validate_sni_name(name) {
            return (StatusCode::BAD_REQUEST, Json(json!({"message": format!("schema violation (name: {})", msg), "name": "schema violation", "code": 2, "fields": {"name": msg}})));
        }
    }
    let result = do_upsert::<Sni>(&state.snis, &id_or_name, body).await;
    if result.0.is_success() { let _ = state.refresh_tx.send("snis"); }
    result
}
pub async fn delete_sni(State(state): State<AdminState>, Path(id_or_name): Path<String>) -> impl IntoResponse {
    let result = do_delete::<Sni>(&state.snis, &id_or_name).await;
    let _ = state.refresh_tx.send("snis");
    result
}
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
            config.entry("key_names".to_string()).or_insert_with(|| json!(["apikey"]));
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
            config.entry("rename".to_string()).or_insert_with(|| json!({"headers": [], "json": []}));
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
        "error-generator" | "error-generator-last" | "error-generator-pre" | "error-generator-post" => {
            config.entry("rewrite".to_string()).or_insert(json!(false));
            config.entry("access".to_string()).or_insert(json!(false));
            config.entry("header_filter".to_string()).or_insert(json!(false));
            config.entry("log".to_string()).or_insert(json!(false));
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
        "error-generator", "error-generator-last", "error-generator-pre", "error-generator-post",
        "short-circuit", "short-circuit-last", "logger", "reports-api",
        "request-transformer-advanced", "response-transformer-advanced",
        "rate-limiting-advanced", "canary", "forward-proxy", "upstream-tls",
        "vault-auth", "key-auth-enc", "opa", "mocking", "degraphql",
        "graphql-proxy-cache-advanced", "graphql-rate-limiting-advanced",
        "jq", "exit-transformer", "kafka-log", "kafka-upstream",
        "mtls-auth", "application-registration", "websocket-size-limit",
        "websocket-validator", "openid-connect", "proxy-cache-advanced",
        "tls-handshake-modifier", "tls-metadata-headers",
        "enable-buffering", "enable-buffering-response",
        "admin-api-method",
    ];
    TEST_PLUGINS.contains(&name)
}

/// Validate transformer plugin config (response-transformer / request-transformer) — 验证转换器插件 config
///
/// Checks:
/// - add/replace/append.headers must contain colon separator (key:value) — add/replace/append.headers 必须包含冒号分隔符
/// - rename.headers must contain colon separator (old:new) — rename.headers 必须包含冒号分隔符
/// - rename.headers must have valid header names — rename.headers 必须包含有效的 header 名称
/// - null arrays are rejected with "required field missing" — null 数组将被拒绝
fn validate_transformer_plugin_config(
    plugin_name: &str,
    config: &Value,
) -> Option<(StatusCode, Json<Value>)> {
    match plugin_name {
        "response-transformer" => validate_response_transformer_config(config),
        "request-transformer" => validate_request_transformer_config(config),
        _ => None,
    }
}

/// Check if a header name is valid (no commas, spaces, or other invalid chars) — 检查 header 名称是否有效
fn is_valid_header_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // RFC 7230: token = 1*tchar, tchar = "!" / "#" / "$" / "%" / "&" / "'" / "*" / "+" / "-" / "." / "^" / "_" / "`" / "|" / "~" / DIGIT / ALPHA
    for b in name.bytes() {
        match b {
            b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' |
            b'^' | b'_' | b'`' | b'|' | b'~' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => {}
            _ => return false,
        }
    }
    true
}

/// Build a "schema violation" 400 error for transformer plugins — 构建转换器插件的 schema violation 400 错误
fn transformer_schema_violation(
    fields: serde_json::Map<String, Value>,
) -> (StatusCode, Json<Value>) {
    // Build message from fields — 从 fields 构建 message
    let mut parts = Vec::new();
    fn collect_parts(prefix: &str, val: &Value, parts: &mut Vec<String>) {
        match val {
            Value::Object(map) => {
                for (k, v) in map {
                    let path = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{}.{}", prefix, k)
                    };
                    collect_parts(&path, v, parts);
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        parts.push(format!("{}: {}", prefix, s));
                    }
                }
            }
            Value::String(s) => {
                parts.push(format!("{}: {}", prefix, s));
            }
            _ => {}
        }
    }
    for (k, v) in &fields {
        collect_parts(k, v, &mut parts);
    }
    let msg = if parts.is_empty() {
        "schema violation".to_string()
    } else {
        format!("schema violation ({})", parts.join("; "))
    };

    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "message": msg,
            "name": "schema violation",
            "code": 2,
            "fields": Value::Object(fields),
        })),
    )
}

/// Validate response-transformer config — 验证 response-transformer 配置
fn validate_response_transformer_config(config: &Value) -> Option<(StatusCode, Json<Value>)> {
    let config_obj = config.as_object()?;
    let mut errors = serde_json::Map::new();

    // Check null arrays first — 先检查 null 数组
    for section_name in &["remove", "rename", "replace", "add", "append"] {
        if let Some(section) = config_obj.get(*section_name) {
            if let Some(section_obj) = section.as_object() {
                let mut section_errors = serde_json::Map::new();
                let array_fields: &[&str] = match *section_name {
                    "remove" => &["headers", "json"],
                    "rename" => &["headers", "json"],
                    "replace" | "add" | "append" => &["headers", "json", "json_types"],
                    _ => &[],
                };
                for field in array_fields {
                    if let Some(val) = section_obj.get(*field) {
                        if val.is_null() {
                            section_errors.insert(field.to_string(), json!("required field missing"));
                        }
                    }
                }
                if !section_errors.is_empty() {
                    errors.insert(section_name.to_string(), Value::Object(section_errors));
                }
            }
        }
    }

    if !errors.is_empty() {
        let mut fields = serde_json::Map::new();
        fields.insert("config".to_string(), Value::Object(errors));
        return Some(transformer_schema_violation(fields));
    }

    // Check colon requirements — 检查冒号要求
    // add/replace/append.headers require colon — add/replace/append.headers 需要冒号
    for section_name in &["add", "replace", "append"] {
        if let Some(section) = config_obj.get(*section_name).and_then(|v| v.as_object()) {
            if let Some(headers) = section.get("headers").and_then(|v| v.as_array()) {
                for h in headers {
                    if let Some(s) = h.as_str() {
                        if !s.contains(':') {
                            let mut section_map = serde_json::Map::new();
                            section_map.insert(
                                "headers".to_string(),
                                json!([format!("invalid value: {}", s)]),
                            );
                            let mut config_map = serde_json::Map::new();
                            config_map
                                .insert(section_name.to_string(), Value::Object(section_map));
                            let mut fields = serde_json::Map::new();
                            fields.insert("config".to_string(), Value::Object(config_map));
                            return Some(transformer_schema_violation(fields));
                        }
                    }
                }
            }
        }
    }

    // rename.headers require colon — rename.headers 需要冒号
    if let Some(rename) = config_obj.get("rename").and_then(|v| v.as_object()) {
        if let Some(headers) = rename.get("headers").and_then(|v| v.as_array()) {
            for h in headers {
                if let Some(s) = h.as_str() {
                    if !s.contains(':') {
                        // No colon — 没有冒号
                        let mut section_map = serde_json::Map::new();
                        section_map.insert(
                            "headers".to_string(),
                            json!([format!("invalid value: {}", s)]),
                        );
                        let mut config_map = serde_json::Map::new();
                        config_map.insert("rename".to_string(), Value::Object(section_map));
                        let mut fields = serde_json::Map::new();
                        fields.insert("config".to_string(), Value::Object(config_map));
                        return Some(transformer_schema_violation(fields));
                    }
                    // Validate both header names — 验证两个 header 名称
                    let parts: Vec<&str> = s.splitn(2, ':').collect();
                    if parts.len() == 2 {
                        if !is_valid_header_name(parts[0]) {
                            let mut section_map = serde_json::Map::new();
                            section_map.insert(
                                "headers".to_string(),
                                json!([format!("'{}' is not a valid header", parts[0])]),
                            );
                            let mut config_map = serde_json::Map::new();
                            config_map.insert("rename".to_string(), Value::Object(section_map));
                            let mut fields = serde_json::Map::new();
                            fields.insert("config".to_string(), Value::Object(config_map));
                            return Some(transformer_schema_violation(fields));
                        }
                        if !is_valid_header_name(parts[1]) {
                            let mut section_map = serde_json::Map::new();
                            section_map.insert(
                                "headers".to_string(),
                                json!([format!("'{}' is not a valid header", parts[1])]),
                            );
                            let mut config_map = serde_json::Map::new();
                            config_map.insert("rename".to_string(), Value::Object(section_map));
                            let mut fields = serde_json::Map::new();
                            fields.insert("config".to_string(), Value::Object(config_map));
                            return Some(transformer_schema_violation(fields));
                        }
                    }
                }
            }
        }
    }

    None
}

/// Validate request-transformer config — 验证 request-transformer 配置
fn validate_request_transformer_config(config: &Value) -> Option<(StatusCode, Json<Value>)> {
    let config_obj = config.as_object()?;
    let mut errors = serde_json::Map::new();

    // Check null arrays first — 先检查 null 数组
    for section_name in &["remove", "rename", "replace", "add", "append"] {
        if let Some(section) = config_obj.get(*section_name) {
            if let Some(section_obj) = section.as_object() {
                let mut section_errors = serde_json::Map::new();
                for field in &["headers", "querystring", "body"] {
                    if let Some(val) = section_obj.get(*field) {
                        if val.is_null() {
                            section_errors.insert(field.to_string(), json!("required field missing"));
                        }
                    }
                }
                if !section_errors.is_empty() {
                    errors.insert(section_name.to_string(), Value::Object(section_errors));
                }
            }
        }
    }

    if !errors.is_empty() {
        let mut fields = serde_json::Map::new();
        fields.insert("config".to_string(), Value::Object(errors));
        return Some(transformer_schema_violation(fields));
    }

    // Check colon requirements for add/replace/append.headers — 检查 add/replace/append.headers 的冒号要求
    for section_name in &["add", "replace", "append"] {
        if let Some(section) = config_obj.get(*section_name).and_then(|v| v.as_object()) {
            if let Some(headers) = section.get("headers").and_then(|v| v.as_array()) {
                for h in headers {
                    if let Some(s) = h.as_str() {
                        if !s.contains(':') {
                            let mut section_map = serde_json::Map::new();
                            section_map.insert(
                                "headers".to_string(),
                                json!([format!("invalid value: {}", s)]),
                            );
                            let mut config_map = serde_json::Map::new();
                            config_map
                                .insert(section_name.to_string(), Value::Object(section_map));
                            let mut fields = serde_json::Map::new();
                            fields.insert("config".to_string(), Value::Object(config_map));
                            return Some(transformer_schema_violation(fields));
                        }
                    }
                }
            }
        }
    }

    None
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

/// Validate target hostname — 验证 target 主机名
fn validate_target_host(target_val: &str) -> Result<(), &'static str> {
    let host = if target_val.starts_with('[') {
        target_val.split(']').next().unwrap_or(target_val)
    } else {
        target_val.split(':').next().unwrap_or(target_val)
    };
    if host.contains(' ') || host.contains('\t') || host.is_empty() {
        return Err("Invalid target; not a valid hostname or ip address");
    }
    for ch in host.chars() {
        if ch == '[' || ch == ']' { continue; }
        if !(ch.is_alphanumeric() || ch == '-' || ch == '.' || ch == '_' || ch == ':') {
            return Err("Invalid target; not a valid hostname or ip address");
        }
    }
    Ok(())
}

/// Find a target within an upstream by name or UUID — 在 upstream 中按名称或 UUID 查找 target
async fn find_target_in_upstream(
    targets_dao: &Arc<dyn Dao<Target>>,
    upstream_id: &uuid::Uuid,
    target_id_or_name: &str,
) -> Result<Option<Target>, (StatusCode, Json<Value>)> {
    if let Ok(uuid) = uuid::Uuid::parse_str(target_id_or_name) {
        match targets_dao.select(&PrimaryKey::Id(uuid)).await {
            Ok(Some(t)) => {
                if t.upstream.id == *upstream_id { return Ok(Some(t)); }
                return Ok(None);
            }
            Ok(None) => return Ok(None),
            Err(e) => return Err((
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            )),
        }
    }
    let page_params = PageParams { size: 10000, ..Default::default() };
    match targets_dao.select_by_foreign_key("upstream", upstream_id, &page_params).await {
        Ok(page) => {
            for t in page.data {
                if t.target == target_id_or_name { return Ok(Some(t)); }
            }
            Ok(None)
        }
        Err(e) => Err((
            StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"message": e.to_string()})),
        )),
    }
}

/// Resolve upstream from path — 解析路径中的 upstream
async fn resolve_upstream(
    state: &AdminState,
    upstream_id_or_name: &str,
) -> Result<Upstream, (StatusCode, Json<Value>)> {
    let upstream_pk = PrimaryKey::from_str_or_uuid(upstream_id_or_name);
    match state.upstreams.select(&upstream_pk).await {
        Ok(Some(u)) => Ok(u),
        Ok(None) => Err((StatusCode::NOT_FOUND, Json(json!({"message": "upstream not found"})))),
        Err(e) => Err((
            StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"message": e.to_string()})),
        )),
    }
}

/// Build paginated response with custom next URL prefix — 构建自定义 next URL 前缀的分页响应
fn build_nested_page_response<T: Serialize>(page: &Page<T>, next_prefix: &str) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("data".to_string(), json!(page.data));
    body.insert("next".to_string(), match &page.offset {
        Some(o) => json!(format!("{}?offset={}", next_prefix, o)),
        None => Value::Null,
    });
    if let Some(ref offset) = page.offset {
        body.insert("offset".to_string(), json!(offset));
    }
    Value::Object(body)
}

/// GET /upstreams/:upstream_id/targets
pub async fn list_nested_targets(
    State(state): State<AdminState>,
    Path(upstream_id_or_name): Path<String>,
    Query(params): Query<ListParams>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    if let Err(err) = params.validate_tags() {
        return err;
    }
    let upstream = match resolve_upstream(&state, &upstream_id_or_name).await {
        Ok(u) => u,
        Err(e) => return e,
    };
    match state.targets.select_by_foreign_key("upstream", &upstream.id, &params.to_page_params()).await {
        Ok(page) => {
            let path = req.uri().path().trim_end_matches('/');
            (StatusCode::OK, Json(build_nested_page_response(&page, path)))
        }
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

    // Inject upstream FK and validate target field — 注入 upstream FK 并验证 target 字段
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "upstream".to_string(),
            json!({"id": upstream.id.to_string()}),
        );

        // Validate target field — 验证 target 字段
        let target_val = obj.get("target").and_then(|v| v.as_str()).unwrap_or("");
        if target_val.is_empty() {
            return (StatusCode::BAD_REQUEST, Json(json!({
                "message": "schema violation (target: required field missing)",
                "name": "schema violation", "code": 2,
                "fields": { "target": "required field missing" },
            })));
        }
        // Validate target hostname — 验证 target 主机名
        if let Err(msg) = validate_target_host(target_val) {
            return (StatusCode::BAD_REQUEST, Json(json!({
                "message": format!("schema violation (target: {})", msg),
                "name": "schema violation", "code": 2,
                "fields": { "target": msg },
            })));
        }
        // Validate weight range — 验证 weight 范围
        if let Some(w) = obj.get("weight").and_then(|v| v.as_i64()) {
            if w < 0 || w > 65535 {
                return (StatusCode::BAD_REQUEST, Json(json!({
                    "message": "schema violation (weight: value should be between 0 and 65535)",
                    "name": "schema violation", "code": 2,
                    "fields": { "weight": "value should be between 0 and 65535" },
                })));
            }
        }
        // Append default port :8000 if missing — 追加默认端口 :8000
        if !target_val.contains(':') {
            obj.insert("target".to_string(), json!(format!("{}:8000", target_val)));
        }
        // Generate cache_key for uniqueness — 生成 cache_key 保证唯一性
        let final_target = obj.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let cache_key = format!("{}:{}:", upstream.id, final_target);
        obj.insert("cache_key".to_string(), json!(cache_key));
        // Inject id (UUIDv7 for natural time-ordering) and timestamps — 注入 id（UUIDv7 保证时间顺序）和时间戳
        if !obj.contains_key("id") {
            obj.insert("id".to_string(), json!(uuid::Uuid::now_v7()));
        }
        let now = chrono::Utc::now().timestamp();
        if !obj.contains_key("created_at") {
            obj.insert("created_at".to_string(), json!(now));
        }
        if !obj.contains_key("updated_at") {
            obj.insert("updated_at".to_string(), json!(now));
        }
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
    let upstream = match resolve_upstream(&state, &upstream_id_or_name).await {
        Ok(u) => u,
        Err(e) => return e,
    };
    match find_target_in_upstream(&state.targets, &upstream.id, &target_id_or_name).await {
        Ok(Some(target)) => {
            let body = serde_json::to_value(&target).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"message": "target not found"}))),
        Err(e) => e,
    }
}

/// PATCH /upstreams/:upstream_id/targets/:id
pub async fn update_nested_target(
    State(state): State<AdminState>,
    Path((upstream_id_or_name, target_id_or_name)): Path<(String, String)>,
    FlexibleBody(mut body): FlexibleBody,
) -> impl IntoResponse {
    let upstream = match resolve_upstream(&state, &upstream_id_or_name).await {
        Ok(u) => u,
        Err(e) => return e,
    };
    let target = match find_target_in_upstream(&state.targets, &upstream.id, &target_id_or_name).await {
        Ok(Some(t)) => t,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"message": "target not found"}))),
        Err(e) => return e,
    };
    if let Some(obj) = body.as_object_mut() {
        // Ensure updated_at is strictly greater than created_at — 确保 updated_at 严格大于 created_at
        let now = chrono::Utc::now().timestamp();
        let new_updated_at = std::cmp::max(now, target.created_at + 1);
        obj.insert("updated_at".to_string(), json!(new_updated_at));
        // Update cache_key if target address changed — 如果 target 地址变了则更新 cache_key
        if let Some(new_target) = obj.get("target").and_then(|v| v.as_str()).map(|s| s.to_string()) {
            let t = if !new_target.contains(':') { format!("{}:8000", new_target) } else { new_target };
            obj.insert("cache_key".to_string(), json!(format!("{}:{}:", upstream.id, t)));
        }
    }
    let pk = PrimaryKey::Id(target.id);
    match state.targets.update(&pk, &body).await {
        Ok(updated) => {
            let _ = state.refresh_tx.send("targets");
            let body = serde_json::to_value(&updated).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

/// DELETE /upstreams/:upstream_id/targets/:id
pub async fn delete_nested_target(
    State(state): State<AdminState>,
    Path((upstream_id_or_name, target_id_or_name)): Path<(String, String)>,
) -> impl IntoResponse {
    let upstream = match resolve_upstream(&state, &upstream_id_or_name).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let target = match find_target_in_upstream(&state.targets, &upstream.id, &target_id_or_name).await {
        Ok(Some(t)) => t,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"message": "target not found"}))).into_response(),
        Err(e) => return e.into_response(),
    };
    let pk = PrimaryKey::Id(target.id);
    match state.targets.delete(&pk).await {
        Ok(_) => {
            let _ = state.refresh_tx.send("targets");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let body = json!({"message": e.to_string()});
            (status, Json(body)).into_response()
        }
    }
}

/// PUT /upstreams/{upstream_id}/targets/{target_id} — upsert target
pub async fn upsert_nested_target(
    State(state): State<AdminState>,
    Path((upstream_id_or_name, target_id_or_name)): Path<(String, String)>,
    FlexibleBody(mut body): FlexibleBody,
) -> impl IntoResponse {
    let upstream = match resolve_upstream(&state, &upstream_id_or_name).await {
        Ok(u) => u,
        Err(e) => return e,
    };
    // Find existing target scoped to this upstream — 在此 upstream 范围内查找已有 target
    let existing = match find_target_in_upstream(&state.targets, &upstream.id, &target_id_or_name).await {
        Ok(t) => t,
        Err(e) => return e,
    };
    if let Some(obj) = body.as_object_mut() {
        obj.insert("upstream".to_string(), json!({"id": upstream.id.to_string()}));
        // Merge existing target fields (PUT preserves unset fields for targets) — 合并已有 target 字段（PUT 保留未设置的字段）
        if let Some(ref existing_target) = existing {
            obj.insert("id".to_string(), json!(existing_target.id));
            // Preserve existing weight if not provided — 如果未提供则保留已有 weight
            if !obj.contains_key("weight") {
                obj.insert("weight".to_string(), json!(existing_target.weight));
            }
            // Preserve existing cache_key — 保留已有 cache_key
            if let Some(ref ck) = existing_target.cache_key {
                if !obj.contains_key("cache_key") {
                    obj.insert("cache_key".to_string(), json!(ck));
                }
            }
        }
        // Normalize target address (append :8000 if no port) — 规范化 target 地址（无端口时追加 :8000）
        if let Some(t) = obj.get("target").and_then(|v| v.as_str()).map(|s| s.to_string()) {
            let t = if !t.contains(':') { format!("{}:8000", t) } else { t };
            obj.insert("target".to_string(), json!(t));
            // Only set cache_key for new targets (existing keeps original) — 仅为新 target 设置 cache_key（已有保留原值）
            if existing.is_none() {
                obj.insert("cache_key".to_string(), json!(format!("{}:{}:", upstream.id, t)));
            }
        }
    }
    let upsert_key = if let Some(ref t) = existing { t.id.to_string() } else { target_id_or_name };
    let result = do_upsert::<Target>(&state.targets, &upsert_key, body).await;
    let _ = state.refresh_tx.send("targets");
    result
}

/// PUT /upstreams/{upstream_id}/targets/{target_id}/healthy or /unhealthy — set target health status — 设置 target 健康状态
pub async fn set_target_health(
    State(state): State<AdminState>,
    Path((upstream_id_or_name, target_id_or_name)): Path<(String, String)>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let upstream = match resolve_upstream(&state, &upstream_id_or_name).await {
        Ok(u) => u,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let target = match find_target_in_upstream(&state.targets, &upstream.id, &target_id_or_name).await {
        Ok(Some(t)) => t,
        _ => return (StatusCode::NOT_FOUND, Json(json!({"message": "Not found"}))).into_response(),
    };
    // Determine health status from request URI — 从请求 URI 确定健康状态
    let path = req.uri().path();
    let health_status = if path.ends_with("/healthy") {
        "HEALTHY"
    } else {
        "UNHEALTHY"
    };
    // Store health status in memory — 将健康状态存储在内存中
    let key = format!("{}:{}", upstream.id, target.target);
    if let Ok(mut map) = state.target_health.write() {
        map.insert(key, health_status.to_string());
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Check if upstream has healthchecks enabled (passive or active) — 检查 upstream 是否启用了健康检查
fn is_healthchecks_enabled(upstream: &Upstream) -> bool {
    let hc = &upstream.healthchecks;
    // Passive healthcheck is "on" if any threshold is non-zero — 被动健康检查"启用"条件：任一阈值非零
    let passive_on = hc.passive.healthy.successes > 0
        || hc.passive.unhealthy.tcp_failures > 0
        || hc.passive.unhealthy.http_failures > 0
        || hc.passive.unhealthy.timeouts > 0;
    // Active healthcheck is "on" if any interval is non-zero — 主动健康检查"启用"条件：任一检查间隔非零
    let active_on = hc.active.healthy.interval > 0.0
        || hc.active.unhealthy.interval > 0.0;
    passive_on || active_on
}

/// Resolve target IP address for health endpoint, using dns_hostsfile and system DNS — 为 health 端点解析 target IP 地址，使用 dns_hostsfile 和系统 DNS
fn resolve_target_ip(target_host: &str, dns_hostsfile: &str) -> Option<String> {
    // If it's already an IP address, return as-is — 如果已经是 IP 地址，直接返回
    if target_host.starts_with('[') || target_host.parse::<std::net::Ipv4Addr>().is_ok() {
        return Some(target_host.to_string());
    }
    // Check dns_hostsfile for custom hostname mappings — 检查 dns_hostsfile 中的自定义主机名映射
    if !dns_hostsfile.is_empty() {
        if let Ok(content) = std::fs::read_to_string(dns_hostsfile) {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') { continue; }
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    for hostname in &parts[1..] {
                        if *hostname == target_host {
                            return Some(parts[0].to_string());
                        }
                    }
                }
            }
        }
    }
    // Try system DNS resolution — 尝试系统 DNS 解析
    use std::net::ToSocketAddrs;
    let addr_str = format!("{}:0", target_host);
    match addr_str.to_socket_addrs() {
        Ok(mut addrs) => addrs.next().map(|a| a.ip().to_string()),
        Err(_) => None,
    }
}

/// GET /upstreams/{upstream_id}/health — upstream health status — 上游健康状态
pub async fn upstream_health(
    State(state): State<AdminState>,
    Path(upstream_id_or_name): Path<String>,
) -> impl IntoResponse {
    let upstream = match resolve_upstream(&state, &upstream_id_or_name).await {
        Ok(u) => u,
        Err(e) => return e,
    };
    let page_params = PageParams { size: 10000, ..Default::default() };
    let mut targets = match state.targets.select_by_foreign_key("upstream", &upstream.id, &page_params).await {
        Ok(page) => page.data,
        Err(_) => vec![],
    };
    // Sort by created_at DESC (newest first, like Kong) — 按 created_at 降序排列（最新在前，与 Kong 一致）
    targets.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
    let hc_enabled = is_healthchecks_enabled(&upstream);
    let active_hc_enabled = upstream.healthchecks.active.healthy.interval > 0.0
        || upstream.healthchecks.active.unhealthy.interval > 0.0;
    let health_map = state.target_health.read().ok();
    let dns_hostsfile = state.config.dns_hostsfile.as_str();
    let health_data: Vec<Value> = targets.iter().map(|t| {
        // Parse target host:port — 解析 target 的 host:port
        let (host, port) = if t.target.starts_with('[') {
            // IPv6 format: [::1]:port
            let parts: Vec<&str> = t.target.rsplitn(2, ':').collect();
            if parts.len() == 2 {
                (parts[1].to_string(), parts[0].parse::<u16>().unwrap_or(0))
            } else {
                (t.target.clone(), 0u16)
            }
        } else {
            let parts: Vec<&str> = t.target.splitn(2, ':').collect();
            if parts.len() == 2 {
                (parts[0].to_string(), parts[1].parse::<u16>().unwrap_or(0))
            } else {
                (t.target.clone(), 0u16)
            }
        };
        // Determine health status — 确定健康状态
        let health_key = format!("{}:{}", upstream.id, t.target);
        let health_status = if t.weight == 0 {
            // Weight 0 targets are always HEALTHCHECKS_OFF — weight=0 的 target 始终为 HEALTHCHECKS_OFF
            "HEALTHCHECKS_OFF".to_string()
        } else if !hc_enabled {
            // No healthchecks configured — 未配置健康检查
            // Try DNS resolution — 尝试 DNS 解析
            let host_only = host.trim_start_matches('[').trim_end_matches(']');
            if resolve_target_ip(host_only, dns_hostsfile).is_none() {
                "DNS_ERROR".to_string()
            } else {
                "HEALTHCHECKS_OFF".to_string()
            }
        } else {
            // Healthchecks enabled — 健康检查已启用
            if let Some(ref map) = health_map {
                if let Some(status) = map.get(&health_key) {
                    status.clone()
                } else {
                    // Try DNS first — 先尝试 DNS
                    let host_only = host.trim_start_matches('[').trim_end_matches(']');
                    if resolve_target_ip(host_only, dns_hostsfile).is_none() {
                        "DNS_ERROR".to_string()
                    } else if active_hc_enabled {
                        // Active healthcheck with probe: try TCP connect — 主动健康检查探测：尝试 TCP 连接
                        if let Some(ref ip) = resolve_target_ip(host_only, dns_hostsfile) {
                            let addr_str = format!("{}:{}", ip, port);
                            if let Ok(addr) = addr_str.parse::<std::net::SocketAddr>() {
                                match std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500)) {
                                    Ok(_) => "HEALTHY".to_string(),
                                    Err(_) => "UNHEALTHY".to_string(),
                                }
                            } else {
                                "HEALTHY".to_string()
                            }
                        } else {
                            "HEALTHY".to_string()
                        }
                    } else {
                        "HEALTHY".to_string()
                    }
                }
            } else {
                "HEALTHY".to_string()
            }
        };
        // Resolve IP for address data — 解析 IP 用于地址数据
        let host_only = host.trim_start_matches('[').trim_end_matches(']');
        let resolved_ip = resolve_target_ip(host_only, dns_hostsfile).unwrap_or_else(|| host_only.to_string());
        json!({
            "id": t.id,
            "target": t.target,
            "weight": t.weight,
            "upstream": { "id": upstream.id },
            "health": health_status,
            "created_at": t.created_at,
            "data": {
                "addresses": [{
                    "ip": resolved_ip,
                    "port": port,
                    "health": health_status,
                    "weight": t.weight,
                }]
            },
        })
    }).collect();
    (StatusCode::OK, Json(json!({
        "id": upstream.id,
        "name": upstream.name,
        "node_id": state.node_id.to_string(),
        "data": health_data,
    })))
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

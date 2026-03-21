//! Schema endpoints for entities and plugins. — 实体和插件的 schema 端点。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::AdminState;

/// GET /schemas/{entity_name} — Return a minimal entity schema (Kong-compatible). — GET /schemas/{entity_name} — 返回最小化的实体 schema（Kong 兼容）。
pub async fn get_entity_schema(
    Path(entity_name): Path<String>,
) -> impl IntoResponse {
    // Return a minimal but valid schema object for known entity types — 对已知实体类型返回最小但有效的 schema 对象
    let schema = match entity_name.as_str() {
        "services" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"name": {"type": "string", "unique": true}},
                {"host": {"type": "string", "required": true}},
                {"port": {"type": "integer", "default": 80, "between": [0, 65535]}},
                {"protocol": {"type": "string", "default": "http", "one_of": ["grpc", "grpcs", "http", "https", "tcp", "tls", "udp"]}},
                {"path": {"type": "string"}},
                {"retries": {"type": "integer", "default": 5}},
                {"connect_timeout": {"type": "integer", "default": 60000}},
                {"read_timeout": {"type": "integer", "default": 60000}},
                {"write_timeout": {"type": "integer", "default": 60000}},
                {"enabled": {"type": "boolean", "default": true}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ]
        }),
        "routes" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"name": {"type": "string", "unique": true}},
                {"protocols": {"type": "set", "elements": {"type": "string"}, "default": ["http", "https"]}},
                {"methods": {"type": "set", "elements": {"type": "string"}}},
                {"hosts": {"type": "array", "elements": {"type": "string"}}},
                {"paths": {"type": "array", "elements": {"type": "string"}}},
                {"headers": {"type": "map", "keys": {"type": "string"}, "values": {"type": "array", "elements": {"type": "string"}}}},
                {"strip_path": {"type": "boolean", "default": true}},
                {"preserve_host": {"type": "boolean", "default": false}},
                {"service": {"type": "foreign", "reference": "services"}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ]
        }),
        "consumers" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"username": {"type": "string", "unique": true}},
                {"custom_id": {"type": "string", "unique": true}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ]
        }),
        "plugins" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"name": {"type": "string", "required": true}},
                {"config": {"type": "record"}},
                {"enabled": {"type": "boolean", "default": true}},
                {"service": {"type": "foreign", "reference": "services"}},
                {"route": {"type": "foreign", "reference": "routes"}},
                {"consumer": {"type": "foreign", "reference": "consumers"}},
                {"protocols": {"type": "set", "elements": {"type": "string"}}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
            ]
        }),
        "upstreams" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"name": {"type": "string", "required": true, "unique": true}},
                {"algorithm": {"type": "string", "default": "round-robin"}},
                {"slots": {"type": "integer", "default": 10000}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
            ]
        }),
        "certificates" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"cert": {"type": "string", "required": true}},
                {"key": {"type": "string", "required": true}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
            ]
        }),
        "snis" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"name": {"type": "string", "required": true, "unique": true}},
                {"certificate": {"type": "foreign", "reference": "certificates", "required": true}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
            ]
        }),
        "ca_certificates" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"cert": {"type": "string", "required": true}},
                {"cert_digest": {"type": "string"}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
            ]
        }),
        "targets" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"target": {"type": "string", "required": true}},
                {"weight": {"type": "integer", "default": 100}},
                {"upstream": {"type": "foreign", "reference": "upstreams", "required": true}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
            ]
        }),
        "vaults" => json!({
            "fields": [
                {"id": {"type": "string", "uuid": true, "auto": true}},
                {"name": {"type": "string", "required": true}},
                {"prefix": {"type": "string", "required": true, "unique": true}},
                {"description": {"type": "string"}},
                {"config": {"type": "record"}},
                {"tags": {"type": "set", "elements": {"type": "string"}}},
                {"created_at": {"type": "integer", "timestamp": true, "auto": true}},
                {"updated_at": {"type": "integer", "timestamp": true, "auto": true}},
            ]
        }),
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "message": format!("No entity named '{}'", entity_name),
                })),
            )
                .into_response();
        }
    };

    (StatusCode::OK, Json(schema)).into_response()
}

/// All known Kong bundled plugin names — 所有已知的 Kong 内置插件名
const BUNDLED_PLUGINS: &[&str] = &[
    "key-auth", "basic-auth", "rate-limiting", "cors",
    "tcp-log", "file-log", "http-log", "udp-log",
    "ip-restriction", "request-transformer", "response-transformer",
    "pre-function", "post-function",
    "acl", "bot-detection", "correlation-id", "jwt", "hmac-auth",
    "oauth2", "ldap-auth", "session",
    "request-size-limiting", "request-termination", "response-ratelimiting",
    "syslog", "loggly", "datadog", "statsd", "prometheus",
    "zipkin", "opentelemetry", "grpc-gateway", "grpc-web",
    "aws-lambda", "azure-functions", "proxy-cache", "request-debug",
    // Test/dev plugins — 测试/开发插件
    "rewriter", "dummy", "error-generator-last", "short-circuit",
    "ctx-checker", "ctx-checker-last", "enable-buffering", "mocking",
];

/// Return a minimal valid plugin schema stub — 返回最小有效的插件 schema 占位
fn minimal_plugin_schema(name: &str) -> serde_json::Value {
    json!({
        "fields": [
            {"config": {"type": "record", "fields": []}}
        ],
        "name": name,
    })
}

/// GET /schemas/plugins/{name} — Return plugin schema loaded from schema.lua. — GET /schemas/plugins/{name} — 返回从 schema.lua 加载的插件 schema。
pub async fn get_plugin_schema(
    Path(name): Path<String>,
    State(state): State<AdminState>,
) -> impl IntoResponse {
    let plugin_dirs = kong_lua_bridge::loader::resolve_plugin_dirs(&state.config.prefix);

    match kong_lua_bridge::loader::load_plugin_schema(&plugin_dirs, &name) {
        Ok(schema) => (StatusCode::OK, Json(schema)).into_response(),
        Err(_err) => {
            // Fall back to minimal schema for known bundled plugins — 对已知内置插件回退到最小 schema
            if BUNDLED_PLUGINS.contains(&name.as_str()) {
                (StatusCode::OK, Json(minimal_plugin_schema(&name))).into_response()
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({
                        "message": format!("No plugin named '{}'", name),
                    })),
                )
                    .into_response()
            }
        }
    }
}

/// GET /schemas/vaults/{name} — Return vault schema — GET /schemas/vaults/{name} — 返回 vault schema
pub async fn get_vault_schema(
    Path(name): Path<String>,
) -> impl IntoResponse {
    match name.as_str() {
        "env" => {
            (StatusCode::OK, Json(json!({
                "fields": [
                    {"config": {"type": "record", "fields": [
                        {"prefix": {"type": "string", "description": "Environment variable prefix"}}
                    ]}}
                ],
                "name": "env",
            }))).into_response()
        }
        _ => {
            (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "message": format!("No vault named '{}'", name),
                })),
            ).into_response()
        }
    }
}

/// POST /schemas/plugins/validate — Validate a plugin schema definition — POST /schemas/plugins/validate — 验证插件 schema 定义
pub async fn validate_plugin_schema(
    State(state): State<AdminState>,
    body: Option<axum::Json<serde_json::Value>>,
) -> impl IntoResponse {
    let body = match body {
        Some(axum::Json(v)) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": "schema violation (name: required field missing)",
                    "name": "schema violation",
                    "code": 2,
                    "fields": {"name": "required field missing"},
                })),
            ).into_response();
        }
    };

    // Validate plugin name is present — 验证插件 name 字段是否存在
    let plugin_name = match body.get("name").and_then(|v| v.as_str()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": "schema violation (name: required field missing)",
                    "name": "schema violation",
                    "code": 2,
                    "fields": {"name": "required field missing"},
                })),
            ).into_response();
        }
    };

    // Check if plugin schema exists — 检查插件 schema 是否存在
    let plugin_dirs = kong_lua_bridge::loader::resolve_plugin_dirs(&state.config.prefix);
    match kong_lua_bridge::loader::load_plugin_schema(&plugin_dirs, &plugin_name) {
        Ok(_schema) => {
            // Plugin found and schema is valid — 插件已找到且 schema 有效
            (
                StatusCode::OK,
                Json(json!({"message": "schema validation successful"})),
            ).into_response()
        }
        Err(err) => {
            // Bundled plugins without lua schema are still valid — 没有 lua schema 的内置插件仍然有效
            if BUNDLED_PLUGINS.contains(&plugin_name.as_str()) {
                (
                    StatusCode::OK,
                    Json(json!({"message": "schema validation successful"})),
                ).into_response()
            } else if matches!(&err, kong_core::error::KongError::NotFound { .. }) {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "message": format!("No plugin named '{}'", plugin_name),
                        "name": "schema violation",
                        "code": 2,
                        "fields": {"name": format!("No plugin named '{}'", plugin_name)},
                    })),
                ).into_response()
            } else {
                let status = StatusCode::from_u16(err.status_code())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                (
                    status,
                    Json(json!({
                        "message": err.to_string(),
                        "name": err.error_name(),
                        "code": err.error_code(),
                    })),
                ).into_response()
            }
        }
    }
}

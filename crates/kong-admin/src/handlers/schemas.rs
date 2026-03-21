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
                    "message": format!("No entity named '{}' was found", entity_name),
                    "name": "not found",
                    "code": 3,
                })),
            )
                .into_response();
        }
    };

    (StatusCode::OK, Json(schema)).into_response()
}

/// GET /schemas/plugins/{name} — Return plugin schema loaded from schema.lua. — GET /schemas/plugins/{name} — 返回从 schema.lua 加载的插件 schema。
pub async fn get_plugin_schema(
    Path(name): Path<String>,
    State(state): State<AdminState>,
) -> impl IntoResponse {
    let plugin_dirs = kong_lua_bridge::loader::resolve_plugin_dirs(&state.config.prefix);

    match kong_lua_bridge::loader::load_plugin_schema(&plugin_dirs, &name) {
        Ok(schema) => (StatusCode::OK, Json(schema)).into_response(),
        Err(err) => {
            let status = StatusCode::from_u16(err.status_code())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (
                status,
                Json(json!({
                    "message": err.to_string(),
                    "name": err.error_name(),
                    "code": err.error_code(),
                })),
            )
                .into_response()
        }
    }
}

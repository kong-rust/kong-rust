//! Schema endpoints for entities and plugins. — 实体和插件的 schema 端点。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::AdminState;

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

//! Debug endpoints — Kong 兼容的调试端点
//!
//! Currently implements: — 目前实现：
//! - `GET  /debug/node/log-level`        — read current log level — 读取当前日志级别
//! - `PUT  /debug/node/log-level/{level}` — change log level at runtime — 运行时修改日志级别
//!
//! Accepted levels (Kong-style): `debug`, `info`, `notice`, `warn`, `error`, `crit`, `alert`, `emerg`.
//! 接受的级别（Kong 风格）：`debug`、`info`、`notice`、`warn`、`error`、`crit`、`alert`、`emerg`。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::AdminState;

/// Validate a Kong-style log level string.
/// 校验 Kong 风格日志级别字符串。
fn is_valid_level(level: &str) -> bool {
    matches!(
        level,
        "debug" | "info" | "notice" | "warn" | "error" | "crit" | "alert" | "emerg"
    )
}

/// `GET /debug/node/log-level` — return the current process-wide log level.
/// `GET /debug/node/log-level` — 返回当前进程级日志级别。
pub async fn get_log_level(State(state): State<AdminState>) -> (StatusCode, Json<Value>) {
    let level = state
        .current_log_level
        .read()
        .map(|g| g.clone())
        .unwrap_or_else(|_| "info".to_string());
    (
        StatusCode::OK,
        Json(json!({ "message": format!("log level: {}", level) })),
    )
}

/// `PUT /debug/node/log-level/{level}` — change the log level at runtime.
/// `PUT /debug/node/log-level/{level}` — 运行时切换日志级别。
pub async fn set_log_level(
    State(state): State<AdminState>,
    Path(level): Path<String>,
) -> (StatusCode, Json<Value>) {
    if !is_valid_level(&level) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "message": format!("unknown log level: {}", level) })),
        );
    }

    let Some(updater) = state.log_updater.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "message": "log level reload not supported (logger initialised without reload handle)"
            })),
        );
    };

    if let Err(err) = updater(&level) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": format!("failed to reload log level: {}", err) })),
        );
    }

    if let Ok(mut cur) = state.current_log_level.write() {
        *cur = level.clone();
    }

    (
        StatusCode::OK,
        Json(json!({ "message": format!("log level changed to {}", level) })),
    )
}

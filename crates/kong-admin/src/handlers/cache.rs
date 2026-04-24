//! Cache management endpoints — Kong 兼容的缓存管理端点
//!
//! Implements Kong's `/cache/{key}` endpoints: — 实现 Kong 的 `/cache/{key}` 端点：
//! - `GET /cache/{key}` — fetch a cached value by key — 按 key 查询缓存值
//! - `DELETE /cache/{key}` — invalidate a single cached entry — 删除单条缓存
//! - `DELETE /cache` — purge the entire cache — 清空整个缓存

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::AdminState;

/// `GET /cache/{key}` — return cached JSON value, or 404 if absent.
/// `GET /cache/{key}` — 返回缓存的 JSON 值，不存在时 404。
///
/// Response format mirrors Kong's: the cached payload is returned as the response body.
/// 响应格式与 Kong 一致：缓存内容作为响应体直接返回。
pub async fn get_cache_entry(
    State(state): State<AdminState>,
    Path(key): Path<String>,
) -> (StatusCode, Json<Value>) {
    match state.cache.get(&key) {
        Some(Some(value)) => (StatusCode::OK, Json(value)),
        Some(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "Not found" })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "Not found" })),
        ),
    }
}

/// `DELETE /cache/{key}` — invalidate a single cache entry.
/// `DELETE /cache/{key}` — 删除单条缓存。
///
/// Always returns 204, matching Kong's idempotent semantics (no error if the key is absent).
/// 始终返回 204，与 Kong 幂等语义一致（key 不存在也不报错）。
pub async fn delete_cache_entry(
    State(state): State<AdminState>,
    Path(key): Path<String>,
) -> StatusCode {
    state.cache.invalidate(&key);
    StatusCode::NO_CONTENT
}

/// `DELETE /cache` — purge the whole cache.
/// `DELETE /cache` — 清空整个缓存。
pub async fn purge_cache(State(state): State<AdminState>) -> StatusCode {
    state.cache.purge();
    StatusCode::NO_CONTENT
}

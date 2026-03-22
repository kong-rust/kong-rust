//! AI Virtual Key Admin API handlers — AI Virtual Key 管理 API 处理器

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use sha2::{Sha256, Digest};
use serde_json::{json, Value};

use kong_ai::models::AiVirtualKey;
use kong_core::traits::{Entity, PrimaryKey};

use crate::extractors::FlexibleBody;
use crate::AdminState;
use super::{ListParams, do_list, do_get, do_update, do_delete};

/// Remove key_hash from response (security) — 从响应中移除 key_hash（安全）
fn strip_key_hash(mut val: Value) -> Value {
    if let Some(obj) = val.as_object_mut() {
        obj.remove("key_hash");
    }
    val
}

/// Strip key_hash from paginated list response — 从分页列表响应中移除 key_hash
fn strip_key_hash_list(mut resp: Value) -> Value {
    if let Some(arr) = resp.get_mut("data").and_then(|d| d.as_array_mut()) {
        for item in arr.iter_mut() {
            if let Some(obj) = item.as_object_mut() {
                obj.remove("key_hash");
            }
        }
    }
    resp
}

/// Generate a new virtual key — 生成新的虚拟密钥
fn generate_key() -> (String, String, String) {
    let raw_key = format!("sk-kr-{}", uuid::Uuid::new_v4().to_string().replace("-", ""));
    let key_hash = format!("{:x}", Sha256::digest(raw_key.as_bytes()));
    let key_prefix = raw_key[..8].to_string();
    (raw_key, key_hash, key_prefix)
}

/// GET /ai-virtual-keys — 列出所有 AI Virtual Key
pub async fn list(
    State(state): State<AdminState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let (status, Json(resp)) = do_list::<AiVirtualKey>(&state.ai_virtual_keys, &params).await;
    (status, Json(strip_key_hash_list(resp)))
}

/// GET /ai-virtual-keys/:id_or_name — 获取单个 AI Virtual Key
pub async fn get_one(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    let (status, Json(resp)) = do_get::<AiVirtualKey>(&state.ai_virtual_keys, &id_or_name).await;
    (status, Json(strip_key_hash(resp)))
}

/// POST /ai-virtual-keys — 创建 AI Virtual Key（生成密钥，存储哈希，一次性返回原始密钥）
pub async fn create(
    State(state): State<AdminState>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let mut body = body;
    let (raw_key, key_hash, key_prefix) = generate_key();

    // 注入 key_hash 和 key_prefix — inject key_hash and key_prefix
    if let Some(obj) = body.as_object_mut() {
        obj.insert("key_hash".to_string(), json!(key_hash));
        obj.insert("key_prefix".to_string(), json!(key_prefix));
        // 设置 budget_used 默认值 — set default budget_used
        if !obj.contains_key("budget_used") {
            obj.insert("budget_used".to_string(), json!(0.0));
        }
        // 设置 enabled 默认值 — set default enabled
        if !obj.contains_key("enabled") {
            obj.insert("enabled".to_string(), json!(true));
        }
    }

    let (status, Json(mut resp)) = super::do_create::<AiVirtualKey>(&state.ai_virtual_keys, body).await;

    if status.is_success() {
        // 一次性返回原始密钥，同时移除 key_hash — return raw key once, strip key_hash
        if let Some(obj) = resp.as_object_mut() {
            obj.remove("key_hash");
            obj.insert("key".to_string(), json!(raw_key));
        }
    }

    (status, Json(resp))
}

/// PATCH /ai-virtual-keys/:id_or_name — 更新 AI Virtual Key
pub async fn update(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let (status, Json(resp)) = do_update::<AiVirtualKey>(&state.ai_virtual_keys, &id_or_name, &body).await;
    (status, Json(strip_key_hash(resp)))
}

/// DELETE /ai-virtual-keys/:id_or_name — 删除 AI Virtual Key
pub async fn delete_one(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    do_delete::<AiVirtualKey>(&state.ai_virtual_keys, &id_or_name).await
}

/// POST /ai-virtual-keys/:id/rotate — 轮换密钥（生成新密钥，更新哈希）
pub async fn rotate(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // 先获取现有 key — fetch existing key first
    let pk = PrimaryKey::from_str_or_uuid(&id);
    let existing = match state.ai_virtual_keys.select(&pk).await {
        Ok(Some(k)) => k,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "message": format!("{} not found", AiVirtualKey::table_name()),
                    "name": "not found",
                    "code": 3,
                })),
            ).into_response();
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            return (status, Json(json!({"message": e.to_string()}))).into_response();
        }
    };

    // 生成新密钥 — generate new key
    let (raw_key, key_hash, key_prefix) = generate_key();

    // 更新 key_hash 和 key_prefix — update key_hash and key_prefix
    let update_body = json!({
        "key_hash": key_hash,
        "key_prefix": key_prefix,
    });

    let (status, Json(mut resp)) = do_update::<AiVirtualKey>(
        &state.ai_virtual_keys,
        &existing.id.to_string(),
        &update_body,
    ).await;

    if status.is_success() {
        // 一次性返回新密钥 — return new key once
        if let Some(obj) = resp.as_object_mut() {
            obj.remove("key_hash");
            obj.insert("key".to_string(), json!(raw_key));
        }
    }

    (status, Json(resp)).into_response()
}

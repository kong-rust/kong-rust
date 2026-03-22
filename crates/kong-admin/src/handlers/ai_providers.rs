//! AI Provider Admin API handlers — AI Provider 管理 API 处理器

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use kong_ai::models::AiProviderConfig;
use kong_core::traits::{Entity, PrimaryKey};

use crate::extractors::FlexibleBody;
use crate::AdminState;
use super::{ListParams, do_list, do_create, do_get, do_update, do_upsert, do_delete, build_page_response};

/// Mask auth_config sensitive fields in provider response — 在 provider 响应中遮蔽 auth_config 敏感字段
fn mask_auth_config(mut val: Value) -> Value {
    if let Some(obj) = val.as_object_mut() {
        if let Some(auth) = obj.get_mut("auth_config") {
            if let Some(auth_obj) = auth.as_object_mut() {
                // 遮蔽所有敏感凭证字段 — mask all sensitive credential fields
                for key in &["header_value", "param_value", "aws_secret_access_key", "gcp_service_account_json"] {
                    if auth_obj.contains_key(*key) {
                        auth_obj.insert(key.to_string(), json!("***"));
                    }
                }
            }
        }
    }
    val
}

/// GET /ai-providers — 列出所有 AI Provider
pub async fn list(
    State(state): State<AdminState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let (status, Json(mut resp)) = do_list::<AiProviderConfig>(&state.ai_providers, &params).await;
    // 遮蔽列表中每个 provider 的 auth_config — mask auth_config in each provider
    if let Some(arr) = resp.get_mut("data").and_then(|d| d.as_array_mut()) {
        for item in arr.iter_mut() {
            *item = mask_auth_config(item.take());
        }
    }
    (status, Json(resp))
}

/// GET /ai-providers/:id_or_name — 获取单个 AI Provider
pub async fn get_one(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    let (status, Json(resp)) = do_get::<AiProviderConfig>(&state.ai_providers, &id_or_name).await;
    (status, Json(mask_auth_config(resp)))
}

/// POST /ai-providers — 创建 AI Provider
pub async fn create(
    State(state): State<AdminState>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let (status, Json(resp)) = do_create::<AiProviderConfig>(&state.ai_providers, body).await;
    (status, Json(mask_auth_config(resp)))
}

/// PATCH /ai-providers/:id_or_name — 更新 AI Provider
pub async fn update(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let (status, Json(resp)) = do_update::<AiProviderConfig>(&state.ai_providers, &id_or_name, &body).await;
    (status, Json(mask_auth_config(resp)))
}

/// PUT /ai-providers/:id_or_name — 替换（upsert）AI Provider
pub async fn upsert(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let (status, Json(resp)) = do_upsert::<AiProviderConfig>(&state.ai_providers, &id_or_name, body).await;
    (status, Json(mask_auth_config(resp)))
}

/// DELETE /ai-providers/:id_or_name — 删除 AI Provider
pub async fn delete_one(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
) -> impl IntoResponse {
    do_delete::<AiProviderConfig>(&state.ai_providers, &id_or_name).await
}

/// GET /ai-providers/:id/ai-models — 列出某个 Provider 下的所有 AI Model
pub async fn list_models(
    State(state): State<AdminState>,
    Path(id_or_name): Path<String>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    // 先解析 provider ID — resolve provider ID first
    let pk = PrimaryKey::from_str_or_uuid(&id_or_name);
    let provider = match state.ai_providers.select(&pk).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "message": format!("{} not found", AiProviderConfig::table_name()),
                    "name": "not found",
                    "code": 3,
                })),
            );
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            return (status, Json(json!({"message": e.to_string()})));
        }
    };

    // 按 provider_id 外键查询 models — query models by provider_id foreign key
    match state
        .ai_models
        .select_by_foreign_key("provider_id", &provider.id, &params.to_page_params())
        .await
    {
        Ok(page) => (StatusCode::OK, Json(build_page_response(&page))),
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

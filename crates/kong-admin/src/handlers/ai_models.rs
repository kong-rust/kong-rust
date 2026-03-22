//! AI Model Admin API handlers — AI Model 管理 API 处理器

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use kong_ai::models::AiModel;

use crate::extractors::FlexibleBody;
use crate::AdminState;
use super::{ListParams, do_list, do_create, do_get, do_update, do_upsert, do_delete};

/// GET /ai-models — 列出所有 AI Model
pub async fn list(
    State(state): State<AdminState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    do_list::<AiModel>(&state.ai_models, &params).await
}

/// GET /ai-models/:id — 获取单个 AI Model
pub async fn get_one(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    do_get::<AiModel>(&state.ai_models, &id).await
}

/// POST /ai-models — 创建 AI Model
pub async fn create(
    State(state): State<AdminState>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    do_create::<AiModel>(&state.ai_models, body).await
}

/// PATCH /ai-models/:id — 更新 AI Model
pub async fn update(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    do_update::<AiModel>(&state.ai_models, &id, &body).await
}

/// PUT /ai-models/:id — 替换（upsert）AI Model
pub async fn upsert(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    do_upsert::<AiModel>(&state.ai_models, &id, body).await
}

/// DELETE /ai-models/:id — 删除 AI Model
pub async fn delete_one(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    do_delete::<AiModel>(&state.ai_models, &id).await
}

/// GET /ai-model-groups — 列出所有不同的 model name（model group = 同名模型组成负载均衡组）
pub async fn list_groups(
    State(state): State<AdminState>,
    Query(_params): Query<ListParams>,
) -> impl IntoResponse {
    // 全量拉取 model，提取 distinct name 作为 group
    // fetch all models (large page), extract distinct names as groups
    use kong_core::traits::PageParams;
    let all_params = PageParams { size: 10000, ..PageParams::default() };
    match state.ai_models.page(&all_params).await {
        Ok(page) => {
            let mut seen = std::collections::HashSet::new();
            let mut groups = Vec::new();
            for model in &page.data {
                if seen.insert(model.name.clone()) {
                    groups.push(json!({ "name": model.name }));
                }
            }
            (StatusCode::OK, Json(json!({ "data": groups, "next": Value::Null })))
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

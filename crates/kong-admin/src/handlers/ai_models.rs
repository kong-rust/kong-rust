//! AI Model Admin API handlers — AI Model 管理 API 处理器

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::OnceLock;

use kong_ai::models::{AiModel, AiProviderConfig};
use kong_ai::usage::model::decimal_12;
use kong_ai::usage::pricing::{
    model_override_version, ModelPriceOverrides, PriceDirection, PricingFeatures,
};
use kong_ai::usage::PriceCatalog;
use rust_decimal::Decimal;

use super::{do_create, do_delete, do_get, do_list, do_update, do_upsert, ListParams};
use crate::extractors::FlexibleBody;
use crate::AdminState;

/// GET /ai-models — 列出所有 AI Model
pub async fn list(
    State(state): State<AdminState>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let (status, Json(body)) = do_list::<AiModel>(&state.ai_models, &params).await;
    (
        status,
        Json(project_model_response(&state, status, body).await),
    )
}

/// GET /ai-models/:id — 获取单个 AI Model
pub async fn get_one(State(state): State<AdminState>, Path(id): Path<String>) -> impl IntoResponse {
    let (status, Json(body)) = do_get::<AiModel>(&state.ai_models, &id).await;
    (
        status,
        Json(project_model_response(&state, status, body).await),
    )
}

/// POST /ai-models — 创建 AI Model
pub async fn create(
    State(state): State<AdminState>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let (status, Json(body)) = do_create::<AiModel>(&state.ai_models, body).await;
    (
        status,
        Json(project_model_response(&state, status, body).await),
    )
}

/// PATCH /ai-models/:id — 更新 AI Model
pub async fn update(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let (status, Json(body)) = do_update::<AiModel>(&state.ai_models, &id, &body).await;
    (
        status,
        Json(project_model_response(&state, status, body).await),
    )
}

/// PUT /ai-models/:id — 替换（upsert）AI Model
pub async fn upsert(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    FlexibleBody(body): FlexibleBody,
) -> impl IntoResponse {
    let (status, Json(body)) = do_upsert::<AiModel>(&state.ai_models, &id, body).await;
    (
        status,
        Json(project_model_response(&state, status, body).await),
    )
}

pub(crate) async fn project_model_response(
    state: &AdminState,
    status: StatusCode,
    mut body: Value,
) -> Value {
    if !status.is_success() {
        return body;
    }
    use kong_core::traits::PageParams;
    let providers = state
        .ai_providers
        .page(&PageParams {
            size: 10_000,
            ..Default::default()
        })
        .await
        .map(|page| {
            page.data
                .into_iter()
                .map(|provider| (provider.id, provider))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    if let Some(data) = body.get_mut("data").and_then(Value::as_array_mut) {
        for model in data {
            project_model(model, &providers);
        }
    } else {
        project_model(&mut body, &providers);
    }
    body
}

fn project_model(value: &mut Value, providers: &HashMap<uuid::Uuid, AiProviderConfig>) {
    let Some(model) = value.as_object_mut() else {
        return;
    };
    let input = model
        .get("input_cost")
        .and_then(Value::as_str)
        .and_then(|value| Decimal::from_str(value).ok());
    let output = model
        .get("output_cost")
        .and_then(Value::as_str)
        .and_then(|value| Decimal::from_str(value).ok());
    model.insert(
        "input_cost_decimal".to_string(),
        input
            .map(decimal_12)
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    model.insert(
        "output_cost_decimal".to_string(),
        output
            .map(decimal_12)
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    model.insert("input_cost".to_string(), decimal_compat_number(input));
    model.insert("output_cost".to_string(), decimal_compat_number(output));

    let provider_id = model
        .get("provider_id")
        .and_then(Value::as_str)
        .and_then(|value| uuid::Uuid::parse_str(value).ok());
    let actual_model = model
        .get("model_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(provider) = provider_id.and_then(|id| providers.get(&id)) else {
        model.insert("effective_pricing".to_string(), Value::Null);
        return;
    };
    let now = chrono::Utc::now();
    let catalog = builtin_catalog();
    let model_id = model
        .get("id")
        .and_then(Value::as_str)
        .and_then(|value| uuid::Uuid::parse_str(value).ok());
    let updated_at = model
        .get("updated_at")
        .and_then(Value::as_i64)
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0));
    let effective_from = updated_at.or_else(|| {
        model
            .get("created_at")
            .and_then(Value::as_i64)
            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
    });
    let model_id_text = model_id.map(|id| id.to_string());
    let pricing = catalog.resolve(
        &provider.provider_type,
        actual_model,
        now,
        None,
        &ModelPriceOverrides {
            input,
            output,
            input_version: input.map(|rate| {
                model_override_version(
                    model_id_text.as_deref(),
                    effective_from,
                    &provider.provider_type,
                    actual_model,
                    PriceDirection::Input,
                    rate,
                )
            }),
            output_version: output.map(|rate| {
                model_override_version(
                    model_id_text.as_deref(),
                    effective_from,
                    &provider.provider_type,
                    actual_model,
                    PriceDirection::Output,
                    rate,
                )
            }),
            version: None,
            effective_from,
        },
        &PricingFeatures::default(),
        true,
    );
    let conditions = (!(input.is_some() && output.is_some()))
        .then(|| catalog.max_prompt_tokens(&provider.provider_type, actual_model, now))
        .flatten()
        .map(|value| {
            vec![json!({
                "type": "max_prompt_tokens",
                "value": value,
            })]
        })
        .unwrap_or_default();
    model.insert(
        "effective_pricing".to_string(),
        json!({
            "currency": "USD",
            "unit": "1M tokens",
            "status": pricing.status,
            "catalog_version": catalog.catalog_version(),
            "catalog_snapshot_date": catalog.snapshot_date(),
            "input": pricing.input.as_ref().map(effective_direction),
            "output": pricing.output.as_ref().map(effective_direction),
            "conditions": conditions,
        }),
    );
}

fn effective_direction(price: &kong_ai::usage::model::PriceSnapshot) -> Value {
    json!({
        "amount": decimal_12(price.usd_per_million),
        "source": price.source,
        "version": price.version,
        "snapshot_date": price.snapshot_date,
    })
}

fn decimal_compat_number(value: Option<Decimal>) -> Value {
    value
        .and_then(|value| value.to_string().parse::<f64>().ok())
        .and_then(serde_json::Number::from_f64)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

fn builtin_catalog() -> &'static PriceCatalog {
    static CATALOG: OnceLock<PriceCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| PriceCatalog::builtin().expect("内置 AI 价表必须在构建期测试中通过校验"))
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
    let all_params = PageParams {
        size: 10000,
        ..PageParams::default()
    };
    match state.ai_models.page(&all_params).await {
        Ok(page) => {
            let mut seen = std::collections::HashSet::new();
            let mut groups = Vec::new();
            for model in &page.data {
                if seen.insert(model.name.clone()) {
                    groups.push(json!({ "name": model.name }));
                }
            }
            (
                StatusCode::OK,
                Json(json!({ "data": groups, "next": Value::Null })),
            )
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

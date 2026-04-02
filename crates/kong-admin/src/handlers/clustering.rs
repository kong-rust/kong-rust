//! Clustering Admin API handlers — 集群 Admin API 处理器
//!
//! Endpoints only available on control_plane nodes: — 仅 CP 节点可用的端点:
//! - GET /clustering/data-planes — list connected data planes — 列出已连接的 DP
//! - GET /clustering/status — cluster status summary — 集群状态摘要

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::AdminState;

/// GET /clustering/data-planes — list connected data planes
/// 获取已连接的数据平面列表（仅 CP 可用）
pub async fn list_data_planes(
    State(state): State<AdminState>,
) -> impl IntoResponse {
    // Only available on CP — 仅 CP 可用
    let Some(ref cp) = state.cp else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "message": "this endpoint is only available on control_plane nodes"
            })),
        ).into_response();
    };

    let dps = cp.list_data_planes().await;
    let data: Vec<serde_json::Value> = dps.iter().map(|dp| {
        json!({
            "id": dp.id.to_string(),
            "ip": dp.ip,
            "hostname": dp.hostname,
            "version": dp.version,
            "sync_status": serde_json::to_value(&dp.sync_status).unwrap_or(serde_json::Value::String("unknown".to_string())),
            "config_hash": dp.config_hash,
            "last_seen": dp.last_seen.timestamp(),
            "labels": dp.labels,
        })
    }).collect();

    let total = data.len();
    Json(json!({
        "data": data,
        "total": total,
    })).into_response()
}

/// GET /clustering/status — cluster status summary
/// 集群状态摘要
pub async fn clustering_status(
    State(state): State<AdminState>,
) -> impl IntoResponse {
    let role = state.config.role.to_string();

    if let Some(ref cp) = state.cp {
        let dps = cp.list_data_planes().await;
        let hash = cp.current_hash().await;
        Json(json!({
            "role": role,
            "connected_data_planes": dps.len(),
            "config_hash": hash,
        }))
    } else {
        Json(json!({
            "role": role,
        }))
    }
}

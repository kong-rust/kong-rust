//! Admin API 请求处理器
//!
//! 实现 Kong 兼容的 REST API 端点:
//! - 通用 CRUD 端点（泛型）
//! - 嵌套端点（如 /services/{id}/routes）
//! - 特殊端点（/, /status）

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use kong_core::error::KongError;
use kong_core::models::*;
use kong_core::traits::{Dao, Entity, PageParams, PrimaryKey};

use crate::AdminState;

// ============ 缓存刷新 ============

impl AdminState {
    /// Admin API 写操作后异步刷新 KongProxy 内存缓存
    pub async fn refresh_proxy_cache(&self, entity_type: &str) {
        let all_params = kong_core::traits::PageParams {
            size: 10000,
            offset: None,
            tags: None,
        };

        match entity_type {
            "services" => {
                match self.services.page(&all_params).await {
                    Ok(page) => self.proxy.update_services(page.data),
                    Err(e) => tracing::error!("刷新 services 缓存失败: {}", e),
                }
            }
            "routes" => {
                match self.routes.page(&all_params).await {
                    Ok(page) => self.proxy.update_routes(&page.data),
                    Err(e) => tracing::error!("刷新 routes 缓存失败: {}", e),
                }
            }
            "plugins" => {
                match self.plugins.page(&all_params).await {
                    Ok(page) => self.proxy.update_plugins(page.data),
                    Err(e) => tracing::error!("刷新 plugins 缓存失败: {}", e),
                }
            }
            "upstreams" | "targets" => {
                let upstreams = self.upstreams.page(&all_params).await;
                let targets = self.targets.page(&all_params).await;
                match (upstreams, targets) {
                    (Ok(u), Ok(t)) => self.proxy.update_upstreams(u.data, t.data),
                    (Err(e), _) | (_, Err(e)) => {
                        tracing::error!("刷新 upstreams 缓存失败: {}", e);
                    }
                }
            }
            "certificates" | "snis" => {
                let certs = self.certificates.page(&all_params).await;
                let snis = self.snis.page(&all_params).await;
                match (certs, snis) {
                    (Ok(c), Ok(s)) => self.proxy.cert_manager.load_certificates(&c.data, &s.data),
                    (Err(e), _) | (_, Err(e)) => {
                        tracing::error!("刷新 certificates 缓存失败: {}", e);
                    }
                }
            }
            "ca_certificates" => {
                match self.ca_certificates.page(&all_params).await {
                    Ok(page) => self.proxy.update_ca_certificates(page.data),
                    Err(e) => tracing::error!("刷新 ca_certificates 缓存失败: {}", e),
                }
            }
            _ => {} // consumers / vaults 等代理流程不直接使用
        }
    }
}

// ============ 查询参数 ============

#[derive(Debug, Deserialize)]
pub struct ListParams {
    pub size: Option<usize>,
    pub offset: Option<String>,
    pub tags: Option<String>,
}

impl ListParams {
    fn to_page_params(&self) -> PageParams {
        PageParams {
            size: self.size.unwrap_or(100).min(1000),
            offset: self.offset.clone(),
            tags: self.tags.as_ref().map(|t| {
                t.split(',').map(|s| s.trim().to_string()).collect()
            }),
        }
    }
}

// ============ 错误响应 ============

/// Kong 兼容的错误响应格式
#[allow(dead_code)]
fn error_response(err: KongError) -> impl IntoResponse {
    let status = StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = json!({
        "message": err.to_string(),
        "name": err.error_name(),
        "code": err.error_code(),
    });
    (status, Json(body))
}

// ============ 特殊端点 ============

/// GET / — 节点信息
pub async fn root_info(State(state): State<AdminState>) -> impl IntoResponse {
    Json(json!({
        "version": "0.1.0",
        "lua_version": "LuaJIT 2.1.0-beta3",
        "tagline": "Welcome to kong-rust",
        "hostname": std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".to_string()),
        "node_id": state.node_id.to_string(),
        "configuration": {
            "database": if state.config.database == "off" { "off" } else { "postgres" },
            "router_flavor": &state.config.router_flavor,
            "role": &state.config.role,
        },
        "plugins": {
            "available_on_server": {},
        },
        "timers": {
            "running": 0,
            "pending": 0,
        }
    }))
}

/// GET /status — 服务状态
pub async fn status_info(State(_state): State<AdminState>) -> impl IntoResponse {
    Json(json!({
        "server": {
            "connections_accepted": 0,
            "connections_active": 0,
            "connections_handled": 0,
            "connections_reading": 0,
            "connections_writing": 0,
            "connections_waiting": 0,
            "total_requests": 0,
        },
        "database": {
            "reachable": true,
        },
        "memory": {
            "lua_shared_dicts": {},
            "workers_lua_vms": [],
        },
        "configuration_hash": "00000000000000000000000000000000",
    }))
}

// ============ 通用 CRUD 端点 ============

// ============ 通用 CRUD 辅助 ============

/// 通用的列表/查询/创建/更新/删除逻辑
/// 因 Rust 泛型限制（无法在运行时根据类型选择 DAO）,
/// 使用具体类型的 handler 通过宏简化注册

/// 通用列表处理
async fn do_list<T: Entity + Serialize + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    params: &ListParams,
) -> (StatusCode, Json<Value>) {
    match dao.page(&params.to_page_params()).await {
        Ok(page) => {
            let body = json!({
                "data": page.data,
                "offset": page.offset,
                "next": page.next,
            });
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

/// 通用查询处理
async fn do_get<T: Entity + Serialize + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    id_or_name: &str,
) -> (StatusCode, Json<Value>) {
    let pk = PrimaryKey::from_str_or_uuid(id_or_name);
    match dao.select(&pk).await {
        Ok(Some(entity)) => {
            let body = serde_json::to_value(&entity).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "message": format!("{} not found", T::table_name()),
                "name": "not found",
                "code": 3,
            })),
        ),
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

/// 通用创建处理
async fn do_create<T: Entity + Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    body: Value,
) -> (StatusCode, Json<Value>) {
    // 自动注入 id 和时间戳（Kong 兼容：创建时这些字段可选）
    let mut body = body;
    if let Some(obj) = body.as_object_mut() {
        if !obj.contains_key("id") {
            obj.insert("id".to_string(), json!(uuid::Uuid::new_v4()));
        }
        let now = chrono::Utc::now().timestamp();
        if !obj.contains_key("created_at") {
            obj.insert("created_at".to_string(), json!(now));
        }
        if !obj.contains_key("updated_at") {
            obj.insert("updated_at".to_string(), json!(now));
        }
        // Kong 兼容：url 字段是 protocol + host + port + path 的快捷方式
        if let Some(url_val) = obj.remove("url") {
            if let Some(url_str) = url_val.as_str() {
                if let Ok(parsed) = url::Url::parse(url_str) {
                    if !obj.contains_key("protocol") {
                        obj.insert("protocol".to_string(), json!(parsed.scheme()));
                    }
                    if !obj.contains_key("host") {
                        if let Some(host) = parsed.host_str() {
                            obj.insert("host".to_string(), json!(host));
                        }
                    }
                    if !obj.contains_key("port") {
                        if let Some(port) = parsed.port_or_known_default() {
                            obj.insert("port".to_string(), json!(port));
                        }
                    }
                    let path = parsed.path();
                    if !obj.contains_key("path") && path != "/" && !path.is_empty() {
                        obj.insert("path".to_string(), json!(path));
                    }
                }
            }
        }
    }

    let entity: T = match serde_json::from_value(body) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": format!("schema violation: {}", e),
                    "name": "schema violation",
                    "code": 2,
                })),
            );
        }
    };

    match dao.insert(&entity).await {
        Ok(created) => {
            let body = serde_json::to_value(&created).unwrap_or(json!(null));
            (StatusCode::CREATED, Json(body))
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({
                "message": e.to_string(),
                "name": e.error_name(),
                "code": e.error_code(),
            })))
        }
    }
}

/// 通用更新处理
async fn do_update<T: Entity + Serialize + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    id_or_name: &str,
    body: &Value,
) -> (StatusCode, Json<Value>) {
    let pk = PrimaryKey::from_str_or_uuid(id_or_name);
    match dao.update(&pk, body).await {
        Ok(updated) => {
            let body = serde_json::to_value(&updated).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({
                "message": e.to_string(),
                "name": e.error_name(),
                "code": e.error_code(),
            })))
        }
    }
}

/// 通用 upsert 处理
async fn do_upsert<T: Entity + Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    id_or_name: &str,
    body: Value,
) -> (StatusCode, Json<Value>) {
    let entity: T = match serde_json::from_value(body) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": format!("schema violation: {}", e),
                    "name": "schema violation",
                    "code": 2,
                })),
            );
        }
    };

    let pk = PrimaryKey::from_str_or_uuid(id_or_name);
    match dao.upsert(&pk, &entity).await {
        Ok(result) => {
            let body = serde_json::to_value(&result).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({
                "message": e.to_string(),
                "name": e.error_name(),
                "code": e.error_code(),
            })))
        }
    }
}

/// 通用删除处理
async fn do_delete<T: Entity + Send + Sync + 'static>(
    dao: &Arc<dyn Dao<T>>,
    id_or_name: &str,
) -> axum::response::Response {
    let pk = PrimaryKey::from_str_or_uuid(id_or_name);
    match dao.delete(&pk).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let body = json!({
                "message": e.to_string(),
                "name": e.error_name(),
                "code": e.error_code(),
            });
            (status, Json(body)).into_response()
        }
    }
}

// ============ 宏生成具体类型的 handler ============

/// 为每个实体类型生成具体的 CRUD handler
macro_rules! entity_handlers {
    ($entity:ty, $dao_field:ident, $entity_name:expr, $list:ident, $get:ident, $create:ident, $update:ident, $upsert:ident, $del:ident) => {
        pub async fn $list(
            State(state): State<AdminState>,
            Query(params): Query<ListParams>,
        ) -> impl IntoResponse {
            do_list::<$entity>(&state.$dao_field, &params).await
        }

        pub async fn $get(
            State(state): State<AdminState>,
            Path(id_or_name): Path<String>,
        ) -> impl IntoResponse {
            do_get::<$entity>(&state.$dao_field, &id_or_name).await
        }

        pub async fn $create(
            State(state): State<AdminState>,
            Json(body): Json<Value>,
        ) -> impl IntoResponse {
            let result = do_create::<$entity>(&state.$dao_field, body).await;
            if result.0.is_success() && !$entity_name.is_empty() {
                let _ = state.refresh_tx.send($entity_name);
            }
            result
        }

        pub async fn $update(
            State(state): State<AdminState>,
            Path(id_or_name): Path<String>,
            Json(body): Json<Value>,
        ) -> impl IntoResponse {
            let result = do_update::<$entity>(&state.$dao_field, &id_or_name, &body).await;
            if result.0.is_success() && !$entity_name.is_empty() {
                let _ = state.refresh_tx.send($entity_name);
            }
            result
        }

        pub async fn $upsert(
            State(state): State<AdminState>,
            Path(id_or_name): Path<String>,
            Json(body): Json<Value>,
        ) -> impl IntoResponse {
            let result = do_upsert::<$entity>(&state.$dao_field, &id_or_name, body).await;
            if result.0.is_success() && !$entity_name.is_empty() {
                let _ = state.refresh_tx.send($entity_name);
            }
            result
        }

        pub async fn $del(
            State(state): State<AdminState>,
            Path(id_or_name): Path<String>,
        ) -> impl IntoResponse {
            let result = do_delete::<$entity>(&state.$dao_field, &id_or_name).await;
            if !$entity_name.is_empty() {
                let _ = state.refresh_tx.send($entity_name);
            }
            result
        }
    };
}

// 为每个实体类型生成 handler
entity_handlers!(Service, services, "services", list_services, get_service, create_service, update_service, upsert_service, delete_service);
entity_handlers!(Route, routes, "routes", list_routes, get_route, create_route, update_route, upsert_route, delete_route);
entity_handlers!(Consumer, consumers, "", list_consumers, get_consumer, create_consumer, update_consumer, upsert_consumer, delete_consumer);
entity_handlers!(Plugin, plugins, "plugins", list_plugins, get_plugin, create_plugin, update_plugin, upsert_plugin, delete_plugin);
entity_handlers!(Upstream, upstreams, "upstreams", list_upstreams, get_upstream, create_upstream, update_upstream, upsert_upstream, delete_upstream);
entity_handlers!(Certificate, certificates, "certificates", list_certificates, get_certificate, create_certificate, update_certificate, upsert_certificate, delete_certificate);
entity_handlers!(Sni, snis, "snis", list_snis, get_sni, create_sni, update_sni, upsert_sni, delete_sni);
entity_handlers!(CaCertificate, ca_certificates, "ca_certificates", list_ca_certificates, get_ca_certificate, create_ca_certificate, update_ca_certificate, upsert_ca_certificate, delete_ca_certificate);
entity_handlers!(Vault, vaults, "", list_vaults, get_vault, create_vault, update_vault, upsert_vault, delete_vault);

// ============ 嵌套端点 ============

/// GET /services/:service_id/routes
pub async fn list_nested_routes(
    State(state): State<AdminState>,
    Path(service_id_or_name): Path<String>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    // 先解析 service ID
    let service_pk = PrimaryKey::from_str_or_uuid(&service_id_or_name);
    let service = match state.services.select(&service_pk).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "service not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    match state
        .routes
        .select_by_foreign_key("service", &service.id, &params.to_page_params())
        .await
    {
        Ok(page) => (
            StatusCode::OK,
            Json(json!({
                "data": page.data,
                "offset": page.offset,
                "next": page.next,
            })),
        ),
        Err(e) => (
            StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"message": e.to_string()})),
        ),
    }
}

/// GET /upstreams/:upstream_id/targets
pub async fn list_nested_targets(
    State(state): State<AdminState>,
    Path(upstream_id_or_name): Path<String>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let upstream_pk = PrimaryKey::from_str_or_uuid(&upstream_id_or_name);
    let upstream = match state.upstreams.select(&upstream_pk).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "upstream not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    match state
        .targets
        .select_by_foreign_key("upstream", &upstream.id, &params.to_page_params())
        .await
    {
        Ok(page) => (
            StatusCode::OK,
            Json(json!({
                "data": page.data,
                "offset": page.offset,
                "next": page.next,
            })),
        ),
        Err(e) => (
            StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"message": e.to_string()})),
        ),
    }
}

/// POST /upstreams/:upstream_id/targets
pub async fn create_nested_target(
    State(state): State<AdminState>,
    Path(upstream_id_or_name): Path<String>,
    Json(mut body): Json<Value>,
) -> impl IntoResponse {
    // 解析 upstream
    let upstream_pk = PrimaryKey::from_str_or_uuid(&upstream_id_or_name);
    let upstream = match state.upstreams.select(&upstream_pk).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "upstream not found"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"message": e.to_string()})),
            );
        }
    };

    // 注入 upstream FK
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "upstream".to_string(),
            json!({"id": upstream.id.to_string()}),
        );
    }

    let target: Target = match serde_json::from_value(body) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"message": format!("schema violation: {}", e)})),
            );
        }
    };

    match state.targets.insert(&target).await {
        Ok(created) => {
            let _ = state.refresh_tx.send("targets");
            let body = serde_json::to_value(&created).unwrap_or(json!(null));
            (StatusCode::CREATED, Json(body))
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

/// GET /upstreams/:upstream_id/targets/:id
pub async fn get_nested_target(
    State(state): State<AdminState>,
    Path((upstream_id_or_name, target_id_or_name)): Path<(String, String)>,
) -> impl IntoResponse {
    // 验证 upstream 存在
    let upstream_pk = PrimaryKey::from_str_or_uuid(&upstream_id_or_name);
    if let Ok(None) = state.upstreams.select(&upstream_pk).await {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"message": "upstream not found"})),
        );
    }

    let pk = PrimaryKey::from_str_or_uuid(&target_id_or_name);
    match state.targets.select(&pk).await {
        Ok(Some(target)) => {
            let body = serde_json::to_value(&target).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"message": "target not found"})),
        ),
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

/// PATCH /upstreams/:upstream_id/targets/:id
pub async fn update_nested_target(
    State(state): State<AdminState>,
    Path((_upstream_id_or_name, target_id_or_name)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let pk = PrimaryKey::from_str_or_uuid(&target_id_or_name);
    match state.targets.update(&pk, &body).await {
        Ok(updated) => {
            let _ = state.refresh_tx.send("targets");
            let body = serde_json::to_value(&updated).unwrap_or(json!(null));
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(json!({"message": e.to_string()})))
        }
    }
}

/// DELETE /upstreams/:upstream_id/targets/:id
pub async fn delete_nested_target(
    State(state): State<AdminState>,
    Path((_upstream_id_or_name, target_id_or_name)): Path<(String, String)>,
) -> impl IntoResponse {
    let pk = PrimaryKey::from_str_or_uuid(&target_id_or_name);
    match state.targets.delete(&pk).await {
        Ok(_) => {
            let _ = state.refresh_tx.send("targets");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            let status = StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let body = json!({"message": e.to_string()});
            (status, Json(body)).into_response()
        }
    }
}

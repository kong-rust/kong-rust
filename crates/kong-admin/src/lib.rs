#![recursion_limit = "512"]
//! Kong Admin API — 100% 兼容 Kong 的 REST Admin API
//!
//! 基于 axum 实现，支持:
//! - 所有核心实体的 CRUD
//! - 分页、标签过滤
//! - 嵌套端点（如 /services/{service}/routes）
//! - 特殊端点（/, /status, /config）

pub mod handlers;

use std::sync::Arc;

use std::sync::RwLock;

use axum::routing::get;
use axum::Router;
use tower_http::cors::{AllowOrigin, AllowMethods, AllowHeaders, CorsLayer};
use kong_core::models::*;
use kong_core::traits::Dao;
use kong_router::stream::StreamRouter;

/// Admin API 应用状态
#[derive(Clone)]
pub struct AdminState {
    pub services: Arc<dyn Dao<Service>>,
    pub routes: Arc<dyn Dao<Route>>,
    pub consumers: Arc<dyn Dao<Consumer>>,
    pub plugins: Arc<dyn Dao<Plugin>>,
    pub upstreams: Arc<dyn Dao<Upstream>>,
    pub targets: Arc<dyn Dao<Target>>,
    pub certificates: Arc<dyn Dao<Certificate>>,
    pub snis: Arc<dyn Dao<Sni>>,
    pub ca_certificates: Arc<dyn Dao<CaCertificate>>,
    pub vaults: Arc<dyn Dao<Vault>>,
    pub node_id: uuid::Uuid,
    pub config: Arc<kong_config::KongConfig>,
    /// 代理引擎引用（Clone 语义，共享底层 Arc 数据），用于写操作后刷新内存缓存
    pub proxy: kong_proxy::KongProxy,
    /// 缓存刷新防抖信号发送端：CUD 操作后发送实体类型名，后台任务合并执行
    pub refresh_tx: tokio::sync::mpsc::UnboundedSender<&'static str>,
    /// Stream 路由器引用（与 Stream Proxy 共享），路由变更时同步更新
    pub stream_router: Option<Arc<RwLock<StreamRouter>>>,
}

/// 缓存刷新防抖循环：收到第一个信号后等待 100ms，合并期间所有刷新请求后一次性执行
pub async fn run_cache_refresher(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<&'static str>,
    state: AdminState,
) {
    use std::collections::HashSet;
    use tokio::time::{Duration, Instant};

    loop {
        // 阻塞等待第一个刷新信号
        let Some(first) = rx.recv().await else {
            break;
        };
        let mut pending = HashSet::new();
        pending.insert(first);

        // 100ms 窗口内收集所有待刷新实体类型
        let deadline = Instant::now() + Duration::from_millis(100);
        loop {
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Some(t) => { pending.insert(t); }
                        None => return,
                    }
                }
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }

        // 合并后一次性刷新
        for entity_type in &pending {
            state.refresh_proxy_cache(entity_type).await;
        }
        tracing::debug!("防抖刷新完成: {:?}", pending);
    }
}

/// 构建 Admin API 路由
pub fn build_admin_router(state: AdminState) -> Router {
    use handlers::*;

    Router::new()
        // 根信息端点
        .route("/", get(root_info))
        .route("/status", get(status_info))
        // Services
        .route("/services", get(list_services).post(create_service))
        .route("/services/{id_or_name}",
            get(get_service).patch(update_service).put(upsert_service).delete(delete_service))
        // Routes
        .route("/routes", get(list_routes).post(create_route))
        .route("/routes/{id_or_name}",
            get(get_route).patch(update_route).put(upsert_route).delete(delete_route))
        // 嵌套: Service 下的 Routes 和 Plugins
        .route("/services/{service_id_or_name}/routes", get(list_nested_routes).post(create_nested_route))
        .route("/services/{service_id_or_name}/plugins", get(list_service_plugins))
        // 嵌套: Route 下的 Plugins
        .route("/routes/{route_id_or_name}/plugins", get(list_route_plugins))
        // Consumers
        .route("/consumers", get(list_consumers).post(create_consumer))
        .route("/consumers/{id_or_name}",
            get(get_consumer).patch(update_consumer).put(upsert_consumer).delete(delete_consumer))
        // 嵌套: Consumer 下的 Plugins
        .route("/consumers/{consumer_id_or_name}/plugins", get(list_consumer_plugins))
        // Plugins
        .route("/plugins", get(list_plugins).post(create_plugin))
        .route("/plugins/{id_or_name}",
            get(get_plugin).patch(update_plugin).put(upsert_plugin).delete(delete_plugin))
        // Upstreams
        .route("/upstreams", get(list_upstreams).post(create_upstream))
        .route("/upstreams/{id_or_name}",
            get(get_upstream).patch(update_upstream).put(upsert_upstream).delete(delete_upstream))
        // Targets (nested under upstreams)
        .route("/upstreams/{upstream_id_or_name}/targets",
            get(list_nested_targets).post(create_nested_target))
        .route("/upstreams/{upstream_id_or_name}/targets/{id_or_name}",
            get(get_nested_target).patch(update_nested_target).delete(delete_nested_target))
        // Certificates
        .route("/certificates", get(list_certificates).post(create_certificate))
        .route("/certificates/{id}",
            get(get_certificate).patch(update_certificate).put(upsert_certificate).delete(delete_certificate))
        // SNIs
        .route("/snis", get(list_snis).post(create_sni))
        .route("/snis/{id_or_name}",
            get(get_sni).patch(update_sni).put(upsert_sni).delete(delete_sni))
        // CA Certificates
        .route("/ca_certificates", get(list_ca_certificates).post(create_ca_certificate))
        .route("/ca_certificates/{id}",
            get(get_ca_certificate).patch(update_ca_certificate).put(upsert_ca_certificate).delete(delete_ca_certificate))
        // Vaults
        .route("/vaults", get(list_vaults).post(create_vault))
        .route("/vaults/{id_or_name}",
            get(get_vault).patch(update_vault).put(upsert_vault).delete(delete_vault))
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::mirror_request())
                .allow_methods(AllowMethods::mirror_request())
                .allow_headers(AllowHeaders::mirror_request())
                .allow_credentials(true)
                .expose_headers(tower_http::cors::ExposeHeaders::list([
                    axum::http::header::CONTENT_TYPE,
                    axum::http::header::CONTENT_LENGTH,
                ]))
        )
        .with_state(state)
}

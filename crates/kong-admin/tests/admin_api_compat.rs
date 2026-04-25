//! Admin API compatibility integration tests — Admin API 兼容性集成测试
//!
//! Uses db-less mode (in-memory store) + axum test client, — 使用 db-less 模式（内存存储）+ axum 测试客户端，
//! verifying all Admin API endpoints' request/response formats are Kong-compatible — 验证所有 Admin API 端点的请求/响应格式与 Kong 兼容

use std::sync::Arc;

use axum::body::Body;
use axum::http::{self, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use kong_admin::{build_admin_router, build_status_router, AdminState};
use kong_core::models::*;
use kong_db::{DblessDao, DblessStore};

/// Create a test Admin API application — 创建测试用的 Admin API 应用
fn create_test_app() -> axum::Router {
    let store = Arc::new(DblessStore::new());

    let config = Arc::new(kong_config::KongConfig::default());

    let (refresh_tx, _refresh_rx) = tokio::sync::mpsc::unbounded_channel();
    let dns_resolver = std::sync::Arc::new(kong_proxy::dns::DnsResolver::new(&config));
    let proxy = kong_proxy::KongProxy::new(
        &[],
        "traditional",
        kong_plugin_system::PluginRegistry::new(),
        kong_proxy::tls::CertificateManager::new(),
        vec![],
        dns_resolver,
        Arc::clone(&config),
    );

    let state = AdminState {
        services: Arc::new(DblessDao::<Service>::new(store.clone())),
        routes: Arc::new(DblessDao::<Route>::new(store.clone())),
        consumers: Arc::new(DblessDao::<Consumer>::new(store.clone())),
        plugins: Arc::new(DblessDao::<Plugin>::new(store.clone())),
        upstreams: Arc::new(DblessDao::<Upstream>::new(store.clone())),
        targets: Arc::new(DblessDao::<Target>::new(store.clone())),
        certificates: Arc::new(DblessDao::<Certificate>::new(store.clone())),
        snis: Arc::new(DblessDao::<Sni>::new(store.clone())),
        ca_certificates: Arc::new(DblessDao::<CaCertificate>::new(store.clone())),
        vaults: Arc::new(DblessDao::<Vault>::new(store.clone())),
        key_sets: Arc::new(DblessDao::<KeySet>::new(store.clone())),
        keys: Arc::new(DblessDao::<Key>::new(store.clone())),
        ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(store.clone())),
        ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(store.clone())),
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(store.clone())),
        node_id: Uuid::new_v4(),
        config: Arc::clone(&config),
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(std::sync::RwLock::new("00000000000000000000000000000000".to_string())),
        dbless_store: None,
        target_health: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        cp: None,
        cache: Arc::new(kong_db::KongCache::from_kong_config(&config)),
        log_updater: None,
        current_log_level: Arc::new(std::sync::RwLock::new("info".to_string())),
    };

    build_admin_router(state)
}

/// Create a test Status API application — 创建测试用的 Status API 应用
fn create_test_status_app() -> axum::Router {
    let store = Arc::new(DblessStore::new());

    let mut config = kong_config::KongConfig::default();
    config.prefix = std::env::current_dir().unwrap().display().to_string();
    let config = Arc::new(config);

    let (refresh_tx, _refresh_rx) = tokio::sync::mpsc::unbounded_channel();
    let dns_resolver = std::sync::Arc::new(kong_proxy::dns::DnsResolver::new(&config));
    let proxy = kong_proxy::KongProxy::new(
        &[],
        "traditional",
        kong_plugin_system::PluginRegistry::new(),
        kong_proxy::tls::CertificateManager::new(),
        vec![],
        dns_resolver,
        Arc::clone(&config),
    );

    let state = AdminState {
        services: Arc::new(DblessDao::<Service>::new(store.clone())),
        routes: Arc::new(DblessDao::<Route>::new(store.clone())),
        consumers: Arc::new(DblessDao::<Consumer>::new(store.clone())),
        plugins: Arc::new(DblessDao::<Plugin>::new(store.clone())),
        upstreams: Arc::new(DblessDao::<Upstream>::new(store.clone())),
        targets: Arc::new(DblessDao::<Target>::new(store.clone())),
        certificates: Arc::new(DblessDao::<Certificate>::new(store.clone())),
        snis: Arc::new(DblessDao::<Sni>::new(store.clone())),
        ca_certificates: Arc::new(DblessDao::<CaCertificate>::new(store.clone())),
        vaults: Arc::new(DblessDao::<Vault>::new(store.clone())),
        key_sets: Arc::new(DblessDao::<KeySet>::new(store.clone())),
        keys: Arc::new(DblessDao::<Key>::new(store.clone())),
        ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(store.clone())),
        ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(store.clone())),
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(store.clone())),
        node_id: Uuid::new_v4(),
        config: Arc::clone(&config),
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(std::sync::RwLock::new("00000000000000000000000000000000".to_string())),
        dbless_store: None,
        target_health: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        cp: None,
        cache: Arc::new(kong_db::KongCache::from_kong_config(&config)),
        log_updater: None,
        current_log_level: Arc::new(std::sync::RwLock::new("info".to_string())),
    };

    build_status_router(state)
}

/// Create a Status API application with a prometheus plugin instance — 创建带 prometheus 插件实例的 Status API 应用
fn create_test_status_app_with_prometheus() -> axum::Router {
    let store = Arc::new(DblessStore::new());

    store
        .load_from_json(&json!({
            "_format_version": "3.0",
            "plugins": [
                {
                    "id": "ba23b46a-6a57-4f78-a8d3-0c12f758f6d7",
                    "name": "prometheus",
                    "enabled": true,
                    "config": {
                        "status_code_metrics": true,
                        "latency_metrics": true,
                        "bandwidth_metrics": true,
                        "upstream_health_metrics": true
                    },
                    "created_at": 1609459200,
                    "updated_at": 1609459200
                }
            ]
        }))
        .unwrap();

    let mut config = kong_config::KongConfig::default();
    config.prefix = std::env::current_dir().unwrap().display().to_string();
    let config = Arc::new(config);

    let (refresh_tx, _refresh_rx) = tokio::sync::mpsc::unbounded_channel();
    let dns_resolver = std::sync::Arc::new(kong_proxy::dns::DnsResolver::new(&config));
    let proxy = kong_proxy::KongProxy::new(
        &[],
        "traditional",
        kong_plugin_system::PluginRegistry::new(),
        kong_proxy::tls::CertificateManager::new(),
        vec![],
        dns_resolver,
        Arc::clone(&config),
    );

    let state = AdminState {
        services: Arc::new(DblessDao::<Service>::new(store.clone())),
        routes: Arc::new(DblessDao::<Route>::new(store.clone())),
        consumers: Arc::new(DblessDao::<Consumer>::new(store.clone())),
        plugins: Arc::new(DblessDao::<Plugin>::new(store.clone())),
        upstreams: Arc::new(DblessDao::<Upstream>::new(store.clone())),
        targets: Arc::new(DblessDao::<Target>::new(store.clone())),
        certificates: Arc::new(DblessDao::<Certificate>::new(store.clone())),
        snis: Arc::new(DblessDao::<Sni>::new(store.clone())),
        ca_certificates: Arc::new(DblessDao::<CaCertificate>::new(store.clone())),
        vaults: Arc::new(DblessDao::<Vault>::new(store.clone())),
        key_sets: Arc::new(DblessDao::<KeySet>::new(store.clone())),
        keys: Arc::new(DblessDao::<Key>::new(store.clone())),
        ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(store.clone())),
        ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(store.clone())),
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(store.clone())),
        node_id: Uuid::new_v4(),
        config: Arc::clone(&config),
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(std::sync::RwLock::new("00000000000000000000000000000000".to_string())),
        dbless_store: None,
        target_health: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        cp: None,
        cache: Arc::new(kong_db::KongCache::from_kong_config(&config)),
        log_updater: None,
        current_log_level: Arc::new(std::sync::RwLock::new("info".to_string())),
    };

    build_status_router(state)
}

/// Create a test application with preloaded data — 创建预加载数据的测试应用
fn create_test_app_with_data() -> axum::Router {
    let store = Arc::new(DblessStore::new());

    // Preload test data — 预加载测试数据
    let test_data = json!({
        "_format_version": "3.0",
        "services": [
            {
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "name": "test-service",
                "host": "httpbin.org",
                "port": 80,
                "protocol": "http",
                "retries": 5,
                "connect_timeout": 60000,
                "write_timeout": 60000,
                "read_timeout": 60000,
                "enabled": true,
                "created_at": 1609459200,
                "updated_at": 1609459200
            },
            {
                "id": "550e8400-e29b-41d4-a716-446655440099",
                "name": "another-service",
                "host": "example.com",
                "port": 443,
                "protocol": "https",
                "retries": 5,
                "connect_timeout": 60000,
                "write_timeout": 60000,
                "read_timeout": 60000,
                "enabled": true,
                "created_at": 1609459200,
                "updated_at": 1609459200
            }
        ],
        "routes": [
            {
                "id": "660e8400-e29b-41d4-a716-446655440001",
                "name": "test-route",
                "paths": ["/test"],
                "protocols": ["http", "https"],
                "strip_path": true,
                "preserve_host": false,
                "regex_priority": 0,
                "path_handling": "v0",
                "https_redirect_status_code": 426,
                "request_buffering": true,
                "response_buffering": true,
                "service": { "id": "550e8400-e29b-41d4-a716-446655440000" },
                "created_at": 1609459200,
                "updated_at": 1609459200
            }
        ],
        "plugins": [
            {
                "id": "877dc65d-b37a-408c-bcf6-5d081ea55f7b",
                "name": "key-auth",
                "enabled": true,
                "service": { "id": "550e8400-e29b-41d4-a716-446655440000" },
                "config": {
                    "key_names": ["apikey"]
                },
                "created_at": 1609459200,
                "updated_at": 1609459200
            }
        ],
        "consumers": [
            {
                "id": "770e8400-e29b-41d4-a716-446655440002",
                "username": "test-consumer",
                "created_at": 1609459200,
                "updated_at": 1609459200
            }
        ],
        "upstreams": [
            {
                "id": "880e8400-e29b-41d4-a716-446655440003",
                "name": "test-upstream",
                "algorithm": "round-robin",
                "hash_on": "none",
                "hash_fallback": "none",
                "slots": 10000,
                "created_at": 1609459200,
                "updated_at": 1609459200
            }
        ],
        "targets": [
            {
                "id": "990e8400-e29b-41d4-a716-446655440004",
                "target": "10.0.0.1:80",
                "weight": 100,
                "upstream": { "id": "880e8400-e29b-41d4-a716-446655440003" },
                "created_at": 1609459200,
                "updated_at": 1609459200
            }
        ]
    });

    store.load_from_json(&test_data).unwrap();

    let config = Arc::new(kong_config::KongConfig::default());

    let (refresh_tx, _refresh_rx) = tokio::sync::mpsc::unbounded_channel();
    let dns_resolver = std::sync::Arc::new(kong_proxy::dns::DnsResolver::new(&config));
    let proxy = kong_proxy::KongProxy::new(
        &[],
        "traditional",
        kong_plugin_system::PluginRegistry::new(),
        kong_proxy::tls::CertificateManager::new(),
        vec![],
        dns_resolver,
        Arc::clone(&config),
    );

    let state = AdminState {
        services: Arc::new(DblessDao::<Service>::new(store.clone())),
        routes: Arc::new(DblessDao::<Route>::new(store.clone())),
        consumers: Arc::new(DblessDao::<Consumer>::new(store.clone())),
        plugins: Arc::new(DblessDao::<Plugin>::new(store.clone())),
        upstreams: Arc::new(DblessDao::<Upstream>::new(store.clone())),
        targets: Arc::new(DblessDao::<Target>::new(store.clone())),
        certificates: Arc::new(DblessDao::<Certificate>::new(store.clone())),
        snis: Arc::new(DblessDao::<Sni>::new(store.clone())),
        ca_certificates: Arc::new(DblessDao::<CaCertificate>::new(store.clone())),
        vaults: Arc::new(DblessDao::<Vault>::new(store.clone())),
        key_sets: Arc::new(DblessDao::<KeySet>::new(store.clone())),
        keys: Arc::new(DblessDao::<Key>::new(store.clone())),
        ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(store.clone())),
        ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(store.clone())),
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(store.clone())),
        node_id: Uuid::new_v4(),
        config: Arc::clone(&config),
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(std::sync::RwLock::new("00000000000000000000000000000000".to_string())),
        dbless_store: None,
        target_health: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        cp: None,
        cache: Arc::new(kong_db::KongCache::from_kong_config(&config)),
        log_updater: None,
        current_log_level: Arc::new(std::sync::RwLock::new("info".to_string())),
    };

    build_admin_router(state)
}

// ========== Special endpoint tests — 特殊端点测试 ==========

#[tokio::test]
async fn test_root_info() {
    let app = create_test_app();

    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    // Verify Kong-compatible root info format — 验证 Kong 兼容的根信息格式
    assert!(json.get("version").is_some());
    assert!(json.get("tagline").is_some());
    assert!(json.get("node_id").is_some());
    assert!(json.get("configuration").is_some());
    assert!(json.get("plugins").is_some());

    let config = json.get("configuration").unwrap();
    assert!(config.get("database").is_some());
    assert!(config.get("router_flavor").is_some());
}

#[tokio::test]
async fn test_plugin_schema_prometheus() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/schemas/plugins/prometheus")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["name"], "prometheus");
    assert_eq!(value["fields"][0]["protocols"]["type"], "set");
}

#[tokio::test]
async fn test_status_endpoint() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    // Verify Kong-compatible status format — 验证 Kong 兼容的状态格式
    assert!(json.get("server").is_some());
    assert!(json.get("database").is_some());
    assert!(json.get("memory").is_some());

    let db = json.get("database").unwrap();
    assert!(db.get("reachable").is_some());
}

#[tokio::test]
async fn test_status_metrics_without_prometheus_plugin() {
    let app = create_test_status_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_status_metrics_with_prometheus_plugin() {
    let app = create_test_status_app_with_prometheus();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(http::header::CONTENT_TYPE).unwrap(),
        "text/plain; charset=utf-8"
    );

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let metrics = String::from_utf8(body.to_vec()).unwrap();
    assert!(metrics.contains("# HELP kong_node_info"));
    assert!(metrics.contains("kong_memory_lua_shared_dict_total_bytes"));
}

// ========== Service CRUD tests — Service CRUD 测试 ==========

#[tokio::test]
async fn test_list_services_empty() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/services")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    // Kong format: { "data": [], "offset": null, "next": null } — Kong 格式：{ "data": [], "offset": null, "next": null }
    assert!(json.get("data").is_some());
    let data = json.get("data").unwrap().as_array().unwrap();
    assert!(data.is_empty());
}

#[tokio::test]
async fn test_list_services_with_data() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/services")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let data = json.get("data").unwrap().as_array().unwrap();
    assert_eq!(data.len(), 2);
}

#[tokio::test]
async fn test_get_service_by_id() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/services/550e8400-e29b-41d4-a716-446655440000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json.get("name").unwrap().as_str().unwrap(), "test-service");
    assert_eq!(json.get("host").unwrap().as_str().unwrap(), "httpbin.org");
    assert_eq!(json.get("port").unwrap().as_u64().unwrap(), 80);
    assert_eq!(json.get("protocol").unwrap().as_str().unwrap(), "http");
}

#[tokio::test]
async fn test_get_service_by_name() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/services/test-service")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json.get("name").unwrap().as_str().unwrap(), "test-service");
}

#[tokio::test]
async fn test_get_service_not_found() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/services/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    // Kong-compatible 404 error format — Kong 兼容的 404 错误格式
    assert!(json.get("message").is_some());
    assert_eq!(json.get("name").unwrap().as_str().unwrap(), "not found");
    assert_eq!(json.get("code").unwrap().as_u64().unwrap(), 3);
}

// ========== DB-less write operation tests — DB-less 写操作测试 ==========

#[tokio::test]
async fn test_create_service_dbless_rejected() {
    let app = create_test_app();

    let body = json!({
        "name": "new-service",
        "host": "newhost.com",
        "port": 80,
        "protocol": "http"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method(http::Method::POST)
                .uri("/services")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    // Write operations should return error in db-less mode — db-less 模式下写操作应返回错误
    assert_ne!(response.status(), StatusCode::CREATED);
}

// ========== Route endpoint tests — Route 端点测试 ==========

#[tokio::test]
async fn test_list_routes() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/routes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let data = json.get("data").unwrap().as_array().unwrap();
    assert_eq!(data.len(), 1);

    let route = &data[0];
    assert_eq!(route.get("name").unwrap().as_str().unwrap(), "test-route");
}

#[tokio::test]
async fn test_get_route_by_id() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/routes/660e8400-e29b-41d4-a716-446655440001")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json.get("name").unwrap().as_str().unwrap(), "test-route");
    let paths = json.get("paths").unwrap().as_array().unwrap();
    assert_eq!(paths[0].as_str().unwrap(), "/test");
}

// ========== Consumer endpoint tests — Consumer 端点测试 ==========

#[tokio::test]
async fn test_list_consumers() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/consumers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let data = json.get("data").unwrap().as_array().unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(
        data[0].get("username").unwrap().as_str().unwrap(),
        "test-consumer"
    );
}

// ========== Nested endpoint tests — 嵌套端点测试 ==========

#[tokio::test]
async fn test_nested_service_routes() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/services/550e8400-e29b-41d4-a716-446655440000/routes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let data = json.get("data").unwrap().as_array().unwrap();
    assert_eq!(data.len(), 1);
}

#[tokio::test]
async fn test_nested_service_routes_by_name() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/services/test-service/routes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let data = json.get("data").unwrap().as_array().unwrap();
    assert_eq!(data.len(), 1);
}

#[tokio::test]
async fn test_create_service_scoped_plugin() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .method(http::Method::POST)
                .uri("/services/550e8400-e29b-41d4-a716-446655440000/plugins")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "name": "key-auth",
                        "enabled": true,
                        "config": {
                            "key_names": ["apikey"]
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_ne!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_get_service_scoped_plugin_detail() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/services/550e8400-e29b-41d4-a716-446655440000/plugins/877dc65d-b37a-408c-bcf6-5d081ea55f7b")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "877dc65d-b37a-408c-bcf6-5d081ea55f7b");
    assert_eq!(
        json["service"]["id"],
        "550e8400-e29b-41d4-a716-446655440000"
    );
}

#[tokio::test]
async fn test_nested_upstream_targets() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/upstreams/880e8400-e29b-41d4-a716-446655440003/targets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let data = json.get("data").unwrap().as_array().unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(
        data[0].get("target").unwrap().as_str().unwrap(),
        "10.0.0.1:80"
    );
}

// ========== Pagination tests — 分页测试 ==========

#[tokio::test]
async fn test_pagination_params() {
    let app = create_test_app_with_data();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/services?size=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let data = json.get("data").unwrap().as_array().unwrap();
    assert_eq!(data.len(), 1);

    // Should have next pagination indicator — 应该有 next 分页指示
    assert!(json.get("next").is_some());
}

// ========== Error format validation — 错误格式验证 ==========

#[tokio::test]
async fn test_not_found_error_format() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/services/nonexistent-uuid-value")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    // Kong-compatible error format — Kong 兼容的错误格式
    assert!(json.get("message").is_some());
    assert_eq!(json.get("name").unwrap().as_str().unwrap(), "not found");
    assert_eq!(json.get("code").unwrap().as_u64().unwrap(), 3);
}

// ========== /cache endpoints (task 16.3) — /cache 端点（任务 16.3） ==========

/// Build an Admin router plus a handle to the shared KongCache, so tests can seed entries.
/// 构建 Admin 路由及共享 KongCache 句柄，便于测试预置缓存条目。
fn create_test_app_with_cache() -> (axum::Router, Arc<kong_db::KongCache>) {
    let store = Arc::new(DblessStore::new());
    let config = Arc::new(kong_config::KongConfig::default());

    let (refresh_tx, _refresh_rx) = tokio::sync::mpsc::unbounded_channel();
    let dns_resolver = std::sync::Arc::new(kong_proxy::dns::DnsResolver::new(&config));
    let proxy = kong_proxy::KongProxy::new(
        &[],
        "traditional",
        kong_plugin_system::PluginRegistry::new(),
        kong_proxy::tls::CertificateManager::new(),
        vec![],
        dns_resolver,
        Arc::clone(&config),
    );

    let cache = Arc::new(kong_db::KongCache::from_kong_config(&config));

    let state = AdminState {
        services: Arc::new(DblessDao::<Service>::new(store.clone())),
        routes: Arc::new(DblessDao::<Route>::new(store.clone())),
        consumers: Arc::new(DblessDao::<Consumer>::new(store.clone())),
        plugins: Arc::new(DblessDao::<Plugin>::new(store.clone())),
        upstreams: Arc::new(DblessDao::<Upstream>::new(store.clone())),
        targets: Arc::new(DblessDao::<Target>::new(store.clone())),
        certificates: Arc::new(DblessDao::<Certificate>::new(store.clone())),
        snis: Arc::new(DblessDao::<Sni>::new(store.clone())),
        ca_certificates: Arc::new(DblessDao::<CaCertificate>::new(store.clone())),
        vaults: Arc::new(DblessDao::<Vault>::new(store.clone())),
        key_sets: Arc::new(DblessDao::<KeySet>::new(store.clone())),
        keys: Arc::new(DblessDao::<Key>::new(store.clone())),
        ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(store.clone())),
        ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(store.clone())),
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(store.clone())),
        node_id: Uuid::new_v4(),
        config,
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(std::sync::RwLock::new("0".repeat(32))),
        dbless_store: None,
        target_health: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        cp: None,
        cache: Arc::clone(&cache),
        log_updater: None,
        current_log_level: Arc::new(std::sync::RwLock::new("info".to_string())),
    };

    (build_admin_router(state), cache)
}

#[tokio::test]
async fn test_cache_get_miss_returns_404() {
    let (app, _cache) = create_test_app_with_cache();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/cache/unknown-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_cache_get_hit_returns_value() {
    let (app, cache) = create_test_app_with_cache();
    cache.set("services:abc", json!({"id": "abc", "name": "svc"}));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/cache/services:abc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["name"], "svc");
}

#[tokio::test]
async fn test_cache_delete_entry() {
    let (app, cache) = create_test_app_with_cache();
    cache.set("services:xyz", json!({"id": "xyz"}));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(http::Method::DELETE)
                .uri("/cache/services:xyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(cache.get("services:xyz").is_none());
}

#[tokio::test]
async fn test_cache_purge_all() {
    let (app, cache) = create_test_app_with_cache();
    cache.set("services:a", json!({"id": "a"}));
    cache.set("routes:b", json!({"id": "b"}));

    let response = app
        .oneshot(
            Request::builder()
                .method(http::Method::DELETE)
                .uri("/cache")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(cache.get("services:a").is_none());
    assert!(cache.get("routes:b").is_none());
}

// ========== /debug/node/log-level endpoints (task 16.4) — 运行时日志级别端点（任务 16.4） ==========

#[tokio::test]
async fn test_log_level_get_returns_current() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/debug/node/log-level")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let msg = json["message"].as_str().unwrap();
    assert!(msg.contains("log level:"), "unexpected body: {}", msg);
}

#[tokio::test]
async fn test_log_level_put_rejects_unknown_level() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .method(http::Method::PUT)
                .uri("/debug/node/log-level/verbose")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

// ========== /timers endpoint (task 16.5) — /timers 端点（任务 16.5） ==========

#[tokio::test]
async fn test_timers_endpoint_returns_kong_shape() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/timers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();

    // Kong-compatible shape: worker.id / worker.count + stats.{sys,timers,flamegraph}
    // Kong 兼容结构：worker.id / worker.count + stats.{sys,timers,flamegraph}
    assert!(json["worker"]["id"].is_i64() || json["worker"]["id"].is_u64());
    assert!(json["worker"]["count"].is_i64() || json["worker"]["count"].is_u64());
    assert!(json["stats"]["sys"].is_object());
    assert!(json["stats"]["timers"].is_object());
    assert!(json["stats"]["flamegraph"].is_object());

    for key in ["total", "runs", "running", "pending", "waiting"] {
        assert!(
            json["stats"]["sys"][key].is_number(),
            "stats.sys.{} missing or not number",
            key
        );
    }
}

#[tokio::test]
async fn test_log_level_put_without_updater_returns_503() {
    // create_test_app uses log_updater: None, so PUT should report "not supported".
    // create_test_app 的 log_updater 为 None，因此 PUT 应返回"不支持"。
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .method(http::Method::PUT)
                .uri("/debug/node/log-level/debug")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// ========== /key-sets and /keys (task 16.1 / 16.2) — KeySet/Key 端点 ==========

/// Build a router preloaded with one key_set + one key for read-path tests.
/// 构建预置 1 个 key_set + 1 个 key 的路由，用于读路径测试。
/// (DblessStore is read-only at runtime — writes route through /config — DblessStore 运行时只读，写入需经由 /config)
fn create_test_app_with_keys() -> (axum::Router, String, String) {
    let store = Arc::new(DblessStore::new());
    let set_id = "11111111-1111-1111-1111-111111111111";
    let key_id = "22222222-2222-2222-2222-222222222222";
    let data = json!({
        "_format_version": "3.0",
        "key_sets": [
            {
                "id": set_id,
                "name": "primary",
                "created_at": 1609459200,
                "updated_at": 1609459200,
            }
        ],
        "keys": [
            {
                "id": key_id,
                "name": "primary-rsa",
                "set": { "id": set_id },
                "kid": "kid-1",
                "jwk": "{\"kid\":\"kid-1\"}",
                "cache_key": format!("kid-1:{}", set_id),
                "created_at": 1609459200,
                "updated_at": 1609459200,
            }
        ]
    });
    store.load_from_json(&data).unwrap();

    let config = Arc::new(kong_config::KongConfig::default());
    let (refresh_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let dns_resolver = std::sync::Arc::new(kong_proxy::dns::DnsResolver::new(&config));
    let proxy = kong_proxy::KongProxy::new(
        &[],
        "traditional",
        kong_plugin_system::PluginRegistry::new(),
        kong_proxy::tls::CertificateManager::new(),
        vec![],
        dns_resolver,
        Arc::clone(&config),
    );
    let cache = Arc::new(kong_db::KongCache::from_kong_config(&config));

    let state = AdminState {
        services: Arc::new(DblessDao::<Service>::new(store.clone())),
        routes: Arc::new(DblessDao::<Route>::new(store.clone())),
        consumers: Arc::new(DblessDao::<Consumer>::new(store.clone())),
        plugins: Arc::new(DblessDao::<Plugin>::new(store.clone())),
        upstreams: Arc::new(DblessDao::<Upstream>::new(store.clone())),
        targets: Arc::new(DblessDao::<Target>::new(store.clone())),
        certificates: Arc::new(DblessDao::<Certificate>::new(store.clone())),
        snis: Arc::new(DblessDao::<Sni>::new(store.clone())),
        ca_certificates: Arc::new(DblessDao::<CaCertificate>::new(store.clone())),
        vaults: Arc::new(DblessDao::<Vault>::new(store.clone())),
        key_sets: Arc::new(DblessDao::<KeySet>::new(store.clone())),
        keys: Arc::new(DblessDao::<Key>::new(store.clone())),
        ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(store.clone())),
        ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(store.clone())),
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(store.clone())),
        node_id: Uuid::new_v4(),
        config,
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(std::sync::RwLock::new("0".repeat(32))),
        dbless_store: Some(store),
        target_health: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        cp: None,
        cache,
        log_updater: None,
        current_log_level: Arc::new(std::sync::RwLock::new("info".to_string())),
    };
    (build_admin_router(state), set_id.to_string(), key_id.to_string())
}

#[tokio::test]
async fn test_key_sets_list_and_get() {
    let (app, set_id, _) = create_test_app_with_keys();

    // List — 列表
    let response = app
        .clone()
        .oneshot(Request::builder().uri("/key-sets").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let list: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert_eq!(list["data"][0]["name"], "primary");

    // Get by id — 按 id 获取
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/key-sets/{}", set_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["id"], set_id);

    // Get by name — 按 name 获取（endpoint_key）
    let response = app
        .oneshot(
            Request::builder()
                .uri("/key-sets/primary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_keys_list_and_get() {
    let (app, _, key_id) = create_test_app_with_keys();

    // List — 列表
    let response = app
        .clone()
        .oneshot(Request::builder().uri("/keys").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let list: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert_eq!(list["data"][0]["kid"], "kid-1");

    // Get by id — 按 id 获取
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/keys/{}", key_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["kid"], "kid-1");
}

#[tokio::test]
async fn test_keys_nested_under_key_set() {
    let (app, set_id, _) = create_test_app_with_keys();

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/key-sets/{}/keys", set_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let list: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert_eq!(list["data"][0]["set"]["id"], set_id);
}

#[tokio::test]
async fn test_keys_nested_unknown_key_set_returns_404() {
    let (app, _, _) = create_test_app_with_keys();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/key-sets/00000000-0000-0000-0000-000000000000/keys")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_keys_create_requires_kid() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .method(http::Method::POST)
                .uri("/keys")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"jwk": "{\"kid\":\"abc\"}"})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let err: Value = serde_json::from_slice(&body).unwrap();
    assert!(err["message"]
        .as_str()
        .unwrap()
        .contains("kid: required field missing"));
}

#[tokio::test]
async fn test_keys_create_requires_jwk_or_pem() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .method(http::Method::POST)
                .uri("/keys")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&json!({"kid": "kid-1"})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_keys_schemas_endpoint() {
    let app = create_test_app();

    for entity in ["key_sets", "keys"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/schemas/{}", entity))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "schema {} should exist", entity);
    }
}

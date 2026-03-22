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
        ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(store.clone())),
        ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(store.clone())),
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(store.clone())),
        node_id: Uuid::new_v4(),
        config,
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(std::sync::RwLock::new("00000000000000000000000000000000".to_string())),
        dbless_store: None,
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
        ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(store.clone())),
        ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(store.clone())),
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(store.clone())),
        node_id: Uuid::new_v4(),
        config,
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(std::sync::RwLock::new("00000000000000000000000000000000".to_string())),
        dbless_store: None,
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
        ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(store.clone())),
        ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(store.clone())),
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(store.clone())),
        node_id: Uuid::new_v4(),
        config,
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(std::sync::RwLock::new("00000000000000000000000000000000".to_string())),
        dbless_store: None,
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
        ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(store.clone())),
        ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(store.clone())),
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(store.clone())),
        node_id: Uuid::new_v4(),
        config,
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(std::sync::RwLock::new("00000000000000000000000000000000".to_string())),
        dbless_store: None,
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

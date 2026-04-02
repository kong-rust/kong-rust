use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use uuid::Uuid;

use kong_admin::{build_admin_router, AdminState};
use kong_core::models::*;
use kong_db::{DblessDao, DblessStore};

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
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(store)),
        node_id: Uuid::new_v4(),
        config,
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(std::sync::RwLock::new("00000000000000000000000000000000".to_string())),
        dbless_store: None,
        target_health: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        cp: None,
    };

    build_admin_router(state)
}

#[tokio::test]
async fn test_plugin_schema_ai_proxy() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/schemas/plugins/ai-proxy")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["name"], "ai-proxy");
    assert_eq!(value["fields"][0]["protocols"]["type"], "set");
    assert_eq!(value["fields"][1]["config"]["type"], "record");
}

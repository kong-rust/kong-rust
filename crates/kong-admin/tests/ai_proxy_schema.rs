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
        key_sets: Arc::new(DblessDao::<KeySet>::new(store.clone())),
        keys: Arc::new(DblessDao::<Key>::new(store.clone())),
        ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(store.clone())),
        ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(store.clone())),
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(store.clone())),
        ai_usage: kong_ai::usage::AiUsageRuntime::unsupported_hybrid(),
        virtual_key_auth: Arc::new(kong_ai::auth::VirtualKeyAuthenticator::new(Arc::new(
            DblessDao::<kong_ai::models::AiVirtualKey>::new(store),
        ))),
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

    let config_fields = value["fields"][1]["config"]["fields"]
        .as_array()
        .expect("ai-proxy config fields must be an array");
    let config_field = |name: &str| {
        config_fields
            .iter()
            .find_map(|field| field.get(name))
            .unwrap_or_else(|| panic!("missing ai-proxy config field: {name}"))
    };

    let model = config_field("model");
    assert_eq!(model["type"], "record");
    assert!(model.get("required").is_none());
    let model_fields = model["fields"]
        .as_array()
        .expect("official Kong model record fields must be present");
    let model_field = |name: &str| {
        model_fields
            .iter()
            .find_map(|field| field.get(name))
            .unwrap_or_else(|| panic!("missing official Kong model field: {name}"))
    };
    assert_eq!(model_field("provider")["type"], "string");
    assert_eq!(model_field("provider")["required"], true);
    assert_eq!(
        model_field("provider")["one_of"],
        serde_json::json!(["openai", "anthropic", "gemini", "openai_compat"])
    );
    assert_eq!(model_field("name")["type"], "string");
    assert_eq!(model_field("options")["type"], "record");
    let model_options = model_field("options")["fields"]
        .as_array()
        .expect("official Kong model options must be present");
    let model_option = |name: &str| {
        model_options
            .iter()
            .find_map(|field| field.get(name))
            .unwrap_or_else(|| panic!("missing official Kong model option: {name}"))
    };
    assert_eq!(model_option("upstream_url")["type"], "string");
    assert_eq!(model_option("anthropic_version")["type"], "string");
    assert_eq!(model_option("azure_api_version")["type"], "string");

    let model_group = config_field("model_group");
    assert_eq!(model_group["type"], "string");
    assert!(model_group.get("required").is_none());
    assert_eq!(config_field("model_source")["type"], "string");
    assert_eq!(config_field("model_source")["default"], "config");
    assert_eq!(config_field("model_source")["one_of"], serde_json::json!(["config", "request"]));
    assert_eq!(config_field("client_protocol")["type"], "string");
    assert_eq!(config_field("client_protocol")["default"], "openai");
    assert_eq!(config_field("client_protocol")["one_of"], serde_json::json!(["openai", "anthropic"]));
    assert_eq!(config_field("route_type")["type"], "string");
    assert_eq!(config_field("route_type")["default"], "llm/v1/chat");
    assert_eq!(config_field("route_type")["one_of"], serde_json::json!(["llm/v1/chat", "llm/v1/completions", "llm/v1/responses"]));
    assert_eq!(config_field("response_streaming")["type"], "string");
    assert_eq!(config_field("response_streaming")["default"], "allow");
    assert_eq!(config_field("response_streaming")["one_of"], serde_json::json!(["allow", "deny", "always"]));
    assert_eq!(config_field("auth")["type"], "record");
    assert_eq!(config_field("logging")["type"], "record");
    assert_eq!(config_field("llm_format")["type"], "string");
}

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use kong_admin::{build_admin_router, AdminState};
use kong_core::models::*;
use kong_db::{DblessDao, DblessStore};

fn create_test_app() -> axum::Router {
    let store = Arc::new(DblessStore::new());
    create_test_app_with_store(store)
}

fn create_test_app_with_store(store: Arc<DblessStore>) -> axum::Router {
    let config = Arc::new(kong_config::KongConfig::default());
    create_test_app_with_store_and_config(store, config)
}

fn create_test_app_with_store_and_config(
    store: Arc<DblessStore>,
    config: Arc<kong_config::KongConfig>,
) -> axum::Router {
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
        ai_providers: Arc::new(DblessDao::<kong_ai::models::AiProviderConfig>::new(
            store.clone(),
        )),
        ai_models: Arc::new(DblessDao::<kong_ai::models::AiModel>::new(store.clone())),
        ai_virtual_keys: Arc::new(DblessDao::<kong_ai::models::AiVirtualKey>::new(
            store.clone(),
        )),
        ai_enforcement: Arc::new(kong_ai::enforcement::AiEnforcementRuntime::unsupported_hybrid()),
        ai_policy_coverage: Arc::new(std::sync::RwLock::new(
            kong_admin::ai_policy_coverage::AiPolicyCoverageIndex::unavailable(Uuid::nil()),
        )),
        default_workspace_id: Uuid::nil(),
        ai_budget_governance: None,
        ai_budget_admin: None,
        ai_usage: kong_ai::usage::AiUsageRuntime::unsupported_hybrid(),
        virtual_key_auth: Arc::new(kong_ai::auth::VirtualKeyAuthenticator::new(Arc::new(
            DblessDao::<kong_ai::models::AiVirtualKey>::new(store),
        ))),
        node_id: Uuid::new_v4(),
        config: Arc::clone(&config),
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(std::sync::RwLock::new(
            "00000000000000000000000000000000".to_string(),
        )),
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
async fn test_status_exposes_only_safe_context_compression_capability() {
    let mut config = kong_config::KongConfig::default();
    config.ai_context_compression_headroom_url = Some("http://127.0.0.1:8787/headroom".to_string());
    config.ai_context_compression_store_scope = "cluster".to_string();
    let app = create_test_app_with_store_and_config(Arc::new(DblessStore::new()), Arc::new(config));

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
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value["ai_context_compression"]["configuration_status"],
        "configured"
    );
    assert_eq!(value["ai_context_compression"]["backend"], "headroom_proxy");
    assert_eq!(value["ai_context_compression"]["transparent_ccr"], true);
    assert_eq!(
        value["ai_context_compression"]["protocols"],
        json!(["openai_responses", "anthropic_messages"])
    );
    assert_eq!(value["ai_context_compression"]["streaming"], false);
    assert_eq!(value["ai_context_compression"]["store_scope"], "local");
    let serialized = serde_json::to_string(&value).unwrap();
    assert!(!serialized.contains("127.0.0.1:8787"));
}

fn create_test_app_with_ai_rate_limit() -> axum::Router {
    let store = Arc::new(DblessStore::new());
    store
        .load_from_json(&serde_json::json!({
            "_format_version": "3.0",
            "plugins": [{
                "id": "877dc65d-b37a-408c-bcf6-5d081ea55f7b",
                "name": "ai-rate-limit",
                "enabled": true,
                "config": {
                    "limit_by": "consumer",
                    "rpm_limit": 100,
                    "tpm_limit": null
                }
            }]
        }))
        .unwrap();
    create_test_app_with_store(store)
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
    assert_eq!(
        config_field("model_source")["one_of"],
        serde_json::json!(["config", "request"])
    );
    assert_eq!(config_field("client_protocol")["type"], "string");
    assert_eq!(config_field("client_protocol")["default"], "openai");
    assert_eq!(
        config_field("client_protocol")["one_of"],
        serde_json::json!(["openai", "anthropic"])
    );
    assert_eq!(config_field("route_type")["type"], "string");
    assert_eq!(config_field("route_type")["default"], "llm/v1/chat");
    assert_eq!(
        config_field("route_type")["one_of"],
        serde_json::json!(["llm/v1/chat", "llm/v1/completions", "llm/v1/responses"])
    );
    assert_eq!(config_field("response_streaming")["type"], "string");
    assert_eq!(config_field("response_streaming")["default"], "allow");
    assert_eq!(
        config_field("response_streaming")["one_of"],
        serde_json::json!(["allow", "deny", "always"])
    );
    assert_eq!(config_field("auth")["type"], "record");
    assert_eq!(config_field("logging")["type"], "record");
    assert_eq!(config_field("llm_format")["type"], "string");
}

#[tokio::test]
async fn test_plugin_schema_ai_rate_limit() {
    let response = create_test_app()
        .oneshot(
            Request::builder()
                .uri("/schemas/plugins/ai-rate-limit")
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
    let config_fields = value["fields"][1]["config"]["fields"].as_array().unwrap();
    let config_field = |name: &str| {
        config_fields
            .iter()
            .find_map(|field| field.get(name))
            .unwrap_or_else(|| panic!("missing ai-rate-limit config field: {name}"))
    };

    assert_eq!(
        config_field("limit_by")["one_of"],
        serde_json::json!(["global", "route", "consumer", "virtual_key"])
    );
    assert_eq!(
        config_field("rpm_limit")["between"],
        serde_json::json!([1, 2_147_483_647])
    );
    assert_eq!(config_field("rpm_limit")["nullable"], true);
    assert!(config_field("rpm_limit")["default"].is_null());
    assert_eq!(config_field("header_name")["deprecated"], true);
    assert!(!value["entity_checks"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_plugin_schema_and_validation_for_context_compression() {
    let app = create_test_app();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/schemas/plugins/ai-context-compression")
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
    assert_eq!(value["name"], "ai-context-compression");
    let fields = value["fields"][1]["config"]["fields"].as_array().unwrap();
    assert!(fields
        .iter()
        .any(|field| field.get("min_input_tokens").is_some()));
    assert!(fields
        .iter()
        .any(|field| field.get("on_unavailable").is_some()));

    let valid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/schemas/plugins/validate")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "ai-context-compression",
                        "config": {
                            "min_input_tokens": 2000,
                            "max_input_bytes": 4194304,
                            "on_unavailable": "pass_through",
                            "streaming": "bypass",
                            "expose_metrics_headers": false
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(valid.status(), StatusCode::OK);

    for config in [
        json!({"max_input_bytes": 0}),
        json!({"on_unavailable": "retry"}),
        json!({"streaming": "compress"}),
        json!({"unexpected": true}),
    ] {
        let invalid = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/schemas/plugins/validate")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "ai-context-compression",
                            "config": config
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn test_ai_rate_limit_schema_validate_uses_shared_entity_rules() {
    let app = create_test_app();
    let valid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/schemas/plugins/validate")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": "ai-rate-limit",
                        "config": {"limit_by": "virtual_key"}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(valid.status(), StatusCode::OK);

    for config in [
        serde_json::json!({"limit_by": "service", "rpm_limit": 10}),
        serde_json::json!({"limit_by": "consumer", "rpm_limit": "10"}),
        serde_json::json!({"limit_by": "consumer", "rpm_limit": 0}),
        serde_json::json!({"limit_by": "consumer", "rpm_limit": 1.5}),
        serde_json::json!({"limit_by": "consumer", "rpm_limit": 2_147_483_648_u64}),
        serde_json::json!({"limit_by": "virtual_key", "rpm_limit": 10}),
        serde_json::json!({"limit_by": "route", "rpm_limit": null, "tpm_limit": null}),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/schemas/plugins/validate")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "ai-rate-limit",
                            "config": config
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn test_ai_rate_limit_create_and_put_validate_full_config() {
    let app = create_test_app();
    for (method, uri) in [
        ("POST", "/plugins"),
        ("PUT", "/plugins/877dc65d-b37a-408c-bcf6-5d081ea55f7b"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": "ai-rate-limit",
                            "config": {
                                "limit_by": "consumer",
                                "rpm_limit": -1
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn test_ai_rate_limit_patch_validates_after_merge_and_defaults() {
    let app = create_test_app_with_ai_rate_limit();
    for config_patch in [
        serde_json::json!({"rpm_limit": null}),
        serde_json::json!({"limit_by": "virtual_key"}),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/plugins/877dc65d-b37a-408c-bcf6-5d081ea55f7b")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"config": config_patch}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(value["fields"]["config"]["@entity"].is_array());
    }
}

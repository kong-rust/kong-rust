use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use kong_admin::{build_admin_router, AdminState};
use kong_ai::budget::{
    BudgetAccountGovernance, BudgetAccountSnapshot, BudgetAccountingState, BudgetErrorKind,
    BudgetLimitMutation, BudgetOptionalMutation, BudgetStoreError, CreateBudgetAccount,
    DeleteBudgetAccount, DeletedBudgetAccount, UpdateBudgetAccount, UpdateBudgetLimit,
};
use kong_ai::models::{parse_budget_amount, AiVirtualKey};
use kong_core::error::{KongError, Result};
use kong_core::models::*;
use kong_core::traits::{Dao, Entity, Page, PageParams, PrimaryKey};
use kong_core::ClusterRole;
use kong_db::{DblessDao, DblessStore};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Default)]
struct MemoryVirtualKeyDao {
    keys: RwLock<Vec<AiVirtualKey>>,
}

struct MemoryBudgetGovernance {
    keys: Arc<MemoryVirtualKeyDao>,
}

fn account_snapshot(key: &AiVirtualKey) -> BudgetAccountSnapshot {
    BudgetAccountSnapshot {
        virtual_key_id: key.id,
        virtual_key_name: key.name.clone(),
        virtual_key_prefix: key.key_prefix.clone(),
        workspace_id: key.ws_id,
        limit_usd: key.budget_limit,
        used_usd: key.budget_used,
        pending_count: key.budget_pending_count,
        unresolved_count: key.budget_unresolved_count,
        accounting_revision: key.budget_accounting_revision,
        checkpoint_tail_events: key.budget_checkpoint_tail_events,
        state: if key.budget_unresolved_count > 0 {
            BudgetAccountingState::Unresolved
        } else if key.budget_pending_count > 0 {
            BudgetAccountingState::Pending
        } else {
            BudgetAccountingState::Clean
        },
        state_updated_at: chrono::Utc::now(),
    }
}

#[async_trait]
impl BudgetAccountGovernance for MemoryBudgetGovernance {
    async fn create_account(
        &self,
        command: CreateBudgetAccount,
    ) -> std::result::Result<BudgetAccountSnapshot, BudgetStoreError> {
        let now = chrono::Utc::now().timestamp();
        let key = AiVirtualKey {
            id: command.virtual_key_id,
            name: command.name,
            key_hash: command.key_hash,
            key_prefix: command.key_prefix,
            consumer_id: command.consumer_id,
            allowed_models: command.allowed_models,
            tpm_limit: command.tpm_limit,
            rpm_limit: command.rpm_limit,
            budget_limit: command.budget_limit_usd,
            enabled: command.enabled,
            expires_at: command.expires_at.map(|value| value.timestamp()),
            ws_id: Some(command.workspace_id),
            created_at: Some(now),
            updated_at: Some(now),
            ..AiVirtualKey::default()
        };
        self.keys.keys.write().unwrap().push(key.clone());
        Ok(account_snapshot(&key))
    }

    async fn update_limit(
        &self,
        command: UpdateBudgetLimit,
    ) -> std::result::Result<BudgetAccountSnapshot, BudgetStoreError> {
        self.update_account(UpdateBudgetAccount {
            virtual_key_id: command.virtual_key_id,
            scope: command.scope,
            name: None,
            consumer_id: BudgetOptionalMutation::Unchanged,
            allowed_models: BudgetOptionalMutation::Unchanged,
            tpm_limit: BudgetOptionalMutation::Unchanged,
            rpm_limit: BudgetOptionalMutation::Unchanged,
            budget_limit: command.mutation,
            enabled: None,
            expires_at: BudgetOptionalMutation::Unchanged,
            tags: BudgetOptionalMutation::Unchanged,
        })
        .await
    }

    async fn update_account(
        &self,
        command: UpdateBudgetAccount,
    ) -> std::result::Result<BudgetAccountSnapshot, BudgetStoreError> {
        let mut keys = self.keys.keys.write().unwrap();
        let key = keys
            .iter_mut()
            .find(|key| key.id == command.virtual_key_id)
            .ok_or_else(|| BudgetStoreError::new(BudgetErrorKind::NotFound, "not found"))?;
        if let Some(name) = command.name {
            key.name = name;
        }
        apply_optional(&mut key.consumer_id, command.consumer_id);
        apply_optional(&mut key.allowed_models, command.allowed_models);
        apply_optional(&mut key.tpm_limit, command.tpm_limit);
        apply_optional(&mut key.rpm_limit, command.rpm_limit);
        match command.budget_limit {
            BudgetLimitMutation::Unchanged => {}
            BudgetLimitMutation::Set(value) => key.budget_limit = Some(value),
            BudgetLimitMutation::Clear => key.budget_limit = None,
        }
        if let Some(enabled) = command.enabled {
            key.enabled = enabled;
        }
        let expires_at = match command.expires_at {
            BudgetOptionalMutation::Unchanged => None,
            BudgetOptionalMutation::Set(value) => Some(Some(value.timestamp())),
            BudgetOptionalMutation::Clear => Some(None),
        };
        if let Some(expires_at) = expires_at {
            key.expires_at = expires_at;
        }
        apply_optional(&mut key.tags, command.tags);
        key.budget_accounting_revision += 1;
        Ok(account_snapshot(key))
    }

    async fn delete_account(
        &self,
        command: DeleteBudgetAccount,
    ) -> std::result::Result<DeletedBudgetAccount, BudgetStoreError> {
        let mut keys = self.keys.keys.write().unwrap();
        let index = keys
            .iter()
            .position(|key| key.id == command.virtual_key_id)
            .ok_or_else(|| BudgetStoreError::new(BudgetErrorKind::NotFound, "not found"))?;
        let key = keys.remove(index);
        Ok(DeletedBudgetAccount {
            account: account_snapshot(&key),
            deleted_at: chrono::Utc::now(),
        })
    }
}

fn apply_optional<T>(target: &mut Option<T>, mutation: BudgetOptionalMutation<T>) {
    match mutation {
        BudgetOptionalMutation::Unchanged => {}
        BudgetOptionalMutation::Set(value) => *target = Some(value),
        BudgetOptionalMutation::Clear => *target = None,
    }
}

impl MemoryVirtualKeyDao {
    fn with_keys(keys: Vec<AiVirtualKey>) -> Self {
        Self {
            keys: RwLock::new(keys),
        }
    }

    fn index_of(keys: &[AiVirtualKey], pk: &PrimaryKey) -> Option<usize> {
        keys.iter().position(|key| match pk {
            PrimaryKey::Id(id) => key.id == *id,
            PrimaryKey::EndpointKey(name) => key.name == *name,
        })
    }
}

#[async_trait]
impl Dao<AiVirtualKey> for MemoryVirtualKeyDao {
    async fn insert(&self, entity: &AiVirtualKey) -> Result<AiVirtualKey> {
        self.keys.write().unwrap().push(entity.clone());
        Ok(entity.clone())
    }

    async fn select(&self, pk: &PrimaryKey) -> Result<Option<AiVirtualKey>> {
        let keys = self.keys.read().unwrap();
        Ok(Self::index_of(&keys, pk).map(|index| keys[index].clone()))
    }

    async fn page(&self, _params: &PageParams) -> Result<Page<AiVirtualKey>> {
        Ok(Page {
            data: self.keys.read().unwrap().clone(),
            offset: None,
            next: None,
        })
    }

    async fn update(&self, pk: &PrimaryKey, entity: &Value) -> Result<AiVirtualKey> {
        let mut keys = self.keys.write().unwrap();
        let index = Self::index_of(&keys, pk).ok_or_else(|| KongError::NotFound {
            entity_type: AiVirtualKey::table_name().to_string(),
            id: format!("{pk:?}"),
        })?;
        let updated: AiVirtualKey = serde_json::from_value(entity.clone())?;
        keys[index] = updated.clone();
        Ok(updated)
    }

    async fn upsert(&self, pk: &PrimaryKey, entity: &AiVirtualKey) -> Result<AiVirtualKey> {
        let mut keys = self.keys.write().unwrap();
        if let Some(index) = Self::index_of(&keys, pk) {
            keys[index] = entity.clone();
        } else {
            keys.push(entity.clone());
        }
        Ok(entity.clone())
    }

    async fn delete(&self, pk: &PrimaryKey) -> Result<()> {
        let mut keys = self.keys.write().unwrap();
        if let Some(index) = Self::index_of(&keys, pk) {
            keys.remove(index);
            Ok(())
        } else {
            Err(KongError::NotFound {
                entity_type: AiVirtualKey::table_name().to_string(),
                id: format!("{pk:?}"),
            })
        }
    }

    async fn select_by_foreign_key(
        &self,
        _foreign_key_field: &str,
        _foreign_key_value: &Uuid,
        _params: &PageParams,
    ) -> Result<Page<AiVirtualKey>> {
        Ok(Page {
            data: Vec::new(),
            offset: None,
            next: None,
        })
    }
}

fn virtual_key(name: &str) -> AiVirtualKey {
    AiVirtualKey {
        id: Uuid::new_v4(),
        name: name.to_string(),
        key_hash: "server-secret-hash".to_string(),
        key_prefix: "sk-test".to_string(),
        enabled: true,
        ..AiVirtualKey::default()
    }
}

fn test_app(role: ClusterRole, database: &str, keys: Vec<AiVirtualKey>) -> axum::Router {
    let config = Arc::new(kong_config::KongConfig {
        role,
        database: database.to_string(),
        ..kong_config::KongConfig::default()
    });
    let store = Arc::new(DblessStore::new());
    let virtual_keys = Arc::new(MemoryVirtualKeyDao::with_keys(keys));
    let governance: Arc<dyn BudgetAccountGovernance> = Arc::new(MemoryBudgetGovernance {
        keys: Arc::clone(&virtual_keys),
    });
    let virtual_keys_for_auth: Arc<dyn Dao<AiVirtualKey>> = virtual_keys.clone();
    let virtual_keys_for_admin: Arc<dyn Dao<AiVirtualKey>> = virtual_keys;
    let ai_enforcement = if role.is_traditional() {
        let quota_store = Arc::new(kong_ai::ratelimit::MemoryRateLimitStore::with_defaults(
            Arc::new(kong_ai::ratelimit::SystemRateLimitClock::new()),
        ));
        Arc::new(
            kong_ai::enforcement::AiEnforcementRuntime::with_local_quota(
                quota_store,
                "admin-test",
                database == "off",
                if database == "off" {
                    kong_ai::enforcement::BudgetCapability::UnsupportedDbLess
                } else {
                    kong_ai::enforcement::BudgetCapability::AccountingUnavailable
                },
            )
            .unwrap(),
        )
    } else {
        Arc::new(kong_ai::enforcement::AiEnforcementRuntime::unsupported_hybrid())
    };

    let (refresh_tx, _refresh_rx) = tokio::sync::mpsc::unbounded_channel();
    let dns_resolver = Arc::new(kong_proxy::dns::DnsResolver::new(&config));
    let proxy = kong_proxy::KongProxy::new(
        &[],
        "traditional",
        kong_plugin_system::PluginRegistry::new(),
        kong_proxy::tls::CertificateManager::new(),
        vec![],
        dns_resolver,
        Arc::clone(&config),
    );

    build_admin_router(AdminState {
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
        ai_providers: Arc::new(DblessDao::new(store.clone())),
        ai_models: Arc::new(DblessDao::new(store.clone())),
        ai_virtual_keys: virtual_keys_for_admin,
        ai_enforcement,
        ai_policy_coverage: Arc::new(RwLock::new(
            kong_admin::ai_policy_coverage::AiPolicyCoverageIndex::unavailable(Uuid::nil()),
        )),
        default_workspace_id: Uuid::nil(),
        ai_budget_governance: Some(governance),
        ai_budget_admin: None,
        ai_usage: kong_ai::usage::AiUsageRuntime::unsupported_hybrid(),
        virtual_key_auth: Arc::new(kong_ai::auth::VirtualKeyAuthenticator::new(
            virtual_keys_for_auth,
        )),
        node_id: Uuid::new_v4(),
        config: Arc::clone(&config),
        proxy,
        refresh_tx,
        stream_router: None,
        configuration_hash: Arc::new(RwLock::new("0".repeat(32))),
        dbless_store: None,
        target_health: Arc::new(RwLock::new(std::collections::HashMap::new())),
        cp: None,
        cache: Arc::new(kong_db::KongCache::from_kong_config(&config)),
        log_updater: None,
        current_log_level: Arc::new(RwLock::new("info".to_string())),
    })
}

async fn request_json(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn request_form(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: &str,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn create_projects_exact_budget_and_conservative_capability_status() {
    let app = test_app(ClusterRole::Traditional, "postgres", Vec::new());
    let (status, created) = request_json(
        app.clone(),
        Method::POST,
        "/ai-virtual-keys",
        json!({
            "name": "  precise-key  ",
            "rpm_limit": 100,
            "tpm_limit": 2000,
            "budget_limit_decimal": "100.500000000000"
        }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["name"], "precise-key");
    assert_eq!(created["budget_limit_decimal"], "100.500000000000");
    assert_eq!(created["budget_limit"].as_f64(), Some(100.5));
    assert_eq!(created["budget_used_decimal"], "0.000000000000");
    assert_eq!(created["budget_used"].as_f64(), Some(0.0));
    assert!(created.get("key_hash").is_none());
    assert!(created["key"].as_str().unwrap().starts_with("sk-kr-"));
    assert_eq!(created["capability"]["quota"], "local_memory");
    assert_eq!(created["capability"]["budget"], "accounting_unavailable");
    assert_eq!(created["coverage_available"], false);
    assert_eq!(created["auth_endpoint_count"], Value::Null);
    assert_eq!(created["quota_enforcement"], "awaiting_plugin");
    assert_eq!(created["budget_status"], "unavailable");
    assert_eq!(created["budget_financial_status"], "active");

    let (status, fetched) = get_json(app, "/ai-virtual-keys/precise-key").await;
    assert_eq!(status, StatusCode::OK);
    assert!(fetched.get("key").is_none());
    assert!(fetched.get("key_hash").is_none());
    assert_eq!(fetched["budget_limit_decimal"], "100.500000000000");
}

#[tokio::test]
async fn form_create_preserves_canonical_decimal_without_float_round_trip() {
    let app = test_app(ClusterRole::Traditional, "postgres", Vec::new());
    let (status, created) = request_form(
        app,
        Method::POST,
        "/ai-virtual-keys",
        "name=form-key&budget_limit_decimal=0.123456789012&rpm_limit=10",
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["budget_limit_decimal"], "0.123456789012");
    assert_eq!(created["rpm_limit"], 10);
}

#[tokio::test]
async fn list_projects_percentage_counts_and_nulls_unsafe_legacy_numbers() {
    let mut warning = virtual_key("warning");
    warning.budget_limit = Some(parse_budget_amount("100.5").unwrap());
    warning.budget_used = parse_budget_amount("83.25").unwrap();
    warning.budget_pending_count = 1;

    let mut large = virtual_key("large");
    large.budget_limit = Some(parse_budget_amount("9500000000000000").unwrap());
    large.budget_used = parse_budget_amount("9400000000000000").unwrap();

    let app = test_app(ClusterRole::Traditional, "postgres", vec![warning, large]);
    let (status, list) = get_json(app, "/ai-virtual-keys").await;
    assert_eq!(status, StatusCode::OK);

    let rows = list["data"].as_array().unwrap();
    let warning = rows.iter().find(|row| row["name"] == "warning").unwrap();
    assert_eq!(warning["budget_percentage_decimal"], "82.835820895522");
    assert_eq!(warning["budget_status"], "unavailable");
    assert_eq!(warning["budget_financial_status"], "warning");
    assert_eq!(warning["pending_intent_count"], 1);
    assert_eq!(warning["unresolved_intent_count"], 0);
    assert!(warning.get("key_hash").is_none());

    let large = rows.iter().find(|row| row["name"] == "large").unwrap();
    assert_eq!(
        large["budget_limit_decimal"],
        "9500000000000000.000000000000"
    );
    assert_eq!(large["budget_limit"], Value::Null);
    assert_eq!(large["budget_used"], Value::Null);
}

#[tokio::test]
async fn create_and_patch_reject_all_server_owned_fields() {
    let key = virtual_key("existing");
    let app = test_app(ClusterRole::Traditional, "postgres", vec![key]);
    let fields = [
        ("budget_used", json!(1)),
        ("budget_used_decimal", json!("1.000000000000")),
        ("budget_pending_count", json!(1)),
        ("budget_unresolved_count", json!(1)),
        ("pending_intent_count", json!(1)),
        ("unresolved_intent_count", json!(1)),
        ("budget_accounting_revision", json!(1)),
        ("budget_checkpoint_tail_events", json!(1)),
        ("budget_state_updated_at", json!(1)),
        ("budget_accounting_state", json!("clean")),
        ("key_hash", json!("hash")),
        ("key_prefix", json!("prefix")),
        ("ws_id", json!(Uuid::new_v4())),
    ];

    for (field, value) in fields {
        let mut create_body = serde_json::Map::new();
        create_body.insert("name".to_string(), json!(format!("create-{field}")));
        create_body.insert(field.to_string(), value.clone());
        let (status, body) = request_json(
            app.clone(),
            Method::POST,
            "/ai-virtual-keys",
            Value::Object(create_body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "POST field={field}");
        assert_eq!(body["fields"][field], "field is read-only");

        let mut patch_body = serde_json::Map::new();
        patch_body.insert(field.to_string(), value);
        let (status, body) = request_json(
            app.clone(),
            Method::PATCH,
            "/ai-virtual-keys/existing",
            Value::Object(patch_body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "PATCH field={field}");
        assert_eq!(body["fields"][field], "field is read-only");
    }
}

#[tokio::test]
async fn virtual_key_limits_require_exact_types_ranges_and_precision() {
    let app = test_app(ClusterRole::Traditional, "postgres", Vec::new());
    let invalid_quotas = [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("1"),
        json!(2_147_483_648_u64),
    ];
    for (index, value) in invalid_quotas.into_iter().enumerate() {
        let (status, body) = request_json(
            app.clone(),
            Method::POST,
            "/ai-virtual-keys",
            json!({"name": format!("quota-{index}"), "rpm_limit": value}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["fields"]["rpm_limit"].is_string());
    }

    let invalid_budgets = [
        json!({"budget_limit_decimal": 1}),
        json!({"budget_limit": "1.0"}),
        json!({
            "budget_limit_decimal": "1.000000000000",
            "budget_limit": 2
        }),
        json!({"budget_limit_decimal": "0.1234567890123"}),
        json!({"budget_limit_decimal": "10000000000000000"}),
    ];
    for (index, fields) in invalid_budgets.into_iter().enumerate() {
        let mut body = fields.as_object().unwrap().clone();
        body.insert("name".to_string(), json!(format!("budget-{index}")));
        let (status, response) = request_json(
            app.clone(),
            Method::POST,
            "/ai-virtual-keys",
            Value::Object(body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["fields"].is_object());
    }

    let (status, created) = request_json(
        app,
        Method::POST,
        "/ai-virtual-keys",
        json!({
            "name": "matching-dual-fields",
            "budget_limit_decimal": "12.500000000000",
            "budget_limit": 12.5,
            "rpm_limit": null,
            "tpm_limit": 1
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["budget_limit_decimal"], "12.500000000000");
}

#[tokio::test]
async fn deployment_modes_project_explicit_capabilities() {
    let mut key = virtual_key("mode-key");
    key.rpm_limit = Some(10);
    key.budget_limit = Some(parse_budget_amount("5").unwrap());
    key.budget_unresolved_count = 1;

    let postgres = test_app(ClusterRole::Traditional, "postgres", vec![key.clone()]);
    let (_, body) = get_json(postgres, "/ai-virtual-keys/mode-key").await;
    assert_eq!(body["budget_status"], "unresolved");

    let db_less = test_app(ClusterRole::Traditional, "off", vec![key.clone()]);
    let (_, body) = get_json(db_less, "/ai-virtual-keys/mode-key").await;
    assert_eq!(body["capability"]["quota"], "local_memory_ephemeral");
    assert_eq!(body["capability"]["budget"], "unsupported");
    assert_eq!(body["quota_enforcement"], "awaiting_plugin");
    assert_eq!(body["budget_status"], "unsupported");
    assert_eq!(body["budget_backend"], Value::Null);

    let mut paused_with_pending = virtual_key("paused-with-pending");
    paused_with_pending.budget_used = parse_budget_amount("1").unwrap();
    paused_with_pending.budget_pending_count = 1;
    let postgres = test_app(
        ClusterRole::Traditional,
        "postgres",
        vec![paused_with_pending],
    );
    let (_, body) = get_json(postgres, "/ai-virtual-keys/paused-with-pending").await;
    assert_eq!(body["budget_financial_status"], "unresolved");
    assert_eq!(body["budget_status"], "unresolved");

    let hybrid = test_app(ClusterRole::ControlPlane, "postgres", vec![key]);
    let (_, body) = get_json(hybrid, "/ai-virtual-keys/mode-key").await;
    assert_eq!(body["capability"]["quota"], "unsupported");
    assert_eq!(body["capability"]["budget"], "unsupported");
    assert_eq!(body["quota_enforcement"], "unsupported");
    assert_eq!(body["budget_status"], "unsupported");
}

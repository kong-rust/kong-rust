//! ai-key-auth integration tests — virtual key authentication end to end
//! ai-key-auth 集成测试 — 虚拟密钥认证端到端

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use kong_ai::auth::{AiAuthContext, VirtualKeyAuthenticator};
use kong_ai::models::AiVirtualKey;
use kong_ai::plugins::AiKeyAuthPlugin;
use kong_core::error::{KongError, Result};
use kong_core::traits::{
    Dao, Entity, Page, PageParams, PluginConfig, PluginHandler, PrimaryKey, RequestCtx,
};
use serde_json::json;
use uuid::Uuid;

// ============ Test fixtures — 测试夹具 ============

/// In-memory virtual key DAO — mirrors PgDao/DblessDao filter semantics
/// 内存虚拟密钥 DAO — 复刻 PgDao/DblessDao 的过滤语义
struct MockKeyDao {
    rows: Arc<RwLock<Vec<AiVirtualKey>>>,
    queries: Arc<AtomicUsize>,
    /// When true, `page` ignores filters — simulates a DAO that silently drops them
    /// 为 true 时 `page` 忽略过滤条件 — 模拟静默丢弃过滤条件的 DAO
    ignore_filters: bool,
}

impl MockKeyDao {
    fn new(rows: Vec<AiVirtualKey>) -> Self {
        Self {
            rows: Arc::new(RwLock::new(rows)),
            queries: Arc::new(AtomicUsize::new(0)),
            ignore_filters: false,
        }
    }

    fn ignoring_filters(rows: Vec<AiVirtualKey>) -> Self {
        Self {
            rows: Arc::new(RwLock::new(rows)),
            queries: Arc::new(AtomicUsize::new(0)),
            ignore_filters: true,
        }
    }
}

#[async_trait]
impl Dao<AiVirtualKey> for MockKeyDao {
    async fn insert(&self, _entity: &AiVirtualKey) -> Result<AiVirtualKey> {
        Err(unsupported())
    }

    async fn select(&self, pk: &PrimaryKey) -> Result<Option<AiVirtualKey>> {
        let rows = self.rows.read().unwrap();
        Ok(rows
            .iter()
            .find(|row| match pk {
                PrimaryKey::Id(id) => row.id() == *id,
                PrimaryKey::EndpointKey(key) => row.endpoint_key_value().as_deref() == Some(key),
            })
            .cloned())
    }

    async fn page(&self, params: &PageParams) -> Result<Page<AiVirtualKey>> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        let rows = self.rows.read().unwrap();
        let data = if self.ignore_filters {
            rows.clone()
        } else {
            rows.iter()
                .filter(|row| {
                    params
                        .filters
                        .iter()
                        .all(|(field, value)| match field.as_str() {
                            "key_hash" => &row.key_hash == value,
                            "name" => &row.name == value,
                            _ => true,
                        })
                })
                .cloned()
                .collect()
        };
        Ok(Page {
            data,
            offset: None,
            next: None,
        })
    }

    async fn update(&self, _pk: &PrimaryKey, _entity: &serde_json::Value) -> Result<AiVirtualKey> {
        Err(unsupported())
    }

    async fn upsert(&self, _pk: &PrimaryKey, _entity: &AiVirtualKey) -> Result<AiVirtualKey> {
        Err(unsupported())
    }

    async fn delete(&self, _pk: &PrimaryKey) -> Result<()> {
        Err(unsupported())
    }

    async fn select_by_foreign_key(
        &self,
        _foreign_key_field: &str,
        _foreign_key_value: &Uuid,
        _params: &PageParams,
    ) -> Result<Page<AiVirtualKey>> {
        Err(unsupported())
    }
}

fn unsupported() -> KongError {
    KongError::DatabaseError("unsupported in test dao".to_string())
}

/// Build a key whose hash matches `raw_key` — 构造哈希匹配 `raw_key` 的密钥
fn make_key(name: &str, raw_key: &str) -> AiVirtualKey {
    AiVirtualKey {
        id: Uuid::new_v4(),
        name: name.to_string(),
        key_hash: VirtualKeyAuthenticator::hash_key(raw_key),
        key_prefix: raw_key.chars().take(8).collect(),
        enabled: true,
        ..Default::default()
    }
}

fn plugin_config(config: serde_json::Value) -> PluginConfig {
    PluginConfig {
        name: "ai-key-auth".to_string(),
        config,
    }
}

/// A chat request carrying `model` — 携带 `model` 的对话请求
fn chat_body(model: &str) -> String {
    json!({
        "model": model,
        "messages": [{"role": "user", "content": "hi"}],
    })
    .to_string()
}

fn ctx_with(headers: &[(&str, &str)], body: Option<String>) -> RequestCtx {
    let mut ctx = RequestCtx::new();
    for (name, value) in headers {
        // The proxy lowercases header names before plugins run — 代理层在插件执行前已将头名转小写
        ctx.request_headers
            .insert(name.to_lowercase(), value.to_string());
    }
    ctx.request_path = "/ai/demo/v1/chat/completions".to_string();
    ctx.request_body = body;
    ctx
}

fn build_plugin(dao: MockKeyDao) -> (AiKeyAuthPlugin, Arc<VirtualKeyAuthenticator>) {
    let authenticator = Arc::new(VirtualKeyAuthenticator::new(Arc::new(dao)));
    (
        AiKeyAuthPlugin::new(Arc::clone(&authenticator)),
        authenticator,
    )
}

fn error_body(ctx: &RequestCtx) -> serde_json::Value {
    serde_json::from_str(ctx.exit_body.as_ref().expect("exit body")).expect("valid json")
}

// ============ Rejection paths — 拒绝路径 ============

#[tokio::test]
async fn missing_credential_is_rejected() {
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![make_key("team-a", "sk-kr-valid")]));
    let mut ctx = ctx_with(&[], Some(chat_body("gpt-4o")));

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert!(ctx.short_circuited);
    assert_eq!(ctx.exit_status, Some(401));
    assert_eq!(error_body(&ctx)["error"]["code"], "missing_api_key");
    assert_eq!(
        ctx.exit_headers.as_ref().unwrap().get("Content-Type"),
        Some(&"application/json".to_string())
    );
}

#[tokio::test]
async fn unknown_credential_is_rejected() {
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![make_key("team-a", "sk-kr-valid")]));
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-nope")],
        Some(chat_body("gpt-4o")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert_eq!(ctx.exit_status, Some(401));
    assert_eq!(error_body(&ctx)["error"]["code"], "invalid_api_key");
}

#[tokio::test]
async fn disabled_key_is_rejected() {
    let mut key = make_key("team-a", "sk-kr-valid");
    key.enabled = false;
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![key]));
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(chat_body("gpt-4o")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert_eq!(ctx.exit_status, Some(401));
    // Must be indistinguishable from an unknown key — 必须与「密钥不存在」无法区分
    assert_eq!(error_body(&ctx)["error"]["code"], "invalid_api_key");
}

#[tokio::test]
async fn expired_key_is_rejected() {
    let mut key = make_key("team-a", "sk-kr-valid");
    key.expires_at = Some(chrono::Utc::now().timestamp() - 60);
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![key]));
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(chat_body("gpt-4o")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert_eq!(ctx.exit_status, Some(401));
    assert_eq!(error_body(&ctx)["error"]["code"], "invalid_api_key");
}

#[tokio::test]
async fn future_expiry_is_accepted() {
    let mut key = make_key("team-a", "sk-kr-valid");
    key.expires_at = Some(chrono::Utc::now().timestamp() + 3600);
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![key]));
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(chat_body("gpt-4o")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert!(!ctx.short_circuited);
}

// ============ Credential carriers — 凭证携带方式 ============

#[tokio::test]
async fn openai_sdk_bearer_header_authenticates() {
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![make_key("team-a", "sk-kr-valid")]));
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(chat_body("gpt-4o")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert!(!ctx.short_circuited, "bearer credential must authenticate");
}

#[tokio::test]
async fn anthropic_sdk_x_api_key_header_authenticates() {
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![make_key("team-a", "sk-kr-valid")]));
    let mut ctx = ctx_with(
        &[("x-api-key", "sk-kr-valid")],
        Some(chat_body("claude-sonnet-5")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert!(
        !ctx.short_circuited,
        "x-api-key credential must authenticate"
    );
}

#[tokio::test]
async fn configured_custom_header_authenticates() {
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![make_key("team-a", "sk-kr-valid")]));
    let mut ctx = ctx_with(&[("X-Team-Key", "sk-kr-valid")], Some(chat_body("gpt-4o")));

    plugin
        .access(
            &plugin_config(json!({"key_header": "X-Team-Key"})),
            &mut ctx,
        )
        .await
        .unwrap();

    assert!(!ctx.short_circuited);
}

// ============ Error dialects — 错误体风格 ============

#[tokio::test]
async fn anthropic_dialect_inferred_from_x_api_key() {
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![]));
    let mut ctx = ctx_with(
        &[("x-api-key", "sk-kr-nope")],
        Some(chat_body("claude-sonnet-5")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    let body = error_body(&ctx);
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "authentication_error");
    assert_eq!(body["error"]["message"], "invalid API key");
}

#[tokio::test]
async fn anthropic_dialect_inferred_from_messages_path() {
    // Missing-credential case carries no header signal — the path decides
    // 无凭证场景没有头部信号 — 由路径判定
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![]));
    let mut ctx = ctx_with(&[], Some(chat_body("claude-sonnet-5")));
    ctx.request_path = "/ai/demo/v1/messages".to_string();

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert_eq!(error_body(&ctx)["type"], "error");
}

#[tokio::test]
async fn explicit_dialect_overrides_inference() {
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![]));
    let mut ctx = ctx_with(&[("x-api-key", "sk-kr-nope")], Some(chat_body("gpt-4o")));

    plugin
        .access(&plugin_config(json!({"error_format": "openai"})), &mut ctx)
        .await
        .unwrap();

    assert_eq!(error_body(&ctx)["error"]["code"], "invalid_api_key");
}

// ============ Model allow list — 模型白名单 ============

#[tokio::test]
async fn model_outside_allow_list_is_forbidden() {
    let mut key = make_key("team-a", "sk-kr-valid");
    key.allowed_models = Some(vec!["gpt-4o".to_string()]);
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![key]));
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(chat_body("gpt-4-turbo")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert_eq!(ctx.exit_status, Some(403));
    let body = error_body(&ctx);
    assert_eq!(body["error"]["code"], "model_not_allowed");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("gpt-4-turbo"),
        "message must name the rejected model"
    );
}

#[tokio::test]
async fn exact_allow_list_entry_permits_model() {
    let mut key = make_key("team-a", "sk-kr-valid");
    key.allowed_models = Some(vec!["gpt-4o".to_string()]);
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![key]));
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(chat_body("gpt-4o")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert!(!ctx.short_circuited);
}

#[tokio::test]
async fn wildcard_allow_list_entry_permits_model_family() {
    let mut key = make_key("team-a", "sk-kr-valid");
    key.allowed_models = Some(vec!["gpt-4*".to_string()]);
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![key]));

    for model in ["gpt-4", "gpt-4o", "gpt-4-turbo"] {
        let mut ctx = ctx_with(
            &[("Authorization", "Bearer sk-kr-valid")],
            Some(chat_body(model)),
        );
        plugin
            .access(&plugin_config(json!({})), &mut ctx)
            .await
            .unwrap();
        assert!(!ctx.short_circuited, "{} must be allowed by gpt-4*", model);
    }

    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(chat_body("gpt-3.5-turbo")),
    );
    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();
    assert_eq!(ctx.exit_status, Some(403));
}

#[tokio::test]
async fn empty_allow_list_permits_any_model() {
    let mut key = make_key("team-a", "sk-kr-valid");
    // The column defaults to an empty array — 该列默认为空数组
    key.allowed_models = Some(vec![]);
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![key]));
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(chat_body("anything-at-all")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert!(!ctx.short_circuited);
}

#[tokio::test]
async fn request_without_model_field_skips_allow_list() {
    // Under model_source=config clients legitimately omit `model`
    // model_source=config 部署下客户端合法地不传 `model`
    let mut key = make_key("team-a", "sk-kr-valid");
    key.allowed_models = Some(vec!["gpt-4o".to_string()]);
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![key]));
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(json!({"messages": [{"role": "user", "content": "hi"}]}).to_string()),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert!(!ctx.short_circuited, "absent model must not be rejected");
}

// ============ Identity injection — 身份注入 ============

#[tokio::test]
async fn authenticated_request_injects_auth_context() {
    let key = make_key("team-a", "sk-kr-valid");
    let key_id = key.id;
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![key]));
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(chat_body("gpt-4o")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    let auth = ctx
        .extensions
        .get::<AiAuthContext>()
        .expect("AiAuthContext must be injected for downstream plugins");
    assert_eq!(auth.virtual_key_id, key_id);
    assert_eq!(auth.key_name, "team-a");
    assert_eq!(auth.consumer_id, None);

    let credential = ctx.authenticated_credential.as_ref().expect("credential");
    assert_eq!(credential["name"], "team-a");
}

#[tokio::test]
async fn bound_consumer_is_propagated_to_ctx() {
    let consumer_id = Uuid::new_v4();
    let mut key = make_key("team-a", "sk-kr-valid");
    key.consumer_id = Some(consumer_id);
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![key]));
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(chat_body("gpt-4o")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    // Enables ai-rate-limit limit_by=consumer — 激活 ai-rate-limit 的 limit_by=consumer
    assert_eq!(ctx.consumer_id, Some(consumer_id));
    assert_eq!(
        ctx.authenticated_consumer.as_ref().unwrap()["id"],
        json!(consumer_id)
    );
}

#[tokio::test]
async fn rejected_request_injects_no_identity() {
    let (plugin, _) = build_plugin(MockKeyDao::new(vec![]));
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-nope")],
        Some(chat_body("gpt-4o")),
    );

    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();

    assert!(ctx.extensions.get::<AiAuthContext>().is_none());
    assert!(ctx.consumer_id.is_none());
    assert!(ctx.authenticated_credential.is_none());
}

// ============ Cache and invalidation — 缓存与失效 ============

#[tokio::test]
async fn repeated_authentication_is_served_from_cache() {
    let dao = MockKeyDao::new(vec![make_key("team-a", "sk-kr-valid")]);
    let queries = Arc::clone(&dao.queries);
    let (plugin, _) = build_plugin(dao);

    for _ in 0..5 {
        let mut ctx = ctx_with(
            &[("Authorization", "Bearer sk-kr-valid")],
            Some(chat_body("gpt-4o")),
        );
        plugin
            .access(&plugin_config(json!({})), &mut ctx)
            .await
            .unwrap();
        assert!(!ctx.short_circuited);
    }

    assert_eq!(
        queries.load(Ordering::SeqCst),
        1,
        "cache hits must not reach the DAO"
    );
}

#[tokio::test]
async fn invalid_keys_are_negatively_cached() {
    let dao = MockKeyDao::new(vec![]);
    let queries = Arc::clone(&dao.queries);
    let (plugin, _) = build_plugin(dao);

    for _ in 0..5 {
        let mut ctx = ctx_with(
            &[("Authorization", "Bearer sk-kr-nope")],
            Some(chat_body("gpt-4o")),
        );
        plugin
            .access(&plugin_config(json!({})), &mut ctx)
            .await
            .unwrap();
        assert_eq!(ctx.exit_status, Some(401));
    }

    assert_eq!(
        queries.load(Ordering::SeqCst),
        1,
        "invalid keys must not hammer the DAO"
    );
}

#[tokio::test]
async fn disabling_a_key_takes_effect_after_invalidation() {
    let dao = MockKeyDao::new(vec![make_key("team-a", "sk-kr-valid")]);
    let rows = Arc::clone(&dao.rows);
    let (plugin, authenticator) = build_plugin(dao);

    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(chat_body("gpt-4o")),
    );
    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();
    assert!(!ctx.short_circuited, "key starts out valid");

    // Admin API disables the key, then invalidates the auth cache
    // Admin API 禁用密钥后失效认证缓存
    rows.write().unwrap()[0].enabled = false;
    authenticator.invalidate_all();

    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-valid")],
        Some(chat_body("gpt-4o")),
    );
    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();
    assert_eq!(ctx.exit_status, Some(401), "disabled key must be rejected");
}

#[tokio::test]
async fn rotated_key_replaces_the_previous_one() {
    let dao = MockKeyDao::new(vec![make_key("team-a", "sk-kr-old")]);
    let rows = Arc::clone(&dao.rows);
    let (plugin, authenticator) = build_plugin(dao);

    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-old")],
        Some(chat_body("gpt-4o")),
    );
    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();
    assert!(!ctx.short_circuited);

    // Rotate: the stored hash is replaced — 轮换：存储的哈希被替换
    rows.write().unwrap()[0].key_hash = VirtualKeyAuthenticator::hash_key("sk-kr-new");
    authenticator.invalidate_all();

    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-old")],
        Some(chat_body("gpt-4o")),
    );
    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();
    assert_eq!(
        ctx.exit_status,
        Some(401),
        "rotated-out key must stop working"
    );

    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-new")],
        Some(chat_body("gpt-4o")),
    );
    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();
    assert!(!ctx.short_circuited, "rotated-in key must work");
}

// ============ Defensive lookup — 防御性查找 ============

#[tokio::test]
async fn dao_dropping_the_filter_cannot_authenticate_a_foreign_key() {
    // A DAO that ignores filters returns unrelated rows; the hash must be re-checked
    // 忽略过滤条件的 DAO 会返回无关行；必须重新校验哈希
    let dao = MockKeyDao::ignoring_filters(vec![
        make_key("team-a", "sk-kr-aaa"),
        make_key("team-b", "sk-kr-bbb"),
    ]);
    let (plugin, _) = build_plugin(dao);

    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-unknown")],
        Some(chat_body("gpt-4o")),
    );
    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();
    assert_eq!(
        ctx.exit_status,
        Some(401),
        "an unknown key must never match an unrelated row"
    );

    // A genuine key still resolves to its own record — 真实密钥仍解析到自己的记录
    let mut ctx = ctx_with(
        &[("Authorization", "Bearer sk-kr-bbb")],
        Some(chat_body("gpt-4o")),
    );
    plugin
        .access(&plugin_config(json!({})), &mut ctx)
        .await
        .unwrap();
    assert!(!ctx.short_circuited);
    assert_eq!(
        ctx.extensions.get::<AiAuthContext>().unwrap().key_name,
        "team-b",
        "must resolve to the key that owns the hash"
    );
}

// ============ Plugin contract — 插件契约 ============

#[test]
fn plugin_runs_ahead_of_every_other_ai_plugin() {
    let dao = MockKeyDao::new(vec![]);
    let (plugin, _) = build_plugin(dao);
    assert_eq!(plugin.name(), "ai-key-auth");
    // ai-prompt-guard is 773; identity must resolve before any policy runs
    // ai-prompt-guard 为 773；身份必须在任何策略执行前解析
    assert!(plugin.priority() > 773);
}

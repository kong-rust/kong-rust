use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kong_ai::models::{AiModel, AiProviderConfig};
use kong_ai::plugins::ai_proxy::AiProxyPlugin;
use kong_ai::plugins::context::AiRequestState;
use kong_ai::provider::ModelGroupResolver;
use kong_core::error::{KongError, Result};
use kong_core::traits::{
    Dao, Entity, Page, PageParams, PluginConfig, PluginHandler, PrimaryKey, RequestCtx,
};
use serde_json::json;
use uuid::Uuid;

struct MockDao<T> {
    rows: Vec<T>,
}

impl<T> MockDao<T> {
    fn new(rows: Vec<T>) -> Self {
        Self { rows }
    }
}

#[async_trait]
impl<T> Dao<T> for MockDao<T>
where
    T: Entity + Clone + Send + Sync + 'static,
{
    async fn insert(&self, _entity: &T) -> Result<T> {
        Err(unsupported())
    }

    async fn select(&self, pk: &PrimaryKey) -> Result<Option<T>> {
        Ok(self
            .rows
            .iter()
            .find(|row| match pk {
                PrimaryKey::Id(id) => row.id() == *id,
                PrimaryKey::EndpointKey(key) => row.endpoint_key_value().as_deref() == Some(key),
            })
            .cloned())
    }

    async fn page(&self, _params: &PageParams) -> Result<Page<T>> {
        Ok(Page {
            data: self.rows.clone(),
            offset: None,
            next: None,
        })
    }

    async fn update(&self, _pk: &PrimaryKey, _entity: &serde_json::Value) -> Result<T> {
        Err(unsupported())
    }

    async fn upsert(&self, _pk: &PrimaryKey, _entity: &T) -> Result<T> {
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
    ) -> Result<Page<T>> {
        Err(unsupported())
    }
}

fn unsupported() -> KongError {
    KongError::InternalError("unsupported mock DAO operation".to_string())
}

fn provider(id: Uuid, provider_type: &str, enabled: bool) -> AiProviderConfig {
    AiProviderConfig {
        id,
        name: format!("{provider_type}-provider"),
        provider_type: provider_type.to_string(),
        endpoint_url: Some("https://models.example.test/v1".to_string()),
        auth_config: json!({"header_value": "Bearer test-token"}),
        enabled,
        ..Default::default()
    }
}

fn model(id: Uuid, provider_id: Uuid, group: &str, actual_name: &str, priority: i32) -> AiModel {
    AiModel {
        id,
        provider_id,
        name: group.to_string(),
        model_name: actual_name.to_string(),
        priority,
        weight: 1,
        enabled: true,
        ..Default::default()
    }
}

fn resolver(models: Vec<AiModel>, providers: Vec<AiProviderConfig>) -> Arc<ModelGroupResolver> {
    resolver_with_refresh(models, providers, Duration::from_secs(2))
}

fn resolver_with_refresh(
    models: Vec<AiModel>,
    providers: Vec<AiProviderConfig>,
    refresh_interval: Duration,
) -> Arc<ModelGroupResolver> {
    Arc::new(ModelGroupResolver::with_refresh_interval(
        Arc::new(MockDao::new(models)),
        Arc::new(MockDao::new(providers)),
        refresh_interval,
    ))
}

fn request_context() -> RequestCtx {
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(
        json!({
            "model": "request-model",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string(),
    );
    ctx
}

#[tokio::test]
async fn ai_proxy_falls_back_to_database_model_group() {
    let provider_id = Uuid::new_v4();
    let plugin = AiProxyPlugin::with_model_resolver(resolver(
        vec![model(
            Uuid::new_v4(),
            provider_id,
            "support-group",
            "provider-model-v2",
            10,
        )],
        vec![provider(provider_id, "openai", true)],
    ));
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({"model": "support-group"}),
    };
    let mut ctx = request_context();

    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.model.model_name, "provider-model-v2");
    assert_eq!(state.provider_config.provider_type, "openai");
    assert_eq!(
        state.provider_config.endpoint_url.as_deref(),
        Some("https://models.example.test/v1")
    );
    assert_eq!(
        state.provider_config.auth_config["header_value"],
        "Bearer test-token"
    );
    let upstream_body: serde_json::Value =
        serde_json::from_str(ctx.upstream_body.as_deref().unwrap()).unwrap();
    assert_eq!(upstream_body["model"], "provider-model-v2");
}

#[tokio::test]
async fn responses_pass_through_uses_database_model_and_preserves_fields() {
    let provider_id = Uuid::new_v4();
    let plugin = AiProxyPlugin::with_model_resolver(resolver(
        vec![model(
            Uuid::new_v4(),
            provider_id,
            "responses-group",
            "provider-responses-model",
            10,
        )],
        vec![provider(provider_id, "openai", true)],
    ));
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "responses-group",
            "model_source": "config",
            "route_type": "llm/v1/responses"
        }),
    };
    let original_body = json!({
        "model": "client-model",
        "input": [{
            "role": "user",
            "content": [{"type": "input_text", "text": "hello"}]
        }],
        "previous_response_id": "resp_previous",
        "metadata": {"request_id": "preserve-me"},
        "store": false
    });
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(original_body.to_string());

    plugin.access(&config, &mut ctx).await.unwrap();

    assert_eq!(ctx.upstream_path.as_deref(), Some("/v1/responses"));
    let upstream_body: serde_json::Value =
        serde_json::from_str(ctx.upstream_body.as_deref().unwrap()).unwrap();
    assert_eq!(upstream_body["model"], "provider-responses-model");
    for field in ["input", "previous_response_id", "metadata", "store"] {
        assert_eq!(upstream_body[field], original_body[field]);
    }
}

#[tokio::test]
async fn responses_pass_through_resolves_request_model_group() {
    let provider_id = Uuid::new_v4();
    let plugin = AiProxyPlugin::with_model_resolver(resolver(
        vec![model(
            Uuid::new_v4(),
            provider_id,
            "request-group",
            "provider-request-model",
            10,
        )],
        vec![provider(provider_id, "openai", true)],
    ));
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model_source": "request",
            "route_type": "llm/v1/responses"
        }),
    };
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(
        json!({
            "model": "request-group",
            "input": "hello",
            "stream": true
        })
        .to_string(),
    );

    plugin.access(&config, &mut ctx).await.unwrap();

    let upstream_body: serde_json::Value =
        serde_json::from_str(ctx.upstream_body.as_deref().unwrap()).unwrap();
    assert_eq!(upstream_body["model"], "provider-request-model");
    assert_eq!(upstream_body["input"], "hello");
    assert_eq!(upstream_body["stream"], true);
}

#[tokio::test]
async fn resolver_filters_disabled_and_workspace_scoped_providers() {
    let disabled_id = Uuid::new_v4();
    let workspace_id = Uuid::new_v4();
    let enabled_id = Uuid::new_v4();
    let mut workspace_model = model(
        Uuid::new_v4(),
        workspace_id,
        "fallback-group",
        "other-workspace-model",
        200,
    );
    workspace_model.ws_id = Some(Uuid::new_v4());
    let mut workspace_provider = provider(workspace_id, "anthropic", true);
    workspace_provider.ws_id = workspace_model.ws_id;
    let resolver = resolver(
        vec![
            workspace_model,
            model(
                Uuid::new_v4(),
                disabled_id,
                "fallback-group",
                "disabled-model",
                100,
            ),
            model(
                Uuid::new_v4(),
                enabled_id,
                "fallback-group",
                "enabled-model",
                10,
            ),
        ],
        vec![
            workspace_provider,
            provider(disabled_id, "anthropic", false),
            provider(enabled_id, "openai", true),
        ],
    );

    let (selected_model, selected_provider) = resolver.resolve("fallback-group").await.unwrap();

    assert_eq!(selected_model.model_name, "enabled-model");
    assert_eq!(selected_provider.provider_type, "openai");
}

#[tokio::test]
async fn inline_provider_takes_precedence_over_database_group() {
    let provider_id = Uuid::new_v4();
    let plugin = AiProxyPlugin::with_model_resolver(resolver(
        vec![model(
            Uuid::new_v4(),
            provider_id,
            "shared-name",
            "database-model",
            100,
        )],
        vec![provider(provider_id, "anthropic", true)],
    ));
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "shared-name",
            "provider": {
                "provider_type": "openai",
                "auth_config": {"header_value": "Bearer inline-test-token"}
            }
        }),
    };
    let mut ctx = request_context();

    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.provider_config.provider_type, "openai");
    assert_eq!(state.model.model_name, "shared-name");
    assert_eq!(
        state.provider_config.auth_config["header_value"],
        "Bearer inline-test-token"
    );
}

#[tokio::test]
async fn explicit_model_group_takes_precedence_over_inline_provider() {
    let provider_id = Uuid::new_v4();
    let plugin = AiProxyPlugin::with_model_resolver(resolver(
        vec![model(
            Uuid::new_v4(),
            provider_id,
            "shared-name",
            "database-model",
            100,
        )],
        vec![provider(provider_id, "anthropic", true)],
    ));
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model_group": "shared-name",
            "provider": {
                "provider_type": "openai",
                "auth_config": {"header_value": "Bearer inline-test-token"}
            }
        }),
    };
    let mut ctx = request_context();

    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.provider_config.provider_type, "anthropic");
    assert_eq!(state.model.model_name, "database-model");
    assert_eq!(
        state.provider_config.auth_config["header_value"],
        "Bearer test-token"
    );
}

#[tokio::test]
async fn unchanged_group_preserves_weighted_selection_across_refreshes() {
    let provider_a = Uuid::new_v4();
    let provider_b = Uuid::new_v4();
    let mut model_a = model(
        Uuid::from_u128(1),
        provider_a,
        "weighted-group",
        "model-a",
        10,
    );
    model_a.weight = 1;
    let mut model_b = model(
        Uuid::from_u128(2),
        provider_b,
        "weighted-group",
        "model-b",
        10,
    );
    model_b.weight = 2;
    let resolver = resolver_with_refresh(
        vec![model_a, model_b],
        vec![
            provider(provider_a, "openai", true),
            provider(provider_b, "openai", true),
        ],
        Duration::ZERO,
    );

    let mut selected = Vec::new();
    for _ in 0..3 {
        selected.push(
            resolver
                .resolve("weighted-group")
                .await
                .unwrap()
                .0
                .model_name,
        );
    }

    assert_eq!(selected, ["model-a", "model-b", "model-b"]);
}

#[tokio::test]
async fn ai_proxy_falls_back_when_high_priority_model_cannot_fit_prompt() {
    let small_provider = Uuid::new_v4();
    let large_provider = Uuid::new_v4();
    let mut small = model(
        Uuid::new_v4(),
        small_provider,
        "sized-group",
        "small-context-model",
        100,
    );
    small.max_input_tokens = Some(1);
    let mut large = model(
        Uuid::new_v4(),
        large_provider,
        "sized-group",
        "large-context-model",
        10,
    );
    large.max_input_tokens = Some(10_000);
    let plugin = AiProxyPlugin::with_model_resolver(resolver(
        vec![small, large],
        vec![
            provider(small_provider, "openai", true),
            provider(large_provider, "openai", true),
        ],
    ));
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({"model": "sized-group"}),
    };
    let mut ctx = request_context();

    plugin.access(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.model.model_name, "large-context-model");
    let upstream_body: serde_json::Value =
        serde_json::from_str(ctx.upstream_body.as_deref().unwrap()).unwrap();
    assert_eq!(upstream_body["model"], "large-context-model");
}

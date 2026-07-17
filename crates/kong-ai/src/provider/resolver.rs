//! Database-backed model-group resolution for ai-proxy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kong_core::error::{KongError, Result};
use kong_core::traits::{Dao, PageParams, PrimaryKey};
use tokio::sync::Mutex as AsyncMutex;

use crate::models::{AiModel, AiProviderConfig};

use super::ModelGroupBalancer;

const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

struct CachedGroup {
    loaded_at: Instant,
    signature: Vec<u8>,
    balancer: Arc<ModelGroupBalancer>,
}

/// Resolves model-group names from the AI model/provider DAOs.
///
/// Groups are cached briefly so the balancer can retain its weighted selection
/// counter while Admin API edits still become visible promptly.
pub struct ModelGroupResolver {
    models: Arc<dyn Dao<AiModel>>,
    providers: Arc<dyn Dao<AiProviderConfig>>,
    refresh_interval: Duration,
    cache: Mutex<HashMap<String, CachedGroup>>,
    refresh_lock: AsyncMutex<()>,
}

impl ModelGroupResolver {
    pub fn new(models: Arc<dyn Dao<AiModel>>, providers: Arc<dyn Dao<AiProviderConfig>>) -> Self {
        Self::with_refresh_interval(models, providers, DEFAULT_REFRESH_INTERVAL)
    }

    pub fn with_refresh_interval(
        models: Arc<dyn Dao<AiModel>>,
        providers: Arc<dyn Dao<AiProviderConfig>>,
        refresh_interval: Duration,
    ) -> Self {
        Self {
            models,
            providers,
            refresh_interval,
            cache: Mutex::new(HashMap::new()),
            refresh_lock: AsyncMutex::new(()),
        }
    }

    /// Resolve one enabled model/provider pair from a named model group.
    pub async fn resolve(&self, group_name: &str) -> Result<(AiModel, AiProviderConfig)> {
        self.resolve_for(group_name, None).await
    }

    /// Resolve one enabled model/provider pair that can accommodate the prompt.
    pub async fn resolve_for(
        &self,
        group_name: &str,
        prompt_tokens: Option<u64>,
    ) -> Result<(AiModel, AiProviderConfig)> {
        let balancer = if let Some(cached) = self.cached_group(group_name) {
            cached
        } else {
            let _refresh_guard = self.refresh_lock.lock().await;

            if let Some(cached) = self.cached_group(group_name) {
                cached
            } else {
                self.load_group(group_name).await?
            }
        };

        let (model, provider) = balancer
            .select_for(prompt_tokens)
            .map_err(|_| group_not_found(group_name))?;
        Ok((model.clone(), provider.clone()))
    }

    fn cached_group(&self, group_name: &str) -> Option<Arc<ModelGroupBalancer>> {
        let cache = self.cache.lock().unwrap();
        cache.get(group_name).and_then(|entry| {
            (entry.loaded_at.elapsed() < self.refresh_interval).then(|| Arc::clone(&entry.balancer))
        })
    }

    async fn load_group(&self, group_name: &str) -> Result<Arc<ModelGroupBalancer>> {
        let mut models = Vec::new();
        let mut offset = None;

        loop {
            let page = self
                .models
                .page(&PageParams {
                    size: 1000,
                    offset: offset.clone(),
                    filters: vec![("name".to_string(), group_name.to_string())],
                    ..Default::default()
                })
                .await?;

            models.extend(page.data.into_iter().filter(|model| {
                model.enabled && model.ws_id.is_none() && model.name == group_name
            }));

            match page.offset {
                Some(next) => offset = Some(next),
                None => break,
            }
        }

        let mut pairs = Vec::with_capacity(models.len());
        let mut provider_cache: HashMap<uuid::Uuid, Option<AiProviderConfig>> = HashMap::new();
        for model in models {
            let provider = if let Some(cached) = provider_cache.get(&model.provider_id) {
                cached.clone()
            } else {
                let loaded = self
                    .providers
                    .select(&PrimaryKey::Id(model.provider_id))
                    .await?;
                provider_cache.insert(model.provider_id, loaded.clone());
                loaded
            };
            if let Some(provider) =
                provider.filter(|provider| provider.enabled && provider.ws_id.is_none())
            {
                pairs.push((model, provider));
            }
        }

        pairs.sort_by_key(|(model, _)| model.id);
        if pairs.is_empty() {
            self.cache.lock().unwrap().remove(group_name);
            return Err(group_not_found(group_name));
        }

        let signature = group_signature(&pairs)?;

        let mut cache = self.cache.lock().unwrap();
        if let Some(cached) = cache.get_mut(group_name) {
            if cached.signature == signature {
                cached.loaded_at = Instant::now();
                return Ok(Arc::clone(&cached.balancer));
            }
        }

        let balancer = Arc::new(ModelGroupBalancer::new(pairs));
        cache.insert(
            group_name.to_string(),
            CachedGroup {
                loaded_at: Instant::now(),
                signature,
                balancer: Arc::clone(&balancer),
            },
        );
        Ok(balancer)
    }
}

fn group_not_found(group_name: &str) -> KongError {
    KongError::PluginError {
        plugin_name: "ai-proxy".to_string(),
        message: format!(
            "no enabled AI models/providers found for model group '{}'",
            group_name
        ),
    }
}

fn group_signature(pairs: &[(AiModel, AiProviderConfig)]) -> Result<Vec<u8>> {
    let normalized: Vec<_> = pairs
        .iter()
        .map(|(model, provider)| {
            let mut model = model.clone();
            model.created_at = None;
            model.updated_at = None;
            model.tags = None;

            let mut provider = provider.clone();
            provider.created_at = None;
            provider.updated_at = None;
            provider.tags = None;
            (model, provider)
        })
        .collect();

    serde_json::to_vec(&normalized).map_err(|error| {
        KongError::InternalError(format!("failed to fingerprint AI model group: {error}"))
    })
}

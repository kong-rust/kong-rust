//! ai-semantic-cache 插件 — LLM 响应的语义缓存
//! Semantic cache for LLM responses based on prompt embedding similarity.
//!
//! 工作流程 / Workflow:
//! 1. access:从请求体提取 cache key text → 调 embedding 服务拿向量
//!    → 在 vector store 里找最相似条目 → 命中(cosine ≥ threshold)直接 short-circuit
//! 2. body_filter:miss 时收集响应,end_of_stream 时把 (vector, response) 写回 store
//!
//! MVP InMemory only;Redis 后端留 trait + TODO(#19B 二期)。

use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

use kong_core::error::Result;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

use crate::embedding::{
    EmbeddingClientArc, InMemoryVectorStore, OpenAiEmbeddingClient, VectorStore,
};

// ============ 插件配置 ============

/// ai-semantic-cache 插件配置
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiSemanticCacheConfig {
    /// embedding provider — MVP 仅支持 "openai_compat"(覆盖 OpenAI/Azure/vLLM/Ollama)
    pub embedding_provider: String,
    /// embedding 服务 endpoint URL(默认 https://api.openai.com)
    pub embedding_endpoint_url: Option<String>,
    /// embedding 服务 API key
    pub embedding_api_key: Option<String>,
    /// 自定义 auth header 名(用于 Azure 的 api-key 等非 Bearer 认证)
    pub embedding_auth_header_name: Option<String>,
    pub embedding_auth_header_value: Option<String>,
    /// embedding model 名(如 text-embedding-3-small / bge-large-zh)
    pub embedding_model: String,
    /// embedding HTTP 单次 timeout(毫秒)
    pub embedding_timeout_ms: u64,
    /// 余弦相似度命中阈值(0.0-1.0)
    pub similarity_threshold: f32,
    /// 缓存 TTL(秒)
    pub cache_ttl_seconds: u64,
    /// 最大缓存条目数(InMemory backend)
    pub max_cache_entries: usize,
    /// 缓存键提取策略 — "LastMessage" | "AllMessages" | "FirstUserMessage"
    pub cache_key_strategy: String,
    /// 向量后端 — "InMemory"(MVP)| "Redis"(TODO,fallback InMemory + warn)
    pub vector_store: String,
    /// 跳过缓存的请求头名称(客户端可主动绕过)
    pub skip_header: String,
}

impl Default for AiSemanticCacheConfig {
    fn default() -> Self {
        Self {
            embedding_provider: "openai_compat".to_string(),
            embedding_endpoint_url: None,
            embedding_api_key: None,
            embedding_auth_header_name: None,
            embedding_auth_header_value: None,
            embedding_model: "text-embedding-3-small".to_string(),
            embedding_timeout_ms: 200,
            similarity_threshold: 0.92,
            cache_ttl_seconds: 3600,
            max_cache_entries: 10_000,
            cache_key_strategy: "AllMessages".to_string(),
            vector_store: "InMemory".to_string(),
            skip_header: "X-AI-Skip-Cache".to_string(),
        }
    }
}

// ============ 插件状态(跨请求共享)============

/// 一个 (config 哈希) 对应的语义缓存实例 — embedding client + vector store.
pub struct SemanticCacheCore {
    pub client: EmbeddingClientArc,
    pub store: Arc<dyn VectorStore>,
    pub threshold: f32,
    pub ttl: Duration,
}

/// 跨请求传递的命中信息(供 body_filter 阶段写回缓存)
pub struct AiSemanticCacheContext {
    /// query 向量(miss 时用于写回 store)— query embedding (used to populate cache on miss)
    pub query_vector: Option<Vec<f32>>,
    /// 缓存命中时的相似度 — similarity score on hit
    pub cache_hit: bool,
    /// 关联的 SemanticCacheCore(共享所有权,body_filter 写回时复用)
    core: Option<Arc<SemanticCacheCore>>,
}

// ============ 插件结构体 ============

/// AI 语义缓存插件
pub struct AiSemanticCachePlugin {
    /// 共享 reqwest client(连接池复用)
    http: reqwest::Client,
    /// per-config-hash 的 SemanticCacheCore 缓存 — 第一次见到某 config 时构建
    cores: DashMap<u64, Arc<SemanticCacheCore>>,
}

impl AiSemanticCachePlugin {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            cores: DashMap::new(),
        }
    }

    /// 注入预构建的 SemanticCacheCore — 测试用,让单测绕过 reqwest HTTP 调用
    /// Inject a pre-built core (used by tests to bypass real HTTP embedding calls).
    pub fn install_core_for_config(&self, cfg: &AiSemanticCacheConfig, core: Arc<SemanticCacheCore>) {
        self.cores.insert(config_hash(cfg), core);
    }

    /// 获取或构造此 config 对应的 SemanticCacheCore
    /// Get-or-build the SemanticCacheCore for this config (hash-keyed, so config changes get new cores).
    fn core_for(&self, cfg: &AiSemanticCacheConfig) -> Arc<SemanticCacheCore> {
        let key = config_hash(cfg);
        if let Some(existing) = self.cores.get(&key) {
            return existing.clone();
        }
        let core = self.build_core(cfg);
        self.cores.insert(key, core.clone());
        core
    }

    fn build_core(&self, cfg: &AiSemanticCacheConfig) -> Arc<SemanticCacheCore> {
        // 目前只支持 OpenAI 兼容的 embedding endpoint;其他 provider 留作扩展点
        // Only OpenAI-compatible embedding endpoint is supported now; extensible point for other providers.
        let mut client = OpenAiEmbeddingClient::new(
            self.http.clone(),
            cfg.embedding_endpoint_url.clone(),
            cfg.embedding_api_key.clone(),
            cfg.embedding_model.clone(),
            Duration::from_millis(cfg.embedding_timeout_ms),
        );
        if let (Some(name), Some(value)) = (
            cfg.embedding_auth_header_name.clone(),
            cfg.embedding_auth_header_value.clone(),
        ) {
            client = client.with_auth_header(name, value);
        }
        let client_arc: EmbeddingClientArc = Arc::new(client);

        // Redis 后端尚未实现 — 任何非 InMemory 值都退化到 InMemory + warn
        // Redis backend is TODO(#19B-stage-2); any non-InMemory value falls back to InMemory with a warning.
        let store: Arc<dyn VectorStore> = match cfg.vector_store.as_str() {
            "InMemory" => Arc::new(InMemoryVectorStore::new(cfg.max_cache_entries)),
            other => {
                warn!(
                    "ai-semantic-cache: vector_store='{}' not yet implemented, falling back to InMemory (TODO #19B Redis backend)",
                    other
                );
                Arc::new(InMemoryVectorStore::new(cfg.max_cache_entries))
            }
        };

        Arc::new(SemanticCacheCore {
            client: client_arc,
            store,
            threshold: cfg.similarity_threshold,
            ttl: Duration::from_secs(cfg.cache_ttl_seconds),
        })
    }
}

impl Default for AiSemanticCachePlugin {
    fn default() -> Self {
        Self::new()
    }
}

// ============ PluginHandler 实现 ============

#[async_trait]
impl PluginHandler for AiSemanticCachePlugin {
    fn name(&self) -> &str {
        "ai-semantic-cache"
    }

    fn priority(&self) -> i32 {
        // 高于 ai-cache (772) — 先做语义匹配,字符串精确缓存作为补充
        // Higher than ai-cache (772): semantic match runs first; string-exact cache is a fallback.
        773
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn has_body_filter(&self) -> bool {
        true
    }

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let cfg: AiSemanticCacheConfig = crate::parse_plugin_config(config)?;

        // 1. 客户端主动跳过缓存 — client opt-out via skip header
        if ctx
            .request_headers
            .get(&cfg.skip_header.to_lowercase())
            .is_some()
        {
            ctx.extensions.insert(AiSemanticCacheContext {
                query_vector: None,
                cache_hit: false,
                core: None,
            });
            return Ok(());
        }

        // 2. 提取缓存键文本 — extract cache-key text from request body
        let body = match &ctx.request_body {
            Some(b) => b,
            None => return Ok(()),
        };
        let key_text = match extract_cache_text(body, &cfg.cache_key_strategy) {
            Some(t) if !t.is_empty() => t,
            _ => return Ok(()),
        };

        // 3. embedding(失败 → 跳过 cache,继续走代理)— embedding failure short-circuits cache, not request
        let core = self.core_for(&cfg);
        let vector = match core.client.embed(&key_text).await {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "ai-semantic-cache: embedding failed, skipping cache lookup: {}",
                    e
                );
                return Ok(());
            }
        };

        // 4. KNN 搜索 — top-1 cosine search above threshold
        if let Some(hit) = core.store.search_top1(&vector, core.threshold) {
            // 命中 — short-circuit + 设响应头
            ctx.short_circuited = true;
            ctx.exit_status = Some(200);
            ctx.exit_body = Some(hit.payload);
            let mut headers = std::collections::HashMap::new();
            headers.insert("Content-Type".to_string(), "application/json".to_string());
            headers.insert("X-Kong-AI-Cache".to_string(), "HIT-SEMANTIC".to_string());
            headers.insert(
                "X-Kong-AI-Cache-Similarity".to_string(),
                format!("{:.4}", hit.similarity),
            );
            ctx.exit_headers = Some(headers);

            ctx.extensions.insert(AiSemanticCacheContext {
                query_vector: None,
                cache_hit: true,
                core: None,
            });
            debug!(
                "ai-semantic-cache: HIT-SEMANTIC similarity={:.4} threshold={:.4}",
                hit.similarity, core.threshold
            );
            return Ok(());
        }

        // 5. miss — 保存 vector + core 引用以便 body_filter 写回
        ctx.extensions.insert(AiSemanticCacheContext {
            query_vector: Some(vector),
            cache_hit: false,
            core: Some(core),
        });
        ctx.response_headers_to_set
            .push(("X-Kong-AI-Cache".to_string(), "MISS-SEMANTIC".to_string()));

        Ok(())
    }

    async fn body_filter(
        &self,
        _config: &PluginConfig,
        ctx: &mut RequestCtx,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
    ) -> Result<()> {
        // miss 路径才需要写回;命中路径 short-circuit 后不会经过 body_filter
        let state = match ctx.extensions.get_mut::<AiSemanticCacheContext>() {
            Some(s) if !s.cache_hit && s.query_vector.is_some() && s.core.is_some() => s,
            _ => return Ok(()),
        };

        // 累积响应体到 ctx.shared(避免与 ai-proxy 的 response_buffer 冲突)
        // Accumulate response body into our own buffer in ctx.shared.
        const KEY: &str = "ai-semantic-cache.response_buffer";
        if let Some(chunk) = body.as_ref() {
            let chunk_str = String::from_utf8_lossy(chunk).into_owned();
            let combined = match ctx.shared.get(KEY).and_then(|v| v.as_str()) {
                Some(prev) => format!("{}{}", prev, chunk_str),
                None => chunk_str,
            };
            ctx.shared
                .insert(KEY.to_string(), serde_json::Value::String(combined));
        }

        if !end_of_stream {
            return Ok(());
        }

        // 仅缓存 200 响应 — only cache successful responses
        if ctx.response_status.unwrap_or(0) != 200 {
            return Ok(());
        }

        let buffered = match ctx.shared.remove(KEY).and_then(|v| match v {
            serde_json::Value::String(s) => Some(s),
            _ => None,
        }) {
            Some(s) => s,
            None => return Ok(()),
        };
        if buffered.is_empty() {
            return Ok(());
        }

        let vector = state.query_vector.take().expect("checked above");
        let core = state.core.clone().expect("checked above");
        core.store.insert(vector, buffered, core.ttl);
        debug!("ai-semantic-cache: stored response in vector store");

        Ok(())
    }
}

// ============ 辅助函数 ============

/// config 字段哈希(只关心影响 core 构造的字段)
fn config_hash(cfg: &AiSemanticCacheConfig) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(cfg.embedding_provider.as_bytes());
    hasher.update(cfg.embedding_endpoint_url.as_deref().unwrap_or("").as_bytes());
    hasher.update(cfg.embedding_api_key.as_deref().unwrap_or("").as_bytes());
    hasher.update(
        cfg.embedding_auth_header_name
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
    );
    hasher.update(
        cfg.embedding_auth_header_value
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
    );
    hasher.update(cfg.embedding_model.as_bytes());
    hasher.update(cfg.embedding_timeout_ms.to_le_bytes());
    hasher.update(cfg.similarity_threshold.to_le_bytes());
    hasher.update(cfg.cache_ttl_seconds.to_le_bytes());
    hasher.update(cfg.max_cache_entries.to_le_bytes());
    hasher.update(cfg.vector_store.as_bytes());
    let digest = hasher.finalize();
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(buf)
}

/// 从 ChatRequest body 提取缓存键文本
/// Extract cache key text from a ChatRequest JSON body.
pub fn extract_cache_text(body: &str, strategy: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let messages = parsed.get("messages")?.as_array()?;

    fn content_text(m: &serde_json::Value) -> Option<String> {
        let content = m.get("content")?;
        // content 可能是字符串(OpenAI 简单格式)或数组(多模态)
        if let Some(s) = content.as_str() {
            return Some(s.to_string());
        }
        if let Some(arr) = content.as_array() {
            // 多模态:拼接所有 text 片段
            let parts: Vec<&str> = arr
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect();
            if !parts.is_empty() {
                return Some(parts.join(" "));
            }
        }
        None
    }

    match strategy {
        "LastMessage" => messages.last().and_then(content_text),
        "FirstUserMessage" => messages
            .iter()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            .and_then(content_text),
        // 默认 AllMessages — concat all messages with role tags
        _ => {
            let parts: Vec<String> = messages
                .iter()
                .filter_map(|m| {
                    let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
                    let text = content_text(m)?;
                    Some(format!("{}: {}", role, text))
                })
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(messages: serde_json::Value) -> String {
        serde_json::json!({ "model": "x", "messages": messages }).to_string()
    }

    #[test]
    fn extract_last_message_string_content() {
        let b = body(serde_json::json!([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"},
            {"role": "user", "content": "what's the weather?"},
        ]));
        let s = extract_cache_text(&b, "LastMessage").unwrap();
        assert_eq!(s, "what's the weather?");
    }

    #[test]
    fn extract_first_user_message() {
        let b = body(serde_json::json!([
            {"role": "system", "content": "be helpful"},
            {"role": "user", "content": "first user q"},
            {"role": "assistant", "content": "answer"},
            {"role": "user", "content": "second user q"},
        ]));
        let s = extract_cache_text(&b, "FirstUserMessage").unwrap();
        assert_eq!(s, "first user q");
    }

    #[test]
    fn extract_all_messages_concatenated() {
        let b = body(serde_json::json!([
            {"role": "user", "content": "Q1"},
            {"role": "assistant", "content": "A1"},
        ]));
        let s = extract_cache_text(&b, "AllMessages").unwrap();
        assert!(s.contains("user: Q1"));
        assert!(s.contains("assistant: A1"));
    }

    #[test]
    fn extract_handles_multimodal_content_array() {
        let b = body(serde_json::json!([
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "describe this:"},
                    {"type": "image_url", "image_url": {"url": "http://x"}},
                    {"type": "text", "text": "in detail"},
                ]
            }
        ]));
        let s = extract_cache_text(&b, "LastMessage").unwrap();
        assert_eq!(s, "describe this: in detail");
    }

    #[test]
    fn extract_returns_none_for_invalid_body() {
        assert!(extract_cache_text("not json", "LastMessage").is_none());
        assert!(extract_cache_text(r#"{"foo":1}"#, "LastMessage").is_none());
    }

    #[test]
    fn config_hash_distinguishes_models() {
        let mut a = AiSemanticCacheConfig::default();
        let mut b = AiSemanticCacheConfig::default();
        b.embedding_model = "different-model".to_string();
        assert_ne!(config_hash(&a), config_hash(&b));
        // 相同 config 哈希一致
        a.embedding_model = "text-embedding-3-small".to_string();
        let mut c = AiSemanticCacheConfig::default();
        c.embedding_model = "text-embedding-3-small".to_string();
        assert_eq!(config_hash(&a), config_hash(&c));
    }
}

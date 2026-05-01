//! ai-semantic-cache 集成测试 — 通过注入 mock EmbeddingClient + InMemoryVectorStore 跑完整流程
//! Integration tests for ai-semantic-cache: inject mock EmbeddingClient + InMemoryVectorStore
//! and exercise the full PluginHandler lifecycle.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;

use kong_core::error::{KongError, Result};
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

use kong_ai::embedding::{EmbeddingClient, InMemoryVectorStore, VectorStore};
use kong_ai::plugins::ai_semantic_cache::{
    AiSemanticCacheConfig, AiSemanticCacheContext, AiSemanticCachePlugin, SemanticCacheCore,
};

// ============ Mock EmbeddingClient ============

/// 字面 → 向量 lookup,未命中时返回固定向量(用于"不同 prompt"测试)
struct MockEmbedder {
    table: Mutex<Vec<(String, Vec<f32>)>>,
    default: Vec<f32>,
    /// 失败开关 — 用于测试 embedding 失败降级路径
    fail: Mutex<bool>,
}

impl MockEmbedder {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            table: Mutex::new(Vec::new()),
            default: vec![0.1, 0.2, 0.3, 0.4],
            fail: Mutex::new(false),
        })
    }

    fn map(&self, key: &str, vector: Vec<f32>) {
        self.table.lock().unwrap().push((key.to_string(), vector));
    }

    fn set_failing(&self, fail: bool) {
        *self.fail.lock().unwrap() = fail;
    }
}

#[async_trait]
impl EmbeddingClient for MockEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        if *self.fail.lock().unwrap() {
            return Err(KongError::UpstreamError("mock failure".into()));
        }
        let table = self.table.lock().unwrap();
        for (k, v) in table.iter() {
            if text.contains(k) {
                return Ok(v.clone());
            }
        }
        Ok(self.default.clone())
    }

    fn identifier(&self) -> &str {
        "mock"
    }
}

// ============ Helpers ============

fn make_plugin_config(cfg: &AiSemanticCacheConfig) -> PluginConfig {
    PluginConfig {
        name: "ai-semantic-cache".to_string(),
        config: serde_json::json!({
            "embedding_model": cfg.embedding_model,
            "embedding_provider": cfg.embedding_provider,
            "similarity_threshold": cfg.similarity_threshold,
            "cache_ttl_seconds": cfg.cache_ttl_seconds,
            "max_cache_entries": cfg.max_cache_entries,
            "cache_key_strategy": cfg.cache_key_strategy,
            "vector_store": cfg.vector_store,
            "skip_header": cfg.skip_header,
            "embedding_timeout_ms": cfg.embedding_timeout_ms,
        }),
    }
}

fn make_ctx(body: &str) -> RequestCtx {
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(body.to_string());
    ctx.response_status = Some(200);
    ctx
}

fn body_with_user_message(content: &str) -> String {
    serde_json::json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": content}]
    })
    .to_string()
}

fn install_mock(
    plugin: &AiSemanticCachePlugin,
    cfg: &AiSemanticCacheConfig,
    embedder: Arc<MockEmbedder>,
    store: Arc<dyn VectorStore>,
) {
    let core = Arc::new(SemanticCacheCore {
        client: embedder,
        store,
        threshold: cfg.similarity_threshold,
        ttl: Duration::from_secs(cfg.cache_ttl_seconds),
    });
    plugin.install_core_for_config(cfg, core);
}

async fn run_access_and_body(
    plugin: &AiSemanticCachePlugin,
    cfg_pc: &PluginConfig,
    ctx: &mut RequestCtx,
    response_body: &str,
) {
    plugin.access(cfg_pc, ctx).await.unwrap();
    if ctx.short_circuited {
        return;
    }
    let mut body = Some(Bytes::from(response_body.to_string()));
    plugin.body_filter(cfg_pc, ctx, &mut body, true).await.unwrap();
}

// ============ 测试 ============

#[tokio::test]
async fn first_request_misses_then_stores() {
    let cfg = AiSemanticCacheConfig::default();
    let plugin = AiSemanticCachePlugin::new();
    let embedder = MockEmbedder::new();
    embedder.map("weather today", vec![1.0, 0.0, 0.0, 0.0]);
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new(100));
    install_mock(&plugin, &cfg, embedder, store.clone());

    let pc = make_plugin_config(&cfg);
    let mut ctx = make_ctx(&body_with_user_message("weather today"));
    run_access_and_body(&plugin, &pc, &mut ctx, r#"{"choices":[{"message":"sunny"}]}"#).await;

    assert!(!ctx.short_circuited, "first request should miss");
    assert_eq!(store.len(), 1, "miss should populate the store");
    let miss_header = ctx
        .response_headers_to_set
        .iter()
        .find(|(k, _)| k == "X-Kong-AI-Cache");
    assert_eq!(miss_header.map(|(_, v)| v.as_str()), Some("MISS-SEMANTIC"));
}

#[tokio::test]
async fn second_identical_request_hits_semantic_cache() {
    let cfg = AiSemanticCacheConfig::default();
    let plugin = AiSemanticCachePlugin::new();
    let embedder = MockEmbedder::new();
    // 同一 prompt 文本 → 同向量
    embedder.map("weather today", vec![1.0, 0.0, 0.0, 0.0]);
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new(100));
    install_mock(&plugin, &cfg, embedder, store.clone());

    let pc = make_plugin_config(&cfg);

    // 1st: miss + store
    let mut ctx1 = make_ctx(&body_with_user_message("weather today"));
    run_access_and_body(&plugin, &pc, &mut ctx1, r#"{"answer":"sunny"}"#).await;
    assert!(!ctx1.short_circuited);

    // 2nd: hit
    let mut ctx2 = make_ctx(&body_with_user_message("weather today"));
    plugin.access(&pc, &mut ctx2).await.unwrap();
    assert!(ctx2.short_circuited, "second identical request should hit");
    assert_eq!(ctx2.exit_status, Some(200));
    assert_eq!(ctx2.exit_body.as_deref(), Some(r#"{"answer":"sunny"}"#));
    let hdrs = ctx2.exit_headers.expect("hit must set exit_headers");
    assert_eq!(hdrs.get("X-Kong-AI-Cache").map(|s| s.as_str()), Some("HIT-SEMANTIC"));
    assert!(hdrs.contains_key("X-Kong-AI-Cache-Similarity"));
}

#[tokio::test]
async fn semantically_close_prompt_hits_above_threshold() {
    let cfg = AiSemanticCacheConfig {
        similarity_threshold: 0.95,
        ..Default::default()
    };
    let plugin = AiSemanticCachePlugin::new();
    let embedder = MockEmbedder::new();
    // 两个语义近似的 prompt → 几乎相同的向量(cosine ≈ 0.9999)
    embedder.map("What's the weather", vec![1.0, 0.0, 0.0, 0.0]);
    embedder.map("How is the weather", vec![0.999, 0.04, 0.0, 0.0]);
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new(100));
    install_mock(&plugin, &cfg, embedder, store.clone());

    let pc = make_plugin_config(&cfg);

    let mut ctx1 = make_ctx(&body_with_user_message("What's the weather"));
    run_access_and_body(&plugin, &pc, &mut ctx1, r#"{"a":"sunny"}"#).await;

    // 不同 prompt 文本但语义近似 → cosine 高于 0.95 → HIT
    let mut ctx2 = make_ctx(&body_with_user_message("How is the weather"));
    plugin.access(&pc, &mut ctx2).await.unwrap();
    assert!(ctx2.short_circuited, "semantically close prompt should hit");
}

#[tokio::test]
async fn unrelated_prompt_misses_then_first_repeat_hits() {
    let cfg = AiSemanticCacheConfig::default();
    let plugin = AiSemanticCachePlugin::new();
    let embedder = MockEmbedder::new();
    embedder.map("weather", vec![1.0, 0.0, 0.0, 0.0]);
    embedder.map("recipe", vec![0.0, 1.0, 0.0, 0.0]);
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new(100));
    install_mock(&plugin, &cfg, embedder, store.clone());
    let pc = make_plugin_config(&cfg);

    // 1st prompt — miss + store
    let mut ctx1 = make_ctx(&body_with_user_message("weather"));
    run_access_and_body(&plugin, &pc, &mut ctx1, r#"{"a":1}"#).await;
    assert!(!ctx1.short_circuited);
    assert_eq!(store.len(), 1);

    // 2nd unrelated prompt — should miss(余弦正交)
    let mut ctx2 = make_ctx(&body_with_user_message("recipe"));
    run_access_and_body(&plugin, &pc, &mut ctx2, r#"{"a":2}"#).await;
    assert!(!ctx2.short_circuited);
    assert_eq!(store.len(), 2);

    // 3rd:重复 1st → HIT
    let mut ctx3 = make_ctx(&body_with_user_message("weather"));
    plugin.access(&pc, &mut ctx3).await.unwrap();
    assert!(ctx3.short_circuited);
    assert_eq!(ctx3.exit_body.as_deref(), Some(r#"{"a":1}"#));
}

#[tokio::test]
async fn ttl_expired_entry_misses() {
    let cfg = AiSemanticCacheConfig {
        // 1 秒 TTL
        cache_ttl_seconds: 1,
        ..Default::default()
    };
    let plugin = AiSemanticCachePlugin::new();
    let embedder = MockEmbedder::new();
    embedder.map("hello", vec![1.0, 0.0, 0.0, 0.0]);
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new(100));
    install_mock(&plugin, &cfg, embedder, store.clone());
    let pc = make_plugin_config(&cfg);

    // 1st — miss + store
    let mut ctx1 = make_ctx(&body_with_user_message("hello"));
    run_access_and_body(&plugin, &pc, &mut ctx1, r#"{"a":1}"#).await;
    assert_eq!(store.len(), 1);

    // 等 TTL 过期 — wait past TTL
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // 2nd — should miss because entry expired
    let mut ctx2 = make_ctx(&body_with_user_message("hello"));
    plugin.access(&pc, &mut ctx2).await.unwrap();
    assert!(!ctx2.short_circuited, "expired entry should not match");
}

#[tokio::test]
async fn lru_eviction_when_capacity_exceeded() {
    let cfg = AiSemanticCacheConfig {
        max_cache_entries: 2,
        ..Default::default()
    };
    let plugin = AiSemanticCachePlugin::new();
    let embedder = MockEmbedder::new();
    embedder.map("A", vec![1.0, 0.0, 0.0, 0.0]);
    embedder.map("B", vec![0.0, 1.0, 0.0, 0.0]);
    embedder.map("C", vec![0.0, 0.0, 1.0, 0.0]);
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new(2));
    install_mock(&plugin, &cfg, embedder, store.clone());
    let pc = make_plugin_config(&cfg);

    // 写入 A、B、C 三条 — 容量 2,A 应被淘汰(B 在中间被命中或不命中无关紧要;
    // 这里先 A 后 B 后 C,LRU 应淘汰 A)
    for q in &["A", "B", "C"] {
        let mut ctx = make_ctx(&body_with_user_message(q));
        run_access_and_body(&plugin, &pc, &mut ctx, &format!(r#"{{"q":"{}"}}"#, q)).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(store.len(), 2);

    // 查 A — 应 miss(已被淘汰)
    let mut ctx_a = make_ctx(&body_with_user_message("A"));
    plugin.access(&pc, &mut ctx_a).await.unwrap();
    // miss 后会再次写入,所以这里只断言之前 A 已被淘汰: short_circuited=false 即可
    assert!(!ctx_a.short_circuited);
}

#[tokio::test]
async fn skip_header_bypasses_cache_completely() {
    let cfg = AiSemanticCacheConfig::default();
    let plugin = AiSemanticCachePlugin::new();
    let embedder = MockEmbedder::new();
    embedder.map("weather", vec![1.0, 0.0, 0.0, 0.0]);
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new(100));
    install_mock(&plugin, &cfg, embedder, store.clone());
    let pc = make_plugin_config(&cfg);

    // 先写入一次
    let mut ctx1 = make_ctx(&body_with_user_message("weather"));
    run_access_and_body(&plugin, &pc, &mut ctx1, r#"{"a":1}"#).await;
    assert_eq!(store.len(), 1);

    // 带 skip header 重发 → 既不命中也不写回
    let mut ctx2 = make_ctx(&body_with_user_message("weather"));
    ctx2.request_headers
        .insert("x-ai-skip-cache".to_string(), "1".to_string());
    plugin.access(&pc, &mut ctx2).await.unwrap();
    assert!(!ctx2.short_circuited);
    let cache_ctx = ctx2.extensions.get::<AiSemanticCacheContext>();
    assert!(cache_ctx.is_some());
    assert!(!cache_ctx.unwrap().cache_hit);
    // skip 路径不写回 → store 仍是 1
    let mut body = Some(Bytes::from_static(b"{\"a\":2}"));
    plugin.body_filter(&pc, &mut ctx2, &mut body, true).await.unwrap();
    assert_eq!(store.len(), 1, "skip header path must not store");
}

#[tokio::test]
async fn embedding_failure_skips_cache_does_not_break_request() {
    let cfg = AiSemanticCacheConfig::default();
    let plugin = AiSemanticCachePlugin::new();
    let embedder = MockEmbedder::new();
    embedder.set_failing(true);
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new(100));
    install_mock(&plugin, &cfg, embedder, store.clone());
    let pc = make_plugin_config(&cfg);

    let mut ctx = make_ctx(&body_with_user_message("hello"));
    plugin.access(&pc, &mut ctx).await.unwrap();
    assert!(!ctx.short_circuited, "embedding failure must not short-circuit");
    assert_eq!(store.len(), 0);
}

#[tokio::test]
async fn non_200_response_is_not_cached() {
    let cfg = AiSemanticCacheConfig::default();
    let plugin = AiSemanticCachePlugin::new();
    let embedder = MockEmbedder::new();
    embedder.map("hello", vec![1.0, 0.0, 0.0, 0.0]);
    let store: Arc<dyn VectorStore> = Arc::new(InMemoryVectorStore::new(100));
    install_mock(&plugin, &cfg, embedder, store.clone());
    let pc = make_plugin_config(&cfg);

    let mut ctx = make_ctx(&body_with_user_message("hello"));
    ctx.response_status = Some(500);
    plugin.access(&pc, &mut ctx).await.unwrap();
    let mut body = Some(Bytes::from_static(b"{\"err\":\"upstream\"}"));
    plugin.body_filter(&pc, &mut ctx, &mut body, true).await.unwrap();
    assert_eq!(store.len(), 0, "non-200 response must not be cached");
}

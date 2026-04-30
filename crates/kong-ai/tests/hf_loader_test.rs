//! HfLoader 集成测试 — 磁盘缓存命中 / 首次降级 / 单飞 / 离线 / 下载失败 / registry 集成
//! Step 3 coverage: disk cache hit, non-blocking first miss, single-flight, offline, fail, registry

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kong_ai::codec::ChatRequest;
use kong_ai::token::{
    estimate_from_request, extract_prompt_text, HfDownloader, HfLoader, TokenizerConfig,
    TokenizerMapping, TokenizerRegistry, TokenizerStrategy,
};
use kong_core::error::{KongError, Result as KongResult};
use tempfile::TempDir;

// ─── Fixture: minimal valid tokenizer.json ──────────────────────────────────
//
// WordLevel + Whitespace pre_tokenizer:词表足够覆盖测试输入,encode 后能产出
// 多个 token,验证 tokenizer 真的在工作(而不是单纯回退)。
const FIXTURE_JSON: &str = r#"{
  "version": "1.0",
  "truncation": null,
  "padding": null,
  "added_tokens": [
    {"id":0,"content":"[UNK]","single_word":false,"lstrip":false,"rstrip":false,"normalized":false,"special":true}
  ],
  "normalizer": null,
  "pre_tokenizer": {"type": "Whitespace"},
  "post_processor": null,
  "decoder": null,
  "model": {
    "type": "WordLevel",
    "vocab": {"[UNK]": 0, "hello": 1, "world": 2, "test": 3, "foo": 4, "bar": 5, "baz": 6, "user": 7},
    "unk_token": "[UNK]"
  }
}"#;

fn write_fixture(cache_dir: &Path, repo_id: &str) {
    let safe = repo_id.replace('/', "__");
    let path = cache_dir.join(safe).join("tokenizer.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, FIXTURE_JSON).unwrap();
}

/// 用 fixture tokenizer 真实 encode `extract_prompt_text(req)` 算出预期 token 数
/// Compute expected token count by encoding the same text the registry would feed.
fn fixture_expected_tokens(req: &ChatRequest) -> u64 {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("tokenizer.json");
    std::fs::write(&path, FIXTURE_JSON).unwrap();
    let tok = tokenizers::Tokenizer::from_file(&path).unwrap();
    let text = extract_prompt_text(req);
    tok.encode(text, false).unwrap().len() as u64
}

fn text_request(model: &str, content: &str) -> ChatRequest {
    serde_json::from_str(&format!(
        r#"{{"model":"{}","messages":[{{"role":"user","content":"{}"}}]}}"#,
        model, content
    ))
    .unwrap()
}

// ─── MockHfDownloader: 计数 + 可控延迟 + 可控失败 ───────────────────────────

struct MockHfDownloader {
    fixture: Vec<u8>,
    call_count: AtomicU32,
    delay: Duration,
    fail: bool,
}

impl MockHfDownloader {
    fn new(delay: Duration, fail: bool) -> Arc<Self> {
        Arc::new(Self {
            fixture: FIXTURE_JSON.as_bytes().to_vec(),
            call_count: AtomicU32::new(0),
            delay,
            fail,
        })
    }
    fn calls(&self) -> u32 {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl HfDownloader for MockHfDownloader {
    async fn download_tokenizer(&self, _repo_id: &str, target: &Path) -> KongResult<()> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        if self.delay > Duration::ZERO {
            tokio::time::sleep(self.delay).await;
        }
        if self.fail {
            return Err(KongError::InternalError("mock download failure".into()));
        }
        let parent = target.parent().unwrap();
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| KongError::InternalError(e.to_string()))?;
        let tmp = target.with_extension("tmp");
        tokio::fs::write(&tmp, &self.fixture)
            .await
            .map_err(|e| KongError::InternalError(e.to_string()))?;
        tokio::fs::rename(&tmp, target)
            .await
            .map_err(|e| KongError::InternalError(e.to_string()))?;
        Ok(())
    }
}

// ─── 0. fixture JSON 验证(确保 schema 能被 tokenizers 0.23 解析)────────────

#[test]
fn fixture_tokenizer_json_is_valid() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("tokenizer.json");
    std::fs::write(&path, FIXTURE_JSON).unwrap();
    let tok = tokenizers::Tokenizer::from_file(&path).expect("fixture must parse");
    let enc = tok.encode("hello world test foo", false).expect("encode ok");
    // Whitespace pre_tokenizer 切成 4 个 word 都在词表 → 4 tokens
    assert_eq!(enc.len(), 4, "expected 4 tokens for 4-word input, got {}", enc.len());
}

// ─── 1. 磁盘缓存命中:try_get 同步返回 Some ─────────────────────────────────

#[tokio::test]
async fn hf_loader_disk_cache_hit_returns_loaded_synchronously() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path(), "test-org/cached-model");
    let downloader = MockHfDownloader::new(Duration::ZERO, false);
    let loader = Arc::new(HfLoader::with_downloader(
        dir.path().to_path_buf(),
        false,
        downloader.clone(),
    ));
    let tok = loader.try_get("test-org/cached-model");
    assert!(tok.is_some(), "disk cache hit should return Some immediately");
    // 第二次也应同步命中(走内存缓存)
    let tok2 = loader.try_get("test-org/cached-model");
    assert!(tok2.is_some());
    // 整个过程 downloader 不应被调用
    assert_eq!(downloader.calls(), 0, "no network when disk cache hits");
}

// ─── 2. 首次 cache miss:返回 None,后台 spawn 下载,等待后第二次命中 ──────

#[tokio::test]
async fn hf_loader_first_miss_returns_none_then_loads_in_background() {
    let dir = TempDir::new().unwrap();
    let downloader = MockHfDownloader::new(Duration::from_millis(40), false);
    let loader = Arc::new(HfLoader::with_downloader(
        dir.path().to_path_buf(),
        false,
        downloader.clone(),
    ));

    // 首次:None(spawn 触发)
    assert!(loader.try_get("test-org/lazy-model").is_none());
    // 等下载完成
    let tok = loader
        .get_or_wait("test-org/lazy-model", Duration::from_secs(2))
        .await;
    assert!(tok.is_some(), "background download should complete");
    // 之后 try_get 同步命中
    assert!(loader.try_get("test-org/lazy-model").is_some());
    assert_eq!(downloader.calls(), 1);
}

// ─── 3. 单飞:并发 N 次 try_get 只触发一次下载 ──────────────────────────────

#[tokio::test]
async fn hf_loader_single_flight_under_concurrent_misses() {
    let dir = TempDir::new().unwrap();
    let downloader = MockHfDownloader::new(Duration::from_millis(80), false);
    let loader = Arc::new(HfLoader::with_downloader(
        dir.path().to_path_buf(),
        false,
        downloader.clone(),
    ));

    // 并发 100 个 try_get,所有都 cache miss
    let mut handles = Vec::new();
    for _ in 0..100 {
        let l = loader.clone();
        handles.push(tokio::spawn(async move {
            l.try_get("test-org/single-flight")
        }));
    }
    for h in handles {
        let _ = h.await.unwrap(); // 都返回 None
    }
    // 等下载完成
    let _ = loader
        .get_or_wait("test-org/single-flight", Duration::from_secs(2))
        .await;

    assert_eq!(
        downloader.calls(),
        1,
        "single-flight should trigger exactly one download"
    );
}

// ─── 4. 离线模式:cache 空 → 永远 None,不发起下载 ──────────────────────────

#[tokio::test]
async fn hf_loader_offline_no_download_attempted() {
    let dir = TempDir::new().unwrap();
    let downloader = MockHfDownloader::new(Duration::ZERO, false);
    let loader = Arc::new(HfLoader::with_downloader(
        dir.path().to_path_buf(),
        true, // offline
        downloader.clone(),
    ));

    for _ in 0..5 {
        assert!(loader.try_get("test-org/offline-miss").is_none());
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        downloader.calls(),
        0,
        "offline mode should never call downloader"
    );
}

#[tokio::test]
async fn hf_loader_offline_disk_cache_still_works() {
    let dir = TempDir::new().unwrap();
    write_fixture(dir.path(), "test-org/offline-cached");
    let downloader = MockHfDownloader::new(Duration::ZERO, false);
    let loader = Arc::new(HfLoader::with_downloader(
        dir.path().to_path_buf(),
        true,
        downloader.clone(),
    ));
    assert!(loader.try_get("test-org/offline-cached").is_some());
    assert_eq!(downloader.calls(), 0);
}

// ─── 5. 下载失败:state=Failed,后续 try_get 永远 None ───────────────────────

#[tokio::test]
async fn hf_loader_download_failure_stays_failed() {
    let dir = TempDir::new().unwrap();
    let downloader = MockHfDownloader::new(Duration::from_millis(20), true);
    let loader = Arc::new(HfLoader::with_downloader(
        dir.path().to_path_buf(),
        false,
        downloader.clone(),
    ));

    // 首次触发 spawn
    assert!(loader.try_get("test-org/will-fail").is_none());
    // 等 spawn 跑完
    tokio::time::sleep(Duration::from_millis(150)).await;
    // 后续多次 try_get 都返回 None,且不会再触发新的下载(state 已 Failed)
    for _ in 0..5 {
        assert!(loader.try_get("test-org/will-fail").is_none());
    }
    assert_eq!(downloader.calls(), 1, "failure should not be retried");
}

// ─── 6. Registry 集成:首次降级 + 后台加载完成后精确计数 ────────────────────

#[tokio::test]
async fn registry_huggingface_strategy_first_request_falls_back_then_warms_up() {
    let dir = TempDir::new().unwrap();
    let downloader = MockHfDownloader::new(Duration::from_millis(40), false);

    let cfg = TokenizerConfig {
        hf_cache_dir: Some(dir.path().to_path_buf()),
        offline: false,
        ..TokenizerConfig::default()
    };
    let registry = TokenizerRegistry::with_hf_downloader(cfg, downloader.clone());

    let req = text_request("test-org/lazy", "hello world test foo");
    let est = estimate_from_request(&req);
    let expected = fixture_expected_tokens(&req);

    // 首次:HF tokenizer 尚未加载 → fallback 到字符估算
    let n1 = registry
        .count_prompt("huggingface", "test-org/lazy", &req)
        .await;
    assert_eq!(n1, est, "first call should fall back to char estimate");

    // 等后台下载 + 加载完成
    tokio::time::sleep(Duration::from_millis(150)).await;

    // 第二次:命中 HF 本地 tokenizer,与 fixture 直接 encode 结果一致
    let n2 = registry
        .count_prompt("huggingface", "test-org/lazy", &req)
        .await;
    assert_eq!(n2, expected, "warm-up must match direct fixture encoding");
    assert!(n2 > 0);
    assert_eq!(downloader.calls(), 1);
}

// ─── 7. Registry mapping:配置 mapping 提供 hf_repo_id ──────────────────────

#[tokio::test]
async fn registry_mapping_provides_explicit_hf_repo_id() {
    let dir = TempDir::new().unwrap();
    let downloader = MockHfDownloader::new(Duration::from_millis(20), false);

    // model 名是 "qwen2-7b"(不含 /),但 mapping 把它指向 "test-org/qwen-fixture"
    let cfg = TokenizerConfig {
        hf_cache_dir: Some(dir.path().to_path_buf()),
        offline: false,
        mappings: vec![TokenizerMapping {
            pattern: "^qwen2-7b$".to_string(),
            strategy: TokenizerStrategy::HuggingFace,
            hf_repo_id: Some("test-org/qwen-fixture".to_string()),
        }],
        ..TokenizerConfig::default()
    };
    let registry = TokenizerRegistry::with_hf_downloader(cfg, downloader.clone());

    let req = text_request("qwen2-7b", "hello world test");
    let expected = fixture_expected_tokens(&req);
    // 首次降级
    let _ = registry.count_prompt("openai", "qwen2-7b", &req).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    // 后台下载完成 — 命中,值与 fixture 直接 encode 一致(证明 mapping 走通)
    let n = registry.count_prompt("openai", "qwen2-7b", &req).await;
    assert_eq!(n, expected, "warm-up must match fixture encoding");
    assert_eq!(downloader.calls(), 1);
}

// ─── 8. Registry resolve_hf_repo: model 含 / 直接当 repo_id ────────────────

#[test]
fn registry_resolve_hf_repo_uses_model_when_contains_slash() {
    let registry = TokenizerRegistry::default();
    assert_eq!(
        registry.resolve_hf_repo("Qwen/Qwen2.5-7B"),
        Some("Qwen/Qwen2.5-7B".to_string())
    );
    assert_eq!(registry.resolve_hf_repo("plain-name"), None);
}

#[test]
fn registry_resolve_hf_repo_mapping_overrides_model() {
    let cfg = TokenizerConfig {
        mappings: vec![TokenizerMapping {
            pattern: "^foo$".to_string(),
            strategy: TokenizerStrategy::HuggingFace,
            hf_repo_id: Some("Bar/Baz".to_string()),
        }],
        ..TokenizerConfig::default()
    };
    let registry = TokenizerRegistry::new(cfg);
    assert_eq!(
        registry.resolve_hf_repo("foo"),
        Some("Bar/Baz".to_string())
    );
}

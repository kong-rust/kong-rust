//! Step 4 远端 count 集成测试 — 路由 / 双轨分流 / 降级 / LRU / 真实 HTTP body 解析
//! Coverage: routing, dual-path, fallback, LRU hit, has_non_text key separation,
//!           and real HTTP body construction via axum mock servers.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use kong_ai::codec::ChatRequest;
use kong_ai::token::{
    estimate_from_request, AnthropicCountClient, GeminiCountClient, OpenAiCountClient,
    RemoteCountCache, RemoteCountClient, TokenizerConfig, TokenizerRegistry,
};
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::oneshot;

// 测试配置:强制 offline=true,避免测试期间触发真实 HF 下载
// (新规则下 OpenAi 路径会注入 HfTokenizer;cache miss 时若非 offline 会 spawn 网络下载)
// Test config: force offline=true so HfTokenizer cache miss won't trigger real HF downloads
// (OpenAi composite path injects HfTokenizer; without offline a miss would spawn a network call)
fn test_config() -> (TokenizerConfig, TempDir) {
    let dir = TempDir::new().unwrap();
    let cfg = TokenizerConfig {
        offline: true,
        hf_cache_dir: Some(dir.path().to_path_buf()),
        ..TokenizerConfig::default()
    };
    (cfg, dir)
}

// ─── 通用 helpers ────────────────────────────────────────────────────────────

fn parse_chat(body: &str) -> ChatRequest {
    serde_json::from_str(body).expect("valid ChatRequest")
}

fn text_request(model: &str, content: &str) -> ChatRequest {
    parse_chat(&format!(
        r#"{{"model":"{}","messages":[{{"role":"user","content":"{}"}}]}}"#,
        model, content
    ))
}

fn image_request(model: &str) -> ChatRequest {
    // 用稍长的文本让 tiktoken 和字符估算明显不同(避免短输入碰巧相等)
    // Long enough text so tiktoken count clearly differs from char estimate
    parse_chat(&format!(
        r#"{{
            "model":"{}",
            "messages":[{{"role":"user","content":[
                {{"type":"text","text":"please describe this image in great detail with structured analysis and key observations"}},
                {{"type":"image_url","image_url":{{"url":"https://x/y.png"}}}}
            ]}}]
        }}"#,
        model
    ))
}

fn tools_request(model: &str) -> ChatRequest {
    parse_chat(&format!(
        r#"{{
            "model":"{}",
            "messages":[{{"role":"user","content":"call my tool"}}],
            "tools":[{{"type":"function","function":{{"name":"f","parameters":{{}}}}}}]
        }}"#,
        model
    ))
}

// ─── MockRemoteCountClient — trait 实现,不发 HTTP ────────────────────────────

struct MockRemoteCountClient {
    provider: &'static str,
    /// None → 模拟失败;Some(v) → 返回值
    fixed: Mutex<Option<u64>>,
    call_count: AtomicU32,
    /// 记录最近一次调用的 has_non_text 值,供 LRU key 测试断言
    last_has_non_text: Mutex<Option<bool>>,
    delay: Duration,
}

impl MockRemoteCountClient {
    fn new(provider: &'static str, fixed: Option<u64>) -> Arc<Self> {
        Arc::new(Self {
            provider,
            fixed: Mutex::new(fixed),
            call_count: AtomicU32::new(0),
            last_has_non_text: Mutex::new(None),
            delay: Duration::ZERO,
        })
    }

    fn with_delay(provider: &'static str, fixed: Option<u64>, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            provider,
            fixed: Mutex::new(fixed),
            call_count: AtomicU32::new(0),
            last_has_non_text: Mutex::new(None),
            delay,
        })
    }

    fn calls(&self) -> u32 {
        self.call_count.load(Ordering::SeqCst)
    }

    fn last_non_text(&self) -> Option<bool> {
        *self.last_has_non_text.lock().unwrap()
    }
}

#[async_trait]
impl RemoteCountClient for MockRemoteCountClient {
    async fn count(
        &self,
        _model: &str,
        _request: &ChatRequest,
        has_non_text: bool,
    ) -> Option<u64> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        *self.last_has_non_text.lock().unwrap() = Some(has_non_text);
        if self.delay > Duration::ZERO {
            tokio::time::sleep(self.delay).await;
        }
        *self.fixed.lock().unwrap()
    }

    fn provider(&self) -> &'static str {
        self.provider
    }
}

// ============================================================================
// 类 A:Mock trait 测试 — 路由 / 双轨 / 降级
// ============================================================================

#[tokio::test]
async fn openai_pure_text_uses_tiktoken_not_remote() {
    let mock = MockRemoteCountClient::new("openai", Some(99999));
    let registry = TokenizerRegistry::with_remote_clients(
        test_config().0,
        Some(mock.clone()),
        None,
        None,
    );
    let req = text_request("gpt-4", "Hello, world!");
    let n = registry.count_prompt("openai", "gpt-4", &req).await;
    assert!(n > 0 && n < 50, "tiktoken-ish count, got {}", n);
    assert_ne!(n, 99999, "must not have come from remote mock");
    assert_eq!(mock.calls(), 0, "remote MUST NOT be called for pure text");
}

#[tokio::test]
async fn openai_with_image_calls_remote_api() {
    let mock = MockRemoteCountClient::new("openai", Some(7777));
    let registry = TokenizerRegistry::with_remote_clients(
        test_config().0,
        Some(mock.clone()),
        None,
        None,
    );
    let req = image_request("gpt-4o");
    let n = registry.count_prompt("openai", "gpt-4o", &req).await;
    assert_eq!(n, 7777, "non-text path must hit remote and use its value");
    assert_eq!(mock.calls(), 1);
    assert_eq!(mock.last_non_text(), Some(true));
}

#[tokio::test]
async fn openai_with_tools_calls_remote_api() {
    let mock = MockRemoteCountClient::new("openai", Some(4242));
    let registry = TokenizerRegistry::with_remote_clients(
        test_config().0,
        Some(mock.clone()),
        None,
        None,
    );
    let req = tools_request("gpt-4");
    let n = registry.count_prompt("openai", "gpt-4", &req).await;
    assert_eq!(n, 4242);
    assert_eq!(mock.calls(), 1);
}

#[tokio::test]
async fn openai_remote_failure_falls_back_to_tiktoken() {
    let mock = MockRemoteCountClient::new("openai", None); // None = 失败
    let registry = TokenizerRegistry::with_remote_clients(
        test_config().0,
        Some(mock.clone()),
        None,
        None,
    );
    let req = image_request("gpt-4o");
    let n = registry.count_prompt("openai", "gpt-4o", &req).await;
    assert!(n > 0, "should fall back to tiktoken");
    let est = estimate_from_request(&req);
    assert_ne!(n, est, "tiktoken count differs from char estimate");
    assert_eq!(mock.calls(), 1, "remote was attempted before fallback");
}

#[tokio::test]
async fn openai_remote_timeout_falls_back_to_tiktoken() {
    // mock 故意延迟 500ms,超过 registry 默认 deadline 300ms
    let mock = MockRemoteCountClient::with_delay("openai", Some(99999), Duration::from_millis(500));
    let registry = TokenizerRegistry::with_remote_clients(
        test_config().0,
        Some(mock.clone()),
        None,
        None,
    );
    let req = image_request("gpt-4o");
    let n = registry.count_prompt("openai", "gpt-4o", &req).await;
    // 整个 count_prompt 被 deadline 打断 → estimate 兜底
    let est = estimate_from_request(&req);
    assert_eq!(n, est, "deadline → estimate fallback (registry-level)");
}

#[tokio::test]
async fn anthropic_remote_success() {
    let mock = MockRemoteCountClient::new("anthropic", Some(123));
    let registry = TokenizerRegistry::with_remote_clients(
        test_config().0,
        None,
        Some(mock.clone()),
        None,
    );
    let req = text_request("claude-3-5-sonnet", "hello");
    let n = registry
        .count_prompt("anthropic", "claude-3-5-sonnet", &req)
        .await;
    assert_eq!(n, 123);
    assert_eq!(mock.calls(), 1);
}

#[tokio::test]
async fn anthropic_remote_failure_falls_back_to_estimate() {
    let mock = MockRemoteCountClient::new("anthropic", None);
    let registry = TokenizerRegistry::with_remote_clients(
        test_config().0,
        None,
        Some(mock.clone()),
        None,
    );
    let req = text_request("claude-3-5-sonnet", "hello world test");
    let est = estimate_from_request(&req);
    let n = registry
        .count_prompt("anthropic", "claude-3-5-sonnet", &req)
        .await;
    assert_eq!(n, est, "Anthropic failure → char estimate (no local tokenizer)");
}

#[tokio::test]
async fn gemini_remote_failure_falls_back_to_estimate() {
    let mock = MockRemoteCountClient::new("gemini", None);
    let registry = TokenizerRegistry::with_remote_clients(
        test_config().0,
        None,
        None,
        Some(mock.clone()),
    );
    let req = text_request("gemini-1.5-pro", "hello world test");
    let est = estimate_from_request(&req);
    let n = registry
        .count_prompt("gemini", "gemini-1.5-pro", &req)
        .await;
    assert_eq!(n, est);
}

#[tokio::test]
async fn anthropic_no_remote_configured_falls_back_to_estimate() {
    let registry = TokenizerRegistry::with_remote_clients(
        test_config().0,
        None,
        None, // anthropic remote 未配置
        None,
    );
    let req = text_request("claude-3-5-sonnet", "hello world test foo");
    let est = estimate_from_request(&req);
    let n = registry
        .count_prompt("anthropic", "claude-3-5-sonnet", &req)
        .await;
    assert_eq!(n, est);
}

// ============================================================================
// 类 A 续:LRU key 区分 has_non_text (同 prompt 不同 has_non_text 不串)
// ============================================================================
//
// MockRemoteCountClient 不内置 LRU(LRU 在真实 client 内部),所以这个测试用
// 真实 OpenAiCountClient + axum mock server 来验证。见类 B。

// ============================================================================
// 类 B:axum mock server 测试 — 真实 HTTP body 解析
// ============================================================================

/// 通用:启动一个 axum 服务器并返回 (base_url, shutdown sender)
async fn spawn_server(app: Router) -> (String, oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await
            .ok();
    });
    (format!("http://{}", addr), tx)
}

#[derive(Clone, Default)]
struct OpenAiServerState {
    /// 收到的请求 body(供断言)
    last_body: Arc<Mutex<Option<Value>>>,
    /// 收到的 Authorization header
    last_auth: Arc<Mutex<Option<String>>>,
    /// 调用次数
    call_count: Arc<AtomicU32>,
    /// 返回的 input_tokens 值
    return_tokens: u64,
}

async fn openai_handler(
    State(state): State<OpenAiServerState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    state.call_count.fetch_add(1, Ordering::SeqCst);
    *state.last_body.lock().unwrap() = Some(body);
    *state.last_auth.lock().unwrap() = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    Json(serde_json::json!({
        "object": "response.input_tokens",
        "input_tokens": state.return_tokens
    }))
}

#[tokio::test]
async fn openai_real_http_client_sends_correct_body_and_parses_response() {
    let state = OpenAiServerState {
        return_tokens: 314,
        ..Default::default()
    };
    let app = Router::new()
        .route("/v1/responses/input_tokens", post(openai_handler))
        .with_state(state.clone());
    let (base_url, shutdown) = spawn_server(app).await;

    let cache = Arc::new(RemoteCountCache::default());
    let client = OpenAiCountClient::new(
        reqwest::Client::new(),
        Some(base_url),
        Some("test-api-key".to_string()),
        cache,
        Duration::from_secs(2),
    );
    let req = parse_chat(
        r#"{
            "model":"gpt-4o",
            "messages":[{"role":"user","content":[
                {"type":"text","text":"hi"},
                {"type":"image_url","image_url":{"url":"https://x/y.png"}}
            ]}],
            "tools":[{"type":"function","function":{"name":"f","parameters":{}}}]
        }"#,
    );

    let n = client.count("gpt-4o", &req, true).await;
    assert_eq!(n, Some(314));

    // 验证发出的 body 形态
    let body = state.last_body.lock().unwrap().clone().unwrap();
    assert_eq!(body["model"], "gpt-4o");
    assert!(body["input"].is_array(), "input must be an array");
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["role"], "user");
    // content 数组结构应原样保留(包含 image_url part)
    assert!(input[0]["content"].is_array());
    let content_arr = input[0]["content"].as_array().unwrap();
    assert!(content_arr.iter().any(|p| p["type"] == "image_url"));
    // tools 透传
    assert!(body["tools"].is_array());
    // Bearer 认证
    let auth = state.last_auth.lock().unwrap().clone().unwrap();
    assert_eq!(auth, "Bearer test-api-key");

    let _ = shutdown.send(());
}

#[tokio::test]
async fn openai_lru_key_distinguishes_has_non_text() {
    // 同一 prompt(extract_prompt_text 结果相同),但 has_non_text=true vs false 应分两条 key
    // OpenAiCountClient 直接调用,验证不会从对方的缓存读到错误值
    let state_true = OpenAiServerState {
        return_tokens: 1000,
        ..Default::default()
    };
    let app = Router::new()
        .route("/v1/responses/input_tokens", post(openai_handler))
        .with_state(state_true.clone());
    let (base_url, shutdown) = spawn_server(app).await;

    let cache = Arc::new(RemoteCountCache::default());
    let client = OpenAiCountClient::new(
        reqwest::Client::new(),
        Some(base_url),
        Some("k".to_string()),
        cache.clone(),
        Duration::from_secs(2),
    );

    // 两次请求文本相同(extract_prompt_text 都得 "user:hi"),但 has_non_text 不同
    let req = text_request("gpt-4", "hi");
    let _ = client.count("gpt-4", &req, true).await;
    let _ = client.count("gpt-4", &req, false).await;

    // 两条不同 key,服务端被调用 2 次(若 LRU key 没区分 has_non_text 就会只调用 1 次)
    assert_eq!(
        state_true.call_count.load(Ordering::SeqCst),
        2,
        "LRU key must distinguish has_non_text"
    );

    // 第三次 has_non_text=true → 命中缓存,不再调用
    let _ = client.count("gpt-4", &req, true).await;
    assert_eq!(
        state_true.call_count.load(Ordering::SeqCst),
        2,
        "second call with same has_non_text must hit LRU"
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn openai_lru_caches_repeated_requests() {
    let state = OpenAiServerState {
        return_tokens: 50,
        ..Default::default()
    };
    let app = Router::new()
        .route("/v1/responses/input_tokens", post(openai_handler))
        .with_state(state.clone());
    let (base_url, shutdown) = spawn_server(app).await;

    let cache = Arc::new(RemoteCountCache::default());
    let client = OpenAiCountClient::new(
        reqwest::Client::new(),
        Some(base_url),
        Some("k".to_string()),
        cache,
        Duration::from_secs(2),
    );
    let req = text_request("gpt-4", "same prompt");

    for _ in 0..5 {
        let n = client.count("gpt-4", &req, true).await;
        assert_eq!(n, Some(50));
    }
    assert_eq!(
        state.call_count.load(Ordering::SeqCst),
        1,
        "5 identical requests must collapse into 1 HTTP call"
    );

    let _ = shutdown.send(());
}

// ─── Anthropic mock server 测试 ─────────────────────────────────────────────

#[derive(Clone, Default)]
struct AnthropicState {
    last_body: Arc<Mutex<Option<Value>>>,
    last_key: Arc<Mutex<Option<String>>>,
    last_version: Arc<Mutex<Option<String>>>,
    return_tokens: u64,
}

async fn anthropic_handler(
    State(state): State<AnthropicState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    *state.last_body.lock().unwrap() = Some(body);
    *state.last_key.lock().unwrap() = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    *state.last_version.lock().unwrap() = headers
        .get("anthropic-version")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    Json(serde_json::json!({"input_tokens": state.return_tokens}))
}

#[tokio::test]
async fn anthropic_real_http_client_sends_correct_body() {
    let state = AnthropicState {
        return_tokens: 256,
        ..Default::default()
    };
    let app = Router::new()
        .route("/v1/messages/count_tokens", post(anthropic_handler))
        .with_state(state.clone());
    let (base_url, shutdown) = spawn_server(app).await;

    let cache = Arc::new(RemoteCountCache::default());
    let client = AnthropicCountClient::new(
        reqwest::Client::new(),
        Some(base_url),
        Some("ant-key".to_string()),
        cache,
        Duration::from_secs(2),
    );
    let req = parse_chat(
        r#"{
            "model":"claude-3-5-sonnet",
            "messages":[
                {"role":"system","content":"you are helpful"},
                {"role":"user","content":"hello"}
            ]
        }"#,
    );
    let n = client.count("claude-3-5-sonnet", &req, false).await;
    assert_eq!(n, Some(256));

    let body = state.last_body.lock().unwrap().clone().unwrap();
    assert_eq!(body["model"], "claude-3-5-sonnet");
    // system role 应被提到顶层 system 字段
    assert_eq!(body["system"], "you are helpful");
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "system removed from messages");
    assert_eq!(messages[0]["role"], "user");
    // headers
    assert_eq!(
        state.last_key.lock().unwrap().as_deref(),
        Some("ant-key")
    );
    assert_eq!(
        state.last_version.lock().unwrap().as_deref(),
        Some("2023-06-01")
    );

    let _ = shutdown.send(());
}

// ─── Gemini mock server 测试 ────────────────────────────────────────────────

#[derive(Clone, Default)]
struct GeminiState {
    last_body: Arc<Mutex<Option<Value>>>,
    last_model: Arc<Mutex<Option<String>>>,
    last_query_key: Arc<Mutex<Option<String>>>,
    return_tokens: u64,
}

async fn gemini_handler(
    State(state): State<GeminiState>,
    // axum 不允许 ":xxx" 与 path 参数共段 → 用通配 *path,在 handler 里解析
    Path(path): Path<String>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    // path 形如 "v1beta/models/gemini-1.5-pro:countTokens"
    let model = path
        .strip_prefix("v1beta/models/")
        .and_then(|s| s.strip_suffix(":countTokens"))
        .unwrap_or(&path)
        .to_string();
    *state.last_body.lock().unwrap() = Some(body);
    *state.last_model.lock().unwrap() = Some(model);
    *state.last_query_key.lock().unwrap() = q.get("key").cloned();
    Json(serde_json::json!({"totalTokens": state.return_tokens}))
}

#[tokio::test]
async fn gemini_real_http_client_sends_correct_body() {
    let state = GeminiState {
        return_tokens: 88,
        ..Default::default()
    };
    let app = Router::new()
        .route("/{*path}", post(gemini_handler))
        .with_state(state.clone());
    let (base_url, shutdown) = spawn_server(app).await;

    let cache = Arc::new(RemoteCountCache::default());
    let client = GeminiCountClient::new(
        reqwest::Client::new(),
        Some(base_url),
        Some("gem-key".to_string()),
        cache,
        Duration::from_secs(2),
    );
    let req = parse_chat(
        r#"{
            "model":"gemini-1.5-pro",
            "messages":[
                {"role":"user","content":"hi"},
                {"role":"assistant","content":"hello"}
            ]
        }"#,
    );
    let n = client.count("gemini-1.5-pro", &req, false).await;
    assert_eq!(n, Some(88));

    let body = state.last_body.lock().unwrap().clone().unwrap();
    let contents = body["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0]["role"], "user");
    // assistant 应被映射成 model 角色
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(contents[0]["parts"][0]["text"], "hi");

    // model 在 path,api_key 在 query
    assert_eq!(
        state.last_model.lock().unwrap().as_deref(),
        Some("gemini-1.5-pro")
    );
    assert_eq!(
        state.last_query_key.lock().unwrap().as_deref(),
        Some("gem-key")
    );

    let _ = shutdown.send(());
}

// ─── HTTP 错误状态降级 ──────────────────────────────────────────────────────

async fn err_handler() -> (axum::http::StatusCode, Json<Value>) {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error":"boom"})),
    )
}

#[tokio::test]
async fn openai_real_http_5xx_returns_none_for_fallback() {
    let app = Router::new().route("/v1/responses/input_tokens", post(err_handler));
    let (base_url, shutdown) = spawn_server(app).await;

    let cache = Arc::new(RemoteCountCache::default());
    let client = OpenAiCountClient::new(
        reqwest::Client::new(),
        Some(base_url),
        Some("k".to_string()),
        cache,
        Duration::from_secs(2),
    );
    let req = text_request("gpt-4", "hi");
    let n = client.count("gpt-4", &req, true).await;
    assert!(n.is_none(), "5xx must surface as None for caller fallback");
    let _ = shutdown.send(());
}

// ─── 端到端:OpenAI 真实 HTTP + 双轨 + 失败降 tiktoken ───────────────────────

#[tokio::test]
async fn end_to_end_openai_hf_xenova_mapping_hit() {
    // 把 fixture tokenizer.json 预填到 cache,模型名 "gpt-4o" 命中内置 Xenova/gpt-4o mapping
    // OpenAi composite 应该优先走 HF,得到 fixture WordLevel 的编码结果(而非 tiktoken)
    const FIXTURE_JSON: &str = r#"{
      "version": "1.0",
      "truncation": null,
      "padding": null,
      "added_tokens": [{"id":0,"content":"[UNK]","single_word":false,"lstrip":false,"rstrip":false,"normalized":false,"special":true}],
      "normalizer": null,
      "pre_tokenizer": {"type": "Whitespace"},
      "post_processor": null,
      "decoder": null,
      "model": {"type":"WordLevel","vocab":{"[UNK]":0,"hello":1,"world":2,"user":3,"colon":4},"unk_token":"[UNK]"}
    }"#;
    let dir = TempDir::new().unwrap();
    // Xenova/gpt-4o → 替换 / 为 __ 后写入 cache
    let cache_path = dir.path().join("Xenova__gpt-4o").join("tokenizer.json");
    std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
    std::fs::write(&cache_path, FIXTURE_JSON).unwrap();

    let cfg = TokenizerConfig {
        offline: true, // 已有缓存,offline 不影响
        hf_cache_dir: Some(dir.path().to_path_buf()),
        ..TokenizerConfig::default()
    };
    let registry = TokenizerRegistry::new(cfg);

    // 纯文本请求 — 不会走远端;HF 主路径命中后,不会再 fallback 到 tiktoken
    let req = text_request("gpt-4o", "hello world");
    let n = registry.count_prompt("openai", "gpt-4o", &req).await;

    // 验证走的是 HF(WordLevel 编码),而非 tiktoken
    // "user:hello world" → Whitespace 切 ["user", ":", "hello", "world"] → 4 tokens(":" 不在词表 → [UNK])
    assert_eq!(n, 4, "HF path must hit Xenova/gpt-4o mapping (got {})", n);
}

#[tokio::test]
async fn end_to_end_openai_o1_no_xenova_falls_to_tiktoken() {
    // o1 没有 Xenova 端口,内置 mapping 返回 None → HfTokenizer 返回 None → tiktoken 兜底
    let (cfg, _dir) = test_config();
    let registry = TokenizerRegistry::new(cfg);
    let req = text_request("o1-preview", "hello world test foo");
    let n = registry.count_prompt("openai", "o1-preview", &req).await;
    assert!(n > 0, "tiktoken should handle o1-preview");
    // tiktoken 对 o1 模型 → 走默认 cl100k_base 编码;此处只断言有效计数
}

#[tokio::test]
async fn end_to_end_openai_dual_path_with_real_http_failure() {
    // 服务端永远 500 → 新链路:Remote 5xx → HF cache miss(offline=true 即时返回 None)→ tiktoken
    // 5xx → composite chain: Remote → HF (offline miss) → tiktoken
    let app = Router::new().route("/v1/responses/input_tokens", post(err_handler));
    let (base_url, shutdown) = spawn_server(app).await;

    let (mut cfg, _dir) = test_config();
    cfg.openai_endpoint = Some(base_url);
    cfg.openai_api_key = Some("k".to_string());
    let registry = TokenizerRegistry::new(cfg);

    let req = image_request("gpt-4o");
    let n = registry.count_prompt("openai", "gpt-4o", &req).await;
    assert!(n > 0, "should fall back through HF (miss) to tiktoken on 5xx");
    let est = estimate_from_request(&req);
    assert_ne!(n, est, "tiktoken count differs from char estimate");

    let _ = shutdown.send(());
}

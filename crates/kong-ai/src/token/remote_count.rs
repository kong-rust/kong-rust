//! Remote token-count clients — OpenAI / Anthropic / Gemini
//! 远端 token 计数客户端 — OpenAI / Anthropic / Gemini
//!
//! 三家都提供官方 count 端点:
//! - OpenAI:  POST /v1/responses/input_tokens   响应字段 `input_tokens`
//! - Anthropic: POST /v1/messages/count_tokens  响应字段 `input_tokens`
//! - Gemini:  POST /v1beta/models/{model}:countTokens?key=API_KEY  响应字段 `totalTokens`
//!
//! 共享 LRU 缓存(moka,容量 1024 / TTL 60s),key=(provider, model, has_non_text, sha256(prompt))。
//! All three share a moka LRU (capacity 1024, TTL 60s) keyed by (provider, model, has_non_text, sha256(prompt)).
//!
//! 单次 HTTP timeout 由调用方传入(默认 1s);整体 per-request deadline 由 [`TokenizerRegistry`] 包外层 300ms。
//! Per-call HTTP timeout supplied by caller (default 1s); overall per-request deadline (300ms)
//! is wrapped at the registry layer.
//!
//! 失败语义 / Failure semantics:
//! - OpenAI 失败 → tokenizer 层兜底 tiktoken(双轨)
//! - Anthropic / Gemini 失败 → registry 兜底字符估算(无本地兜底)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use moka::sync::Cache;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::codec::ChatRequest;

use super::tokenizer::extract_prompt_text;

// ============ LRU 缓存 ============

/// LRU 缓存 key — 区分 provider / 模型 / 是否多模态 / prompt 哈希
/// LRU cache key — distinguishes provider, model, multimodal flag, and prompt hash
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RemoteCountKey {
    pub provider: &'static str,
    pub model: String,
    pub has_non_text: bool,
    pub prompt_sha256: [u8; 32],
}

impl RemoteCountKey {
    /// 用 prompt 文本现场哈希构造 key — sha256(extract_prompt_text(request))
    /// Build a key by hashing the prompt text on the spot
    pub fn new(provider: &'static str, model: &str, has_non_text: bool, prompt: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(prompt.as_bytes());
        let digest = hasher.finalize();
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&digest);
        Self {
            provider,
            model: model.to_string(),
            has_non_text,
            prompt_sha256: sha,
        }
    }
}

/// 共享远端 count 结果缓存(moka:容量 + TTL)
/// Shared remote-count result cache (moka, capacity + TTL)
#[derive(Clone)]
pub struct RemoteCountCache {
    inner: Cache<RemoteCountKey, u64>,
}

impl RemoteCountCache {
    pub fn new(capacity: u64, ttl: Duration) -> Self {
        Self {
            inner: Cache::builder()
                .max_capacity(capacity)
                .time_to_live(ttl)
                .build(),
        }
    }

    pub fn get(&self, key: &RemoteCountKey) -> Option<u64> {
        self.inner.get(key)
    }

    pub fn put(&self, key: RemoteCountKey, value: u64) {
        self.inner.insert(key, value);
    }

    /// 用于单测:返回近似 entry 计数(moka 异步驱逐,值是估算)
    pub fn entry_count(&self) -> u64 {
        self.inner.entry_count()
    }
}

impl Default for RemoteCountCache {
    fn default() -> Self {
        Self::new(1024, Duration::from_secs(60))
    }
}

// ============ trait ============

/// 远端 token-count 客户端抽象 — 三家 provider 各一个实现
/// Remote token-count client abstraction — one impl per provider
#[async_trait]
pub trait RemoteCountClient: Send + Sync {
    /// 调用远端 API 计数;返回 None 表示失败/未配置/超时,由调用方降级
    /// Call the remote API; None signals failure/missing config/timeout for caller fallback
    async fn count(
        &self,
        model: &str,
        request: &ChatRequest,
        has_non_text: bool,
    ) -> Option<u64>;

    /// provider 标识(用于 LRU key 和日志)
    fn provider(&self) -> &'static str;
}

// ============ 共享下注:reqwest::Client + endpoint + api_key + cache + timeout ============

#[derive(Clone)]
struct RemoteCommon {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    cache: Arc<RemoteCountCache>,
    timeout: Duration,
}

impl RemoteCommon {
    fn new(
        http: reqwest::Client,
        base_url: String,
        api_key: Option<String>,
        cache: Arc<RemoteCountCache>,
        timeout: Duration,
    ) -> Self {
        Self {
            http,
            base_url,
            api_key,
            cache,
            timeout,
        }
    }

    fn cache_get(
        &self,
        provider: &'static str,
        model: &str,
        has_non_text: bool,
        prompt: &str,
    ) -> (RemoteCountKey, Option<u64>) {
        let key = RemoteCountKey::new(provider, model, has_non_text, prompt);
        let cached = self.cache.get(&key);
        (key, cached)
    }
}

// ============ OpenAI 客户端 ============

/// OpenAI Responses API count 客户端
/// Endpoint: POST {base}/v1/responses/input_tokens
pub struct OpenAiCountClient {
    common: RemoteCommon,
}

impl OpenAiCountClient {
    pub fn new(
        http: reqwest::Client,
        base_url: Option<String>,
        api_key: Option<String>,
        cache: Arc<RemoteCountCache>,
        timeout: Duration,
    ) -> Self {
        let base_url = base_url.unwrap_or_else(|| "https://api.openai.com".to_string());
        Self {
            common: RemoteCommon::new(http, base_url, api_key, cache, timeout),
        }
    }
}

#[async_trait]
impl RemoteCountClient for OpenAiCountClient {
    async fn count(
        &self,
        model: &str,
        request: &ChatRequest,
        has_non_text: bool,
    ) -> Option<u64> {
        // 缺 api_key → 不发请求(避免 401);双轨外层会自动兜底 tiktoken
        let api_key = self.common.api_key.as_deref()?;

        let prompt_text = extract_prompt_text(request);
        let (key, cached) = self
            .common
            .cache_get("openai", model, has_non_text, &prompt_text);
        if let Some(n) = cached {
            return Some(n);
        }

        // ChatRequest → Responses API input 格式:input 是消息数组
        // 全字段透传 — content 字符串/数组保持原结构,tool_calls / tool_call_id / name 都保留
        // ChatRequest → Responses input format — content (string or array) is preserved,
        // and tool_calls / tool_call_id / name are passed through unchanged
        let body = build_openai_responses_body(model, request);

        let url = format!(
            "{}/v1/responses/input_tokens",
            self.common.base_url.trim_end_matches('/')
        );
        let req = self
            .common
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&body);

        let result = tokio::time::timeout(self.common.timeout, req.send()).await;
        let resp = match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                warn!("openai count request failed: {}", e);
                return None;
            }
            Err(_) => {
                debug!("openai count request timed out");
                return None;
            }
        };

        if !resp.status().is_success() {
            warn!("openai count HTTP {}", resp.status());
            return None;
        }

        let json: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!("openai count body parse failed: {}", e);
                return None;
            }
        };
        // 优先级(按用户规则):input_tokens > usage.input_tokens > usage.prompt_tokens
        // Field priority per user spec: input_tokens > usage.input_tokens > usage.prompt_tokens
        let tokens = json
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                json.get("usage")
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(|v| v.as_u64())
            })
            .or_else(|| {
                json.get("usage")
                    .and_then(|u| u.get("prompt_tokens"))
                    .and_then(|v| v.as_u64())
            })?;

        self.common.cache.put(key, tokens);
        Some(tokens)
    }

    fn provider(&self) -> &'static str {
        "openai"
    }
}

/// Chat Completions ChatRequest → Responses API body 构造
/// Convert Chat Completions ChatRequest to Responses API request body
///
/// 不追求 100% 完美映射,目标是「端点能识别并返回 input_tokens 数」。
/// Tool 调用历史(tool_calls / tool_call_id)保留以便 token 计数包含工具开销。
/// We don't aim for perfect translation — the goal is "the endpoint accepts the body and
/// returns input_tokens." Tool-call history (tool_calls / tool_call_id) is preserved so
/// the count includes tool-related overhead.
pub fn build_openai_responses_body(model: &str, request: &ChatRequest) -> serde_json::Value {
    let mut input_arr = Vec::with_capacity(request.messages.len());
    for msg in &request.messages {
        let mut obj = serde_json::Map::new();
        obj.insert("role".to_string(), serde_json::json!(msg.role));
        if let Some(content) = &msg.content {
            // String / Array(text+image_url+input_audio+...)保持原结构
            obj.insert("content".to_string(), content.clone());
        }
        if let Some(tool_calls) = &msg.tool_calls {
            if let Ok(v) = serde_json::to_value(tool_calls) {
                obj.insert("tool_calls".to_string(), v);
            }
        }
        if let Some(tool_call_id) = &msg.tool_call_id {
            obj.insert("tool_call_id".to_string(), serde_json::json!(tool_call_id));
        }
        if let Some(name) = &msg.name {
            obj.insert("name".to_string(), serde_json::json!(name));
        }
        input_arr.push(serde_json::Value::Object(obj));
    }

    let mut body = serde_json::json!({
        "model": model,
        "input": input_arr,
    });

    if let Some(tools) = &request.tools {
        if let Ok(v) = serde_json::to_value(tools) {
            body["tools"] = v;
        }
    }
    if let Some(tool_choice) = &request.tool_choice {
        body["tool_choice"] = tool_choice.clone();
    }
    // 旧版 functions / function_call 字段透传(从 ChatRequest.extra 出)
    for legacy_key in ["functions", "function_call"] {
        if let Some(v) = request.extra.get(legacy_key) {
            body[legacy_key] = v.clone();
        }
    }

    body
}

// ============ Anthropic 客户端 ============

/// Anthropic count_tokens 客户端
/// Endpoint: POST {base}/v1/messages/count_tokens
pub struct AnthropicCountClient {
    common: RemoteCommon,
    /// anthropic-version header,默认 "2023-06-01"
    api_version: String,
}

impl AnthropicCountClient {
    pub fn new(
        http: reqwest::Client,
        base_url: Option<String>,
        api_key: Option<String>,
        cache: Arc<RemoteCountCache>,
        timeout: Duration,
    ) -> Self {
        let base_url = base_url.unwrap_or_else(|| "https://api.anthropic.com".to_string());
        Self {
            common: RemoteCommon::new(http, base_url, api_key, cache, timeout),
            api_version: "2023-06-01".to_string(),
        }
    }
}

#[async_trait]
impl RemoteCountClient for AnthropicCountClient {
    async fn count(
        &self,
        model: &str,
        request: &ChatRequest,
        has_non_text: bool,
    ) -> Option<u64> {
        let api_key = self.common.api_key.as_deref()?;

        let prompt_text = extract_prompt_text(request);
        let (key, cached) =
            self.common
                .cache_get("anthropic", model, has_non_text, &prompt_text);
        if let Some(n) = cached {
            return Some(n);
        }

        // 把 system role 提到顶层 system 字段(Anthropic 的格式约定)
        // Move system messages to the top-level `system` field per Anthropic API
        let mut sys_text = String::new();
        let mut messages_arr = Vec::with_capacity(request.messages.len());
        for msg in &request.messages {
            if msg.role == "system" {
                if let Some(content) = &msg.content {
                    if let Some(s) = content.as_str() {
                        if !sys_text.is_empty() {
                            sys_text.push('\n');
                        }
                        sys_text.push_str(s);
                    }
                }
                continue;
            }
            let mut obj = serde_json::Map::new();
            obj.insert("role".to_string(), serde_json::json!(msg.role));
            if let Some(content) = &msg.content {
                obj.insert("content".to_string(), content.clone());
            }
            messages_arr.push(serde_json::Value::Object(obj));
        }

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages_arr,
        });
        if !sys_text.is_empty() {
            body["system"] = serde_json::json!(sys_text);
        }
        if let Some(tools) = &request.tools {
            if let Ok(v) = serde_json::to_value(tools) {
                body["tools"] = v;
            }
        }

        let url = format!(
            "{}/v1/messages/count_tokens",
            self.common.base_url.trim_end_matches('/')
        );
        let req = self
            .common
            .http
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", &self.api_version)
            .header("Content-Type", "application/json")
            .json(&body);

        let result = tokio::time::timeout(self.common.timeout, req.send()).await;
        let resp = match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                warn!("anthropic count request failed: {}", e);
                return None;
            }
            Err(_) => {
                debug!("anthropic count request timed out");
                return None;
            }
        };

        if !resp.status().is_success() {
            warn!("anthropic count HTTP {}", resp.status());
            return None;
        }

        let json: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!("anthropic count body parse failed: {}", e);
                return None;
            }
        };
        let tokens = json.get("input_tokens").and_then(|v| v.as_u64())?;

        self.common.cache.put(key, tokens);
        Some(tokens)
    }

    fn provider(&self) -> &'static str {
        "anthropic"
    }
}

// ============ Gemini 客户端 ============

/// Gemini countTokens 客户端
/// Endpoint: POST {base}/v1beta/models/{model}:countTokens?key=API_KEY
pub struct GeminiCountClient {
    common: RemoteCommon,
}

impl GeminiCountClient {
    pub fn new(
        http: reqwest::Client,
        base_url: Option<String>,
        api_key: Option<String>,
        cache: Arc<RemoteCountCache>,
        timeout: Duration,
    ) -> Self {
        let base_url = base_url
            .unwrap_or_else(|| "https://generativelanguage.googleapis.com".to_string());
        Self {
            common: RemoteCommon::new(http, base_url, api_key, cache, timeout),
        }
    }
}

#[async_trait]
impl RemoteCountClient for GeminiCountClient {
    async fn count(
        &self,
        model: &str,
        request: &ChatRequest,
        has_non_text: bool,
    ) -> Option<u64> {
        let api_key = self.common.api_key.as_deref()?;

        let prompt_text = extract_prompt_text(request);
        let (key, cached) = self
            .common
            .cache_get("gemini", model, has_non_text, &prompt_text);
        if let Some(n) = cached {
            return Some(n);
        }

        // ChatRequest → Gemini contents 格式
        // Gemini 的角色:user / model;system 提示通过 systemInstruction 字段传
        let mut sys_text = String::new();
        let mut contents = Vec::with_capacity(request.messages.len());
        for msg in &request.messages {
            if msg.role == "system" {
                if let Some(content) = &msg.content {
                    if let Some(s) = content.as_str() {
                        if !sys_text.is_empty() {
                            sys_text.push('\n');
                        }
                        sys_text.push_str(s);
                    }
                }
                continue;
            }
            // Map roles: assistant -> model
            let role = if msg.role == "assistant" {
                "model"
            } else {
                "user"
            };
            // 文本 part — 数组型 content 取所有 text part 拼接
            let mut text = String::new();
            if let Some(content) = &msg.content {
                match content {
                    serde_json::Value::String(s) => text.push_str(s),
                    serde_json::Value::Array(arr) => {
                        for part in arr {
                            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    text.push(' ');
                                }
                                text.push_str(t);
                            }
                        }
                    }
                    other => text.push_str(&other.to_string()),
                }
            }
            contents.push(serde_json::json!({
                "role": role,
                "parts": [{"text": text}]
            }));
        }

        let mut body = serde_json::json!({ "contents": contents });
        if !sys_text.is_empty() {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{"text": sys_text}]
            });
        }

        let url = format!(
            "{}/v1beta/models/{}:countTokens?key={}",
            self.common.base_url.trim_end_matches('/'),
            model,
            api_key
        );
        let req = self
            .common
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body);

        let result = tokio::time::timeout(self.common.timeout, req.send()).await;
        let resp = match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                warn!("gemini count request failed: {}", e);
                return None;
            }
            Err(_) => {
                debug!("gemini count request timed out");
                return None;
            }
        };

        if !resp.status().is_success() {
            warn!("gemini count HTTP {}", resp.status());
            return None;
        }

        let json: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!("gemini count body parse failed: {}", e);
                return None;
            }
        };
        let tokens = json.get("totalTokens").and_then(|v| v.as_u64())?;

        self.common.cache.put(key, tokens);
        Some(tokens)
    }

    fn provider(&self) -> &'static str {
        "gemini"
    }
}

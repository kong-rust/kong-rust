//! TokenizerRegistry — 中心化 tokenizer 路由 + 全局单例
//! Centralized tokenizer routing with global singleton for plugin access.
//!
//! 职责 / Responsibilities:
//! 1. 按 (provider_type, model) 选择 strategy(配置 mapping > provider_type 默认)
//!    Pick a strategy for (provider_type, model): config mapping > provider_type default
//! 2. 缓存每个 strategy 对应的 tokenizer 实例
//!    Cache one tokenizer instance per strategy
//! 3. 在 per-request deadline 内运行 tokenizer,超时/失败兜底字符估算
//!    Run tokenizer under per-request deadline; fall back to char estimation on miss

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use regex::Regex;

use crate::codec::ChatRequest;

use super::counter::TokenCounter;
use super::tokenizer::{
    estimate_from_request, NoopTokenizer, OpenAiTokenizer, PromptTokenizer, TiktokenTokenizer,
};

/// Tokenizer 配置 — 由 kong.conf [ai.tokenizer] 段加载
#[derive(Debug, Clone)]
pub struct TokenizerConfig {
    /// per-request 整体 deadline(包含远端 API 的网络往返时间)
    /// per-request total deadline (includes remote API round trip)
    pub per_request_deadline: Duration,
    /// 远端 count API 单次 HTTP timeout
    /// remote count API per-call HTTP timeout
    pub remote_count_timeout: Duration,
    /// 模型名 → strategy 显式映射(优先级最高)
    /// model name → strategy explicit mapping (highest precedence)
    pub mappings: Vec<TokenizerMapping>,
    /// 远端 count 结果的 LRU 缓存容量
    /// LRU cache capacity for remote count results
    pub cache_capacity: u64,
    /// 远端 count 结果的 LRU TTL
    /// LRU TTL for remote count results
    pub cache_ttl: Duration,
}

impl Default for TokenizerConfig {
    fn default() -> Self {
        Self {
            // 用户调整后的默认值 — adjusted defaults per requirements
            per_request_deadline: Duration::from_millis(300),
            remote_count_timeout: Duration::from_secs(1),
            mappings: Vec::new(),
            cache_capacity: 1024,
            cache_ttl: Duration::from_secs(60),
        }
    }
}

/// 模型名 → strategy 映射(可选携带 HF repo id)
#[derive(Debug, Clone)]
pub struct TokenizerMapping {
    /// 正则表达式,匹配 model 名 — regex pattern matching model name
    pub pattern: String,
    /// 命中后使用的 strategy
    pub strategy: TokenizerStrategy,
    /// HuggingFace repo id(strategy=HuggingFace 时使用,Step 3 实装)
    pub hf_repo_id: Option<String>,
}

/// Tokenizer 策略 — 决定走哪条优先级链
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenizerStrategy {
    /// OpenAI 系:远端 OpenAI count → tiktoken-rs → 字符估算
    OpenAi,
    /// Anthropic Claude:远端 count_tokens → 字符估算
    Anthropic,
    /// Google Gemini:远端 countTokens → 字符估算
    Gemini,
    /// HuggingFace 开源模型:本地 tokenizer.json 编码 → 字符估算
    HuggingFace,
    /// 直接 tiktoken-rs(用于 vLLM/Ollama 等 OpenAI 兼容接口托管的模型)
    Tiktoken,
    /// 强制字符估算
    Estimate,
}

/// Tokenizer 注册表
pub struct TokenizerRegistry {
    config: TokenizerConfig,
    /// 编译后的 mapping 正则(避免每次请求重新编译)
    /// Pre-compiled mapping regex (avoid per-request recompile)
    compiled_mappings: Vec<(Regex, TokenizerStrategy, Option<String>)>,
    /// 按 strategy 缓存 tokenizer 实例
    /// Tokenizer instance cache keyed by strategy
    tokenizers: DashMap<TokenizerStrategy, Arc<dyn PromptTokenizer>>,
}

impl TokenizerRegistry {
    /// 用配置构造 registry,提前编译 mapping 正则
    pub fn new(config: TokenizerConfig) -> Self {
        let compiled_mappings = config
            .mappings
            .iter()
            .filter_map(|m| {
                Regex::new(&m.pattern)
                    .ok()
                    .map(|re| (re, m.strategy, m.hf_repo_id.clone()))
            })
            .collect();

        Self {
            config,
            compiled_mappings,
            tokenizers: DashMap::new(),
        }
    }

    /// 主入口:基于已解析的 ChatRequest 计算 prompt token
    /// Primary entry point: count prompt tokens from a parsed ChatRequest
    pub async fn count_prompt(
        &self,
        provider_type: &str,
        model: &str,
        request: &ChatRequest,
    ) -> u64 {
        let strategy = self.resolve_strategy(provider_type, model);
        let tokenizer = self.tokenizer_for(strategy);
        let deadline = self.config.per_request_deadline;

        match tokio::time::timeout(deadline, tokenizer.count_prompt(model, request)).await {
            Ok(Some(n)) => n,
            // 超时 / 实现返回 None / 任何 panic-free 错误 → 字符估算兜底
            // Timeout / None / any non-panic error → char estimation fallback
            _ => estimate_from_request(request),
        }
    }

    /// 从 raw body 计算 — ai-rate-limit access 阶段先于 ai-proxy 执行,需要自己解析
    /// Count from raw body — ai-rate-limit runs before ai-proxy, so it must self-parse
    pub async fn count_prompt_from_body(
        &self,
        provider_type: &str,
        model: &str,
        body: &str,
    ) -> u64 {
        match serde_json::from_str::<ChatRequest>(body) {
            Ok(req) => {
                // 当调用方未指定 model 时,从 body 中取
                let effective_model = if model.is_empty() {
                    req.model.as_str()
                } else {
                    model
                };
                self.count_prompt(provider_type, effective_model, &req).await
            }
            // body 不是合法 ChatRequest(可能是 Anthropic/responses 格式或损坏的请求)
            // → 退化到 byte-length 估算
            Err(_) => TokenCounter::count_estimate(body),
        }
    }

    /// 从 model 名启发式推断 provider type — ai-rate-limit 不知道 provider 时使用
    /// Infer provider type from model name — used when caller doesn't have provider context
    ///
    /// Heuristic:
    /// - gpt-* / o1-* / o3-* / text-embedding-* → openai
    /// - claude-* → anthropic
    /// - gemini-* → gemini
    /// - 含 / 或 qwen / llama / mistral / deepseek → huggingface
    /// - 其他 → openai_compat(走 tiktoken)
    pub fn infer_provider_type(model: &str) -> &'static str {
        let m = model.to_ascii_lowercase();
        if m.starts_with("gpt-")
            || m.starts_with("o1")
            || m.starts_with("o3")
            || m.starts_with("o4")
            || m.starts_with("text-embedding-")
            || m.starts_with("davinci")
            || m.starts_with("babbage")
        {
            "openai"
        } else if m.starts_with("claude-") || m.starts_with("claude_") {
            "anthropic"
        } else if m.starts_with("gemini") {
            "gemini"
        } else if m.contains('/')
            || m.contains("qwen")
            || m.contains("llama")
            || m.contains("mistral")
            || m.contains("deepseek")
            || m.contains("yi-")
            || m.contains("phi-")
        {
            "huggingface"
        } else {
            "openai_compat"
        }
    }

    /// 把 model 名解析为 HuggingFace repo_id(Step 3 用)
    /// Resolve a model name to a HuggingFace repo_id (used in step 3+)
    pub fn resolve_hf_repo(&self, model: &str) -> Option<String> {
        for (re, _, repo) in &self.compiled_mappings {
            if re.is_match(model) {
                if let Some(repo) = repo {
                    return Some(repo.clone());
                }
                break;
            }
        }
        if model.contains('/') {
            return Some(model.to_string());
        }
        None
    }

    fn resolve_strategy(&self, provider_type: &str, model: &str) -> TokenizerStrategy {
        // 1. 配置 mapping(优先级最高)— config mapping wins
        for (re, strategy, _) in &self.compiled_mappings {
            if re.is_match(model) {
                return *strategy;
            }
        }

        // 2. 按 provider_type 默认 — provider_type defaults
        match provider_type {
            "openai" => TokenizerStrategy::OpenAi,
            "anthropic" => TokenizerStrategy::Anthropic,
            "gemini" => TokenizerStrategy::Gemini,
            "huggingface" => TokenizerStrategy::HuggingFace,
            // OpenAI 兼容接口(vLLM、Ollama 等)— 直接 tiktoken,无远端
            "openai_compat" => TokenizerStrategy::Tiktoken,
            _ => TokenizerStrategy::Estimate,
        }
    }

    fn tokenizer_for(&self, strategy: TokenizerStrategy) -> Arc<dyn PromptTokenizer> {
        if let Some(t) = self.tokenizers.get(&strategy) {
            return t.clone();
        }
        let t: Arc<dyn PromptTokenizer> = match strategy {
            // OpenAi 双轨:OpenAiTokenizer 内部按 has_non_text_content 分流
            // (step 1 远端是 stub,实际仍落 tiktoken;step 4 填入 remote 调用)
            // OpenAi dual-path: OpenAiTokenizer routes by has_non_text_content
            // (step 1 keeps the remote branch as a no-op until step 4 wires reqwest)
            TokenizerStrategy::OpenAi => Arc::new(OpenAiTokenizer::new()),
            // Tiktoken 直路 — 用于 vLLM / Ollama 等 OpenAI 兼容接口托管的开源模型(无远端 API)
            // Direct tiktoken — for open-source models served via OpenAI-compatible endpoints
            TokenizerStrategy::Tiktoken => Arc::new(TiktokenTokenizer),
            // Step 3 实装 HuggingFace,Step 4 实装 Anthropic/Gemini 远端
            // Stubs until step 3 (HF) and step 4 (remote count) land
            TokenizerStrategy::HuggingFace
            | TokenizerStrategy::Anthropic
            | TokenizerStrategy::Gemini
            | TokenizerStrategy::Estimate => Arc::new(NoopTokenizer),
        };
        self.tokenizers.insert(strategy, t.clone());
        t
    }
}

impl Default for TokenizerRegistry {
    fn default() -> Self {
        Self::new(TokenizerConfig::default())
    }
}

// ============ 全局单例 — global singleton ============

static GLOBAL_REGISTRY: std::sync::OnceLock<Arc<TokenizerRegistry>> = std::sync::OnceLock::new();

/// 注册全局 tokenizer registry — 由 kong-server 启动时调用一次
/// Set the global tokenizer registry — call once during kong-server startup
pub fn set_global_registry(registry: Arc<TokenizerRegistry>) {
    let _ = GLOBAL_REGISTRY.set(registry);
}

/// 获取全局 tokenizer registry — 未注册时返回 None,插件应降级
/// Get the global tokenizer registry; plugins should fall back when None
pub fn global_registry() -> Option<Arc<TokenizerRegistry>> {
    GLOBAL_REGISTRY.get().cloned()
}

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
use super::hf_loader::{default_cache_dir, HfDownloader, HfLoader};
use super::remote_count::{
    AnthropicCountClient, GeminiCountClient, OpenAiCountClient, RemoteCountCache,
    RemoteCountClient,
};
use super::tokenizer::{
    estimate_from_request, openai_default_xenova_repo, AnthropicTokenizer, GeminiTokenizer,
    HfTokenizer, NoopTokenizer, OpenAiTokenizer, PromptTokenizer, TiktokenTokenizer,
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
    /// HuggingFace tokenizer.json 缓存目录
    /// HuggingFace tokenizer.json cache directory
    pub hf_cache_dir: Option<std::path::PathBuf>,
    /// 离线模式:仅读 HF 缓存,不发起下载
    /// offline mode: read HF cache only, no downloads
    pub offline: bool,
    /// 模型名 → strategy 显式映射(优先级最高)
    /// model name → strategy explicit mapping (highest precedence)
    pub mappings: Vec<TokenizerMapping>,
    /// 远端 count 结果的 LRU 缓存容量
    /// LRU cache capacity for remote count results
    pub cache_capacity: u64,
    /// 远端 count 结果的 LRU TTL
    /// LRU TTL for remote count results
    pub cache_ttl: Duration,
    /// OpenAI Responses input_tokens 端点 base URL(默认 https://api.openai.com)
    /// OpenAI Responses input_tokens base URL (default https://api.openai.com)
    pub openai_endpoint: Option<String>,
    /// OpenAI API key(为空时双轨退化为 HF + tiktoken,远端不启用)
    /// OpenAI API key — when absent, dual-path falls back to HF + tiktoken
    pub openai_api_key: Option<String>,
    /// Anthropic count_tokens 端点 base URL(默认 https://api.anthropic.com)
    pub anthropic_endpoint: Option<String>,
    /// Anthropic API key(为空时 Anthropic 永远走字符估算)
    pub anthropic_api_key: Option<String>,
    /// Gemini countTokens 端点 base URL(默认 https://generativelanguage.googleapis.com)
    pub gemini_endpoint: Option<String>,
    /// Gemini API key(为空时 Gemini 永远走字符估算)
    pub gemini_api_key: Option<String>,
}

impl TokenizerConfig {
    /// 从 KongConfig 派生 TokenizerConfig — 由 kong-server 启动时调用
    /// Build TokenizerConfig from KongConfig (called once at kong-server startup)
    pub fn from_kong_config(cfg: &kong_config::KongConfig) -> Self {
        Self {
            per_request_deadline: Duration::from_millis(cfg.ai_tokenizer_per_request_deadline_ms),
            remote_count_timeout: Duration::from_millis(cfg.ai_tokenizer_remote_count_timeout_ms),
            hf_cache_dir: cfg.ai_tokenizer_cache_dir.as_ref().map(std::path::PathBuf::from),
            offline: cfg.ai_tokenizer_offline,
            mappings: Vec::new(),
            cache_capacity: cfg.ai_tokenizer_cache_capacity,
            cache_ttl: Duration::from_secs(cfg.ai_tokenizer_cache_ttl_seconds),
            openai_endpoint: cfg.ai_tokenizer_openai_endpoint.clone(),
            openai_api_key: cfg.ai_tokenizer_openai_api_key.clone(),
            anthropic_endpoint: cfg.ai_tokenizer_anthropic_endpoint.clone(),
            anthropic_api_key: cfg.ai_tokenizer_anthropic_api_key.clone(),
            gemini_endpoint: cfg.ai_tokenizer_gemini_endpoint.clone(),
            gemini_api_key: cfg.ai_tokenizer_gemini_api_key.clone(),
        }
    }
}

impl Default for TokenizerConfig {
    fn default() -> Self {
        Self {
            // 用户调整后的默认值 — adjusted defaults per requirements
            per_request_deadline: Duration::from_millis(300),
            remote_count_timeout: Duration::from_secs(1),
            hf_cache_dir: None,
            offline: false,
            mappings: Vec::new(),
            cache_capacity: 1024,
            cache_ttl: Duration::from_secs(60),
            openai_endpoint: None,
            openai_api_key: None,
            anthropic_endpoint: None,
            anthropic_api_key: None,
            gemini_endpoint: None,
            gemini_api_key: None,
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
    /// HuggingFace 本地 tokenizer 加载器(磁盘缓存 + 单飞下载)
    /// HuggingFace local tokenizer loader (disk cache + single-flight download)
    hf_loader: Arc<HfLoader>,
    /// 远端 count 客户端(OpenAI / Anthropic / Gemini 共享)
    /// Remote count clients (Option — None when api_key 缺失或测试场景)
    openai_remote: Option<Arc<dyn RemoteCountClient>>,
    anthropic_remote: Option<Arc<dyn RemoteCountClient>>,
    gemini_remote: Option<Arc<dyn RemoteCountClient>>,
    /// 共享 LRU 缓存(三家 provider 共用)
    /// Shared LRU cache (used by all remote clients)
    #[allow(dead_code)]
    remote_cache: Arc<RemoteCountCache>,
}

impl TokenizerRegistry {
    /// 用配置构造 registry — 自动建好 HfLoader + 三个远端 client(基于 reqwest + 共享 LRU)
    /// Build registry from config — sets up HfLoader and three remote clients
    pub fn new(config: TokenizerConfig) -> Self {
        let cache_dir = config
            .hf_cache_dir
            .clone()
            .unwrap_or_else(default_cache_dir);
        let hf_loader = Arc::new(HfLoader::new(cache_dir, config.offline));

        // 共享 reqwest::Client + LRU
        let http = reqwest::Client::builder()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let remote_cache = Arc::new(RemoteCountCache::new(
            config.cache_capacity,
            config.cache_ttl,
        ));
        let timeout = config.remote_count_timeout;

        // 缺 api_key 时不构造 client(行为等同未启用),避免无意义请求
        // Skip building a client when api_key is absent — equivalent to disabled
        let openai_remote: Option<Arc<dyn RemoteCountClient>> = config
            .openai_api_key
            .as_ref()
            .map(|_| {
                Arc::new(OpenAiCountClient::new(
                    http.clone(),
                    config.openai_endpoint.clone(),
                    config.openai_api_key.clone(),
                    remote_cache.clone(),
                    timeout,
                )) as Arc<dyn RemoteCountClient>
            });
        let anthropic_remote: Option<Arc<dyn RemoteCountClient>> = config
            .anthropic_api_key
            .as_ref()
            .map(|_| {
                Arc::new(AnthropicCountClient::new(
                    http.clone(),
                    config.anthropic_endpoint.clone(),
                    config.anthropic_api_key.clone(),
                    remote_cache.clone(),
                    timeout,
                )) as Arc<dyn RemoteCountClient>
            });
        let gemini_remote: Option<Arc<dyn RemoteCountClient>> = config
            .gemini_api_key
            .as_ref()
            .map(|_| {
                Arc::new(GeminiCountClient::new(
                    http.clone(),
                    config.gemini_endpoint.clone(),
                    config.gemini_api_key.clone(),
                    remote_cache.clone(),
                    timeout,
                )) as Arc<dyn RemoteCountClient>
            });

        Self::build(
            config,
            hf_loader,
            openai_remote,
            anthropic_remote,
            gemini_remote,
            remote_cache,
        )
    }

    /// 注入式构造 — 测试或自定义 HfLoader 用
    /// Inject a custom HfLoader (e.g. with a mock downloader for tests)
    pub fn with_hf_loader(config: TokenizerConfig, hf_loader: Arc<HfLoader>) -> Self {
        // 远端 client 走默认构造逻辑(同 new),允许测试只关心 HF 行为
        let mut reg = Self::new(config);
        reg.hf_loader = hf_loader;
        reg.tokenizers.clear();
        reg
    }

    /// 注入式构造 — 直接传入 HfDownloader 实现(自动包装 HfLoader)
    pub fn with_hf_downloader(
        config: TokenizerConfig,
        downloader: Arc<dyn HfDownloader>,
    ) -> Self {
        let cache_dir = config
            .hf_cache_dir
            .clone()
            .unwrap_or_else(default_cache_dir);
        let hf_loader = Arc::new(HfLoader::with_downloader(
            cache_dir,
            config.offline,
            downloader,
        ));
        Self::with_hf_loader(config, hf_loader)
    }

    /// 注入式构造 — 测试用,可指定每个 provider 的远端 client(传入 mock)
    /// For tests: inject specific RemoteCountClient implementations per provider
    pub fn with_remote_clients(
        config: TokenizerConfig,
        openai_remote: Option<Arc<dyn RemoteCountClient>>,
        anthropic_remote: Option<Arc<dyn RemoteCountClient>>,
        gemini_remote: Option<Arc<dyn RemoteCountClient>>,
    ) -> Self {
        let cache_dir = config
            .hf_cache_dir
            .clone()
            .unwrap_or_else(default_cache_dir);
        let hf_loader = Arc::new(HfLoader::new(cache_dir, config.offline));
        let remote_cache = Arc::new(RemoteCountCache::new(
            config.cache_capacity,
            config.cache_ttl,
        ));
        Self::build(
            config,
            hf_loader,
            openai_remote,
            anthropic_remote,
            gemini_remote,
            remote_cache,
        )
    }

    fn build(
        config: TokenizerConfig,
        hf_loader: Arc<HfLoader>,
        openai_remote: Option<Arc<dyn RemoteCountClient>>,
        anthropic_remote: Option<Arc<dyn RemoteCountClient>>,
        gemini_remote: Option<Arc<dyn RemoteCountClient>>,
        remote_cache: Arc<RemoteCountCache>,
    ) -> Self {
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
            hf_loader,
            openai_remote,
            anthropic_remote,
            gemini_remote,
            remote_cache,
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

    /// 构造通用 HF repo_resolver — 配置 mapping > model 含 `/` → 直接当 repo_id
    fn build_hf_repo_resolver(&self) -> Arc<dyn Fn(&str) -> Option<String> + Send + Sync> {
        let mappings: Vec<(Regex, TokenizerStrategy, Option<String>)> = self
            .compiled_mappings
            .iter()
            .map(|(r, s, h)| (r.clone(), *s, h.clone()))
            .collect();
        Arc::new(move |model: &str| -> Option<String> {
            for (re, _, repo) in mappings.iter() {
                if re.is_match(model) {
                    if let Some(repo) = repo {
                        return Some(repo.clone());
                    }
                    break;
                }
            }
            if model.contains('/') {
                Some(model.to_string())
            } else {
                None
            }
        })
    }

    /// 构造 OpenAI 专用 HF tokenizer — 内置 Xenova mapping(用户 mapping 优先,内置兜底)
    /// Build OpenAI-specific HF tokenizer with built-in Xenova mapping
    /// (user-supplied mapping wins; built-in is the fallback)
    fn build_openai_hf_tokenizer(&self) -> Arc<HfTokenizer> {
        let mappings: Vec<(Regex, TokenizerStrategy, Option<String>)> = self
            .compiled_mappings
            .iter()
            .map(|(r, s, h)| (r.clone(), *s, h.clone()))
            .collect();
        let resolver = Arc::new(move |model: &str| -> Option<String> {
            // 1. 用户配置的 mapping 优先
            for (re, _, repo) in mappings.iter() {
                if re.is_match(model) {
                    if let Some(repo) = repo {
                        return Some(repo.clone());
                    }
                    break;
                }
            }
            // 2. 内置 OpenAI Xenova 默认映射(o1/o3/o4 返回 None,让 tiktoken 兜底)
            if let Some(repo) = openai_default_xenova_repo(model) {
                return Some(repo);
            }
            // 3. model 含 `/` → 直接当 repo_id(罕见场景)
            if model.contains('/') {
                return Some(model.to_string());
            }
            None
        });
        Arc::new(HfTokenizer::new(self.hf_loader.clone(), resolver))
    }

    fn tokenizer_for(&self, strategy: TokenizerStrategy) -> Arc<dyn PromptTokenizer> {
        if let Some(t) = self.tokenizers.get(&strategy) {
            return t.clone();
        }
        let t: Arc<dyn PromptTokenizer> = match strategy {
            // OpenAi 组合 tokenizer:HF 主路径(Xenova 系列)+ tiktoken 兜底 + 非文本远端叠加
            // OpenAi composite: HF primary (Xenova) + tiktoken fallback + remote on multimodal
            TokenizerStrategy::OpenAi => {
                let hf = self.build_openai_hf_tokenizer();
                let remote = self.openai_remote.clone();
                Arc::new(OpenAiTokenizer::with_hf_and_remote(hf, remote))
            }
            // Tiktoken 直路 — 用于 vLLM / Ollama 等 OpenAI 兼容接口托管的开源模型(无远端 API)
            TokenizerStrategy::Tiktoken => Arc::new(TiktokenTokenizer),
            // HuggingFace 走真实 HfTokenizer — 注入共享 HfLoader + repo_resolver closure
            TokenizerStrategy::HuggingFace => {
                Arc::new(HfTokenizer::new(self.hf_loader.clone(), self.build_hf_repo_resolver()))
            }
            // Anthropic/Gemini:有 remote 走真实 client;没 remote → Noop(让 registry estimate 兜底)
            // Anthropic/Gemini: real client if configured; else Noop (registry falls back to estimate)
            TokenizerStrategy::Anthropic => match &self.anthropic_remote {
                Some(r) => Arc::new(AnthropicTokenizer::new(r.clone())),
                None => Arc::new(NoopTokenizer),
            },
            TokenizerStrategy::Gemini => match &self.gemini_remote {
                Some(r) => Arc::new(GeminiTokenizer::new(r.clone())),
                None => Arc::new(NoopTokenizer),
            },
            TokenizerStrategy::Estimate => Arc::new(NoopTokenizer),
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

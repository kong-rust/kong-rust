//! PromptTokenizer abstraction — async trait + helpers
//! PromptTokenizer 抽象 — 异步 trait,按 strategy 路由到不同实现
//!
//! Strategy 决定如何精确计算 prompt token:
//! - OpenAi: 双轨 — 纯文本走 tiktoken-rs(零延迟),多模态/工具调用走远端 OpenAI count API
//!           远端失败兜底 tiktoken;tiktoken 也失败兜底字符估算
//! - Anthropic / Gemini: 远端 count API → 字符估算(无本地 tokenizer)
//! - HuggingFace: 本地 tokenizers crate 编码(step 3 实装,目前 stub)
//! - Tiktoken: 直接 tiktoken-rs(开源模型走 OpenAI 兼容接口时用)
//! - Estimate: 占位,永远返回 None

use std::sync::Arc;

use async_trait::async_trait;

use crate::codec::ChatRequest;

use super::hf_loader::HfLoader;
use super::remote_count::RemoteCountClient;

/// Async tokenizer — 异步 tokenizer
/// `count_prompt` 返回 None 表示该实现未能精确计算,由 registry 兜底走字符估算
#[async_trait]
pub trait PromptTokenizer: Send + Sync {
    /// 精确计算 prompt token,失败/不支持返回 None
    async fn count_prompt(&self, model: &str, request: &ChatRequest) -> Option<u64>;

    /// 调试日志用名称
    fn name(&self) -> &str;
}

// ============ 占位 / 直接 tiktoken / OpenAI 双轨 ============

/// 占位 tokenizer — 永远返回 None,触发外层 estimate 兜底
pub struct NoopTokenizer;

#[async_trait]
impl PromptTokenizer for NoopTokenizer {
    async fn count_prompt(&self, _model: &str, _request: &ChatRequest) -> Option<u64> {
        None
    }

    fn name(&self) -> &str {
        "noop"
    }
}

/// 直接 tiktoken-rs — 用于 OpenAI 兼容接口托管的开源模型(无远端 API)
/// Direct tiktoken-rs path — used for OpenAI-compatible endpoints serving open-source models
pub struct TiktokenTokenizer;

#[async_trait]
impl PromptTokenizer for TiktokenTokenizer {
    async fn count_prompt(&self, model: &str, request: &ChatRequest) -> Option<u64> {
        count_with_tiktoken(model, request)
    }

    fn name(&self) -> &str {
        "tiktoken"
    }
}

/// OpenAI 组合 tokenizer — 主路径 HF 本地编码,失败/Pending 时降到 tiktoken;非文本时叠加远端 count API
/// OpenAI composite tokenizer — primary local path is HF encoding;
/// non-text inputs prefer remote API; tiktoken is the final local fallback.
///
/// 优先级链 / Priority chain:
/// 1. has_non_text == true 且配置了 remote_client → 远端 `/v1/responses/input_tokens`(成功直返)
/// 2. HF 主路径(Xenova/gpt-4o 等内置 mapping)— HfLoader 命中直接编码
/// 3. tiktoken-rs(GPT 系兜底,o1/o3 等无 Xenova 端口的模型也走这条)
/// 4. 全失败 → registry 字符估算兜底
///
/// 不再区分纯文本 / 多模态作为本地路径选择的唯一开关 —— 多模态优先走远端,但本地永远是
/// 「HF 优先,tiktoken 兜底」。
pub struct OpenAiTokenizer {
    /// HF 主路径(由 registry 注入,内置 OpenAI Xenova mapping)
    /// HF primary path injected by registry, with built-in OpenAI Xenova mapping
    hf: Option<Arc<HfTokenizer>>,
    /// 远端 count client(可选 — has_non_text=true 时优先尝试)
    /// Remote count client (optional) — preferred when has_non_text=true
    remote: Option<Arc<dyn RemoteCountClient>>,
}

impl OpenAiTokenizer {
    pub fn new() -> Self {
        Self {
            hf: None,
            remote: None,
        }
    }

    /// 注入远端 count client(由 registry 构造时传入)
    pub fn with_remote(remote: Arc<dyn RemoteCountClient>) -> Self {
        Self {
            hf: None,
            remote: Some(remote),
        }
    }

    /// 注入 HF 主路径 + 可选远端 client
    /// Inject HF primary path + optional remote client
    pub fn with_hf_and_remote(
        hf: Arc<HfTokenizer>,
        remote: Option<Arc<dyn RemoteCountClient>>,
    ) -> Self {
        Self {
            hf: Some(hf),
            remote,
        }
    }
}

impl Default for OpenAiTokenizer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PromptTokenizer for OpenAiTokenizer {
    async fn count_prompt(&self, model: &str, request: &ChatRequest) -> Option<u64> {
        let has_non_text = has_non_text_content(request);

        // 优先级 1:非文本 → 远端 `/v1/responses/input_tokens`
        if has_non_text {
            if let Some(remote) = &self.remote {
                if let Some(n) = remote.count(model, request, true).await {
                    return Some(n);
                }
                // 远端失败/超时/缺 key → 继续走本地链路
            }
        }

        // 优先级 2:HF 主路径(Xenova/gpt-4o 等)
        if let Some(hf) = &self.hf {
            if let Some(n) = hf.count_prompt(model, request).await {
                return Some(n);
            }
        }

        // 优先级 3:tiktoken-rs 兜底
        count_with_tiktoken(model, request)
    }

    fn name(&self) -> &str {
        "openai-composite"
    }
}

// ============ Anthropic 远端 tokenizer ============

/// Anthropic Claude tokenizer — 永远走远端 count_tokens API
/// Anthropic Claude tokenizer — always hits remote count_tokens API
///
/// 失败/超时返回 None,由 registry 兜底字符估算(无本地 tokenizer)。
/// Failures return None and the registry falls back to char estimation (no local tokenizer).
pub struct AnthropicTokenizer {
    remote: Arc<dyn RemoteCountClient>,
}

impl AnthropicTokenizer {
    pub fn new(remote: Arc<dyn RemoteCountClient>) -> Self {
        Self { remote }
    }
}

#[async_trait]
impl PromptTokenizer for AnthropicTokenizer {
    async fn count_prompt(&self, model: &str, request: &ChatRequest) -> Option<u64> {
        let has_non_text = has_non_text_content(request);
        self.remote.count(model, request, has_non_text).await
    }

    fn name(&self) -> &str {
        "anthropic-remote"
    }
}

// ============ Gemini 远端 tokenizer ============

/// Google Gemini tokenizer — 永远走远端 countTokens API
/// Gemini tokenizer — always hits remote countTokens API
pub struct GeminiTokenizer {
    remote: Arc<dyn RemoteCountClient>,
}

impl GeminiTokenizer {
    pub fn new(remote: Arc<dyn RemoteCountClient>) -> Self {
        Self { remote }
    }
}

#[async_trait]
impl PromptTokenizer for GeminiTokenizer {
    async fn count_prompt(&self, model: &str, request: &ChatRequest) -> Option<u64> {
        let has_non_text = has_non_text_content(request);
        self.remote.count(model, request, has_non_text).await
    }

    fn name(&self) -> &str {
        "gemini-remote"
    }
}

// ============ OpenAI 内置 Xenova mapping ============

/// 内置 OpenAI 系列模型 → HF Xenova repo 映射
/// Built-in OpenAI model → HuggingFace Xenova repo mapping.
///
/// Xenova 在 HF 上发布了 OpenAI 公开 BPE 词表的 tokenizer.json 端口(MIT/Apache),
/// 可直接用 `tokenizers` 加载,精度等同 tiktoken-rs 但走统一 HF 路径。
/// Xenova publishes tokenizer.json ports of OpenAI public BPE vocabularies on HF;
/// these can be loaded by `tokenizers` and match tiktoken-rs precisely.
///
/// o1 / o3 / o4 系列暂未发布 Xenova 端口 → 返回 None,由 tiktoken 兜底处理。
/// o1 / o3 / o4 series have no Xenova port yet → return None, tiktoken handles them.
pub fn openai_default_xenova_repo(model: &str) -> Option<String> {
    let m = model.to_ascii_lowercase();
    if m.starts_with("gpt-4o") {
        Some("Xenova/gpt-4o".to_string())
    } else if m.starts_with("gpt-4") {
        Some("Xenova/gpt-4".to_string())
    } else if m.starts_with("gpt-3.5") {
        Some("Xenova/gpt-3.5-turbo".to_string())
    } else {
        None
    }
}

// ============ 文本提取 + 多模态判定 + 字符估算 ============

/// 把 ChatRequest 的 messages 拼成纯文本,供本地 tokenizer 编码或字符估算用
/// Concatenate ChatRequest messages into plain text for local tokenization or char estimation
pub fn extract_prompt_text(request: &ChatRequest) -> String {
    let mut buf = String::new();
    for msg in &request.messages {
        if !buf.is_empty() {
            buf.push('\n');
        }
        if !msg.role.is_empty() {
            buf.push_str(&msg.role);
            buf.push(':');
        }
        if let Some(content) = &msg.content {
            append_content_text(content, &mut buf);
        }
        if let Some(tool_calls) = &msg.tool_calls {
            // tool_calls 也算入 prompt 文本(发给上游会被计入)
            // count tool_calls toward prompt text (they are sent upstream)
            for tc in tool_calls {
                buf.push('\n');
                buf.push_str(&tc.function.name);
                buf.push(':');
                buf.push_str(&tc.function.arguments);
            }
        }
    }
    buf
}

fn append_content_text(value: &serde_json::Value, buf: &mut String) {
    match value {
        serde_json::Value::String(s) => buf.push_str(s),
        serde_json::Value::Array(arr) => {
            for part in arr {
                // OpenAI ContentPart: { type: "text", text: "..." } — 仅取 text
                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                    buf.push_str(t);
                    buf.push(' ');
                }
            }
        }
        // 其他类型 — 退化为 JSON 序列化
        _ => buf.push_str(&value.to_string()),
    }
}

/// 检测 ChatRequest 是否包含非文本内容(决定 OpenAI 系走 tiktoken 还是远端 API)
/// Detect whether ChatRequest contains non-text content
/// (determines OpenAI tokenizer path: tiktoken vs remote API)
///
/// 返回 true 的情况 / Returns true when:
/// - 任一 message.content 是数组,且数组中有非 text 类型的 part(image_url、input_audio、
///   input_file、document 等)
/// - any message.content array contains a non-text part
/// - 任一 message 包含 tool_calls(已发出的 function call 历史)
/// - any message has tool_calls (assistant function-call history)
/// - 顶层有 tools / tool_choice / functions / function_call(影响远端 token 计算)
/// - top-level tools / tool_choice / functions / function_call (affects remote token count)
/// - request.extra 中有 response_format(json_schema 模式会注入额外 schema token)
/// - request.extra contains response_format (json_schema mode injects extra schema tokens)
pub fn has_non_text_content(request: &ChatRequest) -> bool {
    // 顶层 tools / tool_choice — 这两个字段在 ChatRequest 上有专属位
    // top-level tools / tool_choice are first-class fields
    if request.tools.as_ref().map(|t| !t.is_empty()).unwrap_or(false) {
        return true;
    }
    if request.tool_choice.is_some() {
        return true;
    }

    // request.extra 中的 functions / function_call / response_format / attachments / audio / document
    // 这些是 OpenAI 历史/扩展字段,通过 ChatRequest.extra(flatten 接收)透传
    // Historical/extension fields routed through ChatRequest.extra(serde flatten)
    const NON_TEXT_EXTRA_KEYS: &[&str] = &[
        "functions",
        "function_call",
        "response_format",
        "attachments",
        "audio",
        "document",
        "image",
    ];
    for key in NON_TEXT_EXTRA_KEYS {
        if let Some(v) = request.extra.get(*key) {
            if !v.is_null() {
                return true;
            }
        }
    }

    // 遍历 messages
    for msg in &request.messages {
        // assistant 已发出过 tool_call → 历史上下文中含工具调用
        if msg.tool_calls.as_ref().map(|t| !t.is_empty()).unwrap_or(false) {
            return true;
        }
        // content 是数组 → 检查每个 part 类型
        if let Some(content) = &msg.content {
            if let Some(arr) = content.as_array() {
                for part in arr {
                    if part_is_non_text(part) {
                        return true;
                    }
                }
            }
            // String 类型一定是纯文本,跳过
            // 其他类型(很少见,例如直接的 number/object)— 保守判定为非文本
            // Other rare types (number/object) — conservatively treat as non-text
            else if !content.is_string() && !content.is_null() {
                return true;
            }
        }
    }

    false
}

/// 判定 OpenAI ContentPart 是否为非文本类型
fn part_is_non_text(part: &serde_json::Value) -> bool {
    // 显式 type 字段 — 只有 "text" 算文本,其他都是非文本
    // Explicit type field — only "text" is plain text
    if let Some(part_type) = part.get("type").and_then(|v| v.as_str()) {
        return part_type != "text";
    }
    // 没有 type 但有 image_url / input_audio / file 等结构性 key
    // No explicit type, but structural keys present
    const NON_TEXT_PART_KEYS: &[&str] = &[
        "image_url",
        "image",
        "input_audio",
        "audio",
        "input_file",
        "file",
        "document",
    ];
    for key in NON_TEXT_PART_KEYS {
        if part.get(*key).is_some() {
            return true;
        }
    }
    // 否则保守判为文本(如纯 { text: "..." } 但缺 type 字段的情况)
    false
}

/// 字符估算 — 4 chars ≈ 1 token,向上取整
/// Char-based estimation — 4 chars ≈ 1 token, ceil
pub fn estimate_from_request(request: &ChatRequest) -> u64 {
    let text = extract_prompt_text(request);
    ((text.len() as u64) + 3) / 4
}

/// 共用 tiktoken 计算 — OpenAi 双轨和 Tiktoken 直接路径都调用
pub(crate) fn count_with_tiktoken(model: &str, request: &ChatRequest) -> Option<u64> {
    let bpe = tiktoken_rs::get_bpe_from_model(model).ok()?;
    let text = extract_prompt_text(request);
    Some(bpe.encode_with_special_tokens(&text).len() as u64)
}

// ============ HuggingFace 本地 tokenizer ============

/// HuggingFace tokenizer.json 本地编码 tokenizer
/// HuggingFace local tokenizer.json encoding tokenizer
///
/// - 通过 [`HfLoader`] 拿到已加载的 `tokenizers::Tokenizer`(共享 Arc,无重复加载)
///   Looks up cached `tokenizers::Tokenizer` via shared HfLoader
/// - 找不到 → spawn 后台下载,本次返回 None,registry 自动 fallback 到字符估算
///   Cache miss → spawns background download and returns None this round
/// - 模型名 → repo_id 解析顺序:① 配置 mapping ② model 名直接当 repo_id(若含 `/`)
///   Model name → repo_id resolution: config mapping → model itself if it looks like a repo
pub struct HfTokenizer {
    loader: Arc<HfLoader>,
    /// 模型名 → repo_id 解析器(由 registry 注入,反映配置 mapping)
    /// Model name → repo_id resolver, supplied by registry to reflect config mappings
    repo_resolver: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
}

impl HfTokenizer {
    pub fn new(
        loader: Arc<HfLoader>,
        repo_resolver: Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
    ) -> Self {
        Self {
            loader,
            repo_resolver,
        }
    }
}

#[async_trait]
impl PromptTokenizer for HfTokenizer {
    async fn count_prompt(&self, model: &str, request: &ChatRequest) -> Option<u64> {
        let repo_id = (self.repo_resolver)(model)?;
        // 同步快路径:命中即编码;未命中则后台下载,本次返回 None 让 registry estimate 兜底
        // Hot path returns immediately; misses spawn a background download
        let tok = self.loader.try_get(&repo_id)?;
        // TODO(multimodal): 当前只编码文本内容(extract_prompt_text 在 array content 中只取
        //   type=text 的 part,自动忽略 image_url / input_audio / input_file 等多模态字段)。
        //   后续多模态精确计数需要:
        //   ① 各模型 vision tower 的 patch token 计算(LLaVA/Qwen-VL/InternVL 公式不同)
        //   ② image preprocessing pipeline(原图尺寸 → resize → patch grid → token 数)
        //   ③ audio/file 模态的对应预处理与 token 估算
        //   暂时只算文本部分 — 不降级到字符估算(因为文本部分已经精确了)。
        // TODO(multimodal): currently encodes text-only content; image_url / input_audio /
        //   input_file are dropped by extract_prompt_text. Full multimodal accounting needs
        //   per-model vision/audio token formulas — deferred.
        let text = extract_prompt_text(request);
        let encoding = tok.encode(text, false).ok()?;
        Some(encoding.len() as u64)
    }

    fn name(&self) -> &str {
        "hf-local"
    }
}

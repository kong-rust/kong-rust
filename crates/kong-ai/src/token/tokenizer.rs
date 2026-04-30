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

use async_trait::async_trait;

use crate::codec::ChatRequest;

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

/// OpenAI 双轨 tokenizer — 纯文本走 tiktoken,多模态/结构化走远端 count API
/// OpenAI dual-path tokenizer — pure text uses tiktoken; multimodal/structured uses remote count API
///
/// 选择规则(最高优先级,凌驾于 strategy 之上):
/// Selection rule (highest precedence, overrides strategy):
/// - has_non_text_content == false → tiktoken(零延迟、零配额消耗)
/// - has_non_text_content == true  → 远端 count API → tiktoken 兜底(image patch token 和
///                                    tool schema 注入开销 tiktoken 算不准)
///
/// Step 4 在此结构内部加上 remote count + LRU 缓存。
/// Step 1 阶段:has_non_text 命中时仍走 tiktoken(等同行为),保留分流骨架。
pub struct OpenAiTokenizer {
    // step 4 在这里挂上 RemoteCounter + Arc<LruCache>
    // step 4: hold RemoteCounter + Arc<LruCache> here
}

impl OpenAiTokenizer {
    pub fn new() -> Self {
        Self {}
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
        if has_non_text_content(request) {
            // Multimodal/tools/structured output — should hit remote API per design.
            // 多模态 / tools / structured output — 按设计应走远端 API
            // Step 4 implements remote count here; we deliberately fall through to tiktoken
            // for now so behavior degrades gracefully when remote is unavailable.
            // Step 4 在此填入远端 count;现阶段直落 tiktoken,保证流程跑通且行为安全降级
        }
        count_with_tiktoken(model, request)
    }

    fn name(&self) -> &str {
        "openai-dual"
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

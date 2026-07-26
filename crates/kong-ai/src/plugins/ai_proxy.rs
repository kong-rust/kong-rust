//! ai-proxy 插件 — AI 代理核心插件（非流式）
//! 负责请求/响应的协议转换、上游路由配置、token 统计

use async_trait::async_trait;
use bytes::Bytes;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{debug, warn};

use kong_core::error::{KongError, Result};
use kong_core::traits::{
    LifecyclePhase, PluginConfig, PluginHandler, RequestCtx, RequestLifecycle,
};

use crate::codec::anthropic_format::AnthropicCodec;
use crate::codec::responses_format::{self, ResponsesEventState, ResponsesRequest};
use crate::codec::ChatRequest;
use crate::models::{AiModel, AiProviderConfig};
use crate::plugins::context::{AiRequestState, ClientProtocol};
use crate::provider::router::{ModelRouteConfig, ModelRouter};
use crate::provider::{DriverRegistry, ModelGroupResolver, TokenUsage};

// ============ 插件配置 ============

/// Kong 官方 ai-proxy config.model 格式（record 类型）
/// Official Kong ai-proxy config.model format (record type)
#[derive(Debug, Clone, Deserialize)]
pub struct KongModelConfig {
    /// provider 类型 — "openai", "gemini", "anthropic" 等
    pub provider: String,
    /// 模型名称（可选）— model name (optional)
    #[serde(default)]
    pub name: Option<String>,
    /// 模型选项（可选）— model options (optional)
    #[serde(default)]
    pub options: Option<serde_json::Value>,
}

/// Kong 官方 ai-proxy config.auth 格式
/// Official Kong ai-proxy config.auth format
#[derive(Debug, Clone, Deserialize)]
pub struct KongAuthConfig {
    #[serde(default)]
    pub header_name: Option<String>,
    #[serde(default)]
    pub header_value: Option<String>,
    #[serde(default)]
    pub param_name: Option<String>,
    #[serde(default)]
    pub param_value: Option<String>,
    #[serde(default)]
    pub param_location: Option<String>,
    #[serde(default)]
    pub allow_override: Option<bool>,
    #[serde(default)]
    pub gcp_use_service_account: Option<bool>,
}

/// Kong 官方 ai-proxy config.logging 格式
/// Official Kong ai-proxy config.logging format
#[derive(Debug, Clone, Deserialize)]
pub struct KongLoggingConfig {
    #[serde(default)]
    pub log_payloads: Option<bool>,
    #[serde(default)]
    pub log_statistics: Option<bool>,
}

/// model 字段的灵活反序列化：支持 String（kong-rust 格式）和 Object（Kong 官方格式）
/// Flexible deserialization for model field: supports String (kong-rust) and Object (official Kong)
#[derive(Debug, Clone)]
pub enum ModelField {
    /// kong-rust 自定义格式：model 是模型名字符串
    Simple(String),
    /// Kong 官方格式：model 是包含 provider/name/options 的对象
    Kong(KongModelConfig),
}

impl Default for ModelField {
    fn default() -> Self {
        ModelField::Simple(String::new())
    }
}

impl<'de> Deserialize<'de> for ModelField {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;
        // Use serde_json::Value as intermediate representation — 使用 Value 做中间转换
        let value = serde_json::Value::deserialize(deserializer)?;
        match &value {
            serde_json::Value::String(s) => Ok(ModelField::Simple(s.clone())),
            serde_json::Value::Object(_) => {
                let cfg: KongModelConfig =
                    serde_json::from_value(value).map_err(de::Error::custom)?;
                Ok(ModelField::Kong(cfg))
            }
            serde_json::Value::Null => Ok(ModelField::Simple(String::new())),
            _ => Err(de::Error::custom("model must be a string or object")),
        }
    }
}

impl ModelField {
    /// 提取模型名称 — extract model name
    pub fn model_name(&self) -> &str {
        match self {
            ModelField::Simple(s) => s.as_str(),
            ModelField::Kong(cfg) => cfg.name.as_deref().unwrap_or(""),
        }
    }

    /// 提取 provider 类型（仅 Kong 格式有）— extract provider type (Kong format only)
    pub fn provider_type(&self) -> Option<&str> {
        match self {
            ModelField::Simple(_) => None,
            ModelField::Kong(cfg) => Some(cfg.provider.as_str()),
        }
    }
}

/// ai-proxy 插件配置（从 PluginConfig.config JSON 解析）
/// Supports both kong-rust custom format and official Kong ai-proxy format
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiProxyConfig {
    /// 模型配置：String（kong-rust）或 Object（Kong 官方）
    pub model: ModelField,
    /// AI Gateway 模型组；非空时优先于兼容字段 model
    pub model_group: Option<String>,
    /// 模型来源："config" 从插件配置取，"request" 从请求体取
    pub model_source: String,
    /// 路由类型："llm/v1/chat" | "llm/v1/completions"
    pub route_type: String,
    /// 客户端协议："openai" | "anthropic"
    pub client_protocol: String,
    /// LLM 格式（Kong 官方字段，等同于 client_protocol）— "openai" | "anthropic"
    pub llm_format: Option<String>,
    /// 流式响应策略："allow" | "deny" | "always"
    pub response_streaming: String,
    /// 最大请求体大小（KB）
    pub max_request_body_size: usize,
    /// 是否在响应头中添加模型名称
    pub model_name_header: bool,
    /// 上游超时（毫秒）
    pub timeout: u64,
    /// 重试次数
    pub retries: u32,
    /// 是否记录请求/响应体
    pub log_payloads: bool,
    /// 是否记录 token 统计
    pub log_statistics: bool,
    /// 内联 provider 配置（kong-rust 格式，不走 DAO）
    pub provider: Option<InlineProviderConfig>,
    /// 模型路由规则（正则匹配 + 加权选择） — model routing rules
    #[serde(default)]
    pub model_routes: Vec<ModelRouteConfig>,
    /// Kong 官方 auth 配置 — authentication config (official Kong format)
    pub auth: Option<KongAuthConfig>,
    /// Kong 官方 logging 配置 — logging config (official Kong format)
    pub logging: Option<KongLoggingConfig>,
}

/// 内联 provider 配置（嵌入在插件 config JSON 中）
#[derive(Debug, Clone, Deserialize)]
pub struct InlineProviderConfig {
    /// provider 类型（如 "openai"）
    pub provider_type: String,
    /// 认证配置
    #[serde(default)]
    pub auth_config: serde_json::Value,
    /// 自定义 endpoint URL
    pub endpoint_url: Option<String>,
}

impl Default for AiProxyConfig {
    fn default() -> Self {
        Self {
            model: ModelField::Simple(String::new()),
            model_group: None,
            model_source: "config".to_string(),
            route_type: "llm/v1/chat".to_string(),
            client_protocol: "openai".to_string(),
            llm_format: None,
            response_streaming: "allow".to_string(),
            max_request_body_size: 128, // 128 KB
            model_name_header: true,
            timeout: 60_000, // 60 秒
            retries: 1,
            log_payloads: false,
            log_statistics: true,
            provider: None,
            model_routes: Vec::new(),
            auth: None,
            logging: None,
        }
    }
}

impl AiProxyConfig {
    /// 获取有效的客户端协议（优先 client_protocol，其次 llm_format）
    /// Get effective client protocol (prefer client_protocol, fallback to llm_format)
    pub fn effective_client_protocol(&self) -> &str {
        if self.client_protocol != "openai" {
            return &self.client_protocol;
        }
        // client_protocol 是默认值，检查 llm_format 是否有覆盖
        if let Some(ref fmt) = self.llm_format {
            return fmt.as_str();
        }
        &self.client_protocol
    }

    /// 获取非空模型组名称 — get a non-empty model group name
    pub fn model_group_name(&self) -> Option<&str> {
        self.model_group
            .as_deref()
            .map(str::trim)
            .filter(|model_group| !model_group.is_empty())
    }

    /// 获取有效的模型名称 — get effective model name
    pub fn effective_model_name(&self) -> &str {
        if let Some(model_group) = self.model_group_name() {
            return model_group;
        }
        self.model.model_name()
    }

    /// 构建有效的 InlineProviderConfig（兼容 Kong 官方格式）
    /// Build effective InlineProviderConfig (compatible with official Kong format)
    /// Kong 官方格式：provider 在 model.provider，auth 在顶层 config.auth
    /// kong-rust 格式：provider 在 config.provider
    pub fn effective_provider(&self) -> Option<InlineProviderConfig> {
        // 优先使用 kong-rust 格式的 provider 字段
        if let Some(ref p) = self.provider {
            return Some(p.clone());
        }

        // 尝试从 Kong 官方格式构建：model.provider + config.auth
        let provider_type = self.model.provider_type()?;

        // 从 config.auth 构建 auth_config JSON — build auth_config from config.auth
        let auth_config = if let Some(ref auth) = self.auth {
            let mut map = serde_json::Map::new();
            if let Some(ref hn) = auth.header_name {
                map.insert(
                    "header_name".to_string(),
                    serde_json::Value::String(hn.clone()),
                );
            }
            if let Some(ref hv) = auth.header_value {
                map.insert(
                    "header_value".to_string(),
                    serde_json::Value::String(hv.clone()),
                );
            }
            if let Some(ref pn) = auth.param_name {
                map.insert(
                    "param_name".to_string(),
                    serde_json::Value::String(pn.clone()),
                );
            }
            if let Some(ref pv) = auth.param_value {
                map.insert(
                    "param_value".to_string(),
                    serde_json::Value::String(pv.clone()),
                );
            }
            if let Some(ref pl) = auth.param_location {
                map.insert(
                    "param_location".to_string(),
                    serde_json::Value::String(pl.clone()),
                );
            }
            serde_json::Value::Object(map)
        } else {
            serde_json::Value::Object(serde_json::Map::new())
        };

        // 从 model.options.upstream_url 提取 endpoint_url
        let endpoint_url = match &self.model {
            ModelField::Kong(cfg) => cfg
                .options
                .as_ref()
                .and_then(|o| o.get("upstream_url"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            _ => None,
        };

        Some(InlineProviderConfig {
            provider_type: provider_type.to_string(),
            auth_config,
            endpoint_url,
        })
    }
}

// ============ 插件结构体 ============

/// AI 代理插件 — 实现 PluginHandler trait
pub struct AiProxyPlugin {
    driver_registry: DriverRegistry,
    model_resolver: Option<Arc<ModelGroupResolver>>,
    /// 按 Route 和配置缓存路由器，使加权轮转计数器跨请求保留。
    model_routers: Mutex<HashMap<[u8; 32], Arc<ModelRouter>>>,
}

impl AiProxyPlugin {
    /// 创建新的 ai-proxy 插件实例
    pub fn new() -> Self {
        Self {
            driver_registry: DriverRegistry::new(),
            model_resolver: None,
            model_routers: Mutex::new(HashMap::new()),
        }
    }

    /// Create an ai-proxy plugin backed by the shared AI model/provider DAOs.
    pub fn with_model_resolver(model_resolver: Arc<ModelGroupResolver>) -> Self {
        Self {
            driver_registry: DriverRegistry::new(),
            model_resolver: Some(model_resolver),
            model_routers: Mutex::new(HashMap::new()),
        }
    }

    fn model_router(
        &self,
        route_id: Option<uuid::Uuid>,
        raw_config: &serde_json::Value,
        routes: &[ModelRouteConfig],
    ) -> Result<Arc<ModelRouter>> {
        let mut hasher = Sha256::new();
        if let Some(route_id) = route_id {
            hasher.update(route_id.as_bytes());
        }
        hasher.update(
            serde_json::to_vec(
                raw_config
                    .get("model_routes")
                    .unwrap_or(&serde_json::Value::Null),
            )
            .map_err(|error| {
                KongError::InternalError(format!("failed to fingerprint AI model routes: {error}"))
            })?,
        );
        let key: [u8; 32] = hasher.finalize().into();

        let mut cache = self.model_routers.lock().unwrap();
        if let Some(router) = cache.get(&key) {
            return Ok(Arc::clone(router));
        }

        // 配置种类异常增多时清空旧项，避免长期运行下无界增长。
        if cache.len() >= 256 {
            cache.clear();
        }

        let router = Arc::new(ModelRouter::from_configs(routes)?);
        cache.insert(key, Arc::clone(&router));
        Ok(router)
    }
}

impl Default for AiProxyPlugin {
    fn default() -> Self {
        Self::new()
    }
}

// ============ 辅助函数 ============

/// 重映射 OpenAI chat chunk 中的 tool_calls index
/// Anthropic 的 content_block index 是全局索引（含 text block），
/// 需要转换为 tool_calls 数组内的 0-based 本地索引
fn remap_tool_call_index(data: &str, counter: &mut u32) -> Option<String> {
    let mut chunk: serde_json::Value = serde_json::from_str(data).ok()?;
    let tool_calls = chunk
        .get_mut("choices")
        .and_then(|c| c.as_array_mut())
        .and_then(|a| a.first_mut())
        .and_then(|c| c.get_mut("delta"))
        .and_then(|d| d.get_mut("tool_calls"))
        .and_then(|tc| tc.as_array_mut())?;

    let mut modified = false;
    for tc in tool_calls {
        // 有 id 字段 = 新 tool_call 开始
        if tc.get("id").and_then(|v| v.as_str()).is_some() {
            tc["index"] = serde_json::json!(*counter);
            *counter += 1;
            modified = true;
        } else if tc.get("index").is_some() {
            // delta 续传 → 属于最近一个 tool_call
            tc["index"] = serde_json::json!(counter.saturating_sub(1));
            modified = true;
        }
    }

    if modified {
        Some(serde_json::to_string(&chunk).ok()?)
    } else {
        None
    }
}

fn merge_token_usage(current: &mut TokenUsage, observation: TokenUsage) {
    if observation.prompt_tokens.is_some() {
        current.prompt_tokens = observation.prompt_tokens;
    }
    if observation.completion_tokens.is_some() {
        current.completion_tokens = observation.completion_tokens;
    }
    if observation.total_tokens.is_some() {
        current.total_tokens = observation.total_tokens;
    }
    if observation.reasoning_tokens.is_some() {
        current.reasoning_tokens = observation.reasoning_tokens;
    }
    if observation.cache_read_input_tokens.is_some() {
        current.cache_read_input_tokens = observation.cache_read_input_tokens;
    }
    if observation.cache_write_input_tokens.is_some() {
        current.cache_write_input_tokens = observation.cache_write_input_tokens;
    }
    current.invalid |= observation.invalid;
}

fn derive_total_if_missing(usage: &mut TokenUsage) {
    if usage.total_tokens.is_some() {
        return;
    }
    if let (Some(prompt), Some(completion)) = (usage.prompt_tokens, usage.completion_tokens) {
        match prompt.checked_add(completion) {
            Some(total) => usage.total_tokens = Some(total),
            None => usage.invalid = true,
        }
    }
}

fn responses_token_usage(usage: &serde_json::Value) -> TokenUsage {
    TokenUsage::from_observation(crate::usage::normalizer::openai_observation(usage))
}

fn mark_response_transform_error(
    lifecycle: &mut RequestLifecycle,
    response_status: Option<u16>,
    component: &'static str,
) {
    let upstream_status = lifecycle.upstream_status.or(response_status);
    if matches!(upstream_status, Some(200..=299)) {
        lifecycle.mark_gateway_error(LifecyclePhase::BodyFilter, component);
    }
}

/// 增量解码流式响应，保留跨 chunk 的不完整 UTF-8 尾字节。
///
/// 返回 `(可安全送入协议 parser 的文本, 是否观察到真正无效的 UTF-8)`。
fn decode_stream_chunk(state: &mut AiRequestState, chunk: &[u8]) -> (Option<String>, bool) {
    let mut bytes = std::mem::take(&mut state.stream_utf8_buffer);
    bytes.extend_from_slice(chunk);
    match std::str::from_utf8(&bytes) {
        Ok(text) => ((!text.is_empty()).then(|| text.to_string()), false),
        Err(error) => {
            let valid_up_to = error.valid_up_to();
            let valid = (!bytes[..valid_up_to].is_empty()).then(|| {
                std::str::from_utf8(&bytes[..valid_up_to])
                    .expect("valid_up_to 之前必为合法 UTF-8")
                    .to_string()
            });
            if error.error_len().is_none() {
                state
                    .stream_utf8_buffer
                    .extend_from_slice(&bytes[valid_up_to..]);
                (valid, false)
            } else {
                (valid, true)
            }
        }
    }
}

fn discard_incomplete_stream_utf8(state: &mut AiRequestState) -> bool {
    if state.stream_utf8_buffer.is_empty() {
        false
    } else {
        state.stream_utf8_buffer.clear();
        true
    }
}

fn estimate_completion(model: &str, text: &str) -> u64 {
    crate::token::TokenCounter::new().count(model, text, None)
}

fn estimate_completion_segments<'a>(
    model: &str,
    segments: impl IntoIterator<Item = &'a str>,
) -> Option<u64> {
    segments.into_iter().try_fold(0u64, |total, text| {
        total.checked_add(estimate_completion(model, text))
    })
}

fn update_chat_completion_estimate(
    state: &mut AiRequestState,
    response: &crate::codec::ChatResponse,
) {
    if state.usage.completion_tokens.is_some() || response.choices.is_empty() {
        return;
    }
    let mut choice_texts = Vec::with_capacity(response.choices.len());
    for choice in &response.choices {
        let mut text = String::new();
        if let Some(content) = &choice.message.content {
            collect_content_text(content, &mut text);
        }
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tool_call in tool_calls {
                text.push_str(&tool_call.function.arguments);
            }
        }
        choice_texts.push(text);
    }
    match estimate_completion_segments(
        &state.model.model_name,
        choice_texts.iter().map(String::as_str),
    ) {
        Some(tokens) => state.estimated_completion_tokens = Some(tokens),
        None => state.completion_observation_invalid = true,
    }
}

fn collect_content_text(value: &serde_json::Value, output: &mut String) {
    match value {
        serde_json::Value::String(value) => output.push_str(value),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_content_text(value, output);
            }
        }
        serde_json::Value::Object(object) => {
            if let Some(text) = object.get("text").and_then(serde_json::Value::as_str) {
                output.push_str(text);
            } else if let Some(content) = object.get("content") {
                collect_content_text(content, output);
            }
        }
        _ => {}
    }
}

fn append_chat_stream_completion_text(state: &mut AiRequestState, data: &str) {
    let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    let Some(choices) = chunk.get("choices").and_then(serde_json::Value::as_array) else {
        return;
    };
    for choice in choices {
        let Some(choice_index) = choice.get("index").and_then(serde_json::Value::as_u64) else {
            state.completion_observation_invalid = true;
            continue;
        };
        let Some(delta) = choice.get("delta") else {
            continue;
        };
        let choice_text = state
            .completion_text_by_choice
            .entry(choice_index)
            .or_default();
        if let Some(content) = delta.get("content") {
            collect_content_text(content, choice_text);
        }
        if let Some(tool_calls) = delta
            .get("tool_calls")
            .and_then(serde_json::Value::as_array)
        {
            for tool_call in tool_calls {
                if let Some(arguments) = tool_call
                    .pointer("/function/arguments")
                    .and_then(serde_json::Value::as_str)
                {
                    choice_text.push_str(arguments);
                }
            }
        }
    }
}

fn collect_responses_output(value: &serde_json::Value, output: &mut String) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_responses_output(value, output);
            }
        }
        serde_json::Value::Object(object) => {
            match object
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
            {
                "output_text" => {
                    if let Some(text) = object.get("text").and_then(serde_json::Value::as_str) {
                        output.push_str(text);
                    }
                }
                "function_call" => {
                    if let Some(arguments) =
                        object.get("arguments").and_then(serde_json::Value::as_str)
                    {
                        output.push_str(arguments);
                    }
                }
                _ => {}
            }
            for key in ["output", "content"] {
                if let Some(value) = object.get(key) {
                    collect_responses_output(value, output);
                }
            }
        }
        _ => {}
    }
}

fn update_responses_completion_estimate(state: &mut AiRequestState, response: &serde_json::Value) {
    if state.usage.completion_tokens.is_some() {
        return;
    }
    let Some(output) = response.get("output") else {
        return;
    };
    let mut text = String::new();
    collect_responses_output(output, &mut text);
    state.estimated_completion_tokens = Some(estimate_completion(&state.model.model_name, &text));
}

fn append_responses_stream_completion_text(
    state: &mut AiRequestState,
    event: &serde_json::Value,
) -> bool {
    let event_type = event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if matches!(
        event_type,
        "response.output_text.delta" | "response.function_call_arguments.delta"
    ) {
        if let Some(delta) = event.get("delta").and_then(serde_json::Value::as_str) {
            state.completion_text.push_str(delta);
        }
        return false;
    }
    if matches!(event_type, "response.completed" | "response.incomplete") {
        if let Some(response) = event.get("response") {
            if let Some(output) = response.get("output") {
                state.completion_text.clear();
                collect_responses_output(output, &mut state.completion_text);
                return true;
            }
        }
    }
    false
}

fn finalize_stream_completion_estimate(state: &mut AiRequestState) {
    if state.stream_terminal == crate::usage::StreamTerminalState::Complete
        && state.usage.completion_tokens.is_none()
        && !state.completion_observation_invalid
    {
        state.estimated_completion_tokens = if state.completion_text_by_choice.is_empty() {
            Some(estimate_completion(
                &state.model.model_name,
                &state.completion_text,
            ))
        } else {
            estimate_completion_segments(
                &state.model.model_name,
                state.completion_text_by_choice.values().map(String::as_str),
            )
        };
        if state.estimated_completion_tokens.is_none() {
            state.completion_observation_invalid = true;
        }
    }
}

// ============ PluginHandler 实现 ============

#[async_trait]
impl PluginHandler for AiProxyPlugin {
    fn name(&self) -> &str {
        "ai-proxy"
    }

    fn priority(&self) -> i32 {
        // Kong ai-proxy 优先级 770
        770
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn has_body_filter(&self) -> bool {
        true
    }

    async fn rewrite(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        crate::usage::collector::observe_request_metadata(&config.config, ctx);
        Ok(())
    }

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let cfg: AiProxyConfig = crate::parse_plugin_config(config)?;

        // 1. 解析请求体
        let body_str = ctx
            .request_body
            .as_ref()
            .ok_or_else(|| KongError::PluginError {
                plugin_name: "ai-proxy".to_string(),
                message: "request body is empty".to_string(),
            })?;

        // 检查请求体大小限制
        if body_str.len() > cfg.max_request_body_size * 1024 {
            ctx.short_circuited = true;
            ctx.exit_status = Some(413);
            ctx.exit_body = Some(
                serde_json::json!({
                    "message": "request body exceeds max size limit"
                })
                .to_string(),
            );
            return Ok(());
        }

        // v1/responses 路由类型的特殊处理
        let is_responses_route = cfg.route_type == "llm/v1/responses";
        let mut responses_request: Option<ResponsesRequest> = None;
        let mut stripped_tools = None;

        // 根据 route_type 和 client_protocol 选择解码方式
        let effective_protocol = cfg.effective_client_protocol();
        let mut chat_request: ChatRequest = if is_responses_route {
            // v1/responses：解析为 ResponsesRequest，然后降级为 ChatRequest
            let req: ResponsesRequest =
                serde_json::from_str(body_str).map_err(|e| KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!("invalid v1/responses request body: {}", e),
                })?;

            // 检查 background 参数
            if req.background == Some(true) {
                ctx.short_circuited = true;
                ctx.exit_status = Some(400);
                ctx.exit_body = Some(
                    serde_json::to_string(&responses_format::responses_error(
                        "invalid_request_error",
                        "background mode is not supported",
                    ))
                    .unwrap_or_default(),
                );
                return Ok(());
            }

            let (chat_req, stripped) =
                responses_format::responses_to_chat(&req).map_err(|e| KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!("failed to convert responses request: {}", e),
                })?;

            stripped_tools = Some(stripped);
            responses_request = Some(req);
            chat_req
        } else {
            match effective_protocol {
                "anthropic" => AnthropicCodec::decode_request(body_str).map_err(|e| {
                    KongError::PluginError {
                        plugin_name: "ai-proxy".to_string(),
                        message: format!("invalid Anthropic chat request body: {}", e),
                    }
                })?,
                _ => serde_json::from_str(body_str).map_err(|e| KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!("invalid chat request body: {}", e),
                })?,
            }
        };

        // 2. 确定模型名称
        let config_model_name = cfg.effective_model_name().to_string();
        let model_name = match cfg.model_source.as_str() {
            "request" => {
                if chat_request.model.is_empty() {
                    return Err(KongError::PluginError {
                        plugin_name: "ai-proxy".to_string(),
                        message: "model_source=request but no model specified in request body"
                            .to_string(),
                    });
                }
                chat_request.model.clone()
            }
            _ => {
                // model_source=config（默认）
                if !config_model_name.is_empty() {
                    // 用配置中的模型覆盖请求中的模型
                    chat_request.model = config_model_name.clone();
                    config_model_name
                } else if !chat_request.model.is_empty() {
                    chat_request.model.clone()
                } else {
                    return Err(KongError::PluginError {
                        plugin_name: "ai-proxy".to_string(),
                        message: "no model specified in config or request body".to_string(),
                    });
                }
            }
        };

        // 3. 确定客户端协议
        let client_protocol = match effective_protocol {
            "anthropic" => ClientProtocol::Anthropic,
            _ => ClientProtocol::OpenAi,
        };

        // 4. 智能路由 / Intelligent routing
        // Priority: model_routes > explicit model_group > inline provider >
        // database fallback for legacy/request-selected model names.
        // An explicit model_group skips inline provider fields so credentials and
        // provider selection always come from the server-side AI entities.
        let inline_provider = if cfg.model_group_name().is_some() {
            None
        } else {
            cfg.effective_provider()
        };
        let (driver, ai_model, provider_config) = if !cfg.model_routes.is_empty() {
            // AI 网关智能路由：正则匹配 model 名 → 具体 provider + model（含加权选择）
            // AI Gateway routing: regex match model name → concrete provider + model (with weighted selection)
            let router = self.model_router(ctx.route_id, &config.config, &cfg.model_routes)?;
            let resolution = router
                .resolve(&model_name)
                .ok_or_else(|| KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!(
                        "no model route matched for model '{}' — 无路由规则匹配",
                        model_name
                    ),
                })?;

            let driver = self
                .driver_registry
                .get(&resolution.provider_type)
                .ok_or_else(|| KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!("unsupported provider type: {}", resolution.provider_type),
                })?
                .clone();

            // 用路由选中的 model_name 覆盖请求体中的 model（实际发给 provider 的名称可能不同）
            // Override request model with routed model_name (actual name sent to provider may differ)
            chat_request.model = resolution.model.model_name.clone();

            debug!(
                "ai-proxy: model '{}' routed → provider={}, model_name={}",
                model_name, resolution.provider_type, resolution.model.model_name
            );

            (driver, resolution.model, resolution.provider_config)
        } else if let Some(inline_provider) = inline_provider {
            // Fallback：使用内联 provider 配置（兼容 kong-rust 格式和 Kong 官方格式）
            // Fallback: use inline provider config (supports both kong-rust and official Kong format)
            let provider_type = &inline_provider.provider_type;
            let driver = self
                .driver_registry
                .get(provider_type)
                .ok_or_else(|| KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!("unsupported provider type: {}", provider_type),
                })?
                .clone();

            let ai_model = AiModel {
                name: model_name.clone(),
                model_name: model_name.clone(),
                enabled: true,
                ..Default::default()
            };

            let provider_config = AiProviderConfig {
                name: provider_type.clone(),
                provider_type: provider_type.clone(),
                auth_config: inline_provider.auth_config.clone(),
                endpoint_url: inline_provider.endpoint_url.clone(),
                enabled: true,
                ..Default::default()
            };

            (driver, ai_model, provider_config)
        } else if let Some(resolver) = &self.model_resolver {
            let routing_prompt_tokens = crate::token::estimate_from_request(&chat_request);
            let (ai_model, provider_config) = resolver
                .resolve_for(&model_name, Some(routing_prompt_tokens))
                .await?;
            let driver = self
                .driver_registry
                .get(&provider_config.provider_type)
                .ok_or_else(|| KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!(
                        "unsupported provider type: {}",
                        provider_config.provider_type
                    ),
                })?
                .clone();

            chat_request.model = ai_model.model_name.clone();
            debug!(
                "ai-proxy: model group '{}' resolved → provider={}, model_name={}",
                model_name, provider_config.provider_type, ai_model.model_name
            );
            (driver, ai_model, provider_config)
        } else {
            return Err(KongError::PluginError {
                plugin_name: "ai-proxy".to_string(),
                message: "missing provider: configure model_routes, inline provider, model.provider, or a database model group — 需要配置 model_routes、inline provider、model.provider 或数据库模型组".to_string(),
            });
        };

        // 6.5 v1/responses pass-through 检测
        // 如果是 v1/responses 且 provider 是 OpenAI，直接 pass-through（不做格式转换）
        let is_responses_pass_through = is_responses_route && driver.provider_type() == "openai";

        if is_responses_pass_through {
            // Pass-through：保留原始 Responses 字段，仅用路由后的实际模型名覆盖 model。
            // 不走 ChatRequest 转换管线，不调用 driver.transform_request()
            let stream_mode = responses_request
                .as_ref()
                .and_then(|r| r.stream)
                .unwrap_or(false);
            let mut upstream_body: serde_json::Value =
                serde_json::from_str(body_str).map_err(|e| KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!("failed to prepare v1/responses request body: {}", e),
                })?;
            let body_object =
                upstream_body
                    .as_object_mut()
                    .ok_or_else(|| KongError::PluginError {
                        plugin_name: "ai-proxy".to_string(),
                        message: "v1/responses request body must be a JSON object".to_string(),
                    })?;
            body_object.insert(
                "model".to_string(),
                serde_json::Value::String(ai_model.model_name.clone()),
            );
            let upstream_body =
                serde_json::to_string(&upstream_body).map_err(|e| KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!("failed to serialize v1/responses request body: {}", e),
                })?;

            // 通过 TokenizerRegistry 计算 prompt token 估值(供下游消费)
            // body 是 v1/responses 格式,registry 内部尝试解析为 ChatRequest 失败时
            // 自动 fallback 到 byte-length 估算(等同当前行为)
            let estimated_prompt_tokens = match crate::token::global_registry() {
                Some(registry) => {
                    registry
                        .count_prompt_from_body(
                            &provider_config.provider_type,
                            &ai_model.model_name,
                            &upstream_body,
                        )
                        .await
                }
                None => crate::token::TokenCounter::count_estimate(&upstream_body),
            };

            let upstream = driver
                .configure_upstream(&ai_model, &provider_config, stream_mode)
                .map_err(|e| KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!("failed to configure upstream: {}", e),
                })?;

            // 覆盖上游路径为 /v1/responses（configure_upstream 默认返回 /v1/chat/completions）
            ctx.upstream_target_host = Some(upstream.host);
            ctx.upstream_target_port = Some(upstream.port);
            ctx.upstream_scheme = Some(upstream.scheme);
            ctx.upstream_path = Some("/v1/responses".to_string());
            ctx.upstream_body = Some(upstream_body);

            // 设置上游请求头
            ctx.upstream_headers_to_set
                .push(("Content-Type".to_string(), "application/json".to_string()));
            for (k, v) in &upstream.headers {
                ctx.upstream_headers_to_set.push((k.clone(), v.clone()));
            }

            debug!(
                "ai-proxy access: v1/responses pass-through to OpenAI, model={}, stream={}",
                ai_model.model_name, stream_mode
            );

            crate::usage::collector::observe_model_selection(
                ctx,
                &ai_model,
                &provider_config,
                stream_mode,
            );
            let ai_state = AiRequestState {
                driver,
                model: ai_model,
                provider_config,
                stream_mode,
                client_protocol: ClientProtocol::OpenAi,
                sse_parser: if stream_mode {
                    Some(crate::codec::SseParser::new(
                        crate::codec::SseFormat::Standard,
                    ))
                } else {
                    None
                },
                stream_utf8_buffer: Vec::new(),
                usage: TokenUsage::default(),
                estimated_completion_tokens: None,
                completion_text: String::new(),
                completion_text_by_choice: Default::default(),
                completion_observation_invalid: false,
                response_buffer: None,
                request_start: Instant::now(),
                ttft: None,
                valid_stream_event_seen: false,
                stream_terminal: if stream_mode {
                    crate::usage::StreamTerminalState::Pending
                } else {
                    crate::usage::StreamTerminalState::NotStreaming
                },
                first_stream_event_at: None,
                route_type: cfg.route_type.clone(),
                is_first_stream_event: true,
                responses_mode: false,
                responses_pass_through: true,
                responses_event_state: None,
                stripped_tools: None,
                stream_tool_call_count: 0,
                estimated_prompt_tokens,
            };
            ctx.extensions.insert(ai_state);
            ctx.upstream_force_http1 = true;

            // 添加 X-Kong-AI-Route-Type 响应头
            ctx.response_headers_to_set.push((
                "X-Kong-AI-Route-Type".to_string(),
                "responses-pass-through".to_string(),
            ));

            return Ok(());
        }

        // 7. 确定流式模式（需在 configure_upstream 之前，Gemini 依赖此参数选择 API 端点）
        let stream_requested = chat_request.stream == Some(true);
        let stream_mode = match cfg.response_streaming.as_str() {
            "always" => {
                chat_request.stream = Some(true);
                true
            }
            "deny" => {
                chat_request.stream = Some(false);
                false
            }
            // "allow" — 尊重客户端请求
            _ => stream_requested,
        };

        // 8. 配置上游连接
        let upstream = driver
            .configure_upstream(&ai_model, &provider_config, stream_mode)
            .map_err(|e| KongError::PluginError {
                plugin_name: "ai-proxy".to_string(),
                message: format!("failed to configure upstream: {}", e),
            })?;

        // 9. 转换请求
        let provider_request = driver
            .transform_request(&chat_request, &ai_model, &provider_config)
            .map_err(|e| KongError::PluginError {
                plugin_name: "ai-proxy".to_string(),
                message: format!("failed to transform request: {}", e),
            })?;

        // 10. 设置上游连接参数
        ctx.upstream_target_host = Some(upstream.host);
        ctx.upstream_target_port = Some(upstream.port);
        ctx.upstream_scheme = Some(upstream.scheme);
        ctx.upstream_path = Some(upstream.path);
        ctx.upstream_body = Some(provider_request.body);

        // 设置上游请求头（认证 + Content-Type + provider 额外头）
        ctx.upstream_headers_to_set
            .push(("Content-Type".to_string(), provider_request.content_type));
        for (k, v) in &upstream.headers {
            ctx.upstream_headers_to_set.push((k.clone(), v.clone()));
        }
        for (k, v) in &provider_request.extra_headers {
            ctx.upstream_headers_to_set.push((k.clone(), v.clone()));
        }

        debug!(
            "ai-proxy access: model={}, provider={}, stream={}",
            ai_model.model_name, provider_config.provider_type, stream_mode
        );

        // 通过 TokenizerRegistry 精确计算 prompt token 估值
        // (chat_request 已规范化,直接传 ChatRequest 引用,registry 内含 deadline + estimate 兜底)
        let estimated_prompt_tokens = match crate::token::global_registry() {
            Some(registry) => {
                registry
                    .count_prompt(
                        &provider_config.provider_type,
                        &chat_request.model,
                        &chat_request,
                    )
                    .await
            }
            None => crate::token::estimate_from_request(&chat_request),
        };

        // 11. 存储跨阶段状态
        let responses_mode = is_responses_route;
        crate::usage::collector::observe_model_selection(
            ctx,
            &ai_model,
            &provider_config,
            stream_mode,
        );
        let ai_state = AiRequestState {
            driver,
            model: ai_model,
            provider_config,
            stream_mode,
            client_protocol,
            sse_parser: None,
            stream_utf8_buffer: Vec::new(),
            usage: TokenUsage::default(),
            estimated_completion_tokens: None,
            completion_text: String::new(),
            completion_text_by_choice: Default::default(),
            completion_observation_invalid: false,
            response_buffer: None,
            request_start: Instant::now(),
            ttft: None,
            valid_stream_event_seen: false,
            stream_terminal: if stream_mode {
                crate::usage::StreamTerminalState::Pending
            } else {
                crate::usage::StreamTerminalState::NotStreaming
            },
            first_stream_event_at: None,
            route_type: cfg.route_type.clone(),
            is_first_stream_event: true,
            responses_mode,
            responses_pass_through: false,
            responses_event_state: if responses_mode {
                Some(ResponsesEventState::new())
            } else {
                None
            },
            stripped_tools,
            stream_tool_call_count: 0,
            estimated_prompt_tokens,
        };

        ctx.extensions.insert(ai_state);

        // 11.5 添加 X-Kong-AI-Route-Type 响应头
        if responses_mode {
            ctx.response_headers_to_set.push((
                "X-Kong-AI-Route-Type".to_string(),
                "responses-translation".to_string(),
            ));
        }

        // 12. Force HTTP/1.1 for AI upstream connections — 强制 AI 上游使用 HTTP/1.1
        // Avoid H2 connection pool multiplexing issues with AI providers (rate-limit GOAWAY, stream stalls)
        // 避免 H2 连接池多路复用在 AI 提供商处引起的问题（限流 GOAWAY、流停滞）
        ctx.upstream_force_http1 = true;

        Ok(())
    }

    async fn header_filter(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let cfg: AiProxyConfig = crate::parse_plugin_config(config)?;

        // 检查 AiRequestState 是否存在（access 阶段应已设置）
        let ai_state = match ctx.extensions.get_mut::<AiRequestState>() {
            Some(s) => s,
            None => {
                warn!("ai-proxy header_filter: AiRequestState not found in extensions");
                return Ok(());
            }
        };

        // 检测上游响应是否为流式 — 通过 Content-Type 判断
        let content_type = ctx
            .response_headers
            .get("content-type")
            .cloned()
            .unwrap_or_default();
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim();
        let is_ndjson = media_type.eq_ignore_ascii_case("application/x-ndjson")
            || media_type.eq_ignore_ascii_case("application/stream+json");
        let is_stream = media_type.eq_ignore_ascii_case("text/event-stream") || is_ndjson;

        // Remove Content-Length: body transformation changes response size — 移除 Content-Length：body 转换会改变响应体大小
        ctx.response_headers_to_remove
            .push("content-length".to_string());
        ctx.response_headers_to_remove
            .push("content-encoding".to_string());

        if is_stream {
            // 初始化流式解析状态
            ai_state.stream_mode = true;
            let stream_format = if is_ndjson {
                crate::codec::SseFormat::Ndjson
            } else {
                crate::codec::SseFormat::Standard
            };
            ai_state.sse_parser = Some(crate::codec::SseParser::new(
                stream_format,
            ));
            ai_state.response_buffer = Some(String::new());

            // 设置客户端响应 Content-Type 为 SSE
            ctx.response_headers_to_set
                .push(("content-type".to_string(), "text/event-stream".to_string()));

            debug!(
                "ai-proxy header_filter: detected streaming response, content-type={}",
                content_type
            );
        } else if !ai_state.responses_pass_through {
            // Translation paths always emit JSON. Queue this before Pingora sends
            // the downstream headers; body_filter runs too late to change them.
            ctx.response_headers_to_set
                .push(("content-type".to_string(), "application/json".to_string()));
        }

        if cfg.model_name_header && !ai_state.model.model_name.is_empty() {
            ctx.response_headers_to_set.push((
                "x-kong-llm-model".to_string(),
                ai_state.model.model_name.clone(),
            ));
        }

        Ok(())
    }

    async fn body_filter(
        &self,
        _config: &PluginConfig,
        ctx: &mut RequestCtx,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
    ) -> Result<()> {
        // 取出状态（需要可变引用来更新 usage）
        let state = match ctx.extensions.get_mut::<AiRequestState>() {
            Some(s) => s,
            None => return Ok(()),
        };

        // ---- v1/responses pass-through 分支：只提取 usage，不做格式转换 ----
        if state.responses_pass_through {
            if state.stream_mode {
                // 流式 pass-through：通过 SSE parser 提取 usage（从 response.completed 事件）
                // 使用带缓冲的 SseParser 处理跨 chunk 边界的 SSE 事件
                let mut events = Vec::new();
                if let Some(chunk) = body.as_ref() {
                    let (chunk, invalid) = decode_stream_chunk(state, chunk);
                    state.completion_observation_invalid |= invalid;
                    if let Some(chunk) = chunk {
                        if let Some(ref mut parser) = state.sse_parser {
                            events.extend(parser.feed(&chunk));
                        }
                    }
                }
                if end_of_stream {
                    state.completion_observation_invalid |= discard_incomplete_stream_utf8(state);
                    if let Some(ref mut parser) = state.sse_parser {
                        events.extend(parser.flush());
                    }
                }
                // 从 SSE 事件中提取 usage（response.completed 事件中 usage 嵌套在 response 内）
                for event in &events {
                    state.observe_stream_event(event);
                    if state.stream_terminal == crate::usage::StreamTerminalState::ProviderFailed {
                        ctx.lifecycle.mark_upstream_semantic_error(Some(
                            state.provider_config.provider_type.clone(),
                        ));
                    }
                    match serde_json::from_str::<serde_json::Value>(&event.data) {
                        Ok(val) => {
                            if append_responses_stream_completion_text(state, &val) {
                                state.completion_observation_invalid = false;
                            }
                            // 优先查找顶层 usage，回退查找 response.usage
                            let usage = val
                                .get("usage")
                                .or_else(|| val.get("response").and_then(|r| r.get("usage")));
                            if let Some(usage) = usage {
                                merge_token_usage(&mut state.usage, responses_token_usage(usage));
                            }
                        }
                        Err(_) if !event.is_done() => {
                            state.completion_observation_invalid = true;
                        }
                        Err(_) => {}
                    }
                }
                if end_of_stream {
                    finalize_stream_completion_estimate(state);
                }
            } else {
                // 非流式 pass-through：收集完整响应体，解析 JSON 提取 usage
                if let Some(chunk) = body.as_ref() {
                    let chunk_str = String::from_utf8_lossy(chunk);
                    match state.response_buffer.as_mut() {
                        Some(buf) => buf.push_str(&chunk_str),
                        None => state.response_buffer = Some(chunk_str.into_owned()),
                    }
                }
                if end_of_stream {
                    let data = state
                        .response_buffer
                        .as_deref()
                        .and_then(|buffer| serde_json::from_str::<serde_json::Value>(buffer).ok());
                    if let Some(data) = data {
                        if let Some(usage) = data.get("usage") {
                            state.usage = responses_token_usage(usage);
                        }
                        update_responses_completion_estimate(state, &data);
                    }
                }
            }
            // 响应体原样透传，不做任何修改
            return Ok(());
        }

        // ---- v1/responses 翻译模式 — 流式分支 ----
        if state.responses_mode && state.stream_mode {
            // 解析 SSE 事件：feed chunk（如有）+ end_of_stream 时 flush
            let mut events = Vec::new();
            if let Some(body_bytes) = body.as_ref() {
                let (chunk, invalid) = decode_stream_chunk(state, body_bytes);
                if let Some(chunk) = chunk {
                    if let Some(ref mut parser) = state.sse_parser {
                        events.extend(parser.feed(&chunk));
                    }
                }
                if invalid {
                    state.completion_observation_invalid = true;
                    mark_response_transform_error(
                        &mut ctx.lifecycle,
                        ctx.response_status,
                        "responses_stream_utf8_decode",
                    );
                }
            }
            // flush 必须在 body 检查外部，确保 body=None + end_of_stream=true 时也能触发
            if end_of_stream {
                if discard_incomplete_stream_utf8(state) {
                    state.completion_observation_invalid = true;
                    mark_response_transform_error(
                        &mut ctx.lifecycle,
                        ctx.response_status,
                        "responses_stream_utf8_decode",
                    );
                }
                if let Some(ref mut parser) = state.sse_parser {
                    events.extend(parser.flush());
                }
            }

            let mut output = String::new();

            for event in &events {
                state.observe_stream_event(event);
                if state.stream_terminal == crate::usage::StreamTerminalState::ProviderFailed {
                    ctx.lifecycle.mark_upstream_semantic_error(Some(
                        state.provider_config.provider_type.clone(),
                    ));
                }
                // 提取 usage
                if let Some(usage) = state.driver.extract_stream_usage(event) {
                    merge_token_usage(&mut state.usage, usage);
                }

                if event.is_done() {
                    // [DONE] → 注入 usage 后通过状态机生成 response.completed 事件
                    let pt = state.usage.prompt_tokens.unwrap_or(0);
                    let ct = state.usage.completion_tokens.unwrap_or(0);
                    if let Some(total_tokens) =
                        state.usage.total_tokens.or_else(|| pt.checked_add(ct))
                    {
                        if let Some(ref mut es) = state.responses_event_state {
                            es.usage = crate::codec::responses_format::ResponsesUsage {
                                input_tokens: pt,
                                output_tokens: ct,
                                total_tokens,
                            };
                            for e in es.process_done() {
                                output.push_str(&e);
                            }
                        }
                    } else {
                        state.usage.invalid = true;
                        mark_response_transform_error(
                            &mut ctx.lifecycle,
                            ctx.response_status,
                            "responses_usage_overflow",
                        );
                        if let Some(ref mut es) = state.responses_event_state {
                            for e in es.process_error("invalid provider usage") {
                                output.push_str(&e);
                            }
                        }
                    }
                    continue;
                }

                // 通过 driver 转换事件格式 → OpenAI chat chunk
                match state.driver.transform_stream_event(event, &state.model) {
                    Ok(Some(mut transformed)) => {
                        append_chat_stream_completion_text(state, &transformed.data);
                        // 重映射 tool_call index（Anthropic 全局 → 本地 0-based）
                        if let Some(remapped) = remap_tool_call_index(
                            &transformed.data,
                            &mut state.stream_tool_call_count,
                        ) {
                            transformed.data = remapped;
                        }
                        // 将 OpenAI chat chunk 通过状态机转换为 responses 事件
                        if let Some(ref mut es) = state.responses_event_state {
                            for e in es.process_chat_chunk(&transformed.data) {
                                output.push_str(&e);
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(
                            "ai-proxy body_filter (responses): SSE transform error: {}",
                            e
                        );
                        state.completion_observation_invalid = true;
                        if state.stream_terminal
                            != crate::usage::StreamTerminalState::ProviderFailed
                        {
                            mark_response_transform_error(
                                &mut ctx.lifecycle,
                                ctx.response_status,
                                "responses_stream_transform",
                            );
                        }
                    }
                }
            }

            if !output.is_empty() {
                *body = Some(bytes::Bytes::from(output));
            } else {
                // 始终替换原始 body，防止上游原始数据（如 "data: [DONE]"）泄漏到客户端
                *body = Some(bytes::Bytes::new());
            }

            if end_of_stream {
                finalize_stream_completion_estimate(state);
                derive_total_if_missing(&mut state.usage);
            }

            return Ok(());
        }

        // ---- 流式处理分支（chat completions / Anthropic 客户端协议）----
        if state.stream_mode {
            let had_body = body.is_some();
            let mut events = Vec::new();
            if let Some(body_bytes) = body.as_ref() {
                let (chunk, invalid) = decode_stream_chunk(state, body_bytes);
                if let Some(chunk) = chunk {
                    if let Some(ref mut parser) = state.sse_parser {
                        events.extend(parser.feed(&chunk));
                    }
                }
                if invalid {
                    warn!("ai-proxy body_filter: invalid UTF-8 in stream chunk");
                    state.completion_observation_invalid = true;
                    mark_response_transform_error(
                        &mut ctx.lifecycle,
                        ctx.response_status,
                        "stream_utf8_decode",
                    );
                }
            }
            // Pingora 可以用独立的 body=None + EOS 通知结束，必须照样 flush 残留事件。
            if end_of_stream {
                if discard_incomplete_stream_utf8(state) {
                    warn!("ai-proxy body_filter: incomplete UTF-8 at end of stream");
                    state.completion_observation_invalid = true;
                    mark_response_transform_error(
                        &mut ctx.lifecycle,
                        ctx.response_status,
                        "stream_utf8_decode",
                    );
                }
                if let Some(ref mut parser) = state.sse_parser {
                    events.extend(parser.flush());
                }
            }

            // 转换每个流事件并拼装输出
            let mut output = String::new();
            let is_anthropic_client = state.client_protocol == ClientProtocol::Anthropic;

            for event in &events {
                state.observe_stream_event(event);
                if state.stream_terminal == crate::usage::StreamTerminalState::ProviderFailed {
                    ctx.lifecycle.mark_upstream_semantic_error(Some(
                        state.provider_config.provider_type.clone(),
                    ));
                }
                // [DONE] 终止事件
                if event.is_done() {
                    if is_anthropic_client {
                        // Anthropic 客户端协议：[DONE] → message_delta + message_stop
                        match AnthropicCodec::encode_stream_event(event, false) {
                            Ok(encoded) => {
                                for enc_event in &encoded {
                                    output.push_str(&format!(
                                        "event: {}\ndata: {}\n\n",
                                        enc_event.event_type, enc_event.data
                                    ));
                                }
                            }
                            Err(error) => {
                                warn!(
                                    "ai-proxy body_filter: Anthropic 终态编码失败: {}",
                                    error
                                );
                                mark_response_transform_error(
                                    &mut ctx.lifecycle,
                                    ctx.response_status,
                                    "anthropic_stream_encode",
                                );
                            }
                        }
                    } else {
                        output.push_str("data: [DONE]\n\n");
                    }
                    continue;
                }

                // 提取 token usage（在 transform 之前，使用原始事件格式）
                // Extract token usage before transform — using raw provider event format
                if let Some(usage) = state.driver.extract_stream_usage(event) {
                    // 使用替换而非累加：兼容所有 provider 的语义
                    // - OpenAI：仅最后一个 chunk 携带 usage，替换 = 赋值
                    // - Anthropic：分两次发送（input_tokens / output_tokens），各字段独立替换
                    // - Gemini：每个 chunk 携带累计值，替换 = 取最新值
                    // Use replacement instead of accumulation — works for all providers
                    merge_token_usage(&mut state.usage, usage);
                }

                // 通过 driver 转换事件格式（OpenAI 直通，Anthropic provider 需转换）
                match state.driver.transform_stream_event(event, &state.model) {
                    Ok(Some(mut transformed)) => {
                        append_chat_stream_completion_text(state, &transformed.data);
                        // 重映射 tool_call index（Anthropic 全局 → 本地 0-based）
                        if let Some(remapped) = remap_tool_call_index(
                            &transformed.data,
                            &mut state.stream_tool_call_count,
                        ) {
                            transformed.data = remapped;
                        }
                        // 如果客户端协议为 Anthropic，进一步编码为 Anthropic SSE 格式
                        if is_anthropic_client {
                            let is_first = state.is_first_stream_event;
                            match AnthropicCodec::encode_stream_event(&transformed, is_first) {
                                Ok(encoded) => {
                                    for enc_event in &encoded {
                                        output.push_str(&format!(
                                            "event: {}\ndata: {}\n\n",
                                            enc_event.event_type, enc_event.data
                                        ));
                                    }
                                    state.is_first_stream_event = false;
                                }
                                Err(error) => {
                                    warn!(
                                        "ai-proxy body_filter: Anthropic 事件编码失败: {}",
                                        error
                                    );
                                    mark_response_transform_error(
                                        &mut ctx.lifecycle,
                                        ctx.response_status,
                                        "anthropic_stream_encode",
                                    );
                                }
                            }
                        } else {
                            output.push_str(&format!("data: {}\n\n", transformed.data));
                        }

                        // 累积到 response_buffer（供 ai-cache 等插件回写使用）
                        if let Some(ref mut buf) = state.response_buffer {
                            buf.push_str(&transformed.data);
                        }
                    }
                    Ok(None) => {
                        // transform_stream_event 返回 None 表示 [DONE] 或需跳过的事件
                    }
                    Err(e) => {
                        warn!("ai-proxy body_filter: SSE event transform error: {}", e);
                        state.completion_observation_invalid = true;
                        if state.stream_terminal
                            != crate::usage::StreamTerminalState::ProviderFailed
                        {
                            mark_response_transform_error(
                                &mut ctx.lifecycle,
                                ctx.response_status,
                                "stream_event_transform",
                            );
                        }
                    }
                }
            }

            // 更新 body：有输出则替换；已消费的原始 chunk 即使在 EOS 也不能泄漏。
            if !output.is_empty() {
                *body = Some(bytes::Bytes::from(output));
            } else if had_body {
                *body = Some(bytes::Bytes::new());
            }

            // 流结束时汇总 total_tokens
            if end_of_stream {
                finalize_stream_completion_estimate(state);
                derive_total_if_missing(&mut state.usage);
            }

            return Ok(());
        }

        // ---- v1/responses 翻译模式 — 非流式分支 ----
        if state.responses_mode && !state.stream_mode {
            // 收集响应体
            if let Some(chunk) = body.as_ref() {
                let chunk_str = String::from_utf8_lossy(chunk);
                match state.response_buffer.as_mut() {
                    Some(buf) => buf.push_str(&chunk_str),
                    None => state.response_buffer = Some(chunk_str.into_owned()),
                }
            }

            if end_of_stream {
                let full_body = state.response_buffer.take().unwrap_or_default();
                let status = ctx.response_status.unwrap_or(200);

                // 提取 usage
                if let Some(usage) = state.driver.extract_usage(&full_body) {
                    state.usage = usage;
                }

                // 转换响应
                match state.driver.transform_response(
                    status,
                    &ctx.response_headers,
                    &full_body,
                    &state.model,
                ) {
                    Ok(chat_response) => {
                        update_chat_completion_estimate(state, &chat_response);
                        // ChatResponse → ResponsesResponse
                        let stripped = state.stripped_tools.as_ref().cloned().unwrap_or_default();
                        let responses_resp =
                            responses_format::chat_to_responses(&chat_response, &stripped);
                        let json = serde_json::to_string(&responses_resp).unwrap_or_default();
                        *body = Some(Bytes::from(json));
                    }
                    Err(e) => {
                        warn!("ai-proxy body_filter (responses): transform error: {}", e);
                        mark_response_transform_error(
                            &mut ctx.lifecycle,
                            ctx.response_status,
                            "responses_body_transform",
                        );
                        // 返回 responses 格式的错误
                        let err_resp = responses_format::responses_error(
                            "server_error",
                            &format!("upstream provider error: {}", e),
                        );
                        let json = serde_json::to_string(&err_resp).unwrap_or_default();
                        *body = Some(Bytes::from(json));
                    }
                }
            } else {
                *body = None;
            }

            return Ok(());
        }

        // ---- 非流式处理分支（chat completions）----
        // 非流式：收集响应体
        if let Some(chunk) = body.as_ref() {
            let chunk_str = String::from_utf8_lossy(chunk);
            match state.response_buffer.as_mut() {
                Some(buf) => buf.push_str(&chunk_str),
                None => state.response_buffer = Some(chunk_str.into_owned()),
            }
        }

        // 非流式：end_of_stream 时处理完整响应
        if end_of_stream {
            let full_body = state.response_buffer.take().unwrap_or_default();
            let status = ctx.response_status.unwrap_or(200);

            // 提取 token 使用量
            if let Some(usage) = state.driver.extract_usage(&full_body) {
                state.usage = usage;
            }

            // 转换响应格式
            match state.driver.transform_response(
                status,
                &ctx.response_headers,
                &full_body,
                &state.model,
            ) {
                Ok(chat_response) => {
                    update_chat_completion_estimate(state, &chat_response);
                    // 根据 client_protocol 编码响应
                    let response_json = if state.client_protocol == ClientProtocol::Anthropic {
                        AnthropicCodec::encode_response(&chat_response).map_err(|e| {
                            KongError::PluginError {
                                plugin_name: "ai-proxy".to_string(),
                                message: format!("failed to encode Anthropic response: {}", e),
                            }
                        })?
                    } else {
                        serde_json::to_string(&chat_response).map_err(|e| {
                            KongError::PluginError {
                                plugin_name: "ai-proxy".to_string(),
                                message: format!("failed to serialize response: {}", e),
                            }
                        })?
                    };

                    // 替换响应体
                    *body = Some(Bytes::from(response_json));
                }
                Err(e) => {
                    // 上游返回错误（如 4xx/5xx），透传错误信息
                    warn!("ai-proxy body_filter: transform_response failed: {}", e);
                    mark_response_transform_error(
                        &mut ctx.lifecycle,
                        ctx.response_status,
                        "response_body_transform",
                    );
                    // 保留原始响应体，不做转换
                    *body = Some(Bytes::from(full_body));
                }
            }
        } else {
            // 非 end_of_stream：清空 body（缓冲中，不向下游发送）
            *body = None;
        }

        Ok(())
    }

    async fn log(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let log_statistics = config
            .config
            .get("logging")
            .and_then(|value| value.get("log_statistics"))
            .and_then(serde_json::Value::as_bool)
            .or_else(|| {
                config
                    .config
                    .get("log_statistics")
                    .and_then(serde_json::Value::as_bool)
            })
            .unwrap_or(true);
        if !log_statistics {
            return Ok(());
        }

        // lifecycle observer 已先于普通 log 阶段生成事实；优先复用该事实，
        // 早期策略拒绝没有 AiRequestState 时也能得到一致的兼容日志。
        let ai_log = if let Some(fact) = ctx.extensions.get::<Arc<crate::usage::AiUsageFact>>() {
            let route_type = ctx
                .extensions
                .get::<AiRequestState>()
                .map(|state| state.route_type.clone());
            serde_json::json!({
                "ai": {
                    "proxy": {
                        "provider": fact.provider_type,
                        "model": fact.actual_model,
                        "route_type": route_type,
                        "stream": fact.stream,
                    },
                    "usage": {
                        "prompt_tokens": fact.prompt_tokens.map(|field| field.value),
                        "completion_tokens": fact.completion_tokens.map(|field| field.value),
                        "total_tokens": fact.total_tokens.map(|field| field.value),
                    },
                    "latency": {
                        "e2e_ms": fact.e2e_ms,
                    }
                }
            })
        } else if let Some(state) = ctx.extensions.get::<AiRequestState>() {
            serde_json::json!({
                "ai": {
                    "proxy": {
                        "provider": state.provider_config.provider_type,
                        "model": state.model.model_name,
                        "route_type": state.route_type,
                        "stream": state.stream_mode,
                    },
                    "usage": {
                        "prompt_tokens": state.usage.prompt_tokens,
                        "completion_tokens": state.usage.completion_tokens,
                        "total_tokens": state.usage.total_tokens,
                    },
                    "latency": {
                        "e2e_ms": state.request_start.elapsed().as_millis() as u64,
                    }
                }
            })
        } else {
            return Ok(());
        };

        // 合并到 ctx.log_serialize
        match ctx.log_serialize.as_mut() {
            Some(existing) => {
                if let (Some(existing_obj), Some(ai_obj)) =
                    (existing.as_object_mut(), ai_log.as_object())
                {
                    for (k, v) in ai_obj {
                        existing_obj.insert(k.clone(), v.clone());
                    }
                }
            }
            None => {
                ctx.log_serialize = Some(ai_log);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod completion_estimate_tests {
    use super::{
        estimate_completion, estimate_completion_segments, merge_token_usage,
        TokenUsage,
    };

    #[test]
    fn completion_estimate_sums_choices_independently() {
        let model = "gpt-5.6-sol";
        let choices = ["hello", "world"];
        let expected = estimate_completion(model, choices[0])
            .checked_add(estimate_completion(model, choices[1]))
            .unwrap();
        assert_eq!(
            estimate_completion_segments(model, choices.iter().copied()),
            Some(expected)
        );
    }

    #[test]
    fn merged_stream_usage_keeps_invalid_observation_sticky() {
        let mut current = TokenUsage {
            invalid: true,
            ..Default::default()
        };
        merge_token_usage(
            &mut current,
            TokenUsage {
                prompt_tokens: Some(10),
                completion_tokens: Some(20),
                total_tokens: Some(30),
                ..Default::default()
            },
        );

        assert!(current.invalid);
        assert_eq!(current.prompt_tokens, Some(10));
        assert_eq!(current.completion_tokens, Some(20));
    }
}

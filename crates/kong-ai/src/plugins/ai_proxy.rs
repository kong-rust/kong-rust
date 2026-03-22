//! ai-proxy 插件 — AI 代理核心插件（非流式）
//! 负责请求/响应的协议转换、上游路由配置、token 统计

use async_trait::async_trait;
use bytes::Bytes;
use serde::Deserialize;
use std::time::Instant;
use tracing::{debug, warn};

use kong_core::error::{KongError, Result};
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

use crate::codec::anthropic_format::AnthropicCodec;
use crate::codec::ChatRequest;
use crate::models::{AiModel, AiProviderConfig};
use crate::plugins::context::{AiRequestState, ClientProtocol};
use crate::provider::router::{ModelRouteConfig, ModelRouter};
use crate::provider::{DriverRegistry, TokenUsage};

// ============ 插件配置 ============

/// ai-proxy 插件配置（从 PluginConfig.config JSON 解析）
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiProxyConfig {
    /// 模型组名称
    pub model: String,
    /// 模型来源："config" 从插件配置取，"request" 从请求体取
    pub model_source: String,
    /// 路由类型："llm/v1/chat" | "llm/v1/completions"
    pub route_type: String,
    /// 客户端协议："openai" | "anthropic"
    pub client_protocol: String,
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
    /// 内联 provider 配置（MVP 阶段使用，不走 DAO）
    pub provider: Option<InlineProviderConfig>,
    /// 模型路由规则（正则匹配 + 加权选择） — model routing rules
    #[serde(default)]
    pub model_routes: Vec<ModelRouteConfig>,
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
            model: String::new(),
            model_source: "config".to_string(),
            route_type: "llm/v1/chat".to_string(),
            client_protocol: "openai".to_string(),
            response_streaming: "allow".to_string(),
            max_request_body_size: 128, // 128 KB
            model_name_header: true,
            timeout: 60_000, // 60 秒
            retries: 1,
            log_payloads: false,
            log_statistics: true,
            provider: None,
            model_routes: Vec::new(),
        }
    }
}

// ============ 插件结构体 ============

/// AI 代理插件 — 实现 PluginHandler trait
pub struct AiProxyPlugin {
    driver_registry: DriverRegistry,
}

impl AiProxyPlugin {
    /// 创建新的 ai-proxy 插件实例
    pub fn new() -> Self {
        Self {
            driver_registry: DriverRegistry::new(),
        }
    }
}

impl Default for AiProxyPlugin {
    fn default() -> Self {
        Self::new()
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

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let cfg: AiProxyConfig = crate::parse_plugin_config(config)?;

        // 1. 解析请求体
        let body_str = ctx.request_body.as_ref().ok_or_else(|| KongError::PluginError {
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

        // 根据 client_protocol 选择解码方式
        let mut chat_request: ChatRequest = match cfg.client_protocol.as_str() {
            "anthropic" => {
                AnthropicCodec::decode_request(body_str).map_err(|e| KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!("invalid Anthropic chat request body: {}", e),
                })?
            }
            _ => {
                serde_json::from_str(body_str).map_err(|e| KongError::PluginError {
                    plugin_name: "ai-proxy".to_string(),
                    message: format!("invalid chat request body: {}", e),
                })?
            }
        };

        // 2. 确定模型名称
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
                if !cfg.model.is_empty() {
                    // 用配置中的模型覆盖请求中的模型
                    chat_request.model = cfg.model.clone();
                    cfg.model.clone()
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
        let client_protocol = match cfg.client_protocol.as_str() {
            "anthropic" => ClientProtocol::Anthropic,
            _ => ClientProtocol::OpenAi,
        };

        // 4. 智能路由 / Intelligent routing
        // 优先使用 model_routes（AI 网关级智能路由）；fallback 到 inline provider 配置
        // Priority: model_routes (AI Gateway-level routing) > inline provider config
        let (driver, ai_model, provider_config) = if !cfg.model_routes.is_empty() {
            // AI 网关智能路由：正则匹配 model 名 → 具体 provider + model（含加权选择）
            // AI Gateway routing: regex match model name → concrete provider + model (with weighted selection)
            let router = ModelRouter::from_configs(&cfg.model_routes)?;
            let resolution = router.resolve(&model_name).ok_or_else(|| KongError::PluginError {
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
        } else {
            // Fallback：使用内联 provider 配置（无智能路由时的默认行为）
            // Fallback: use inline provider config (default behavior without routing)
            let inline_provider = cfg.provider.as_ref().ok_or_else(|| KongError::PluginError {
                plugin_name: "ai-proxy".to_string(),
                message: "missing provider: configure model_routes or inline provider — 需要配置 model_routes 或 inline provider".to_string(),
            })?;

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
        };

        // 7. 配置上游连接
        let upstream = driver
            .configure_upstream(&ai_model, &provider_config)
            .map_err(|e| KongError::PluginError {
                plugin_name: "ai-proxy".to_string(),
                message: format!("failed to configure upstream: {}", e),
            })?;

        // 8. 确定流式模式
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
        ctx.upstream_headers_to_set.push((
            "Content-Type".to_string(),
            provider_request.content_type,
        ));
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

        // 11. 存储跨阶段状态
        let ai_state = AiRequestState {
            driver,
            model: ai_model,
            provider_config,
            stream_mode,
            client_protocol,
            sse_parser: None,
            usage: TokenUsage::default(),
            response_buffer: None,
            request_start: Instant::now(),
            ttft: None,
            route_type: cfg.route_type.clone(),
            is_first_stream_event: true,
        };

        ctx.extensions.insert(ai_state);

        Ok(())
    }

    async fn header_filter(&self, _config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
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

        let is_stream = content_type.contains("text/event-stream")
            || content_type.contains("application/x-ndjson")
            || content_type.contains("application/stream+json");

        if is_stream {
            // 初始化流式解析状态
            ai_state.stream_mode = true;
            ai_state.sse_parser = Some(crate::codec::SseParser::new(
                crate::codec::SseFormat::Standard,
            ));
            ai_state.response_buffer = Some(String::new());

            // 设置客户端响应 Content-Type 为 SSE
            ctx.response_headers_to_set.push((
                "content-type".to_string(),
                "text/event-stream".to_string(),
            ));

            debug!("ai-proxy header_filter: detected streaming response, content-type={}", content_type);
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

        // ---- 流式处理分支 ----
        if state.stream_mode {
            if let Some(body_bytes) = body.as_ref() {
                let chunk = match std::str::from_utf8(body_bytes) {
                    Ok(s) => s.to_string(),
                    Err(e) => {
                        warn!("ai-proxy body_filter: invalid UTF-8 in SSE chunk: {}", e);
                        return Ok(());
                    }
                };

                // 解析 SSE 事件：end_of_stream 时同时 flush 缓冲区
                let events = if end_of_stream {
                    let mut evts = if let Some(ref mut parser) = state.sse_parser {
                        parser.feed(&chunk)
                    } else {
                        vec![]
                    };
                    if let Some(ref mut parser) = state.sse_parser {
                        evts.extend(parser.flush());
                    }
                    evts
                } else {
                    if let Some(ref mut parser) = state.sse_parser {
                        parser.feed(&chunk)
                    } else {
                        vec![]
                    }
                };

                // 记录首 token 时间（TTFT）
                if !events.is_empty() && state.ttft.is_none() {
                    state.ttft = Some(std::time::Instant::now());
                }

                // 转换每个 SSE 事件并拼装输出
                let mut output = String::new();
                let is_anthropic_client = state.client_protocol == ClientProtocol::Anthropic;

                for event in &events {
                    // [DONE] 终止事件
                    if event.is_done() {
                        if is_anthropic_client {
                            // Anthropic 客户端协议：[DONE] → message_delta + message_stop
                            if let Ok(encoded) = AnthropicCodec::encode_stream_event(event, false) {
                                for enc_event in &encoded {
                                    output.push_str(&format!(
                                        "event: {}\ndata: {}\n\n",
                                        enc_event.event_type, enc_event.data
                                    ));
                                }
                            }
                        } else {
                            output.push_str("data: [DONE]\n\n");
                        }
                        continue;
                    }

                    // 通过 driver 转换事件格式（OpenAI 直通，Anthropic provider 需转换）
                    match state.driver.transform_stream_event(event, &state.model) {
                        Ok(Some(transformed)) => {
                            // 如果客户端协议为 Anthropic，进一步编码为 Anthropic SSE 格式
                            if is_anthropic_client {
                                let is_first = state.is_first_stream_event;
                                if let Ok(encoded) = AnthropicCodec::encode_stream_event(&transformed, is_first) {
                                    for enc_event in &encoded {
                                        output.push_str(&format!(
                                            "event: {}\ndata: {}\n\n",
                                            enc_event.event_type, enc_event.data
                                        ));
                                    }
                                    state.is_first_stream_event = false;
                                }
                            } else {
                                output.push_str(&format!("data: {}\n\n", transformed.data));
                            }

                            // 累积 token usage（如果事件携带了 usage 数据）
                            if let Some(usage) = state.driver.extract_stream_usage(&transformed) {
                                if let Some(pt) = usage.prompt_tokens {
                                    state.usage.prompt_tokens =
                                        Some(state.usage.prompt_tokens.unwrap_or(0) + pt);
                                }
                                if let Some(ct) = usage.completion_tokens {
                                    state.usage.completion_tokens =
                                        Some(state.usage.completion_tokens.unwrap_or(0) + ct);
                                }
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
                        }
                    }
                }

                // 更新 body：有输出则替换，无输出则清空避免透传原始 chunk
                if !output.is_empty() {
                    *body = Some(bytes::Bytes::from(output));
                } else if !end_of_stream {
                    // 无完整事件产出时清空 body（事件尚在缓冲中）
                    *body = Some(bytes::Bytes::new());
                }
            }

            // 流结束时汇总 total_tokens
            if end_of_stream {
                let pt = state.usage.prompt_tokens.unwrap_or(0);
                let ct = state.usage.completion_tokens.unwrap_or(0);
                if pt > 0 || ct > 0 {
                    state.usage.total_tokens = Some(pt + ct);
                }
            }

            return Ok(());
        }

        // ---- 非流式处理分支 ----
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
            match state
                .driver
                .transform_response(status, &ctx.response_headers, &full_body, &state.model)
            {
                Ok(chat_response) => {
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

                    // 设置响应头
                    ctx.response_headers_to_set.push((
                        "Content-Type".to_string(),
                        "application/json".to_string(),
                    ));
                    if let Some(state) = ctx.extensions.get::<AiRequestState>() {
                        if !state.model.model_name.is_empty() {
                            ctx.response_headers_to_set.push((
                                "X-Kong-LLM-Model".to_string(),
                                state.model.model_name.clone(),
                            ));
                        }
                    }
                }
                Err(e) => {
                    // 上游返回错误（如 4xx/5xx），透传错误信息
                    warn!("ai-proxy body_filter: transform_response failed: {}", e);
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

    async fn log(&self, _config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let state = match ctx.extensions.get::<AiRequestState>() {
            Some(s) => s,
            None => return Ok(()),
        };

        // 计算端到端延迟
        let e2e_ms = state.request_start.elapsed().as_millis() as u64;

        // 构建分析数据
        let ai_log = serde_json::json!({
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
                    "e2e_ms": e2e_ms,
                }
            }
        });

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

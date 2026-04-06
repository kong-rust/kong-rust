//! OpenAI Responses API (v1/responses) 格式编解码
//! 实现 responses ↔ chat completions 的双向转换（降级/升级）
//! 含流式事件状态机，将 chat delta 合成为 responses 事件序列

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{ChatRequest, ChatResponse, FunctionCall, Message, Tool, ToolCall};

// ============ Responses API 数据结构 ============

/// v1/responses 请求
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    /// 输入：字符串或 InputItem 数组
    pub input: serde_json::Value,
    /// 系统级指令（等同于 system message）
    #[serde(default)]
    pub instructions: Option<String>,
    /// 工具列表
    #[serde(default)]
    pub tools: Vec<serde_json::Value>,
    /// 工具选择策略
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
    /// 采样温度
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    /// 输出 token 上限
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// 是否流式
    #[serde(default)]
    pub stream: Option<bool>,
    /// 前一个 response id（多轮对话）
    #[serde(default)]
    pub previous_response_id: Option<String>,
    /// 是否后台运行
    #[serde(default)]
    pub background: Option<bool>,
    /// 透传未识别的字段
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// v1/responses 响应
#[derive(Debug, Clone, Serialize)]
pub struct ResponsesResponse {
    pub id: String,
    pub object: String,
    pub status: String,
    pub model: String,
    pub output: Vec<serde_json::Value>,
    pub usage: ResponsesUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ResponsesUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// 被剥离的不支持的工具类型列表（通过 metadata.warnings 返回给客户端）
#[derive(Debug, Clone, Default)]
pub struct StrippedTools {
    pub unsupported: Vec<String>,
}

// ============ 请求降级：ResponsesRequest → ChatRequest ============

/// 将 v1/responses 请求降级为 v1/chat/completions 请求
/// 返回 (ChatRequest, StrippedTools)
pub fn responses_to_chat(req: &ResponsesRequest) -> Result<(ChatRequest, StrippedTools), String> {
    // 检查 background 参数
    if req.background == Some(true) {
        return Err("background mode is not supported in translation path".to_string());
    }

    let mut messages: Vec<Message> = Vec::new();
    let mut stripped = StrippedTools::default();

    // instructions → system message
    if let Some(ref instructions) = req.instructions {
        if !instructions.is_empty() {
            messages.push(Message {
                role: "system".to_string(),
                content: Some(serde_json::Value::String(instructions.clone())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
    }

    // input → messages
    match &req.input {
        serde_json::Value::String(s) => {
            // 纯文本 input → 单个 user message
            messages.push(Message {
                role: "user".to_string(),
                content: Some(serde_json::Value::String(s.clone())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
        serde_json::Value::Array(items) => {
            // InputItem 数组
            for item in items {
                if let Some(msg) = input_item_to_message(item) {
                    messages.push(msg);
                }
            }
        }
        _ => {
            return Err("input must be a string or array".to_string());
        }
    }

    // tools：只保留 function 类型，剥离 built-in tools
    let mut chat_tools: Vec<Tool> = Vec::new();
    for tool in &req.tools {
        let tool_type = tool.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if tool_type == "function" {
            // 直接透传 function tool（格式兼容）
            if let Ok(t) = serde_json::from_value::<Tool>(tool.clone()) {
                chat_tools.push(t);
            }
        } else if !tool_type.is_empty() {
            // 记录不支持的工具类型
            stripped.unsupported.push(tool_type.to_string());
        }
    }

    let chat_request = ChatRequest {
        model: req.model.clone(),
        messages,
        temperature: req.temperature,
        max_tokens: req.max_output_tokens,
        top_p: req.top_p,
        top_k: None,
        stream: req.stream,
        stream_options: None,
        tools: if chat_tools.is_empty() {
            None
        } else {
            Some(chat_tools)
        },
        tool_choice: req.tool_choice.clone(),
        extra: HashMap::new(),
    };

    Ok((chat_request, stripped))
}

/// 将 input item 转换为 chat message
fn input_item_to_message(item: &serde_json::Value) -> Option<Message> {
    // responses API 的 input item 格式：
    // - {role: "user", content: "text"} 或 {role: "user", content: [{type: "input_text", text: "..."}]}
    // - {type: "function_call_output", call_id: "...", output: "..."}
    let item_type = item.get("type").and_then(|t| t.as_str());
    let role = item.get("role").and_then(|r| r.as_str());

    match (item_type, role) {
        // function_call → assistant message with tool_calls
        (Some("function_call"), _) => {
            let call_id = item.get("call_id").and_then(|v| v.as_str())?.to_string();
            let name = item.get("name").and_then(|v| v.as_str())?.to_string();
            let arguments = item
                .get("arguments")
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            Some(Message {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: call_id,
                    call_type: "function".to_string(),
                    function: FunctionCall { name, arguments },
                }]),
                tool_call_id: None,
                name: None,
            })
        }
        // function_call_output → tool message
        (Some("function_call_output"), _) => {
            let call_id = item.get("call_id").and_then(|v| v.as_str())?.to_string();
            let output = item
                .get("output")
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            Some(Message {
                role: "tool".to_string(),
                content: Some(serde_json::Value::String(output)),
                tool_calls: None,
                tool_call_id: Some(call_id),
                name: None,
            })
        }
        // message item with role
        (_, Some(r)) => {
            let content = item.get("content").cloned();
            // 如果 content 是数组，尝试提取文本
            let content = match content {
                Some(serde_json::Value::Array(parts)) => {
                    // 检查是否包含非 input_text 类型的 content part
                    let has_non_text = parts.iter().any(|p| {
                        p.get("type")
                            .and_then(|t| t.as_str())
                            .map(|t| t != "input_text")
                            .unwrap_or(true)
                    });
                    if has_non_text {
                        // 包含非文本内容（如 input_image），转换为 Chat API 格式
                        let converted: Vec<serde_json::Value> = parts
                            .iter()
                            .filter_map(|p| {
                                let pt = p.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                match pt {
                                    "input_text" => {
                                        let text = p.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                        Some(serde_json::json!({"type": "text", "text": text}))
                                    }
                                    "input_image" => {
                                        // Responses: {type: "input_image", image_url: "..."}
                                        // Chat API: {type: "image_url", image_url: {url: "..."}}
                                        let url = p.get("image_url").and_then(|u| u.as_str())
                                            .or_else(|| p.get("url").and_then(|u| u.as_str()));
                                        url.map(|u| serde_json::json!({
                                            "type": "image_url",
                                            "image_url": {"url": u}
                                        }))
                                    }
                                    "input_audio" => {
                                        // 透传 audio 数据（格式基本兼容）
                                        let data = p.get("data").cloned().unwrap_or(serde_json::Value::Null);
                                        let format = p.get("format").and_then(|f| f.as_str()).unwrap_or("wav");
                                        Some(serde_json::json!({
                                            "type": "input_audio",
                                            "input_audio": {"data": data, "format": format}
                                        }))
                                    }
                                    _ => {
                                        // 不支持的类型（如 input_file），跳过
                                        None
                                    }
                                }
                            })
                            .collect();
                        Some(serde_json::Value::Array(converted))
                    } else {
                        // 全部为 input_text，合并为纯文本字符串
                        let text: Vec<String> = parts
                            .iter()
                            .filter_map(|p| {
                                p.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                            })
                            .collect();
                        if text.is_empty() {
                            Some(serde_json::Value::Array(parts))
                        } else {
                            Some(serde_json::Value::String(text.join("")))
                        }
                    }
                }
                Some(serde_json::Value::String(s)) => Some(serde_json::Value::String(s)),
                other => other,
            };

            // assistant message 中可能包含 tool_calls
            let tool_calls = if r == "assistant" {
                // 检查 content 中是否有 function_call output items
                // (responses API 中 assistant 消息的 tool_calls 在 output items 中)
                None
            } else {
                None
            };

            Some(Message {
                role: r.to_string(),
                content,
                tool_calls,
                tool_call_id: None,
                name: None,
            })
        }
        _ => None,
    }
}

// ============ 响应升级：ChatResponse → ResponsesResponse ============

/// 将 v1/chat/completions 非流式响应升级为 v1/responses 格式
pub fn chat_to_responses(
    resp: &ChatResponse,
    stripped: &StrippedTools,
) -> ResponsesResponse {
    let mut output: Vec<serde_json::Value> = Vec::new();

    if let Some(choice) = resp.choices.first() {
        let msg = &choice.message;

        // 文本内容 → output message item
        if let Some(ref content) = msg.content {
            let text = match content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if !text.is_empty() {
                output.push(serde_json::json!({
                    "type": "message",
                    "id": format!("msg_{}", &resp.id),
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": text,
                        "annotations": []
                    }]
                }));
            }
        }

        // tool_calls → function_call output items
        if let Some(ref tool_calls) = msg.tool_calls {
            for tc in tool_calls {
                output.push(serde_json::json!({
                    "type": "function_call",
                    "id": format!("fc_{}", tc.id),
                    "call_id": tc.id,
                    "name": tc.function.name,
                    "arguments": tc.function.arguments,
                    "status": "completed"
                }));
            }
        }
    }

    // usage 映射
    let usage = resp
        .usage
        .as_ref()
        .map(|u| ResponsesUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        })
        .unwrap_or_default();

    // metadata（包含 stripped tools warnings）
    let metadata = if !stripped.unsupported.is_empty() {
        Some(serde_json::json!({
            "warnings": {
                "unsupported_tools": stripped.unsupported
            }
        }))
    } else {
        None
    };

    // 生成 response id
    let id = format!("resp_{}", uuid::Uuid::new_v4().simple());

    ResponsesResponse {
        id,
        object: "response".to_string(),
        status: "completed".to_string(),
        model: resp.model.clone(),
        output,
        usage,
        error: None,
        metadata,
    }
}

/// 构建 responses 格式的错误响应
pub fn responses_error(error_type: &str, message: &str) -> ResponsesResponse {
    let id = format!("resp_{}", uuid::Uuid::new_v4().simple());
    ResponsesResponse {
        id,
        object: "response".to_string(),
        status: "failed".to_string(),
        model: String::new(),
        output: Vec::new(),
        usage: ResponsesUsage::default(),
        error: Some(serde_json::json!({
            "type": error_type,
            "message": message
        })),
        metadata: None,
    }
}

// ============ 流式事件状态机 ============

/// 流式事件阶段
#[derive(Debug, Clone, PartialEq)]
enum StreamPhase {
    Init,
    ContentStreaming,
    ToolCallStreaming,
    Done,
}

/// 追踪的 output item（含累积内容，用于 done 事件）
#[derive(Debug, Clone)]
struct TrackedItem {
    item_type: String, // "message" | "function_call"
    index: u32,
    emitted_added: bool,
    emitted_done: bool,
    // message 类型：累积的文本内容
    accumulated_text: String,
    // function_call 类型：call_id、函数名、累积的参数
    call_id: String,
    fn_name: String,
    accumulated_arguments: String,
}

/// Responses 流式事件状态机
/// 将 chat completions 的 SSE delta 转换为 responses 事件序列
pub struct ResponsesEventState {
    phase: StreamPhase,
    response_id: String,
    model: String,
    output_items: Vec<TrackedItem>,
    content_part_index: u32,
    has_emitted_init: bool,
    /// 外部注入的 usage 数据（由 ai_proxy body_filter 在提取 usage 后设置）
    pub usage: ResponsesUsage,
}

impl ResponsesEventState {
    pub fn new() -> Self {
        let id = format!("resp_{}", uuid::Uuid::new_v4().simple());
        Self {
            phase: StreamPhase::Init,
            response_id: id,
            model: String::new(),
            output_items: Vec::new(),
            content_part_index: 0,
            has_emitted_init: false,
            usage: ResponsesUsage::default(),
        }
    }

    /// 处理一个 chat stream delta，输出 responses 事件序列
    /// 输入：OpenAI chat completion chunk 的 JSON 字符串
    /// 输出：多个 SSE 事件字符串（每个 "event: xxx\ndata: {...}\n\n"）
    pub fn process_chat_chunk(&mut self, chunk_json: &str) -> Vec<String> {
        let chunk: serde_json::Value = match serde_json::from_str(chunk_json) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        // 提取模型名称
        if let Some(m) = chunk.get("model").and_then(|v| v.as_str()) {
            if self.model.is_empty() {
                self.model = m.to_string();
            }
        }

        let choice = match chunk.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first())
        {
            Some(c) => c,
            None => return vec![],
        };

        let delta = match choice.get("delta") {
            Some(d) => d,
            None => return vec![],
        };

        let _finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());

        let mut events = Vec::new();

        // 初始化事件（首次 delta 到达时）
        if !self.has_emitted_init {
            self.has_emitted_init = true;
            events.push(self.format_event("response.created", &self.make_response_shell()));
            events.push(self.format_event("response.in_progress", &self.make_response_shell()));
        }

        // 处理 content delta
        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
            if !content.is_empty() {
                // 从 ToolCallStreaming 切回 ContentStreaming（interleaved）
                if self.phase == StreamPhase::ToolCallStreaming {
                    self.close_current_tool_call(&mut events);
                    // 开始新的 message output item
                }

                if self.phase != StreamPhase::ContentStreaming {
                    self.phase = StreamPhase::ContentStreaming;
                    let item_idx = self.output_items.len() as u32;
                    self.output_items.push(TrackedItem {
                        item_type: "message".to_string(),
                        index: item_idx,
                        emitted_added: true,
                        emitted_done: false,
                        accumulated_text: String::new(),
                        call_id: String::new(),
                        fn_name: String::new(),
                        accumulated_arguments: String::new(),
                    });
                    self.content_part_index = 0;

                    // output_item.added
                    events.push(self.format_event(
                        "response.output_item.added",
                        &serde_json::json!({
                            "output_index": item_idx,
                            "item": {
                                "type": "message",
                                "id": format!("msg_{}", item_idx),
                                "status": "in_progress",
                                "role": "assistant",
                                "content": []
                            }
                        }),
                    ));

                    // content_part.added
                    events.push(self.format_event(
                        "response.content_part.added",
                        &serde_json::json!({
                            "output_index": item_idx,
                            "content_index": self.content_part_index,
                            "part": {
                                "type": "output_text",
                                "text": "",
                                "annotations": []
                            }
                        }),
                    ));
                }

                // 累积文本内容（用于 done 事件）
                if let Some(item) = self.output_items.last_mut() {
                    item.accumulated_text.push_str(content);
                }

                // output_text.delta
                let item_idx = self.output_items.last().map(|i| i.index).unwrap_or(0);
                events.push(self.format_event(
                    "response.output_text.delta",
                    &serde_json::json!({
                        "output_index": item_idx,
                        "content_index": self.content_part_index,
                        "delta": content
                    }),
                ));
            }
        }

        // 处理 tool_calls delta
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tool_calls {
                let _tc_index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let has_id = tc.get("id").and_then(|v| v.as_str()).is_some();

                if has_id {
                    // 新的 tool call 开始
                    // 先关闭之前的 content streaming 或 tool call
                    if self.phase == StreamPhase::ContentStreaming {
                        self.close_current_content(&mut events);
                    } else if self.phase == StreamPhase::ToolCallStreaming {
                        self.close_current_tool_call(&mut events);
                    }

                    self.phase = StreamPhase::ToolCallStreaming;
                    let item_idx = self.output_items.len() as u32;
                    let call_id = tc
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("call_unknown");
                    let fn_name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    self.output_items.push(TrackedItem {
                        item_type: "function_call".to_string(),
                        index: item_idx,
                        emitted_added: true,
                        emitted_done: false,
                        accumulated_text: String::new(),
                        call_id: call_id.to_string(),
                        fn_name: fn_name.to_string(),
                        accumulated_arguments: String::new(),
                    });

                    // output_item.added
                    events.push(self.format_event(
                        "response.output_item.added",
                        &serde_json::json!({
                            "output_index": item_idx,
                            "item": {
                                "type": "function_call",
                                "id": format!("fc_{}", call_id),
                                "call_id": call_id,
                                "name": fn_name,
                                "arguments": "",
                                "status": "in_progress"
                            }
                        }),
                    ));
                }

                // function call arguments delta
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                {
                    if !args.is_empty() {
                        // 累积参数（用于 done 事件）
                        if let Some(item) = self.output_items.last_mut() {
                            item.accumulated_arguments.push_str(args);
                        }
                        let item_idx = self.output_items.last().map(|i| i.index).unwrap_or(0);
                        events.push(self.format_event(
                            "response.function_call_arguments.delta",
                            &serde_json::json!({
                                "output_index": item_idx,
                                "delta": args
                            }),
                        ));
                    }
                }
            }
        }

        // 注意：不在 finish_reason 时关闭，等待 process_done（[DONE] 事件）
        // 此时 usage 已由调用方注入，response.completed 才能携带正确的 usage 数据

        events
    }

    /// 处理 [DONE] 事件
    pub fn process_done(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        if self.phase != StreamPhase::Done {
            self.close_all(&mut events);
        }
        events
    }

    /// 处理上游错误
    pub fn process_error(&mut self, error_msg: &str) -> Vec<String> {
        let mut events = Vec::new();
        if !self.has_emitted_init {
            self.has_emitted_init = true;
            events.push(self.format_event("response.created", &self.make_response_shell()));
        }
        events.push(self.format_event(
            "response.failed",
            &serde_json::json!({
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "status": "failed",
                    "error": {
                        "type": "server_error",
                        "message": error_msg
                    }
                }
            }),
        ));
        self.phase = StreamPhase::Done;
        events
    }

    // ---- 辅助方法 ----

    fn close_current_content(&mut self, events: &mut Vec<String>) {
        let info = self.output_items.last().map(|item| {
            (item.item_type.clone(), item.index, item.emitted_done, item.accumulated_text.clone())
        });
        if let Some((item_type, idx, emitted_done, text)) = info {
            if item_type == "message" && !emitted_done {
                let cpi = self.content_part_index;
                events.push(self.format_event(
                    "response.content_part.done",
                    &serde_json::json!({
                        "output_index": idx,
                        "content_index": cpi,
                        "part": { "type": "output_text", "text": text, "annotations": [] }
                    }),
                ));
                events.push(self.format_event(
                    "response.output_item.done",
                    &serde_json::json!({
                        "output_index": idx,
                        "item": {
                            "type": "message",
                            "id": format!("msg_{}", idx),
                            "status": "completed",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": text, "annotations": [] }]
                        }
                    }),
                ));
                if let Some(item) = self.output_items.last_mut() {
                    item.emitted_done = true;
                }
            }
        }
    }

    fn close_current_tool_call(&mut self, events: &mut Vec<String>) {
        let info = self.output_items.last().map(|item| {
            (
                item.item_type.clone(),
                item.index,
                item.emitted_done,
                item.call_id.clone(),
                item.fn_name.clone(),
                item.accumulated_arguments.clone(),
            )
        });
        if let Some((item_type, idx, emitted_done, call_id, fn_name, arguments)) = info {
            if item_type == "function_call" && !emitted_done {
                events.push(self.format_event(
                    "response.function_call_arguments.done",
                    &serde_json::json!({ "output_index": idx, "arguments": arguments }),
                ));
                events.push(self.format_event(
                    "response.output_item.done",
                    &serde_json::json!({
                        "output_index": idx,
                        "item": {
                            "type": "function_call",
                            "id": format!("fc_{}", call_id),
                            "call_id": call_id,
                            "name": fn_name,
                            "arguments": arguments,
                            "status": "completed"
                        }
                    }),
                ));
                if let Some(item) = self.output_items.last_mut() {
                    item.emitted_done = true;
                }
            }
        }
    }

    fn close_all(&mut self, events: &mut Vec<String>) {
        // 关闭当前活跃的 streaming
        match self.phase {
            StreamPhase::ContentStreaming => self.close_current_content(events),
            StreamPhase::ToolCallStreaming => self.close_current_tool_call(events),
            _ => {}
        }

        // 从追踪的 output items 构建完整 output 数组
        let output: Vec<serde_json::Value> = self
            .output_items
            .iter()
            .map(|item| match item.item_type.as_str() {
                "message" => serde_json::json!({
                    "type": "message",
                    "id": format!("msg_{}", item.index),
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": item.accumulated_text,
                        "annotations": []
                    }]
                }),
                "function_call" => serde_json::json!({
                    "type": "function_call",
                    "id": format!("fc_{}", item.call_id),
                    "call_id": item.call_id,
                    "name": item.fn_name,
                    "arguments": item.accumulated_arguments,
                    "status": "completed"
                }),
                _ => serde_json::json!({}),
            })
            .collect();

        // response.completed（包含完整 output 和 usage）
        events.push(self.format_event(
            "response.completed",
            &serde_json::json!({
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "status": "completed",
                    "model": self.model,
                    "output": output,
                    "usage": {
                        "input_tokens": self.usage.input_tokens,
                        "output_tokens": self.usage.output_tokens,
                        "total_tokens": self.usage.total_tokens
                    }
                }
            }),
        ));

        self.phase = StreamPhase::Done;
    }

    fn make_response_shell(&self) -> serde_json::Value {
        serde_json::json!({
            "response": {
                "id": self.response_id,
                "object": "response",
                "status": "in_progress",
                "model": self.model
            }
        })
    }

    fn format_event(&self, event_type: &str, data: &serde_json::Value) -> String {
        format!("event: {}\ndata: {}\n\n", event_type, data.to_string())
    }
}

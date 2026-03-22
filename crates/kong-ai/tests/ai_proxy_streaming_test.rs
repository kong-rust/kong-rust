//! ai-proxy 流式 SSE 支持测试 — Task 6

use bytes::Bytes;
use kong_ai::plugins::ai_proxy::AiProxyPlugin;
use kong_ai::plugins::context::AiRequestState;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};
use serde_json::json;

// ============ 测试辅助函数 ============

/// 构建测试用插件配置
fn make_plugin_config(config_json: serde_json::Value) -> PluginConfig {
    PluginConfig {
        name: "ai-proxy".to_string(),
        config: config_json,
    }
}

/// 标准的内联 provider 配置
fn inline_provider_config(model: &str) -> serde_json::Value {
    json!({
        "model": model,
        "model_source": "config",
        "route_type": "llm/v1/chat",
        "response_streaming": "allow",
        "provider": {
            "provider_type": "openai",
            "auth_config": {
                "header_value": "Bearer sk-test-key"
            }
        }
    })
}

/// 带 stream=true 的 chat 请求体
fn openai_stream_body(model: &str) -> String {
    json!({
        "model": model,
        "stream": true,
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    })
    .to_string()
}

/// 构建 OpenAI 格式的单个 SSE 数据行
fn make_sse_chunk(content: &str, finish_reason: Option<&str>) -> String {
    let finish = match finish_reason {
        Some(r) => format!("\"{}\"", r),
        None => "null".to_string(),
    };
    format!(
        "data: {{\"id\":\"chatcmpl-test\",\"object\":\"chat.completion.chunk\",\"created\":1234567890,\"model\":\"gpt-4\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{}\"}},\"finish_reason\":{}}}]}}\n\n",
        content, finish
    )
}

/// 构建带 usage 的 SSE 数据块
fn make_sse_usage_chunk(prompt_tokens: u64, completion_tokens: u64, total_tokens: u64) -> String {
    format!(
        "data: {{\"id\":\"chatcmpl-test\",\"object\":\"chat.completion.chunk\",\"created\":1234567890,\"model\":\"gpt-4\",\"choices\":[],\"usage\":{{\"prompt_tokens\":{},\"completion_tokens\":{},\"total_tokens\":{}}}}}\n\n",
        prompt_tokens, completion_tokens, total_tokens
    )
}

// ============ header_filter 测试 ============

/// 测试：header_filter 检测到 text/event-stream 时激活流式模式
#[tokio::test]
async fn test_ai_proxy_header_filter_detects_streaming() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));

    // 先运行 access 阶段设置 AiRequestState（stream=true）
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(openai_stream_body("gpt-4"));
    plugin.access(&config, &mut ctx).await.unwrap();

    // 模拟上游返回 SSE Content-Type
    ctx.response_headers
        .insert("content-type".to_string(), "text/event-stream".to_string());

    plugin.header_filter(&config, &mut ctx).await.unwrap();

    // 验证 stream_mode 已激活
    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert!(state.stream_mode, "header_filter 应将 stream_mode 设为 true");
    assert!(state.sse_parser.is_some(), "header_filter 应初始化 sse_parser");
    assert!(
        state.response_buffer.is_some(),
        "header_filter 应初始化 response_buffer"
    );

    // 验证客户端响应 Content-Type 已设置
    let has_sse_ct = ctx
        .response_headers_to_set
        .iter()
        .any(|(k, v)| k == "content-type" && v == "text/event-stream");
    assert!(has_sse_ct, "header_filter 应设置 content-type: text/event-stream");
}

/// 测试：header_filter 对 application/x-ndjson 也激活流式模式
#[tokio::test]
async fn test_ai_proxy_header_filter_detects_ndjson_streaming() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(openai_stream_body("gpt-4"));
    plugin.access(&config, &mut ctx).await.unwrap();

    ctx.response_headers
        .insert("content-type".to_string(), "application/x-ndjson".to_string());

    plugin.header_filter(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert!(state.stream_mode, "application/x-ndjson 应触发流式模式");
}

/// 测试：非流式响应不激活流式模式
#[tokio::test]
async fn test_ai_proxy_header_filter_non_streaming_unchanged() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "Hello"}]
    }).to_string());
    plugin.access(&config, &mut ctx).await.unwrap();

    // 非流式响应
    ctx.response_headers
        .insert("content-type".to_string(), "application/json".to_string());

    plugin.header_filter(&config, &mut ctx).await.unwrap();

    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert!(!state.stream_mode, "application/json 不应触发流式模式");
    assert!(state.sse_parser.is_none(), "非流式模式不应初始化 sse_parser");
}

// ============ body_filter 流式分支测试 ============

/// 辅助函数：设置流式模式并运行 access + header_filter
async fn setup_streaming_ctx(plugin: &AiProxyPlugin, config: &PluginConfig) -> RequestCtx {
    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(openai_stream_body("gpt-4"));
    plugin.access(config, &mut ctx).await.unwrap();

    ctx.response_headers
        .insert("content-type".to_string(), "text/event-stream".to_string());
    plugin.header_filter(config, &mut ctx).await.unwrap();

    ctx
}

/// 测试：单个完整 SSE 事件正确转换输出
#[tokio::test]
async fn test_ai_proxy_body_filter_streaming_single_chunk() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));
    let mut ctx = setup_streaming_ctx(&plugin, &config).await;

    // 发送单个完整 SSE 事件
    let sse_data = make_sse_chunk("Hello", None);
    let mut body: Option<Bytes> = Some(Bytes::from(sse_data));

    plugin
        .body_filter(&config, &mut ctx, &mut body, false)
        .await
        .unwrap();

    // 应该有输出（事件完整，可立即转换）
    assert!(body.is_some(), "完整 SSE 事件应立即产生输出");
    let output = String::from_utf8_lossy(body.as_ref().unwrap());
    assert!(
        output.starts_with("data: "),
        "输出应以 'data: ' 开头，实际：{}",
        output
    );
    assert!(
        output.contains("Hello"),
        "输出应包含消息内容，实际：{}",
        output
    );
}

/// 测试：分多个 chunk 到达时正确重组 SSE 事件
#[tokio::test]
async fn test_ai_proxy_body_filter_streaming_multi_chunk() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));
    let mut ctx = setup_streaming_ctx(&plugin, &config).await;

    // 构造完整 SSE 事件并在中间截断
    let full_event = make_sse_chunk("World", None);
    let split_pos = full_event.len() / 2;
    let part1 = &full_event[..split_pos];
    let part2 = &full_event[split_pos..];

    // 第一个不完整 chunk
    let mut body1: Option<Bytes> = Some(Bytes::from(part1.to_string()));
    plugin
        .body_filter(&config, &mut ctx, &mut body1, false)
        .await
        .unwrap();

    // 事件不完整，输出应为空字节（清空 body 避免透传原始 chunk）
    let output1_len = body1
        .as_ref()
        .map(|b| b.len())
        .unwrap_or(0);
    assert_eq!(output1_len, 0, "不完整事件不应产生非空输出，实际长度：{}", output1_len);

    // 第二个 chunk 补全事件
    let mut body2: Option<Bytes> = Some(Bytes::from(part2.to_string()));
    plugin
        .body_filter(&config, &mut ctx, &mut body2, false)
        .await
        .unwrap();

    // 事件完整，应有输出
    assert!(body2.is_some(), "完整事件组合后应有输出");
    let output2 = String::from_utf8_lossy(body2.as_ref().unwrap());
    assert!(
        output2.starts_with("data: "),
        "输出应以 'data: ' 开头，实际：{}",
        output2
    );
    assert!(
        output2.contains("World"),
        "输出应包含消息内容，实际：{}",
        output2
    );
}

/// 测试：[DONE] 终止事件正确透传
#[tokio::test]
async fn test_ai_proxy_body_filter_streaming_done() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));
    let mut ctx = setup_streaming_ctx(&plugin, &config).await;

    let mut body: Option<Bytes> = Some(Bytes::from("data: [DONE]\n\n"));

    plugin
        .body_filter(&config, &mut ctx, &mut body, true)
        .await
        .unwrap();

    assert!(body.is_some(), "end_of_stream 时应有输出");
    let output = String::from_utf8_lossy(body.as_ref().unwrap());
    assert!(
        output.contains("data: [DONE]"),
        "输出应包含 [DONE]，实际：{}",
        output
    );
}

/// 测试：多个事件的 token usage 正确累积
#[tokio::test]
async fn test_ai_proxy_body_filter_streaming_usage_accumulation() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));
    let mut ctx = setup_streaming_ctx(&plugin, &config).await;

    // 第一个事件：携带 usage（prompt_tokens=10, completion_tokens=5）
    let usage_chunk1 = make_sse_usage_chunk(10, 5, 15);
    let mut body1: Option<Bytes> = Some(Bytes::from(usage_chunk1));
    plugin
        .body_filter(&config, &mut ctx, &mut body1, false)
        .await
        .unwrap();

    // 第二个事件：再携带 usage（completion_tokens=3）
    let usage_chunk2 = make_sse_usage_chunk(0, 3, 3);
    let mut body2: Option<Bytes> = Some(Bytes::from(usage_chunk2));
    plugin
        .body_filter(&config, &mut ctx, &mut body2, true)
        .await
        .unwrap();

    // 验证 usage 累积
    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    let pt = state.usage.prompt_tokens.unwrap_or(0);
    let ct = state.usage.completion_tokens.unwrap_or(0);
    // 两个事件的 prompt_tokens = 10 + 0 = 10，completion_tokens = 5 + 3 = 8
    assert_eq!(pt, 10, "prompt_tokens 应累积为 10，实际：{}", pt);
    assert_eq!(ct, 8, "completion_tokens 应累积为 8，实际：{}", ct);
    // end_of_stream 时应计算 total_tokens
    assert_eq!(
        state.usage.total_tokens,
        Some(18),
        "total_tokens 应为 18，实际：{:?}",
        state.usage.total_tokens
    );
}

/// 测试：TTFT（首 token 时间）在第一个事件到达时记录
#[tokio::test]
async fn test_ai_proxy_body_filter_streaming_ttft_recorded() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));
    let mut ctx = setup_streaming_ctx(&plugin, &config).await;

    // 初始状态：ttft 应为 None
    {
        let state = ctx.extensions.get::<AiRequestState>().unwrap();
        assert!(state.ttft.is_none(), "初始状态 ttft 应为 None");
    }

    // 发送第一个有效事件
    let sse_data = make_sse_chunk("Hi", None);
    let mut body: Option<Bytes> = Some(Bytes::from(sse_data));
    plugin
        .body_filter(&config, &mut ctx, &mut body, false)
        .await
        .unwrap();

    // 第一个事件到达后 ttft 应被记录
    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert!(
        state.ttft.is_some(),
        "第一个 SSE 事件到达后应记录 TTFT"
    );
}

/// 测试：response_buffer 在流式模式下正确累积内容
#[tokio::test]
async fn test_ai_proxy_body_filter_streaming_response_buffer() {
    let plugin = AiProxyPlugin::new();
    let config = make_plugin_config(inline_provider_config("gpt-4"));
    let mut ctx = setup_streaming_ctx(&plugin, &config).await;

    // 发送两个内容事件
    for content in &["Hello", " World"] {
        let sse_data = make_sse_chunk(content, None);
        let mut body: Option<Bytes> = Some(Bytes::from(sse_data));
        plugin
            .body_filter(&config, &mut ctx, &mut body, false)
            .await
            .unwrap();
    }

    // response_buffer 应已累积内容（用于 ai-cache 等插件回写）
    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert!(
        state.response_buffer.is_some(),
        "流式模式应维护 response_buffer"
    );
    let buf = state.response_buffer.as_ref().unwrap();
    assert!(!buf.is_empty(), "response_buffer 应已累积内容，实际为空");
}

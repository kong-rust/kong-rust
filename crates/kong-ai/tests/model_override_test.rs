//! 模型覆盖/路由测试 — model_source 策略 + X-Kong-LLM-Model header

mod helpers;

use bytes::Bytes;
use kong_ai::plugins::ai_proxy::AiProxyPlugin;
use kong_ai::plugins::context::AiRequestState;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};
use serde_json::json;

use helpers::MockLlmServer;

fn mock_openai_response() -> String {
    json!({
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "created": 1700000000u64,
        "model": "gpt-4",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hi!"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
    .to_string()
}

// ============ model_source=config 覆盖请求 model ============

#[tokio::test]
async fn test_model_source_config_ignores_request_model() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "model_source": "config",
            "route_type": "llm/v1/chat",
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "Bearer test-key" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(
        json!({"model": "gpt-3.5-turbo", "messages": [{"role": "user", "content": "Hello"}]})
            .to_string(),
    );

    plugin.access(&config, &mut ctx).await.unwrap();
    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(
        state.model.model_name, "gpt-4",
        "config model 应覆盖请求 model"
    );

    // 上游请求体也应被覆盖
    let upstream: serde_json::Value =
        serde_json::from_str(ctx.upstream_body.as_ref().unwrap()).unwrap();
    assert_eq!(upstream["model"], "gpt-4");

    mock.shutdown().await;
}

// ============ model_source=request 使用请求 model ============

#[tokio::test]
async fn test_model_source_request_uses_request_model() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "",
            "model_source": "request",
            "route_type": "llm/v1/chat",
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "Bearer test-key" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(
        json!({"model": "gpt-4-turbo", "messages": [{"role": "user", "content": "Hello"}]})
            .to_string(),
    );

    plugin.access(&config, &mut ctx).await.unwrap();
    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.model.model_name, "gpt-4-turbo");

    mock.shutdown().await;
}

// ============ X-Kong-LLM-Model header ============

#[tokio::test]
async fn test_model_override_in_response_header() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "model_name_header": true,
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "Bearer test-key" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(
        json!({"model": "gpt-4", "messages": [{"role": "user", "content": "Hello"}]}).to_string(),
    );

    plugin.access(&config, &mut ctx).await.unwrap();

    // 模拟上游响应
    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "application/json".to_string());
    let mut body = Some(Bytes::from(mock_openai_response()));

    plugin.header_filter(&config, &mut ctx).await.unwrap();
    plugin
        .body_filter(&config, &mut ctx, &mut body, true)
        .await
        .unwrap();

    // 验证 X-Kong-LLM-Model header
    let model_header = ctx
        .response_headers_to_set
        .iter()
        .find(|(k, _)| k == "X-Kong-LLM-Model");
    assert!(
        model_header.is_some(),
        "应设置 X-Kong-LLM-Model header"
    );
    assert_eq!(model_header.unwrap().1, "gpt-4");

    mock.shutdown().await;
}

// ============ config model 覆盖多个不同请求 ============

#[tokio::test]
async fn test_model_source_config_overrides_different_requests() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "gpt-4",
            "model_source": "config",
            "route_type": "llm/v1/chat",
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "Bearer test-key" }
            }
        }),
    };

    // 请求 1: model=gpt-3.5-turbo → 应被覆盖为 gpt-4
    let mut ctx1 = RequestCtx::new();
    ctx1.request_body = Some(
        json!({"model": "gpt-3.5-turbo", "messages": [{"role": "user", "content": "Hi"}]})
            .to_string(),
    );
    plugin.access(&config, &mut ctx1).await.unwrap();
    assert_eq!(
        ctx1.extensions.get::<AiRequestState>().unwrap().model.model_name,
        "gpt-4"
    );

    // 请求 2: model=claude-3-opus → 也应被覆盖为 gpt-4
    let mut ctx2 = RequestCtx::new();
    ctx2.request_body = Some(
        json!({"model": "claude-3-opus", "messages": [{"role": "user", "content": "Hi"}]})
            .to_string(),
    );
    plugin.access(&config, &mut ctx2).await.unwrap();
    assert_eq!(
        ctx2.extensions.get::<AiRequestState>().unwrap().model.model_name,
        "gpt-4"
    );

    mock.shutdown().await;
}

// ============ model_source=request 保留原始 model ============

#[tokio::test]
async fn test_model_source_request_preserves_original() {
    let mock = MockLlmServer::start().await;
    let plugin = AiProxyPlugin::new();
    let config = PluginConfig {
        name: "ai-proxy".to_string(),
        config: json!({
            "model": "ignored-model",
            "model_source": "request",
            "route_type": "llm/v1/chat",
            "provider": {
                "provider_type": "openai",
                "endpoint_url": format!("http://127.0.0.1:{}", mock.port),
                "auth_config": { "header_value": "Bearer test-key" }
            }
        }),
    };

    let mut ctx = RequestCtx::new();
    ctx.request_body = Some(
        json!({"model": "gpt-4-0125-preview", "messages": [{"role": "user", "content": "Hello"}]})
            .to_string(),
    );

    plugin.access(&config, &mut ctx).await.unwrap();
    let state = ctx.extensions.get::<AiRequestState>().unwrap();
    assert_eq!(state.model.model_name, "gpt-4-0125-preview");

    // 上游请求体也应保持原值
    let upstream: serde_json::Value =
        serde_json::from_str(ctx.upstream_body.as_ref().unwrap()).unwrap();
    assert_eq!(upstream["model"], "gpt-4-0125-preview");

    mock.shutdown().await;
}

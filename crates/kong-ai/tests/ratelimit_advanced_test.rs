//! 高级限流测试 — TPM 预扣/修正、多维度限流、RPM+TPM 联合

mod helpers;

use kong_ai::plugins::ai_proxy::AiProxyPlugin;
use kong_ai::plugins::ai_rate_limit::AiRateLimitPlugin;
use kong_ai::ratelimit::memory::MemoryRateLimiter;
use kong_ai::ratelimit::RateLimiter;
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

use helpers::MockLlmServer;

fn make_chat_body() -> String {
    json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "Hello!"}]
    })
    .to_string()
}

fn make_large_chat_body() -> String {
    // 约 800 字节 → 估算 ~200 tokens
    let content = "x".repeat(700);
    json!({
        "model": "gpt-4",
        "messages": [{"role": "user", "content": content}]
    })
    .to_string()
}

// ============ TPM 限流阻断 ============

#[tokio::test]
async fn test_rate_limit_tpm_blocks() {
    let limiter = Arc::new(MemoryRateLimiter::new(Duration::from_secs(60)));
    let rate_plugin = AiRateLimitPlugin::with_limiter(limiter);
    let config = PluginConfig {
        name: "ai-rate-limit".to_string(),
        config: json!({"tpm_limit": 100, "limit_by": "global"}),
    };

    // 第一个大请求: 通过（初始计数为 0，预扣后 > 100）
    let mut ctx1 = RequestCtx::new();
    ctx1.request_body = Some(make_large_chat_body());
    rate_plugin.access(&config, &mut ctx1).await.unwrap();
    assert!(!ctx1.short_circuited, "第一个请求应通过");

    // 第二个大请求: 应被阻断（预扣已超过 100）
    let mut ctx2 = RequestCtx::new();
    ctx2.request_body = Some(make_large_chat_body());
    rate_plugin.access(&config, &mut ctx2).await.unwrap();
    assert!(ctx2.short_circuited, "第二个请求应被 TPM 阻断");
    assert_eq!(ctx2.exit_status, Some(429));
}

// ============ TPM log 阶段修正 ============

#[tokio::test]
async fn test_rate_limit_tpm_log_correction() {
    let limiter = Arc::new(MemoryRateLimiter::new(Duration::from_secs(60)));
    let rate_plugin = AiRateLimitPlugin::with_limiter(limiter.clone());
    let config = PluginConfig {
        name: "ai-rate-limit".to_string(),
        config: json!({"tpm_limit": 10000, "limit_by": "global"}),
    };

    let mock = MockLlmServer::start().await;
    let proxy_plugin = AiProxyPlugin::new();
    let proxy_config = PluginConfig {
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
    let body = make_chat_body();
    ctx.request_body = Some(body);

    // access: rate limit → proxy
    rate_plugin.access(&config, &mut ctx).await.unwrap();
    assert!(!ctx.short_circuited);
    proxy_plugin.access(&proxy_config, &mut ctx).await.unwrap();

    // 模拟上游响应（actual usage = 150 tokens）
    ctx.response_status = Some(200);
    ctx.response_headers
        .insert("content-type".to_string(), "application/json".to_string());
    let resp = json!({
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "created": 1700000000u64,
        "model": "gpt-4",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hi!"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150}
    })
    .to_string();
    let mut body_bytes = Some(bytes::Bytes::from(resp));

    proxy_plugin.header_filter(&proxy_config, &mut ctx).await.unwrap();
    proxy_plugin
        .body_filter(&proxy_config, &mut ctx, &mut body_bytes, true)
        .await
        .unwrap();

    // log: proxy → rate limit 修正
    proxy_plugin.log(&proxy_config, &mut ctx).await.unwrap();
    rate_plugin.log(&config, &mut ctx).await.unwrap();

    // 验证 TPM 计数器 >= 150（actual）
    let (_, count) = limiter.check("global:tpm", 100000);
    assert!(
        count >= 150,
        "修正后 TPM 计数应 >= 150 (actual tokens)，实际: {}",
        count
    );

    mock.shutdown().await;
}

// ============ limit_by=route 独立限流 ============

#[tokio::test]
async fn test_rate_limit_by_route() {
    let limiter = Arc::new(MemoryRateLimiter::new(Duration::from_secs(60)));
    let rate_plugin = AiRateLimitPlugin::with_limiter(limiter);
    let config = PluginConfig {
        name: "ai-rate-limit".to_string(),
        config: json!({"rpm_limit": 1, "limit_by": "route"}),
    };

    let route_a = uuid::Uuid::new_v4();
    let route_b = uuid::Uuid::new_v4();

    // Route A 第一个请求: 通过
    let mut ctx_a1 = RequestCtx::new();
    ctx_a1.route_id = Some(route_a);
    ctx_a1.request_body = Some(make_chat_body());
    rate_plugin.access(&config, &mut ctx_a1).await.unwrap();
    assert!(!ctx_a1.short_circuited);

    // Route A 第二个请求: 阻断
    let mut ctx_a2 = RequestCtx::new();
    ctx_a2.route_id = Some(route_a);
    ctx_a2.request_body = Some(make_chat_body());
    rate_plugin.access(&config, &mut ctx_a2).await.unwrap();
    assert!(ctx_a2.short_circuited, "Route A 应被限流");

    // Route B 第一个请求: 通过（独立计数）
    let mut ctx_b1 = RequestCtx::new();
    ctx_b1.route_id = Some(route_b);
    ctx_b1.request_body = Some(make_chat_body());
    rate_plugin.access(&config, &mut ctx_b1).await.unwrap();
    assert!(!ctx_b1.short_circuited, "Route B 应通过");
}

// ============ limit_by=consumer 独立限流 ============

#[tokio::test]
async fn test_rate_limit_by_consumer() {
    let limiter = Arc::new(MemoryRateLimiter::new(Duration::from_secs(60)));
    let rate_plugin = AiRateLimitPlugin::with_limiter(limiter);
    let config = PluginConfig {
        name: "ai-rate-limit".to_string(),
        config: json!({"rpm_limit": 1, "limit_by": "consumer"}),
    };

    let consumer_a = uuid::Uuid::new_v4();
    let consumer_b = uuid::Uuid::new_v4();

    // Consumer A: 通过 → 阻断
    let mut ctx_a1 = RequestCtx::new();
    ctx_a1.consumer_id = Some(consumer_a);
    ctx_a1.request_body = Some(make_chat_body());
    rate_plugin.access(&config, &mut ctx_a1).await.unwrap();
    assert!(!ctx_a1.short_circuited);

    let mut ctx_a2 = RequestCtx::new();
    ctx_a2.consumer_id = Some(consumer_a);
    ctx_a2.request_body = Some(make_chat_body());
    rate_plugin.access(&config, &mut ctx_a2).await.unwrap();
    assert!(ctx_a2.short_circuited, "Consumer A 应被限流");

    // Consumer B: 通过
    let mut ctx_b1 = RequestCtx::new();
    ctx_b1.consumer_id = Some(consumer_b);
    ctx_b1.request_body = Some(make_chat_body());
    rate_plugin.access(&config, &mut ctx_b1).await.unwrap();
    assert!(!ctx_b1.short_circuited, "Consumer B 应通过");
}

// ============ RPM + TPM 联合限流 ============

#[tokio::test]
async fn test_rate_limit_combined_rpm_tpm() {
    let limiter = Arc::new(MemoryRateLimiter::new(Duration::from_secs(60)));
    let rate_plugin = AiRateLimitPlugin::with_limiter(limiter);
    // RPM=100（不触发），TPM=50（容易触发）
    let config = PluginConfig {
        name: "ai-rate-limit".to_string(),
        config: json!({"rpm_limit": 100, "tpm_limit": 50, "limit_by": "global"}),
    };

    // 第一个大请求: 通过
    let mut ctx1 = RequestCtx::new();
    ctx1.request_body = Some(make_large_chat_body());
    rate_plugin.access(&config, &mut ctx1).await.unwrap();
    assert!(!ctx1.short_circuited);

    // 第二个大请求: TPM 超限阻断
    let mut ctx2 = RequestCtx::new();
    ctx2.request_body = Some(make_large_chat_body());
    rate_plugin.access(&config, &mut ctx2).await.unwrap();
    assert!(ctx2.short_circuited, "TPM 超限应阻断");

    let exit_body: serde_json::Value =
        serde_json::from_str(ctx2.exit_body.as_ref().unwrap()).unwrap();
    assert!(
        exit_body.get("current_tpm").is_some(),
        "TPM 阻断时应包含 current_tpm"
    );
}

// ============ RPM 先触发 ============

#[tokio::test]
async fn test_rate_limit_rpm_blocks_before_tpm() {
    let limiter = Arc::new(MemoryRateLimiter::new(Duration::from_secs(60)));
    let rate_plugin = AiRateLimitPlugin::with_limiter(limiter);
    // RPM=1（低），TPM=100000（高）
    let config = PluginConfig {
        name: "ai-rate-limit".to_string(),
        config: json!({"rpm_limit": 1, "tpm_limit": 100000, "limit_by": "global"}),
    };

    let mut ctx1 = RequestCtx::new();
    ctx1.request_body = Some(make_chat_body());
    rate_plugin.access(&config, &mut ctx1).await.unwrap();
    assert!(!ctx1.short_circuited);

    let mut ctx2 = RequestCtx::new();
    ctx2.request_body = Some(make_chat_body());
    rate_plugin.access(&config, &mut ctx2).await.unwrap();
    assert!(ctx2.short_circuited, "RPM 应阻断");

    let exit_body: serde_json::Value =
        serde_json::from_str(ctx2.exit_body.as_ref().unwrap()).unwrap();
    assert!(
        exit_body.get("current_rpm").is_some(),
        "RPM 阻断时应包含 current_rpm"
    );
}

//! Token 计数 + 成本计算集成测试

use kong_ai::provider::TokenUsage;
use kong_ai::token::{calculate_cost, TokenCounter};

// ─── TokenCounter 测试 ────────────────────────────────────────────────────────

#[test]
fn test_token_count_provider_usage_takes_priority() {
    // provider 提供精确值时，应直接返回，忽略本地计算
    let counter = TokenCounter::new();
    assert_eq!(counter.count("gpt-4", "hello world", Some(42)), 42);
}

#[test]
fn test_token_count_tiktoken_fallback() {
    // 无 provider usage 时，tiktoken 应对 GPT 系列模型给出合理计数
    let counter = TokenCounter::new();
    let count = counter.count("gpt-4", "Hello, world!", None);
    assert!(count > 0);
    assert!(count < 10); // "Hello, world!" 通常约 4 个 token
}

#[test]
fn test_token_count_estimate_fallback() {
    // 未知模型 tiktoken 无法识别，回落到字符估算
    let counter = TokenCounter::new();
    // "hello world test" = 16 chars → (16+3)/4 = 4 tokens
    let count = counter.count("some-unknown-model-xyz", "hello world test", None);
    assert_eq!(count, 4);
}

#[test]
fn test_token_count_estimate_edge_cases() {
    // 空字符串
    assert_eq!(TokenCounter::count_estimate(""), 0);
    // 4 字符恰好 1 token
    assert_eq!(TokenCounter::count_estimate("abcd"), 1);
    // 8 字符 = 2 tokens
    assert_eq!(TokenCounter::count_estimate("abcdefgh"), 2);
    // 1 字符向上取整 = 1 token
    assert_eq!(TokenCounter::count_estimate("a"), 1);
}

// ─── calculate_cost 测试 ──────────────────────────────────────────────────────

#[test]
fn test_cost_calculation() {
    // input_cost=30/M，output_cost=60/M
    // (1000*30 + 500*60) / 1_000_000 = (30000 + 30000) / 1_000_000 = 0.06
    let usage = TokenUsage {
        prompt_tokens: Some(1000),
        completion_tokens: Some(500),
        total_tokens: Some(1500),
    };
    let cost = calculate_cost(&usage, Some(30.0), Some(60.0));
    assert!((cost - 0.06).abs() < 1e-10);
}

#[test]
fn test_cost_calculation_no_costs() {
    // 未配置价格时成本为 0
    let usage = TokenUsage {
        prompt_tokens: Some(100),
        completion_tokens: Some(50),
        total_tokens: Some(150),
    };
    assert_eq!(calculate_cost(&usage, None, None), 0.0);
}

#[test]
fn test_cost_calculation_no_usage() {
    // usage 全为 None（default）时成本为 0
    let usage = TokenUsage::default();
    assert_eq!(calculate_cost(&usage, Some(30.0), Some(60.0)), 0.0);
}

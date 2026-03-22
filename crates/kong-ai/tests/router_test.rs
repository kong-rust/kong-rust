//! Model Router 测试 — AI 网关智能路由（正则匹配 + 加权选择）
//! Tests for AI Gateway intelligent model routing: regex matching + weighted selection

use kong_ai::provider::router::{ModelRouteConfig, ModelRouter, ModelTargetConfig};

// ============ 辅助函数 — test helpers ============

fn target(provider: &str, model: &str, weight: u32) -> ModelTargetConfig {
    ModelTargetConfig {
        provider_type: provider.to_string(),
        model_name: model.to_string(),
        endpoint_url: None,
        auth_config: serde_json::json!({}),
        weight,
    }
}

fn target_with_endpoint(
    provider: &str,
    model: &str,
    endpoint: &str,
    weight: u32,
) -> ModelTargetConfig {
    ModelTargetConfig {
        provider_type: provider.to_string(),
        model_name: model.to_string(),
        endpoint_url: Some(endpoint.to_string()),
        auth_config: serde_json::json!({"header_value": "test-key"}),
        weight,
    }
}

fn rule(pattern: &str, targets: Vec<ModelTargetConfig>) -> ModelRouteConfig {
    ModelRouteConfig {
        pattern: pattern.to_string(),
        targets,
    }
}

// ============ 正则匹配 — regex matching ============

#[test]
fn test_exact_match_routes_to_provider() {
    // 精确匹配 "^gpt-4$" → OpenAI gpt-4，不匹配 "gpt-4o"
    let router = ModelRouter::from_configs(&[rule(
        "^gpt-4$",
        vec![target("openai", "gpt-4", 1)],
    )])
    .unwrap();

    let res = router.resolve("gpt-4").unwrap();
    assert_eq!(res.provider_type, "openai");
    assert_eq!(res.model.model_name, "gpt-4");
    assert!(router.resolve("gpt-4o").is_none());
}

#[test]
fn test_prefix_match_maps_variants() {
    // "^gpt-4" 匹配 gpt-4/gpt-4o/gpt-4-turbo → 全部路由到同一 provider
    let router = ModelRouter::from_configs(&[rule(
        "^gpt-4",
        vec![target("openai", "gpt-4o", 1)],
    )])
    .unwrap();

    for name in &["gpt-4", "gpt-4o", "gpt-4-turbo"] {
        let res = router.resolve(name).unwrap();
        assert_eq!(res.provider_type, "openai");
        // model_name 是路由目标的 model_name，不是请求中的
        assert_eq!(res.model.model_name, "gpt-4o");
    }
    assert!(router.resolve("claude-3").is_none());
}

#[test]
fn test_model_name_preserved_in_resolution() {
    // resolve 结果中 model.name 保留原始请求的 model 名
    let router = ModelRouter::from_configs(&[rule(
        "^gpt-4",
        vec![target("openai", "gpt-4o-2024-08-06", 1)],
    )])
    .unwrap();

    let res = router.resolve("gpt-4o-mini").unwrap();
    assert_eq!(res.model.name, "gpt-4o-mini"); // 原始请求 model
    assert_eq!(res.model.model_name, "gpt-4o-2024-08-06"); // 实际发给 provider 的
}

#[test]
fn test_first_rule_wins() {
    // 多条规则按顺序匹配，第一条命中即返回（不走后面的通配符）
    let router = ModelRouter::from_configs(&[
        rule("^gpt-4", vec![target("openai", "gpt-4o", 1)]),
        rule("^gpt", vec![target("openai_compat", "gpt-3.5-turbo", 1)]),
        rule(".*", vec![target("openai", "fallback-model", 1)]),
    ])
    .unwrap();

    let res = router.resolve("gpt-4o").unwrap();
    assert_eq!(res.provider_type, "openai");
    assert_eq!(res.model.model_name, "gpt-4o");

    let res = router.resolve("gpt-3.5-turbo").unwrap();
    assert_eq!(res.provider_type, "openai_compat");

    let res = router.resolve("claude-3-opus").unwrap();
    assert_eq!(res.model.model_name, "fallback-model");
}

#[test]
fn test_multi_provider_routing() {
    // 不同 pattern → 不同 provider
    let router = ModelRouter::from_configs(&[
        rule("^gpt-4", vec![target("openai", "gpt-4o", 1)]),
        rule("^claude", vec![target("anthropic", "claude-3-opus-20240229", 1)]),
        rule("^gemini", vec![target("gemini", "gemini-pro", 1)]),
        rule("^qwen", vec![target_with_endpoint("openai_compat", "qwen-turbo", "https://dashscope.aliyuncs.com", 1)]),
    ])
    .unwrap();

    let res = router.resolve("gpt-4o").unwrap();
    assert_eq!(res.provider_type, "openai");

    let res = router.resolve("claude-3-opus").unwrap();
    assert_eq!(res.provider_type, "anthropic");
    assert_eq!(res.model.model_name, "claude-3-opus-20240229");

    let res = router.resolve("gemini-pro-vision").unwrap();
    assert_eq!(res.provider_type, "gemini");

    let res = router.resolve("qwen-turbo-latest").unwrap();
    assert_eq!(res.provider_type, "openai_compat");
    assert_eq!(
        res.provider_config.endpoint_url.as_deref(),
        Some("https://dashscope.aliyuncs.com")
    );

    assert!(router.resolve("llama-3").is_none());
}

#[test]
fn test_no_match_returns_none() {
    let router = ModelRouter::from_configs(&[rule(
        "^gpt-4",
        vec![target("openai", "gpt-4o", 1)],
    )])
    .unwrap();
    assert!(router.resolve("claude-3").is_none());
}

#[test]
fn test_case_sensitive_by_default() {
    let router = ModelRouter::from_configs(&[rule(
        "^GPT-4",
        vec![target("openai", "gpt-4", 1)],
    )])
    .unwrap();
    assert!(router.resolve("GPT-4").is_some());
    assert!(router.resolve("gpt-4").is_none());
}

#[test]
fn test_case_insensitive_with_flag() {
    let router = ModelRouter::from_configs(&[rule(
        "(?i)^gpt-4",
        vec![target("openai", "gpt-4", 1)],
    )])
    .unwrap();
    assert!(router.resolve("GPT-4").is_some());
    assert!(router.resolve("gpt-4").is_some());
    assert!(router.resolve("Gpt-4-turbo").is_some());
}

// ============ 加权路由 — weighted routing ============

#[test]
fn test_weighted_routing_80_20_distribution() {
    // 同一 pattern 下两个 target：80% OpenAI / 20% Azure
    // 加权轮询是确定性的，1000 次应精确 800/200
    let router = ModelRouter::from_configs(&[rule(
        "^gpt-4",
        vec![
            target_with_endpoint("openai", "gpt-4o", "https://api.openai.com", 80),
            target_with_endpoint("openai_compat", "gpt-4o", "https://azure.openai.com", 20),
        ],
    )])
    .unwrap();

    let mut openai_count = 0;
    let mut azure_count = 0;
    for _ in 0..1000 {
        let res = router.resolve("gpt-4o").unwrap();
        match res.provider_type.as_str() {
            "openai" => openai_count += 1,
            "openai_compat" => azure_count += 1,
            other => panic!("unexpected provider: {}", other),
        }
    }
    assert_eq!(openai_count, 800, "OpenAI should get exactly 800/1000");
    assert_eq!(azure_count, 200, "Azure should get exactly 200/1000");
}

#[test]
fn test_weighted_routing_equal_weights() {
    // 权重 1:1 → 50/50 分布
    let router = ModelRouter::from_configs(&[rule(
        "^gpt-4",
        vec![
            target("openai", "gpt-4o", 1),
            target("openai_compat", "gpt-4o", 1),
        ],
    )])
    .unwrap();

    let mut counts = [0u32; 2];
    for _ in 0..100 {
        let res = router.resolve("gpt-4o").unwrap();
        match res.provider_type.as_str() {
            "openai" => counts[0] += 1,
            "openai_compat" => counts[1] += 1,
            _ => panic!("unexpected"),
        }
    }
    assert_eq!(counts[0], 50);
    assert_eq!(counts[1], 50);
}

#[test]
fn test_weighted_routing_single_target() {
    // 只有一个 target → 始终返回该 target（无需加权）
    let router = ModelRouter::from_configs(&[rule(
        "^gpt-4",
        vec![target("openai", "gpt-4o", 100)],
    )])
    .unwrap();

    for _ in 0..100 {
        let res = router.resolve("gpt-4o").unwrap();
        assert_eq!(res.provider_type, "openai");
    }
}

#[test]
fn test_weighted_routing_three_targets() {
    // 三路加权：50/30/20
    let router = ModelRouter::from_configs(&[rule(
        "^gpt-4",
        vec![
            target("openai", "gpt-4o", 50),
            target_with_endpoint("openai_compat", "gpt-4o", "https://azure.com", 30),
            target_with_endpoint("openai_compat", "gpt-4o", "https://us-east.azure.com", 20),
        ],
    )])
    .unwrap();

    let mut counts = std::collections::HashMap::new();
    for _ in 0..1000 {
        let res = router.resolve("gpt-4o").unwrap();
        let key = res
            .provider_config
            .endpoint_url
            .unwrap_or_else(|| "default".to_string());
        *counts.entry(key).or_insert(0u32) += 1;
    }
    assert_eq!(*counts.get("default").unwrap_or(&0), 500);
    assert_eq!(*counts.get("https://azure.com").unwrap_or(&0), 300);
    assert_eq!(*counts.get("https://us-east.azure.com").unwrap_or(&0), 200);
}

#[test]
fn test_weighted_routing_preserves_auth_config() {
    // 验证路由结果保留了 auth_config
    let router = ModelRouter::from_configs(&[rule(
        "^gpt",
        vec![ModelTargetConfig {
            provider_type: "openai".to_string(),
            model_name: "gpt-4o".to_string(),
            endpoint_url: None,
            auth_config: serde_json::json!({"header_value": "sk-my-secret-key"}),
            weight: 1,
        }],
    )])
    .unwrap();

    let res = router.resolve("gpt-4o").unwrap();
    assert_eq!(
        res.provider_config.auth_config["header_value"],
        "sk-my-secret-key"
    );
}

// ============ 边界情况 — edge cases ============

#[test]
fn test_empty_routes_returns_none() {
    let router = ModelRouter::from_configs(&[]).unwrap();
    assert!(router.resolve("gpt-4").is_none());
}

#[test]
fn test_invalid_regex_returns_error() {
    let result = ModelRouter::from_configs(&[rule(
        "[invalid",
        vec![target("openai", "gpt-4", 1)],
    )]);
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(err_msg.contains("invalid model route regex"));
}

#[test]
fn test_empty_targets_returns_error() {
    let result = ModelRouter::from_configs(&[rule("^gpt-4", vec![])]);
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(err_msg.contains("has no targets"));
}

#[test]
fn test_wildcard_fallback() {
    // ".*" 作为最后的兜底规则
    let router = ModelRouter::from_configs(&[
        rule("^gpt", vec![target("openai", "gpt-4o", 1)]),
        rule(".*", vec![target("openai_compat", "default-model", 1)]),
    ])
    .unwrap();

    // 不匹配 gpt → 命中通配符
    let res = router.resolve("llama-3").unwrap();
    assert_eq!(res.provider_type, "openai_compat");
    assert_eq!(res.model.model_name, "default-model");
}

// ============ 配置序列化 — config deserialization ============

#[test]
fn test_ai_proxy_config_with_model_routes() {
    // 验证完整 JSON 配置可正确反序列化
    let json = serde_json::json!({
        "model_routes": [
            {
                "pattern": "^gpt-4",
                "targets": [
                    { "provider_type": "openai", "model_name": "gpt-4o", "weight": 80,
                      "auth_config": { "header_value": "sk-openai" } },
                    { "provider_type": "openai_compat", "model_name": "gpt-4o", "weight": 20,
                      "endpoint_url": "https://azure.openai.com",
                      "auth_config": { "header_value": "azure-key" } }
                ]
            },
            {
                "pattern": "^claude",
                "targets": [
                    { "provider_type": "anthropic", "model_name": "claude-3-opus-20240229",
                      "auth_config": { "header_value": "sk-ant-xxx" } }
                ]
            }
        ]
    });

    let cfg: kong_ai::plugins::ai_proxy::AiProxyConfig =
        serde_json::from_value(json).expect("should parse");

    assert_eq!(cfg.model_routes.len(), 2);
    assert_eq!(cfg.model_routes[0].pattern, "^gpt-4");
    assert_eq!(cfg.model_routes[0].targets.len(), 2);
    assert_eq!(cfg.model_routes[0].targets[0].provider_type, "openai");
    assert_eq!(cfg.model_routes[0].targets[0].weight, 80);
    assert_eq!(cfg.model_routes[0].targets[1].provider_type, "openai_compat");
    assert_eq!(
        cfg.model_routes[0].targets[1].endpoint_url.as_deref(),
        Some("https://azure.openai.com")
    );
    assert_eq!(cfg.model_routes[1].targets[0].provider_type, "anthropic");
    // 默认 weight=1
    assert_eq!(cfg.model_routes[1].targets[0].weight, 1);
}

#[test]
fn test_ai_proxy_config_without_model_routes() {
    // 不配置 model_routes 时默认为空
    let json = serde_json::json!({
        "model": "gpt-4",
        "provider": { "provider_type": "openai" }
    });

    let cfg: kong_ai::plugins::ai_proxy::AiProxyConfig =
        serde_json::from_value(json).expect("should parse");

    assert!(cfg.model_routes.is_empty());
}

// ============ 完整使用场景 — realistic usage scenarios ============

#[test]
fn test_scenario_ab_testing_between_providers() {
    // 场景：A/B 测试 — 70% 请求走 OpenAI，30% 走自建 vLLM
    let router = ModelRouter::from_configs(&[rule(
        "^llm-v1",
        vec![
            target("openai", "gpt-4o", 70),
            target_with_endpoint("openai_compat", "qwen2.5-72b", "http://vllm.internal:8000", 30),
        ],
    )])
    .unwrap();

    let mut openai = 0;
    let mut vllm = 0;
    for _ in 0..1000 {
        let res = router.resolve("llm-v1-chat").unwrap();
        match res.provider_type.as_str() {
            "openai" => openai += 1,
            "openai_compat" => vllm += 1,
            _ => panic!("unexpected"),
        }
    }
    assert_eq!(openai, 700);
    assert_eq!(vllm, 300);
}

#[test]
fn test_scenario_cost_optimization_routing() {
    // 场景：成本优化 — 简单请求走便宜模型，复杂请求走贵模型
    // 通过不同 model 名前缀路由
    let router = ModelRouter::from_configs(&[
        rule("^cheap-", vec![target("openai", "gpt-3.5-turbo", 1)]),
        rule("^smart-", vec![target("anthropic", "claude-3-opus-20240229", 1)]),
        rule(".*", vec![target("openai", "gpt-4o-mini", 1)]),
    ])
    .unwrap();

    let res = router.resolve("cheap-summarize").unwrap();
    assert_eq!(res.model.model_name, "gpt-3.5-turbo");

    let res = router.resolve("smart-reasoning").unwrap();
    assert_eq!(res.provider_type, "anthropic");

    let res = router.resolve("anything-else").unwrap();
    assert_eq!(res.model.model_name, "gpt-4o-mini");
}

#[test]
fn test_scenario_multi_region_failover() {
    // 场景：多区域部署 — 同一模型跨区域加权分配
    let router = ModelRouter::from_configs(&[rule(
        "^gpt-4",
        vec![
            target_with_endpoint("openai_compat", "gpt-4o", "https://us-east.azure.com", 50),
            target_with_endpoint("openai_compat", "gpt-4o", "https://eu-west.azure.com", 30),
            target_with_endpoint("openai_compat", "gpt-4o", "https://ap-east.azure.com", 20),
        ],
    )])
    .unwrap();

    let mut counts = std::collections::HashMap::new();
    for _ in 0..1000 {
        let res = router.resolve("gpt-4o").unwrap();
        let ep = res.provider_config.endpoint_url.unwrap();
        *counts.entry(ep).or_insert(0u32) += 1;
    }
    assert_eq!(counts["https://us-east.azure.com"], 500);
    assert_eq!(counts["https://eu-west.azure.com"], 300);
    assert_eq!(counts["https://ap-east.azure.com"], 200);
}

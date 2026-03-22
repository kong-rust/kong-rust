//! Model Router 测试 — 正则匹配 + 加权选择
//! Tests for ModelRouter: regex matching and weighted selection

use kong_ai::provider::router::{ModelRouteConfig, ModelRouter};

// ============ 辅助函数 ============

fn route(pattern: &str, target: &str, weight: u32) -> ModelRouteConfig {
    ModelRouteConfig {
        pattern: pattern.to_string(),
        target_group: target.to_string(),
        weight,
    }
}

fn route_default(pattern: &str, target: &str) -> ModelRouteConfig {
    route(pattern, target, 1)
}

// ============ 基础正则匹配 ============

#[test]
fn test_exact_match() {
    // pattern "^gpt-4$" 匹配 "gpt-4" 但不匹配 "gpt-4o"
    let router = ModelRouter::from_configs(&[route_default("^gpt-4$", "exact-gpt4")]).unwrap();
    assert_eq!(router.resolve("gpt-4"), Some("exact-gpt4".to_string()));
    assert_eq!(router.resolve("gpt-4o"), None);
    assert_eq!(router.resolve("gpt-4-turbo"), None);
}

#[test]
fn test_prefix_match() {
    // pattern "^gpt-4" 匹配 "gpt-4", "gpt-4o", "gpt-4-turbo"
    let router = ModelRouter::from_configs(&[route_default("^gpt-4", "openai-gpt4")]).unwrap();
    assert_eq!(router.resolve("gpt-4"), Some("openai-gpt4".to_string()));
    assert_eq!(router.resolve("gpt-4o"), Some("openai-gpt4".to_string()));
    assert_eq!(
        router.resolve("gpt-4-turbo"),
        Some("openai-gpt4".to_string())
    );
    assert_eq!(router.resolve("claude-3"), None);
}

#[test]
fn test_wildcard_match() {
    // pattern ".*" 匹配所有内容（默认 fallback）
    let router = ModelRouter::from_configs(&[route_default(".*", "default-fallback")]).unwrap();
    assert_eq!(
        router.resolve("gpt-4"),
        Some("default-fallback".to_string())
    );
    assert_eq!(
        router.resolve("claude-3"),
        Some("default-fallback".to_string())
    );
    assert_eq!(
        router.resolve("anything"),
        Some("default-fallback".to_string())
    );
}

#[test]
fn test_no_match_returns_none() {
    // pattern "^gpt-4" 不匹配 "claude-3"
    let router = ModelRouter::from_configs(&[route_default("^gpt-4", "openai")]).unwrap();
    assert_eq!(router.resolve("claude-3"), None);
}

#[test]
fn test_first_match_wins_no_weights() {
    // 两条规则："^gpt.*" → "group-a", ".*" → "default"
    // "gpt-4" 同时匹配两条，但只有一条匹配时返回该条
    // 当 "claude-3" 只匹配 ".*" 时返回 "default"
    let router = ModelRouter::from_configs(&[
        route_default("^gpt.*", "group-a"),
        route_default(".*", "default"),
    ])
    .unwrap();

    // "claude-3" 只匹配 ".*" → "default"
    assert_eq!(router.resolve("claude-3"), Some("default".to_string()));

    // "gpt-4" 匹配两条，加权轮询（equal weight=1,1）→ 交替返回
    // 但两条都有 weight=1，所以应该 50/50 分布
    let mut group_a_count = 0;
    let mut default_count = 0;
    for _ in 0..100 {
        match router.resolve("gpt-4").as_deref() {
            Some("group-a") => group_a_count += 1,
            Some("default") => default_count += 1,
            other => panic!("unexpected result: {:?}", other),
        }
    }
    assert_eq!(group_a_count, 50);
    assert_eq!(default_count, 50);
}

// ============ 加权路由 ============

#[test]
fn test_weighted_routing_distribution() {
    // pattern "^gpt-4" → "group-a" weight=80
    // pattern "^gpt-4" → "group-b" weight=20
    // 1000 次调用：group-a ~800, group-b ~200
    let router = ModelRouter::from_configs(&[
        route("^gpt-4", "group-a", 80),
        route("^gpt-4", "group-b", 20),
    ])
    .unwrap();

    let mut counts = std::collections::HashMap::new();
    for _ in 0..1000 {
        let result = router.resolve("gpt-4").unwrap();
        *counts.entry(result).or_insert(0) += 1;
    }

    let a = *counts.get("group-a").unwrap_or(&0);
    let b = *counts.get("group-b").unwrap_or(&0);

    // 加权轮询是确定性的：800/200 精确分布
    assert_eq!(a, 800, "group-a should get exactly 800 out of 1000");
    assert_eq!(b, 200, "group-b should get exactly 200 out of 1000");
}

#[test]
fn test_weighted_routing_single_weight() {
    // 只有一条匹配规则 → 始终返回该目标
    let router = ModelRouter::from_configs(&[route("^gpt-4", "only-group", 50)]).unwrap();

    for _ in 0..100 {
        assert_eq!(
            router.resolve("gpt-4"),
            Some("only-group".to_string())
        );
    }
}

#[test]
fn test_weighted_routing_equal_weights() {
    // 两条规则 weight=1, weight=1 → ~50/50 分布
    let router = ModelRouter::from_configs(&[
        route("^gpt-4", "group-x", 1),
        route("^gpt-4", "group-y", 1),
    ])
    .unwrap();

    let mut x_count = 0;
    let mut y_count = 0;
    for _ in 0..100 {
        match router.resolve("gpt-4").as_deref() {
            Some("group-x") => x_count += 1,
            Some("group-y") => y_count += 1,
            other => panic!("unexpected: {:?}", other),
        }
    }
    assert_eq!(x_count, 50);
    assert_eq!(y_count, 50);
}

// ============ 复杂场景 ============

#[test]
fn test_multiple_patterns_different_models() {
    // 多模式匹配不同模型
    let router = ModelRouter::from_configs(&[
        route_default("^gpt-4", "openai"),
        route_default("^claude", "anthropic"),
        route_default("^gemini", "google"),
    ])
    .unwrap();

    assert_eq!(router.resolve("gpt-4o"), Some("openai".to_string()));
    assert_eq!(
        router.resolve("claude-3-opus"),
        Some("anthropic".to_string())
    );
    assert_eq!(
        router.resolve("gemini-pro"),
        Some("google".to_string())
    );
    assert_eq!(router.resolve("llama-3"), None);
}

#[test]
fn test_case_sensitive_by_default() {
    // 默认大小写敏感："^GPT-4" 不匹配 "gpt-4"
    let router = ModelRouter::from_configs(&[route_default("^GPT-4", "uppercase")]).unwrap();
    assert_eq!(router.resolve("GPT-4"), Some("uppercase".to_string()));
    assert_eq!(router.resolve("gpt-4"), None);
}

#[test]
fn test_case_insensitive_with_flag() {
    // 使用 (?i) 标志进行大小写不敏感匹配
    let router =
        ModelRouter::from_configs(&[route_default("(?i)^gpt-4", "case-insensitive")]).unwrap();
    assert_eq!(
        router.resolve("GPT-4"),
        Some("case-insensitive".to_string())
    );
    assert_eq!(
        router.resolve("gpt-4"),
        Some("case-insensitive".to_string())
    );
    assert_eq!(
        router.resolve("Gpt-4-turbo"),
        Some("case-insensitive".to_string())
    );
}

#[test]
fn test_empty_routes_returns_none() {
    // 空路由表 → resolve 返回 None
    let router = ModelRouter::from_configs(&[]).unwrap();
    assert_eq!(router.resolve("gpt-4"), None);
    assert_eq!(router.resolve("anything"), None);
}

#[test]
fn test_invalid_regex_returns_error() {
    // 无效正则 → from_configs 返回 Err
    let result = ModelRouter::from_configs(&[route_default("[invalid", "target")]);
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("invalid model route regex"),
        "error message should mention invalid regex, got: {}",
        err_msg
    );
}

// ============ from_configs 构建 ============

#[test]
fn test_from_configs_builds_correctly() {
    // 验证 ModelRouteConfig → ModelRouter 转换正常
    let configs = vec![
        route("^gpt-4.*", "openai-gpt4", 80),
        route("^gpt-4.*", "azure-gpt4", 20),
        route_default("^claude-.*", "anthropic-claude"),
        route_default(".*", "default-fallback"),
    ];

    let router = ModelRouter::from_configs(&configs).unwrap();

    // gpt-4o 匹配前两条 + 最后一条 → 加权选择
    let result = router.resolve("gpt-4o");
    assert!(result.is_some());

    // claude-3 匹配第三条 + 最后一条
    let result = router.resolve("claude-3");
    assert!(result.is_some());

    // unknown 只匹配 ".*" → default-fallback
    assert_eq!(
        router.resolve("unknown-model"),
        Some("default-fallback".to_string())
    );
}

// ============ 集成测试 — 与 ai-proxy 配置的集成 ============

#[test]
fn test_ai_proxy_config_with_model_routes() {
    // 验证 AiProxyConfig 能正确解析 model_routes 字段
    let json = serde_json::json!({
        "model": "gpt-4",
        "model_routes": [
            { "pattern": "^gpt-4.*", "target_group": "openai-gpt4", "weight": 80 },
            { "pattern": "^gpt-4.*", "target_group": "azure-gpt4", "weight": 20 },
            { "pattern": "^claude-.*", "target_group": "anthropic-claude" },
            { "pattern": ".*", "target_group": "default-fallback" }
        ],
        "provider": {
            "provider_type": "openai",
            "auth_config": { "api_key": "test-key" }
        }
    });

    let cfg: kong_ai::plugins::ai_proxy::AiProxyConfig =
        serde_json::from_value(json).expect("should parse AiProxyConfig with model_routes");

    assert_eq!(cfg.model_routes.len(), 4);
    assert_eq!(cfg.model_routes[0].pattern, "^gpt-4.*");
    assert_eq!(cfg.model_routes[0].target_group, "openai-gpt4");
    assert_eq!(cfg.model_routes[0].weight, 80);
    assert_eq!(cfg.model_routes[2].target_group, "anthropic-claude");
    // 默认 weight=1
    assert_eq!(cfg.model_routes[2].weight, 1);
}

#[test]
fn test_ai_proxy_config_without_model_routes() {
    // 不配置 model_routes 时默认为空
    let json = serde_json::json!({
        "model": "gpt-4",
        "provider": {
            "provider_type": "openai"
        }
    });

    let cfg: kong_ai::plugins::ai_proxy::AiProxyConfig =
        serde_json::from_value(json).expect("should parse AiProxyConfig without model_routes");

    assert!(cfg.model_routes.is_empty());
}

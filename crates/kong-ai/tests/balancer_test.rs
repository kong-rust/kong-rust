//! ModelGroupBalancer 测试套件 — 加权 LB、优先级 fallback、冷却机制
//! Test suite for ModelGroupBalancer: weighted LB, priority fallback, cooldown

use kong_ai::models::{AiModel, AiProviderConfig};
use kong_ai::provider::ModelGroupBalancer;
use uuid::Uuid;

// ============ 测试辅助函数 — test helpers ============

/// 构建测试用 AiModel — build a minimal AiModel for testing
fn make_model(id: Uuid, priority: i32, weight: i32, enabled: bool) -> AiModel {
    AiModel {
        id,
        name: "test-group".to_string(),
        provider_id: Uuid::nil(),
        model_name: "gpt-4".to_string(),
        priority,
        weight,
        enabled,
        ..Default::default()
    }
}

/// 构建测试用 AiProviderConfig — build a minimal AiProviderConfig for testing
fn make_provider(id: Uuid) -> AiProviderConfig {
    AiProviderConfig {
        id,
        name: "test-provider".to_string(),
        provider_type: "openai".to_string(),
        enabled: true,
        ..Default::default()
    }
}

/// 从 spec 列表构建 balancer — build balancer from (id, priority, weight, enabled) specs
fn make_balancer(specs: Vec<(Uuid, i32, i32, bool)>) -> ModelGroupBalancer {
    let pairs = specs
        .into_iter()
        .map(|(id, priority, weight, enabled)| {
            (make_model(id, priority, weight, enabled), make_provider(id))
        })
        .collect();
    ModelGroupBalancer::new(pairs)
}

// ============ 测试用例 — test cases ============

/// 加权 round-robin 分布验证
/// 3 个 model，权重 80/10/10，1000 次 select，统计分布 ±5%
#[test]
fn test_balancer_weighted_round_robin() {
    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();
    let id_c = Uuid::new_v4();

    let balancer = make_balancer(vec![
        (id_a, 10, 80, true),
        (id_b, 10, 10, true),
        (id_c, 10, 10, true),
    ]);

    let mut counts = std::collections::HashMap::new();
    let n = 1000usize;
    for _ in 0..n {
        let (model, _) = balancer.select().expect("select should succeed");
        *counts.entry(model.id).or_insert(0usize) += 1;
    }

    let count_a = *counts.get(&id_a).unwrap_or(&0);
    let count_b = *counts.get(&id_b).unwrap_or(&0);
    let count_c = *counts.get(&id_c).unwrap_or(&0);

    // 期望分布：80%/10%/10%，容忍 ±5% — expected 80/10/10 ±5%
    let tolerance = (n as f64 * 0.05) as usize;
    assert!(
        count_a >= n * 75 / 100 && count_a <= n * 85 / 100,
        "model_a should be ~80%, got {}/{} (tolerance ±{})",
        count_a, n, tolerance
    );
    assert!(
        count_b >= n * 5 / 100 && count_b <= n * 15 / 100,
        "model_b should be ~10%, got {}/{} (tolerance ±{})",
        count_b, n, tolerance
    );
    assert!(
        count_c >= n * 5 / 100 && count_c <= n * 15 / 100,
        "model_c should be ~10%, got {}/{} (tolerance ±{})",
        count_c, n, tolerance
    );
}

/// 优先级 fallback：高优先级全部不可用时自动选低优先级
/// Priority fallback: when all high-priority models are unavailable, fall back to lower priority
#[test]
fn test_balancer_priority_fallback() {
    let id_high = Uuid::new_v4();
    let id_low = Uuid::new_v4();

    let balancer = make_balancer(vec![
        (id_high, 10, 1, true),
        (id_low, 5, 1, true),
    ]);

    // 先让 id_high 连续失败 3 次触发冷却 — trigger cooldown for high-priority model
    for _ in 0..3 {
        balancer.report_failure(&id_high, None);
    }

    // 现在选择应返回低优先级 model — should now select low-priority model
    let (model, _) = balancer.select().expect("select should succeed");
    assert_eq!(
        model.id, id_low,
        "should fall back to low-priority model after high-priority enters cooldown"
    );
}

/// 冷却期内不被选中；冷却到期后恢复
/// Model is skipped during cooldown; recovers after expiry
#[test]
fn test_balancer_cooldown_recovery() {
    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();

    let balancer = make_balancer(vec![
        (id_a, 10, 1, true),
        (id_b, 10, 1, true),
    ]);

    // 让 id_a 进入冷却 — put id_a into cooldown
    for _ in 0..3 {
        balancer.report_failure(&id_a, None);
    }

    // 冷却期内 id_a 不应被选到 — id_a should not be selected during cooldown
    for _ in 0..20 {
        let (model, _) = balancer.select().expect("select should succeed");
        assert_ne!(model.id, id_a, "id_a should be in cooldown");
    }

    // 直接操控冷却截止时间为过去（测试冷却到期）
    // Manipulate cooldown expiry to simulate recovery
    // 找到 id_a 对应的条目并设置冷却过期 — set cooldown to expired
    // 由于 entries 是私有的，我们用 report_success 来重置冷却
    // entries is private; use report_success to simulate recovery
    balancer.report_success(&id_a);

    // 重置后应能再次被选中 — after recovery, id_a can be selected again
    let mut seen_a = false;
    for _ in 0..40 {
        let (model, _) = balancer.select().expect("select should succeed");
        if model.id == id_a {
            seen_a = true;
            break;
        }
    }
    assert!(seen_a, "id_a should be selectable after cooldown recovery");
}

/// 429 立即触发冷却 — HTTP 429 immediately triggers cooldown
#[test]
fn test_balancer_429_immediate_cooldown() {
    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();

    let balancer = make_balancer(vec![
        (id_a, 10, 1, true),
        (id_b, 10, 1, true),
    ]);

    // 单次 429 即触发冷却 — one 429 immediately triggers cooldown
    balancer.report_failure(&id_a, Some(429));

    for _ in 0..20 {
        let (model, _) = balancer.select().expect("select should succeed");
        assert_ne!(model.id, id_a, "id_a should be in cooldown after 429");
    }
}

/// 成功后重置连续失败计数 — success resets consecutive failure count
#[test]
fn test_balancer_success_resets_failures() {
    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();

    let balancer = make_balancer(vec![
        (id_a, 10, 1, true),
        (id_b, 10, 1, true),
    ]);

    // 2 次失败（不触发冷却，需要 3 次）— 2 failures (no cooldown yet, needs 3)
    balancer.report_failure(&id_a, None);
    balancer.report_failure(&id_a, None);

    // 成功重置 — success resets
    balancer.report_success(&id_a);

    // 再失败 2 次（加上之前的 0 次 = 共 2 次，不应触发冷却）
    // 2 more failures after reset → total 2, should not trigger cooldown
    balancer.report_failure(&id_a, None);
    balancer.report_failure(&id_a, None);

    // id_a 应仍可选中（未触发冷却）— id_a should still be selectable
    let mut seen_a = false;
    for _ in 0..40 {
        let (model, _) = balancer.select().expect("select should succeed");
        if model.id == id_a {
            seen_a = true;
            break;
        }
    }
    assert!(seen_a, "id_a should still be available after success reset 2 failures");
}

/// 全部 model 冷却时 select() 返回 Err — all models in cooldown → select() returns Err
#[test]
fn test_balancer_all_models_down() {
    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();

    let balancer = make_balancer(vec![
        (id_a, 10, 1, true),
        (id_b, 10, 1, true),
    ]);

    // 所有 model 进入冷却 — put all models into cooldown
    balancer.report_failure(&id_a, Some(429));
    balancer.report_failure(&id_b, Some(429));

    let result = balancer.select();
    assert!(result.is_err(), "select() should return Err when all models are in cooldown");
}

/// disabled model 不参与选择 — disabled models are never selected
#[test]
fn test_balancer_disabled_models_skipped() {
    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();

    let balancer = make_balancer(vec![
        (id_a, 10, 1, false), // disabled — 禁用
        (id_b, 10, 1, true),
    ]);

    for _ in 0..20 {
        let (model, _) = balancer.select().expect("select should succeed");
        assert_ne!(model.id, id_a, "disabled model should never be selected");
        assert_eq!(model.id, id_b);
    }
}

/// 单一 model 始终返回自身 — single model is always returned
#[test]
fn test_balancer_single_model() {
    let id_a = Uuid::new_v4();
    let balancer = make_balancer(vec![(id_a, 10, 1, true)]);

    for _ in 0..10 {
        let (model, _) = balancer.select().expect("select should succeed");
        assert_eq!(model.id, id_a);
    }
}

/// 空 balancer：is_empty() 为 true，select() 返回 Err
/// Empty balancer: is_empty() is true, select() returns Err
#[test]
fn test_balancer_empty() {
    let balancer = ModelGroupBalancer::new(vec![]);
    assert!(balancer.is_empty(), "empty balancer should report is_empty() == true");
    let result = balancer.select();
    assert!(result.is_err(), "empty balancer select() should return Err");
}

// ============ by_token_size 路由测试 — max_input_tokens filtering ============

/// 构建带 max_input_tokens 的 model — build a model with a per-request prompt cap
fn make_model_with_cap(
    id: Uuid,
    priority: i32,
    weight: i32,
    enabled: bool,
    max_input_tokens: Option<i32>,
) -> AiModel {
    AiModel {
        id,
        name: "test-group".to_string(),
        provider_id: Uuid::nil(),
        model_name: "gpt-4".to_string(),
        priority,
        weight,
        enabled,
        max_input_tokens,
        ..Default::default()
    }
}

fn make_balancer_with_caps(
    specs: Vec<(Uuid, i32, i32, bool, Option<i32>)>,
) -> ModelGroupBalancer {
    let pairs = specs
        .into_iter()
        .map(|(id, priority, weight, enabled, cap)| {
            (
                make_model_with_cap(id, priority, weight, enabled, cap),
                make_provider(id),
            )
        })
        .collect();
    ModelGroupBalancer::new(pairs)
}

/// prompt_tokens=None 时 token 过滤完全禁用,行为与 select() 一致
#[test]
fn test_select_for_none_disables_token_filtering() {
    let id_a = Uuid::new_v4();
    let balancer = make_balancer_with_caps(vec![(id_a, 10, 1, true, Some(100))]);

    // 即使 prompt 远超 cap,prompt_tokens=None 不启用过滤
    let (model, _) = balancer.select_for(None).expect("None disables filter");
    assert_eq!(model.id, id_a);
}

/// model.max_input_tokens=None 视为无限制
#[test]
fn test_select_for_unbounded_model_always_matches() {
    let id_a = Uuid::new_v4();
    let balancer = make_balancer_with_caps(vec![(id_a, 10, 1, true, None)]);

    let (model, _) = balancer
        .select_for(Some(1_000_000))
        .expect("unbounded model should match any size");
    assert_eq!(model.id, id_a);
}

/// 同 priority 内,prompt 超出 cap 的 model 被过滤
/// Within the same priority, models exceeding cap are filtered out
#[test]
fn test_select_for_filters_oversized_within_priority() {
    let id_small = Uuid::new_v4();
    let id_large = Uuid::new_v4();
    // 都在 priority=10,small cap=100,large cap=10000
    let balancer = make_balancer_with_caps(vec![
        (id_small, 10, 1, true, Some(100)),
        (id_large, 10, 1, true, Some(10_000)),
    ]);

    // prompt=500 → small 被过滤,只剩 large
    for _ in 0..20 {
        let (model, _) = balancer.select_for(Some(500)).expect("large fits 500");
        assert_eq!(model.id, id_large);
    }
}

/// 同 priority 全部超出 cap → fallback 到下一 priority
/// All candidates in tier exceed cap → fall back to next priority tier
#[test]
fn test_select_for_priority_fallback_on_oversize() {
    let id_high_small = Uuid::new_v4(); // priority=20, cap=100
    let id_low_huge = Uuid::new_v4(); // priority=10, cap=1_000_000

    let balancer = make_balancer_with_caps(vec![
        (id_high_small, 20, 1, true, Some(100)),
        (id_low_huge, 10, 1, true, Some(1_000_000)),
    ]);

    // 短 prompt → 高优先级胜出
    let (m, _) = balancer.select_for(Some(50)).expect("short prompt");
    assert_eq!(m.id, id_high_small);

    // 长 prompt → 高优先级被过滤,fallback 到低优先级大模型
    let (m, _) = balancer.select_for(Some(500)).expect("falls back to low tier");
    assert_eq!(m.id, id_low_huge);
}

/// 全部模型都装不下 → 返回 Err
/// All models too small → returns Err
#[test]
fn test_select_for_all_oversize_returns_err() {
    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();
    let balancer = make_balancer_with_caps(vec![
        (id_a, 20, 1, true, Some(100)),
        (id_b, 10, 1, true, Some(200)),
    ]);
    let result = balancer.select_for(Some(10_000));
    assert!(result.is_err(), "no model can fit huge prompt");
}

/// max_input_tokens 边界:prompt_tokens == cap 应通过
/// Boundary: prompt_tokens == cap should pass
#[test]
fn test_select_for_exact_boundary_passes() {
    let id_a = Uuid::new_v4();
    let balancer = make_balancer_with_caps(vec![(id_a, 10, 1, true, Some(100))]);
    let (m, _) = balancer.select_for(Some(100)).expect("equality is allowed");
    assert_eq!(m.id, id_a);
}

/// max_input_tokens 边界:prompt_tokens == cap+1 应被过滤
#[test]
fn test_select_for_just_over_cap_filtered() {
    let id_small = Uuid::new_v4();
    let id_huge = Uuid::new_v4();
    let balancer = make_balancer_with_caps(vec![
        (id_small, 20, 1, true, Some(100)),
        (id_huge, 10, 1, true, None),
    ]);
    let (m, _) = balancer.select_for(Some(101)).expect("falls back to unbounded");
    assert_eq!(m.id, id_huge, "small model should be filtered when prompt=cap+1");
}

/// max_input_tokens<=0 视为无限制(防御 i32 负值或 0 值)
#[test]
fn test_select_for_non_positive_cap_treated_as_unlimited() {
    let id_a = Uuid::new_v4();
    let balancer = make_balancer_with_caps(vec![(id_a, 10, 1, true, Some(0))]);
    let (m, _) = balancer
        .select_for(Some(1_000_000))
        .expect("cap=0 means unlimited");
    assert_eq!(m.id, id_a);
}

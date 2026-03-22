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

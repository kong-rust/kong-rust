//! Model Group 负载均衡器 — 加权 round-robin + 优先级 fallback + 冷却机制
//! Weighted round-robin load balancer with priority-based fallback and cooldown

use crate::models::{AiModel, AiProviderConfig};
use kong_core::error::{KongError, Result};
use uuid::Uuid;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 模型健康状态 — per-model health tracking
struct ModelHealth {
    /// 连续失败次数（原子操作）— consecutive failure count (atomic)
    consecutive_failures: AtomicU32,
    /// 冷却截止时间，None 表示正常 — cooldown expiry, None means healthy
    cooldown_until: Mutex<Option<Instant>>,
}

impl ModelHealth {
    fn new() -> Self {
        Self {
            consecutive_failures: AtomicU32::new(0),
            cooldown_until: Mutex::new(None),
        }
    }

    /// 检查是否可用（未在冷却期内）— check if available (not in cooldown)
    fn is_available(&self) -> bool {
        let guard = self.cooldown_until.lock().unwrap();
        match *guard {
            Some(t) => Instant::now() >= t, // 冷却到期则可用 — available once cooldown expires
            None => true,
        }
    }

    /// 进入冷却状态 — enter cooldown for the given duration
    fn enter_cooldown(&self, duration: Duration) {
        let mut guard = self.cooldown_until.lock().unwrap();
        *guard = Some(Instant::now() + duration);
    }

    /// 重置健康状态（成功时调用）— reset health state on success
    fn reset(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        let mut guard = self.cooldown_until.lock().unwrap();
        *guard = None;
    }
}

/// 模型条目（model + provider config + 健康状态）
/// Model entry combining model metadata, provider config, and health state
struct ModelEntry {
    model: AiModel,
    provider_config: AiProviderConfig,
    health: ModelHealth,
}

/// Model Group 负载均衡器
/// 同 name 的多个 AiModel 组成一个 group
/// 同 priority 组内做加权 round-robin，不同 priority 间做 fallback
///
/// Model Group load balancer.
/// Models sharing the same name form a group.
/// Within a priority tier: weighted round-robin.
/// Across priority tiers: highest priority first, fallback on exhaustion.
pub struct ModelGroupBalancer {
    /// 所有模型条目，按 priority 降序排列以方便分组
    /// All model entries, sorted descending by priority for grouping
    entries: Vec<ModelEntry>,
    /// 全局轮转计数器 — global round-robin counter
    counter: AtomicU64,
}

impl ModelGroupBalancer {
    /// 从 model+provider pairs 构建 balancer（应在配置刷新时预构建并 Arc 缓存）
    /// Build balancer from model+provider pairs. Pre-build and cache behind Arc on config refresh.
    pub fn new(models: Vec<(AiModel, AiProviderConfig)>) -> Self {
        let mut entries: Vec<ModelEntry> = models
            .into_iter()
            .map(|(model, provider_config)| ModelEntry {
                model,
                provider_config,
                health: ModelHealth::new(),
            })
            .collect();

        // 按 priority 降序排序，priority 相同时按 id 保证稳定性
        // Sort by priority descending; use id as tiebreaker for stability
        entries.sort_by(|a, b| {
            b.model.priority.cmp(&a.model.priority)
                .then_with(|| a.model.id.cmp(&b.model.id))
        });

        Self {
            entries,
            counter: AtomicU64::new(0),
        }
    }

    /// 选择一个健康的 model
    /// Select a healthy model.
    ///
    /// 算法 / Algorithm:
    /// 1. 按 priority 降序分组（高 priority 优先）
    /// 2. 从最高 priority 组开始，过滤掉冷却中和 disabled 的
    /// 3. 在可用组内按 weight 加权 round-robin
    /// 4. 该组全不可用 → fallback 到下一 priority 组
    /// 5. 全组不可用 → 返回 Err
    pub fn select(&self) -> Result<(&AiModel, &AiProviderConfig)> {
        if self.entries.is_empty() {
            return Err(KongError::InternalError(
                "model group is empty".to_string(),
            ));
        }

        // 收集所有 priority 档位（已排序，直接取唯一值）
        // Collect distinct priority tiers (entries are already sorted)
        let mut priorities: Vec<i32> = self.entries.iter().map(|e| e.model.priority).collect();
        priorities.dedup();

        for priority in &priorities {
            // 同 priority 的可用条目（enabled 且未冷却）
            // Candidates in this priority tier: enabled and not in cooldown
            let candidates: Vec<&ModelEntry> = self.entries
                .iter()
                .filter(|e| {
                    e.model.priority == *priority
                        && e.model.enabled
                        && e.health.is_available()
                })
                .collect();

            if candidates.is_empty() {
                // 该优先级组全部不可用，fallback 到下一级
                // This tier is exhausted, fall back to next priority
                continue;
            }

            // 计算加权总量 — compute total weight for this tier
            let total_weight: i32 = candidates.iter().map(|e| e.model.weight).sum();
            if total_weight <= 0 {
                // 权重全为 0 时退化为简单轮转 — degenerate to simple round-robin on zero weights
                let idx = self.counter.fetch_add(1, Ordering::Relaxed) as usize % candidates.len();
                let entry = candidates[idx];
                return Ok((&entry.model, &entry.provider_config));
            }

            // 加权 round-robin — weighted round-robin
            // 在 u64 域取模后转 i32，避免 u64 → i32 截断导致溢出 panic
            let position = (self.counter.fetch_add(1, Ordering::Relaxed)
                % total_weight as u64) as i32;
            let mut cumulative: i32 = 0;
            for entry in &candidates {
                cumulative += entry.model.weight;
                if position < cumulative {
                    return Ok((&entry.model, &entry.provider_config));
                }
            }

            // 理论上不会到这里（浮点/整数溢出保险）— should never reach here
            let entry = candidates.last().unwrap();
            return Ok((&entry.model, &entry.provider_config));
        }

        Err(KongError::InternalError(
            "all models in group are unavailable (cooldown or disabled)".to_string(),
        ))
    }

    /// 标记请求成功 — 重置连续失败计数和冷却
    /// Report success: reset failure count and cooldown for the given model.
    pub fn report_success(&self, model_id: &Uuid) {
        if let Some(entry) = self.entries.iter().find(|e| e.model.id == *model_id) {
            entry.health.reset();
        }
    }

    /// 标记请求失败
    /// Report failure:
    /// - HTTP 429 (Rate Limit) → 立即 10s 冷却 — immediate 10s cooldown
    /// - 连续 3 次失败 → 30s 冷却 — 30s cooldown after 3 consecutive failures
    pub fn report_failure(&self, model_id: &Uuid, status: Option<u16>) {
        let Some(entry) = self.entries.iter().find(|e| e.model.id == *model_id) else {
            return;
        };

        if status == Some(429) {
            // 429 立即触发冷却 — 429 triggers immediate cooldown
            entry.health.enter_cooldown(Duration::from_secs(10));
            return;
        }

        // 累积连续失败计数 — increment consecutive failure count
        let failures = entry.health.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= 3 {
            entry.health.enter_cooldown(Duration::from_secs(30));
        }
    }

    /// 检查 balancer 是否为空（无任何 model）
    /// Check whether the balancer has no models.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

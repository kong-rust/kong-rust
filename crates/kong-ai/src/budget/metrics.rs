//! 预算执行链的低基数进程内 telemetry。
//!
//! 所有维度均由封闭枚举定义，禁止把 request、Virtual Key、route 或 intent
//! 等高基数字段作为标签。部署可以把快照适配到 Prometheus/OpenTelemetry。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::{BudgetBackendKind, BudgetErrorKind};

const OPERATION_COUNT: usize = 15;
const RESULT_COUNT: usize = 5;
const ERROR_KIND_COUNT: usize = 14;
const CHECKPOINT_RESULT_COUNT: usize = 4;
const RECONCILIATION_ACTION_COUNT: usize = 5;
const RECONCILIATION_RESULT_COUNT: usize = 5;
pub const BUDGET_REGISTRY_STATE_COUNT: usize = 8;

/// 预算后端调用或运行时动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BudgetMetricOperation {
    Inspect = 0,
    AdmissionPermit = 1,
    CreateIntent = 2,
    MarkDispatching = 3,
    Settle = 4,
    RegisterOwner = 5,
    HeartbeatOwner = 6,
    StopOwner = 7,
    RegistryRecovery = 8,
    StaleRecovery = 9,
    Checkpoint = 10,
    Reconcile = 11,
    Verify = 12,
    Rebuild = 13,
    LookupIntent = 14,
}

impl BudgetMetricOperation {
    pub const ALL: [Self; OPERATION_COUNT] = [
        Self::Inspect,
        Self::AdmissionPermit,
        Self::CreateIntent,
        Self::MarkDispatching,
        Self::Settle,
        Self::RegisterOwner,
        Self::HeartbeatOwner,
        Self::StopOwner,
        Self::RegistryRecovery,
        Self::StaleRecovery,
        Self::Checkpoint,
        Self::Reconcile,
        Self::Verify,
        Self::Rebuild,
        Self::LookupIntent,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::AdmissionPermit => "admission_permit",
            Self::CreateIntent => "create_intent",
            Self::MarkDispatching => "mark_dispatching",
            Self::Settle => "settle",
            Self::RegisterOwner => "register_owner",
            Self::HeartbeatOwner => "heartbeat_owner",
            Self::StopOwner => "stop_owner",
            Self::RegistryRecovery => "registry_recovery",
            Self::StaleRecovery => "stale_recovery",
            Self::Checkpoint => "checkpoint",
            Self::Reconcile => "reconcile",
            Self::Verify => "verify",
            Self::Rebuild => "rebuild",
            Self::LookupIntent => "lookup_intent",
        }
    }
}

/// 预算动作的有界结果维度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BudgetMetricResult {
    Success = 0,
    Replayed = 1,
    Rejected = 2,
    Unresolved = 3,
    Failed = 4,
}

impl BudgetMetricResult {
    pub const ALL: [Self; RESULT_COUNT] = [
        Self::Success,
        Self::Replayed,
        Self::Rejected,
        Self::Unresolved,
        Self::Failed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Replayed => "replayed",
            Self::Rejected => "rejected",
            Self::Unresolved => "unresolved",
            Self::Failed => "failed",
        }
    }
}

/// 活动 intent 注册表的封闭状态维度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BudgetRegistryMetricState {
    Preparing = 0,
    ActivePrepared = 1,
    DispatchCommitPending = 2,
    ActiveDispatching = 3,
    RetryWithFact = 4,
    NeedsIntentLookup = 5,
    NeedsSafeZero = 6,
    NeedsUnresolved = 7,
}

impl BudgetRegistryMetricState {
    pub const ALL: [Self; BUDGET_REGISTRY_STATE_COUNT] = [
        Self::Preparing,
        Self::ActivePrepared,
        Self::DispatchCommitPending,
        Self::ActiveDispatching,
        Self::RetryWithFact,
        Self::NeedsIntentLookup,
        Self::NeedsSafeZero,
        Self::NeedsUnresolved,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::ActivePrepared => "active_prepared",
            Self::DispatchCommitPending => "dispatch_commit_pending",
            Self::ActiveDispatching => "active_dispatching",
            Self::RetryWithFact => "retry_with_fact",
            Self::NeedsIntentLookup => "needs_intent_lookup",
            Self::NeedsSafeZero => "needs_safe_zero",
            Self::NeedsUnresolved => "needs_unresolved",
        }
    }

    pub(crate) const fn is_recovery(self) -> bool {
        matches!(
            self,
            Self::RetryWithFact
                | Self::NeedsIntentLookup
                | Self::NeedsSafeZero
                | Self::NeedsUnresolved
        )
    }
}

/// checkpoint 调度结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BudgetCheckpointMetricResult {
    Applied = 0,
    Replayed = 1,
    Skipped = 2,
    Failed = 3,
}

impl BudgetCheckpointMetricResult {
    pub const ALL: [Self; CHECKPOINT_RESULT_COUNT] =
        [Self::Applied, Self::Replayed, Self::Skipped, Self::Failed];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Replayed => "replayed",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// reconciliation 的封闭动作集合。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BudgetReconciliationMetricAction {
    ResolveNotIncurred = 0,
    ResolveObservedCost = 1,
    Verify = 2,
    Rebuild = 3,
    RepairCheckpoint = 4,
}

impl BudgetReconciliationMetricAction {
    pub const ALL: [Self; RECONCILIATION_ACTION_COUNT] = [
        Self::ResolveNotIncurred,
        Self::ResolveObservedCost,
        Self::Verify,
        Self::Rebuild,
        Self::RepairCheckpoint,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResolveNotIncurred => "resolve_not_incurred",
            Self::ResolveObservedCost => "resolve_observed_cost",
            Self::Verify => "verify",
            Self::Rebuild => "rebuild",
            Self::RepairCheckpoint => "repair_checkpoint",
        }
    }
}

/// reconciliation 的封闭结果集合。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BudgetReconciliationMetricResult {
    Applied = 0,
    Replayed = 1,
    NoChange = 2,
    Rejected = 3,
    Failed = 4,
}

impl BudgetReconciliationMetricResult {
    pub const ALL: [Self; RECONCILIATION_RESULT_COUNT] = [
        Self::Applied,
        Self::Replayed,
        Self::NoChange,
        Self::Rejected,
        Self::Failed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Replayed => "replayed",
            Self::NoChange => "no_change",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }
}

/// registry 自身提供的有界状态快照。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BudgetRegistryMetricsSnapshot {
    pub active_entries: usize,
    pub rejected_entries: u64,
    pub field_limit_rejections: u64,
    pub states: [usize; BUDGET_REGISTRY_STATE_COUNT],
}

/// 单个非零 operation counter。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetOperationMetric {
    pub backend: BudgetBackendKind,
    pub operation: BudgetMetricOperation,
    pub result: BudgetMetricResult,
    pub error_kind: Option<BudgetErrorKind>,
    pub count: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetRegistryStateMetric {
    pub state: BudgetRegistryMetricState,
    pub count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetCheckpointResultMetric {
    pub result: BudgetCheckpointMetricResult,
    pub count: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetReconciliationMetric {
    pub action: BudgetReconciliationMetricAction,
    pub result: BudgetReconciliationMetricResult,
    pub count: u64,
}

/// 可由监控 adapter 周期拉取的稳定快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetTelemetrySnapshot {
    pub backend: BudgetBackendKind,
    pub operations: Vec<BudgetOperationMetric>,
    pub pending_intents: u64,
    pub unresolved_intents: u64,
    pub registry_active_entries: usize,
    pub registry_rejected_entries: u64,
    pub registry_field_limit_rejections: u64,
    pub registry_states: Vec<BudgetRegistryStateMetric>,
    pub owner_available: bool,
    pub heartbeat_lag_ms: u64,
    pub recovery_depth: usize,
    pub recovery_scanned: u64,
    pub recovery_settled: u64,
    pub recovery_failed: u64,
    pub checkpoint_results: Vec<BudgetCheckpointResultMetric>,
    pub checkpoint_tail_events: u64,
    pub reconciliation: Vec<BudgetReconciliationMetric>,
}

/// 固定维度的无锁累计器。
pub struct BudgetTelemetry {
    backend: BudgetBackendKind,
    operations: Box<[AtomicU64]>,
    pending_intents: AtomicU64,
    unresolved_intents: AtomicU64,
    started_at: Instant,
    last_heartbeat_elapsed_ms: AtomicU64,
    recovery_scanned: AtomicU64,
    recovery_settled: AtomicU64,
    recovery_failed: AtomicU64,
    checkpoint_results: Box<[AtomicU64]>,
    checkpoint_tail_events: AtomicU64,
    reconciliation: Box<[AtomicU64]>,
}

impl BudgetTelemetry {
    pub fn new(backend: BudgetBackendKind) -> Self {
        Self {
            backend,
            operations: atomic_counters(OPERATION_COUNT * RESULT_COUNT * (ERROR_KIND_COUNT + 1)),
            pending_intents: AtomicU64::new(0),
            unresolved_intents: AtomicU64::new(0),
            started_at: Instant::now(),
            last_heartbeat_elapsed_ms: AtomicU64::new(0),
            recovery_scanned: AtomicU64::new(0),
            recovery_settled: AtomicU64::new(0),
            recovery_failed: AtomicU64::new(0),
            checkpoint_results: atomic_counters(CHECKPOINT_RESULT_COUNT),
            checkpoint_tail_events: AtomicU64::new(0),
            reconciliation: atomic_counters(
                RECONCILIATION_ACTION_COUNT * RECONCILIATION_RESULT_COUNT,
            ),
        }
    }

    pub fn backend(&self) -> BudgetBackendKind {
        self.backend
    }

    pub fn record_operation(
        &self,
        operation: BudgetMetricOperation,
        result: BudgetMetricResult,
        error_kind: Option<BudgetErrorKind>,
    ) {
        let index = operation_index(operation, result, error_kind);
        self.operations[index].fetch_add(1, Ordering::Relaxed);
    }

    /// 写入后端聚合后的全局深度；不得用单个 key 的账户快照覆盖该值。
    pub fn set_accounting_depth(&self, pending: u64, unresolved: u64) {
        self.pending_intents.store(pending, Ordering::Relaxed);
        self.unresolved_intents.store(unresolved, Ordering::Relaxed);
    }

    /// owner_store heartbeat 成功后调用；仅保存相对时间，不依赖单机墙钟。
    pub fn record_owner_heartbeat(&self) {
        self.last_heartbeat_elapsed_ms
            .store(self.elapsed_ms(), Ordering::Relaxed);
    }

    pub fn record_registry_recovery(&self, scanned: u32, settled: u32, failed: u32) {
        self.recovery_scanned
            .fetch_add(u64::from(scanned), Ordering::Relaxed);
        self.recovery_settled
            .fetch_add(u64::from(settled), Ordering::Relaxed);
        self.recovery_failed
            .fetch_add(u64::from(failed), Ordering::Relaxed);
        self.record_operation(
            BudgetMetricOperation::RegistryRecovery,
            if failed == 0 {
                BudgetMetricResult::Success
            } else {
                BudgetMetricResult::Failed
            },
            None,
        );
    }

    pub fn record_checkpoint(&self, result: BudgetCheckpointMetricResult, tail_events: u64) {
        self.checkpoint_results[result as usize].fetch_add(1, Ordering::Relaxed);
        self.checkpoint_tail_events
            .store(tail_events, Ordering::Relaxed);
    }

    pub fn record_reconciliation(
        &self,
        action: BudgetReconciliationMetricAction,
        result: BudgetReconciliationMetricResult,
    ) {
        let index = action as usize * RECONCILIATION_RESULT_COUNT + result as usize;
        self.reconciliation[index].fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(
        &self,
        registry: BudgetRegistryMetricsSnapshot,
        owner_available: bool,
    ) -> BudgetTelemetrySnapshot {
        let registry_states = BudgetRegistryMetricState::ALL
            .into_iter()
            .filter_map(|state| {
                let count = registry.states[state as usize];
                (count > 0).then_some(BudgetRegistryStateMetric { state, count })
            })
            .collect::<Vec<_>>();
        let recovery_depth = registry_states
            .iter()
            .filter(|metric| metric.state.is_recovery())
            .map(|metric| metric.count)
            .sum();
        BudgetTelemetrySnapshot {
            backend: self.backend,
            operations: self.operation_snapshot(),
            pending_intents: self.pending_intents.load(Ordering::Relaxed),
            unresolved_intents: self.unresolved_intents.load(Ordering::Relaxed),
            registry_active_entries: registry.active_entries,
            registry_rejected_entries: registry.rejected_entries,
            registry_field_limit_rejections: registry.field_limit_rejections,
            registry_states,
            owner_available,
            heartbeat_lag_ms: self
                .elapsed_ms()
                .saturating_sub(self.last_heartbeat_elapsed_ms.load(Ordering::Relaxed)),
            recovery_depth,
            recovery_scanned: self.recovery_scanned.load(Ordering::Relaxed),
            recovery_settled: self.recovery_settled.load(Ordering::Relaxed),
            recovery_failed: self.recovery_failed.load(Ordering::Relaxed),
            checkpoint_results: BudgetCheckpointMetricResult::ALL
                .into_iter()
                .filter_map(|result| {
                    let count = self.checkpoint_results[result as usize].load(Ordering::Relaxed);
                    (count > 0).then_some(BudgetCheckpointResultMetric { result, count })
                })
                .collect(),
            checkpoint_tail_events: self.checkpoint_tail_events.load(Ordering::Relaxed),
            reconciliation: BudgetReconciliationMetricAction::ALL
                .into_iter()
                .flat_map(|action| {
                    BudgetReconciliationMetricResult::ALL
                        .into_iter()
                        .filter_map(move |result| {
                            let index =
                                action as usize * RECONCILIATION_RESULT_COUNT + result as usize;
                            let count = self.reconciliation[index].load(Ordering::Relaxed);
                            (count > 0).then_some(BudgetReconciliationMetric {
                                action,
                                result,
                                count,
                            })
                        })
                })
                .collect(),
        }
    }

    fn operation_snapshot(&self) -> Vec<BudgetOperationMetric> {
        BudgetMetricOperation::ALL
            .into_iter()
            .flat_map(|operation| {
                BudgetMetricResult::ALL.into_iter().flat_map(move |result| {
                    error_kinds().into_iter().filter_map(move |error_kind| {
                        let count = self.operations[operation_index(operation, result, error_kind)]
                            .load(Ordering::Relaxed);
                        (count > 0).then_some(BudgetOperationMetric {
                            backend: self.backend,
                            operation,
                            result,
                            error_kind,
                            count,
                        })
                    })
                })
            })
            .collect()
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl std::fmt::Debug for BudgetTelemetry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BudgetTelemetry")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

fn atomic_counters(count: usize) -> Box<[AtomicU64]> {
    std::iter::repeat_with(|| AtomicU64::new(0))
        .take(count)
        .collect()
}

fn operation_index(
    operation: BudgetMetricOperation,
    result: BudgetMetricResult,
    error_kind: Option<BudgetErrorKind>,
) -> usize {
    (operation as usize * RESULT_COUNT + result as usize) * (ERROR_KIND_COUNT + 1)
        + error_kind.map_or(0, |kind| error_kind_index(kind) + 1)
}

fn error_kinds() -> [Option<BudgetErrorKind>; ERROR_KIND_COUNT + 1] {
    [
        None,
        Some(BudgetErrorKind::Exhausted),
        Some(BudgetErrorKind::AccountingUnavailable),
        Some(BudgetErrorKind::AccountingUnresolved),
        Some(BudgetErrorKind::Unsupported),
        Some(BudgetErrorKind::PricingUnavailable),
        Some(BudgetErrorKind::OutcomeUnknown),
        Some(BudgetErrorKind::Corrupt),
        Some(BudgetErrorKind::NumericOverflow),
        Some(BudgetErrorKind::NotFound),
        Some(BudgetErrorKind::Conflict),
        Some(BudgetErrorKind::ReconciliationRequired),
        Some(BudgetErrorKind::IntentActive),
        Some(BudgetErrorKind::AlreadyReconciled),
        Some(BudgetErrorKind::AccountBusy),
    ]
}

const fn error_kind_index(kind: BudgetErrorKind) -> usize {
    match kind {
        BudgetErrorKind::Exhausted => 0,
        BudgetErrorKind::AccountingUnavailable => 1,
        BudgetErrorKind::AccountingUnresolved => 2,
        BudgetErrorKind::Unsupported => 3,
        BudgetErrorKind::PricingUnavailable => 4,
        BudgetErrorKind::OutcomeUnknown => 5,
        BudgetErrorKind::Corrupt => 6,
        BudgetErrorKind::NumericOverflow => 7,
        BudgetErrorKind::NotFound => 8,
        BudgetErrorKind::Conflict => 9,
        BudgetErrorKind::ReconciliationRequired => 10,
        BudgetErrorKind::IntentActive => 11,
        BudgetErrorKind::AlreadyReconciled => 12,
        BudgetErrorKind::AccountBusy => 13,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_exposes_only_bounded_non_zero_dimensions() {
        let telemetry = BudgetTelemetry::new(BudgetBackendKind::Postgres);
        telemetry.record_operation(
            BudgetMetricOperation::Inspect,
            BudgetMetricResult::Failed,
            Some(BudgetErrorKind::AccountingUnavailable),
        );
        telemetry.set_accounting_depth(7, 2);
        telemetry.record_registry_recovery(3, 2, 1);
        telemetry.record_checkpoint(BudgetCheckpointMetricResult::Applied, 91);
        telemetry.record_reconciliation(
            BudgetReconciliationMetricAction::ResolveObservedCost,
            BudgetReconciliationMetricResult::Replayed,
        );
        let mut registry = BudgetRegistryMetricsSnapshot {
            active_entries: 3,
            rejected_entries: 4,
            field_limit_rejections: 1,
            ..BudgetRegistryMetricsSnapshot::default()
        };
        registry.states[BudgetRegistryMetricState::ActivePrepared as usize] = 1;
        registry.states[BudgetRegistryMetricState::NeedsUnresolved as usize] = 2;

        let snapshot = telemetry.snapshot(registry, false);

        assert_eq!(snapshot.backend, BudgetBackendKind::Postgres);
        assert_eq!(snapshot.pending_intents, 7);
        assert_eq!(snapshot.unresolved_intents, 2);
        assert_eq!(snapshot.registry_active_entries, 3);
        assert_eq!(snapshot.registry_rejected_entries, 4);
        assert_eq!(snapshot.registry_field_limit_rejections, 1);
        assert_eq!(snapshot.recovery_depth, 2);
        assert_eq!(snapshot.recovery_scanned, 3);
        assert_eq!(snapshot.recovery_settled, 2);
        assert_eq!(snapshot.recovery_failed, 1);
        assert_eq!(snapshot.checkpoint_tail_events, 91);
        assert!(!snapshot.owner_available);
        assert!(snapshot.operations.contains(&BudgetOperationMetric {
            backend: BudgetBackendKind::Postgres,
            operation: BudgetMetricOperation::Inspect,
            result: BudgetMetricResult::Failed,
            error_kind: Some(BudgetErrorKind::AccountingUnavailable),
            count: 1,
        }));
        assert!(snapshot
            .reconciliation
            .contains(&BudgetReconciliationMetric {
                action: BudgetReconciliationMetricAction::ResolveObservedCost,
                result: BudgetReconciliationMetricResult::Replayed,
                count: 1,
            }));
    }
}

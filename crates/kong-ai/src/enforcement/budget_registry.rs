//! cancellation-safe 的活动预算 intent 注册表。

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use uuid::Uuid;

use crate::budget::{
    BudgetRegistryMetricState, BudgetRegistryMetricsSnapshot, CreateBudgetIntent,
    SettleBudgetIntent,
};

pub const ACTIVE_BUDGET_INTENT_MAX_REQUEST_ID_BYTES: usize = 256;
pub const ACTIVE_BUDGET_INTENT_MAX_OPERATION_ID_BYTES: usize = 256;
pub const ACTIVE_BUDGET_INTENT_MAX_FINGERPRINT_BYTES: usize = 128;
pub const ACTIVE_BUDGET_INTENT_MAX_PRICING_SNAPSHOT_BYTES: usize = 4 * 1024;
pub const ACTIVE_BUDGET_INTENT_MAX_COST_REASONS: usize = 8;
pub const ACTIVE_BUDGET_INTENT_MAX_COST_REASON_BYTES: usize = 256;
/// Arc/Mutex/DashMap 节点、UUID、Decimal 与小对象的保守固定开销。
pub const ACTIVE_BUDGET_INTENT_ESTIMATED_FIXED_BYTES: usize = 1024;
/// 一个 entry 同时保留 create 与 settlement 时的保守内存上限。
pub const ACTIVE_BUDGET_INTENT_ESTIMATED_WORST_CASE_BYTES: usize =
    ACTIVE_BUDGET_INTENT_ESTIMATED_FIXED_BYTES
        + ACTIVE_BUDGET_INTENT_MAX_REQUEST_ID_BYTES * 3
        + ACTIVE_BUDGET_INTENT_MAX_OPERATION_ID_BYTES * 2
        + ACTIVE_BUDGET_INTENT_MAX_FINGERPRINT_BYTES * 4
        + ACTIVE_BUDGET_INTENT_MAX_PRICING_SNAPSHOT_BYTES
        + ACTIVE_BUDGET_INTENT_MAX_COST_REASONS * ACTIVE_BUDGET_INTENT_MAX_COST_REASON_BYTES;

/// registry 容量的稳定、可执行估算。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveBudgetIntentCapacityEstimate {
    pub max_entries: usize,
    pub estimated_bytes_per_entry: usize,
    pub estimated_total_bytes: usize,
}

impl ActiveBudgetIntentCapacityEstimate {
    pub fn structured_line(self) -> String {
        format!(
            "budget_registry_capacity{{entries={},estimated_entry_bytes={},estimated_total_bytes={}}}",
            self.max_entries, self.estimated_bytes_per_entry, self.estimated_total_bytes
        )
    }
}

pub fn estimate_active_budget_intent_capacity(
    max_entries: usize,
) -> Option<ActiveBudgetIntentCapacityEstimate> {
    ACTIVE_BUDGET_INTENT_ESTIMATED_WORST_CASE_BYTES
        .checked_mul(max_entries)
        .map(|estimated_total_bytes| ActiveBudgetIntentCapacityEstimate {
            max_entries,
            estimated_bytes_per_entry: ACTIVE_BUDGET_INTENT_ESTIMATED_WORST_CASE_BYTES,
            estimated_total_bytes,
        })
}

/// 请求 intent 在进程内的恢复状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveBudgetIntentState {
    Preparing,
    ActivePrepared,
    DispatchCommitPending,
    ActiveDispatching,
    RetryWithFact,
    NeedsIntentLookup,
    NeedsSafeZero,
    NeedsUnresolved,
}

#[derive(Debug, Clone)]
pub struct ActiveBudgetIntentSnapshot {
    pub request_id: Arc<str>,
    pub intent_id: Option<Uuid>,
    pub state: ActiveBudgetIntentState,
    pub guard_alive: bool,
    pub create_command: Option<CreateBudgetIntent>,
    pub settlement: Option<SettleBudgetIntent>,
}

struct ActiveBudgetIntentEntry {
    data: Mutex<ActiveBudgetIntentData>,
}

struct ActiveBudgetIntentData {
    intent_id: Option<Uuid>,
    state: ActiveBudgetIntentState,
    guard_alive: bool,
    create_command: Option<CreateBudgetIntent>,
    settlement: Option<SettleBudgetIntent>,
}

/// 固定容量注册表。容量只在终态 ack 后释放，恢复项不会被新请求挤掉。
pub struct ActiveBudgetIntentRegistry {
    max_entries: usize,
    entries: DashMap<Arc<str>, Arc<ActiveBudgetIntentEntry>>,
    active_entries: AtomicUsize,
    rejected_entries: AtomicU64,
    field_limit_rejections: AtomicU64,
}

impl ActiveBudgetIntentRegistry {
    pub fn new(max_entries: usize) -> Result<Arc<Self>, BudgetIntentRegistryError> {
        if max_entries == 0 {
            return Err(BudgetIntentRegistryError::InvalidCapacity);
        }
        Ok(Arc::new(Self {
            max_entries,
            entries: DashMap::new(),
            active_entries: AtomicUsize::new(0),
            rejected_entries: AtomicU64::new(0),
            field_limit_rejections: AtomicU64::new(0),
        }))
    }

    pub fn try_reserve(
        self: &Arc<Self>,
        request_id: impl Into<Arc<str>>,
    ) -> Result<BudgetIntentGuard, BudgetIntentRegistryError> {
        let request_id = request_id.into();
        if request_id.is_empty() {
            self.record_rejection(false);
            return Err(BudgetIntentRegistryError::EmptyRequestId);
        }
        if let Err(error) = validate_text_field(
            BudgetIntentRegistryField::RequestId,
            &request_id,
            ACTIVE_BUDGET_INTENT_MAX_REQUEST_ID_BYTES,
        ) {
            self.record_rejection(true);
            return Err(error);
        }
        self.active_entries
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < self.max_entries).then_some(current + 1)
            })
            .map_err(|_| {
                self.record_rejection(false);
                BudgetIntentRegistryError::Overloaded
            })?;

        let entry = Arc::new(ActiveBudgetIntentEntry {
            data: Mutex::new(ActiveBudgetIntentData {
                intent_id: None,
                state: ActiveBudgetIntentState::Preparing,
                guard_alive: true,
                create_command: None,
                settlement: None,
            }),
        });
        match self.entries.entry(Arc::clone(&request_id)) {
            Entry::Vacant(slot) => {
                slot.insert(entry);
            }
            Entry::Occupied(_) => {
                self.active_entries.fetch_sub(1, Ordering::AcqRel);
                self.record_rejection(false);
                return Err(BudgetIntentRegistryError::DuplicateRequest);
            }
        }

        Ok(BudgetIntentGuard {
            registry: Arc::clone(self),
            request_id,
            completed: AtomicBool::new(false),
        })
    }

    pub fn len(&self) -> usize {
        self.active_entries.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn metrics_snapshot(&self) -> BudgetRegistryMetricsSnapshot {
        let mut snapshot = BudgetRegistryMetricsSnapshot {
            active_entries: self.len(),
            rejected_entries: self.rejected_entries.load(Ordering::Relaxed),
            field_limit_rejections: self.field_limit_rejections.load(Ordering::Relaxed),
            ..BudgetRegistryMetricsSnapshot::default()
        };
        for entry in &self.entries {
            let Ok(data) = entry.value().data.lock() else {
                continue;
            };
            snapshot.states[registry_metric_state(data.state) as usize] += 1;
        }
        snapshot
    }

    pub fn snapshots(&self) -> Vec<ActiveBudgetIntentSnapshot> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let data = entry.value().data.lock().ok()?;
                Some(ActiveBudgetIntentSnapshot {
                    request_id: Arc::clone(entry.key()),
                    intent_id: data.intent_id,
                    state: data.state,
                    guard_alive: data.guard_alive,
                    create_command: data.create_command.clone(),
                    settlement: data.settlement.clone(),
                })
            })
            .collect()
    }

    fn transition(
        &self,
        request_id: &Arc<str>,
        expected: &[ActiveBudgetIntentState],
        next: ActiveBudgetIntentState,
        intent_id: Option<Uuid>,
        settlement: Option<SettleBudgetIntent>,
    ) -> Result<(), BudgetIntentRegistryError> {
        let entry = self
            .entries
            .get(request_id)
            .ok_or(BudgetIntentRegistryError::MissingEntry)?;
        let mut data = entry
            .data
            .lock()
            .map_err(|_| BudgetIntentRegistryError::Poisoned)?;
        if !expected.contains(&data.state) {
            return Err(BudgetIntentRegistryError::InvalidTransition {
                current: data.state,
                next,
            });
        }
        if let Some(intent_id) = intent_id {
            if data.intent_id.is_some_and(|current| current != intent_id) {
                return Err(BudgetIntentRegistryError::IntentMismatch);
            }
            data.intent_id = Some(intent_id);
        }
        if settlement.is_some() {
            data.settlement = settlement;
        }
        data.state = next;
        Ok(())
    }

    fn acknowledge(&self, request_id: &Arc<str>) {
        if self.entries.remove(request_id).is_some() {
            self.active_entries.fetch_sub(1, Ordering::AcqRel);
        }
    }

    pub fn acknowledge_request(&self, request_id: &Arc<str>) {
        self.acknowledge(request_id);
    }

    pub fn mark_recovered_intent(
        &self,
        request_id: &Arc<str>,
        intent_id: Uuid,
        dispatched: bool,
    ) -> Result<(), BudgetIntentRegistryError> {
        self.transition(
            request_id,
            &[
                ActiveBudgetIntentState::NeedsIntentLookup,
                ActiveBudgetIntentState::NeedsSafeZero,
            ],
            if dispatched {
                ActiveBudgetIntentState::NeedsUnresolved
            } else {
                ActiveBudgetIntentState::NeedsSafeZero
            },
            Some(intent_id),
            None,
        )
    }

    pub fn record_recovery_settlement(
        &self,
        request_id: &Arc<str>,
        command: SettleBudgetIntent,
        unresolved: bool,
    ) -> Result<(), BudgetIntentRegistryError> {
        if let Err(error) = validate_settlement_command(request_id, &command) {
            self.record_rejection(error.is_field_limit());
            return Err(error);
        }
        self.transition(
            request_id,
            &[
                ActiveBudgetIntentState::NeedsSafeZero,
                ActiveBudgetIntentState::NeedsUnresolved,
                ActiveBudgetIntentState::RetryWithFact,
            ],
            if unresolved {
                ActiveBudgetIntentState::NeedsUnresolved
            } else {
                ActiveBudgetIntentState::NeedsSafeZero
            },
            Some(command.intent_id),
            Some(command),
        )
    }

    fn record_rejection(&self, field_limit: bool) {
        self.rejected_entries.fetch_add(1, Ordering::Relaxed);
        if field_limit {
            self.field_limit_rejections.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn guard_dropped(&self, request_id: &Arc<str>) {
        let Some(entry) = self.entries.get(request_id) else {
            return;
        };
        let Ok(mut data) = entry.data.lock() else {
            return;
        };
        data.guard_alive = false;
        data.state = match data.state {
            ActiveBudgetIntentState::Preparing => ActiveBudgetIntentState::NeedsIntentLookup,
            ActiveBudgetIntentState::ActivePrepared => ActiveBudgetIntentState::NeedsSafeZero,
            ActiveBudgetIntentState::DispatchCommitPending
            | ActiveBudgetIntentState::ActiveDispatching => {
                ActiveBudgetIntentState::NeedsUnresolved
            }
            ActiveBudgetIntentState::RetryWithFact
            | ActiveBudgetIntentState::NeedsIntentLookup
            | ActiveBudgetIntentState::NeedsSafeZero
            | ActiveBudgetIntentState::NeedsUnresolved => data.state,
        };
    }
}

/// 请求任务持有的 guard。`Drop` 只改变内存状态，不执行 I/O。
pub struct BudgetIntentGuard {
    registry: Arc<ActiveBudgetIntentRegistry>,
    request_id: Arc<str>,
    completed: AtomicBool,
}

impl BudgetIntentGuard {
    pub fn request_id(&self) -> &Arc<str> {
        &self.request_id
    }

    pub fn mark_prepared(&self, intent_id: Uuid) -> Result<(), BudgetIntentRegistryError> {
        self.registry.transition(
            &self.request_id,
            &[ActiveBudgetIntentState::Preparing],
            ActiveBudgetIntentState::ActivePrepared,
            Some(intent_id),
            None,
        )
    }

    pub fn record_create_command(
        &self,
        command: CreateBudgetIntent,
    ) -> Result<(), BudgetIntentRegistryError> {
        if let Err(error) = validate_create_command(&self.request_id, &command) {
            self.registry.record_rejection(error.is_field_limit());
            self.completed.store(true, Ordering::Release);
            self.registry.acknowledge(&self.request_id);
            return Err(error);
        }
        let entry = self
            .registry
            .entries
            .get(&self.request_id)
            .ok_or(BudgetIntentRegistryError::MissingEntry)?;
        let mut data = entry
            .data
            .lock()
            .map_err(|_| BudgetIntentRegistryError::Poisoned)?;
        if data.state != ActiveBudgetIntentState::Preparing {
            return Err(BudgetIntentRegistryError::InvalidTransition {
                current: data.state,
                next: ActiveBudgetIntentState::Preparing,
            });
        }
        if data.create_command.is_some() {
            return Err(BudgetIntentRegistryError::DuplicateRequest);
        }
        data.create_command = Some(command);
        Ok(())
    }

    pub fn mark_dispatch_commit_pending(&self) -> Result<(), BudgetIntentRegistryError> {
        self.registry.transition(
            &self.request_id,
            &[ActiveBudgetIntentState::ActivePrepared],
            ActiveBudgetIntentState::DispatchCommitPending,
            None,
            None,
        )
    }

    pub fn mark_dispatch_rolled_back(&self) -> Result<(), BudgetIntentRegistryError> {
        self.registry.transition(
            &self.request_id,
            &[ActiveBudgetIntentState::DispatchCommitPending],
            ActiveBudgetIntentState::ActivePrepared,
            None,
            None,
        )
    }

    pub fn mark_dispatching(&self) -> Result<(), BudgetIntentRegistryError> {
        self.registry.transition(
            &self.request_id,
            &[ActiveBudgetIntentState::DispatchCommitPending],
            ActiveBudgetIntentState::ActiveDispatching,
            None,
            None,
        )
    }

    pub fn record_settlement(
        &self,
        command: SettleBudgetIntent,
    ) -> Result<(), BudgetIntentRegistryError> {
        if let Err(error) = validate_settlement_command(&self.request_id, &command) {
            self.registry.record_rejection(error.is_field_limit());
            return Err(error);
        }
        self.registry.transition(
            &self.request_id,
            &[
                ActiveBudgetIntentState::ActiveDispatching,
                ActiveBudgetIntentState::RetryWithFact,
            ],
            ActiveBudgetIntentState::RetryWithFact,
            Some(command.intent_id),
            Some(command),
        )
    }

    pub fn mark_needs_safe_zero(&self) -> Result<(), BudgetIntentRegistryError> {
        self.registry.transition(
            &self.request_id,
            &[
                ActiveBudgetIntentState::ActivePrepared,
                ActiveBudgetIntentState::DispatchCommitPending,
                ActiveBudgetIntentState::ActiveDispatching,
                ActiveBudgetIntentState::NeedsSafeZero,
            ],
            ActiveBudgetIntentState::NeedsSafeZero,
            None,
            None,
        )
    }

    pub fn record_safe_zero_settlement(
        &self,
        command: SettleBudgetIntent,
    ) -> Result<(), BudgetIntentRegistryError> {
        if let Err(error) = validate_settlement_command(&self.request_id, &command) {
            self.registry.record_rejection(error.is_field_limit());
            return Err(error);
        }
        self.registry.transition(
            &self.request_id,
            &[
                ActiveBudgetIntentState::ActivePrepared,
                ActiveBudgetIntentState::DispatchCommitPending,
                ActiveBudgetIntentState::ActiveDispatching,
                ActiveBudgetIntentState::NeedsSafeZero,
            ],
            ActiveBudgetIntentState::NeedsSafeZero,
            Some(command.intent_id),
            Some(command),
        )
    }

    pub fn mark_needs_unresolved(&self) -> Result<(), BudgetIntentRegistryError> {
        self.registry.transition(
            &self.request_id,
            &[
                ActiveBudgetIntentState::DispatchCommitPending,
                ActiveBudgetIntentState::ActiveDispatching,
                ActiveBudgetIntentState::RetryWithFact,
            ],
            ActiveBudgetIntentState::NeedsUnresolved,
            None,
            None,
        )
    }

    pub fn acknowledge(&self) {
        if !self.completed.swap(true, Ordering::AcqRel) {
            self.registry.acknowledge(&self.request_id);
        }
    }
}

impl Drop for BudgetIntentGuard {
    fn drop(&mut self) {
        if !self.completed.load(Ordering::Acquire) {
            self.registry.guard_dropped(&self.request_id);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetIntentRegistryField {
    RequestId,
    OperationId,
    CommandFingerprint,
    PricingFingerprint,
    PricingSnapshot,
    CostReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetIntentRegistryError {
    InvalidCapacity,
    EmptyRequestId,
    RequestMismatch,
    FieldTooLarge {
        field: BudgetIntentRegistryField,
        actual_bytes: usize,
        max_bytes: usize,
    },
    TooManyCostReasons {
        actual: usize,
        max: usize,
    },
    InvalidPricingSnapshot,
    Overloaded,
    DuplicateRequest,
    MissingEntry,
    Poisoned,
    IntentMismatch,
    InvalidTransition {
        current: ActiveBudgetIntentState,
        next: ActiveBudgetIntentState,
    },
}

impl BudgetIntentRegistryError {
    fn is_field_limit(&self) -> bool {
        matches!(
            self,
            Self::FieldTooLarge { .. } | Self::TooManyCostReasons { .. }
        )
    }
}

impl fmt::Display for BudgetIntentRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for BudgetIntentRegistryError {}

fn registry_metric_state(state: ActiveBudgetIntentState) -> BudgetRegistryMetricState {
    match state {
        ActiveBudgetIntentState::Preparing => BudgetRegistryMetricState::Preparing,
        ActiveBudgetIntentState::ActivePrepared => BudgetRegistryMetricState::ActivePrepared,
        ActiveBudgetIntentState::DispatchCommitPending => {
            BudgetRegistryMetricState::DispatchCommitPending
        }
        ActiveBudgetIntentState::ActiveDispatching => BudgetRegistryMetricState::ActiveDispatching,
        ActiveBudgetIntentState::RetryWithFact => BudgetRegistryMetricState::RetryWithFact,
        ActiveBudgetIntentState::NeedsIntentLookup => BudgetRegistryMetricState::NeedsIntentLookup,
        ActiveBudgetIntentState::NeedsSafeZero => BudgetRegistryMetricState::NeedsSafeZero,
        ActiveBudgetIntentState::NeedsUnresolved => BudgetRegistryMetricState::NeedsUnresolved,
    }
}

fn validate_create_command(
    request_id: &Arc<str>,
    command: &CreateBudgetIntent,
) -> Result<(), BudgetIntentRegistryError> {
    validate_request_identity(request_id, &command.request_id)?;
    validate_text_field(
        BudgetIntentRegistryField::OperationId,
        &command.operation_id,
        ACTIVE_BUDGET_INTENT_MAX_OPERATION_ID_BYTES,
    )?;
    validate_text_field(
        BudgetIntentRegistryField::CommandFingerprint,
        &command.command_fingerprint,
        ACTIVE_BUDGET_INTENT_MAX_FINGERPRINT_BYTES,
    )?;
    validate_text_field(
        BudgetIntentRegistryField::PricingFingerprint,
        &command.pricing_fingerprint,
        ACTIVE_BUDGET_INTENT_MAX_FINGERPRINT_BYTES,
    )?;
    let pricing_bytes = serde_json::to_vec(&command.pricing_snapshot)
        .map_err(|_| BudgetIntentRegistryError::InvalidPricingSnapshot)?
        .len();
    validate_field_size(
        BudgetIntentRegistryField::PricingSnapshot,
        pricing_bytes,
        ACTIVE_BUDGET_INTENT_MAX_PRICING_SNAPSHOT_BYTES,
    )
}

fn validate_settlement_command(
    request_id: &Arc<str>,
    command: &SettleBudgetIntent,
) -> Result<(), BudgetIntentRegistryError> {
    validate_request_identity(request_id, &command.request_id)?;
    validate_text_field(
        BudgetIntentRegistryField::OperationId,
        &command.operation_id,
        ACTIVE_BUDGET_INTENT_MAX_OPERATION_ID_BYTES,
    )?;
    validate_text_field(
        BudgetIntentRegistryField::CommandFingerprint,
        &command.command_fingerprint,
        ACTIVE_BUDGET_INTENT_MAX_FINGERPRINT_BYTES,
    )?;
    validate_text_field(
        BudgetIntentRegistryField::PricingFingerprint,
        &command.pricing_fingerprint,
        ACTIVE_BUDGET_INTENT_MAX_FINGERPRINT_BYTES,
    )?;
    if command.cost.reasons.len() > ACTIVE_BUDGET_INTENT_MAX_COST_REASONS {
        return Err(BudgetIntentRegistryError::TooManyCostReasons {
            actual: command.cost.reasons.len(),
            max: ACTIVE_BUDGET_INTENT_MAX_COST_REASONS,
        });
    }
    for reason in &command.cost.reasons {
        validate_text_field(
            BudgetIntentRegistryField::CostReason,
            reason,
            ACTIVE_BUDGET_INTENT_MAX_COST_REASON_BYTES,
        )?;
    }
    Ok(())
}

fn validate_request_identity(
    request_id: &Arc<str>,
    command_request_id: &Arc<str>,
) -> Result<(), BudgetIntentRegistryError> {
    validate_text_field(
        BudgetIntentRegistryField::RequestId,
        command_request_id,
        ACTIVE_BUDGET_INTENT_MAX_REQUEST_ID_BYTES,
    )?;
    if request_id != command_request_id {
        return Err(BudgetIntentRegistryError::RequestMismatch);
    }
    Ok(())
}

fn validate_text_field(
    field: BudgetIntentRegistryField,
    value: &str,
    max_bytes: usize,
) -> Result<(), BudgetIntentRegistryError> {
    validate_field_size(field, value.len(), max_bytes)
}

fn validate_field_size(
    field: BudgetIntentRegistryField,
    actual_bytes: usize,
    max_bytes: usize,
) -> Result<(), BudgetIntentRegistryError> {
    if actual_bytes > max_bytes {
        return Err(BudgetIntentRegistryError::FieldTooLarge {
            field,
            actual_bytes,
            max_bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetCostOutcome, BudgetPricingSnapshot, BUDGET_SCHEMA_VERSION};
    use crate::usage::CostStatus;
    use std::time::Duration;

    fn create_command(request_id: &str) -> CreateBudgetIntent {
        CreateBudgetIntent {
            intent_id: Uuid::new_v4(),
            virtual_key_id: Uuid::new_v4(),
            request_id: Arc::from(request_id),
            operation_id: Arc::from("budget-create:v1:test"),
            command_fingerprint: Arc::from("a".repeat(64)),
            pricing_fingerprint: Arc::from("b".repeat(64)),
            pricing_snapshot: BudgetPricingSnapshot {
                schema_version: BUDGET_SCHEMA_VERSION,
                provider_type: "openai".to_string(),
                model: "gpt-test".to_string(),
                input: None,
                output: None,
                max_prompt_tokens: None,
            },
            node_id: Uuid::new_v4(),
            owner_session_id: Uuid::new_v4(),
            stale_after: Duration::from_secs(30),
        }
    }

    fn settlement_command(request_id: &str, intent_id: Uuid) -> SettleBudgetIntent {
        SettleBudgetIntent {
            intent_id,
            virtual_key_id: Uuid::new_v4(),
            request_id: Arc::from(request_id),
            operation_id: Arc::from("budget-settle:v1:test"),
            command_fingerprint: Arc::from("c".repeat(64)),
            pricing_fingerprint: Arc::from("d".repeat(64)),
            usage_fact_id: None,
            cost: BudgetCostOutcome {
                status: CostStatus::NotIncurred,
                amount_usd: Some(rust_decimal::Decimal::ZERO),
                reasons: vec!["upstream_not_attempted".to_string()],
            },
        }
    }

    #[test]
    fn dropped_preparing_guard_is_retained_for_lookup_and_holds_capacity() {
        let registry = ActiveBudgetIntentRegistry::new(1).unwrap();
        let guard = registry.try_reserve("request-a").unwrap();

        drop(guard);

        let snapshots = registry.snapshots();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            snapshots[0].state,
            ActiveBudgetIntentState::NeedsIntentLookup
        );
        assert!(!snapshots[0].guard_alive);
        assert!(matches!(
            registry.try_reserve("request-b"),
            Err(BudgetIntentRegistryError::Overloaded)
        ));
    }

    #[test]
    fn cancellation_boundary_distinguishes_prepared_and_dispatch_pending() {
        let prepared_registry = ActiveBudgetIntentRegistry::new(1).unwrap();
        let prepared = prepared_registry.try_reserve("prepared").unwrap();
        prepared.mark_prepared(Uuid::new_v4()).unwrap();
        drop(prepared);
        assert_eq!(
            prepared_registry.snapshots()[0].state,
            ActiveBudgetIntentState::NeedsSafeZero
        );

        let dispatch_registry = ActiveBudgetIntentRegistry::new(1).unwrap();
        let dispatch = dispatch_registry.try_reserve("dispatch").unwrap();
        dispatch.mark_prepared(Uuid::new_v4()).unwrap();
        dispatch.mark_dispatch_commit_pending().unwrap();
        drop(dispatch);
        assert_eq!(
            dispatch_registry.snapshots()[0].state,
            ActiveBudgetIntentState::NeedsUnresolved
        );
    }

    #[test]
    fn acknowledge_releases_capacity() {
        let registry = ActiveBudgetIntentRegistry::new(1).unwrap();
        let guard = registry.try_reserve("request-a").unwrap();

        guard.acknowledge();
        drop(guard);

        assert!(registry.is_empty());
        assert!(registry.try_reserve("request-b").is_ok());
    }

    #[test]
    fn registry_rejects_request_and_command_fields_over_hard_limits() {
        let registry = ActiveBudgetIntentRegistry::new(4).unwrap();
        let oversized_request = "r".repeat(ACTIVE_BUDGET_INTENT_MAX_REQUEST_ID_BYTES + 1);
        assert!(matches!(
            registry.try_reserve(oversized_request),
            Err(BudgetIntentRegistryError::FieldTooLarge {
                field: BudgetIntentRegistryField::RequestId,
                ..
            })
        ));

        let guard = registry.try_reserve("bounded-request").unwrap();
        let mut oversized_operation = create_command("bounded-request");
        oversized_operation.operation_id =
            Arc::from("o".repeat(ACTIVE_BUDGET_INTENT_MAX_OPERATION_ID_BYTES + 1));
        assert!(matches!(
            guard.record_create_command(oversized_operation),
            Err(BudgetIntentRegistryError::FieldTooLarge {
                field: BudgetIntentRegistryField::OperationId,
                ..
            })
        ));

        assert_eq!(registry.len(), 0);
        let fingerprint_guard = registry.try_reserve("fingerprint-request").unwrap();
        let mut oversized_fingerprint = create_command("fingerprint-request");
        oversized_fingerprint.command_fingerprint =
            Arc::from("f".repeat(ACTIVE_BUDGET_INTENT_MAX_FINGERPRINT_BYTES + 1));
        assert!(matches!(
            fingerprint_guard.record_create_command(oversized_fingerprint),
            Err(BudgetIntentRegistryError::FieldTooLarge {
                field: BudgetIntentRegistryField::CommandFingerprint,
                ..
            })
        ));
        assert_eq!(registry.len(), 0);

        let metrics = registry.metrics_snapshot();
        assert_eq!(metrics.rejected_entries, 3);
        assert_eq!(metrics.field_limit_rejections, 3);
    }

    #[test]
    fn registry_rejects_pricing_snapshot_over_four_kib() {
        let registry = ActiveBudgetIntentRegistry::new(1).unwrap();
        let guard = registry.try_reserve("pricing-request").unwrap();
        let mut command = create_command("pricing-request");
        command.pricing_snapshot.model =
            "m".repeat(ACTIVE_BUDGET_INTENT_MAX_PRICING_SNAPSHOT_BYTES);

        assert!(matches!(
            guard.record_create_command(command),
            Err(BudgetIntentRegistryError::FieldTooLarge {
                field: BudgetIntentRegistryField::PricingSnapshot,
                ..
            })
        ));
        assert_eq!(registry.metrics_snapshot().field_limit_rejections, 1);
    }

    #[test]
    fn registry_rejects_unbounded_settlement_reasons() {
        let registry = ActiveBudgetIntentRegistry::new(1).unwrap();
        let guard = registry.try_reserve("settle-request").unwrap();
        let create = create_command("settle-request");
        let intent_id = create.intent_id;
        guard.record_create_command(create).unwrap();
        guard.mark_prepared(intent_id).unwrap();
        guard.mark_dispatch_commit_pending().unwrap();
        guard.mark_dispatching().unwrap();

        let mut command = settlement_command("settle-request", intent_id);
        command.cost.reasons =
            vec!["reason".to_string(); ACTIVE_BUDGET_INTENT_MAX_COST_REASONS + 1];
        assert!(matches!(
            guard.record_settlement(command),
            Err(BudgetIntentRegistryError::TooManyCostReasons { .. })
        ));

        let mut command = settlement_command("settle-request", intent_id);
        command.cost.reasons = vec!["x".repeat(ACTIVE_BUDGET_INTENT_MAX_COST_REASON_BYTES + 1)];
        assert!(matches!(
            guard.record_settlement(command),
            Err(BudgetIntentRegistryError::FieldTooLarge {
                field: BudgetIntentRegistryField::CostReason,
                ..
            })
        ));
        let metrics = registry.metrics_snapshot();
        assert_eq!(metrics.rejected_entries, 2);
        assert_eq!(metrics.field_limit_rejections, 2);
    }

    #[test]
    fn registry_metrics_report_bounded_states_and_recovery_depth_inputs() {
        let registry = ActiveBudgetIntentRegistry::new(2).unwrap();
        let live = registry.try_reserve("live").unwrap();
        let dropped = registry.try_reserve("dropped").unwrap();
        drop(dropped);

        let metrics = registry.metrics_snapshot();
        assert_eq!(metrics.active_entries, 2);
        assert_eq!(
            metrics.states[BudgetRegistryMetricState::Preparing as usize],
            1
        );
        assert_eq!(
            metrics.states[BudgetRegistryMetricState::NeedsIntentLookup as usize],
            1
        );

        live.acknowledge();
    }

    #[test]
    fn capacity_formula_is_stable_and_overflow_safe() {
        assert_eq!(ACTIVE_BUDGET_INTENT_ESTIMATED_WORST_CASE_BYTES, 8_960);
        let estimate = estimate_active_budget_intent_capacity(100_000).unwrap();
        assert_eq!(estimate.estimated_total_bytes, 896_000_000);
        assert_eq!(
            estimate.structured_line(),
            "budget_registry_capacity{entries=100000,estimated_entry_bytes=8960,estimated_total_bytes=896000000}"
        );
        assert!(estimate_active_budget_intent_capacity(usize::MAX).is_none());
        println!("{}", estimate.structured_line());
    }
}

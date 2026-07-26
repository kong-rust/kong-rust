//! 配额编排的 Memory 与确定性 outcome-unknown fake 契约测试。

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kong_ai::ratelimit::{
    admit_with_recovery, settle_with_recovery, AdmissionDecision, AdmitCommand, InspectQuery,
    InspectResult, ManualRateLimitClock, MemoryRateLimitStore, QuotaCharge, QuotaLimits,
    RateLimitBackendDescriptor, RateLimitKey, RateLimitStore, RateLimitStoreError,
    RateLimitStoreErrorKind, RateLimitStoreStatsSnapshot, RateLimitSubject, SettleCommand,
    SettlementResult, WindowSpec,
};

#[derive(Clone, Copy)]
enum FaultScript {
    Pass,
    ApplyThenUnknown,
    DropThenApplyUnknown,
    AlwaysUnknown,
}

#[derive(Clone, Copy)]
enum FaultAction {
    Pass,
    DropUnknown,
    ApplyUnknown,
}

impl FaultScript {
    fn action(self, attempt: usize) -> FaultAction {
        match self {
            Self::Pass => FaultAction::Pass,
            Self::ApplyThenUnknown if attempt == 0 => FaultAction::ApplyUnknown,
            Self::ApplyThenUnknown => FaultAction::Pass,
            Self::DropThenApplyUnknown if attempt == 0 => FaultAction::DropUnknown,
            Self::DropThenApplyUnknown if attempt == 1 => FaultAction::ApplyUnknown,
            Self::DropThenApplyUnknown => FaultAction::Pass,
            Self::AlwaysUnknown => FaultAction::DropUnknown,
        }
    }
}

struct DeterministicOutcomeStore {
    inner: Arc<MemoryRateLimitStore>,
    admit_script: FaultScript,
    settle_script: FaultScript,
    admit_attempts: AtomicUsize,
    settle_attempts: AtomicUsize,
    seen_admits: Mutex<Vec<AdmitCommand>>,
    seen_settlements: Mutex<Vec<SettleCommand>>,
    events: Mutex<Vec<&'static str>>,
}

impl DeterministicOutcomeStore {
    fn new(admit_script: FaultScript, settle_script: FaultScript) -> Self {
        Self {
            inner: Arc::new(MemoryRateLimitStore::with_defaults(Arc::new(
                ManualRateLimitClock::default(),
            ))),
            admit_script,
            settle_script,
            admit_attempts: AtomicUsize::new(0),
            settle_attempts: AtomicUsize::new(0),
            seen_admits: Mutex::new(Vec::new()),
            seen_settlements: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
        }
    }

    fn outcome_unknown() -> RateLimitStoreError {
        RateLimitStoreError::new(
            RateLimitStoreErrorKind::OutcomeUnknown,
            "deterministic ACK loss",
        )
    }
}

#[async_trait]
impl RateLimitStore for DeterministicOutcomeStore {
    fn descriptor(&self) -> RateLimitBackendDescriptor {
        self.inner.descriptor()
    }

    async fn admit(&self, command: AdmitCommand) -> Result<AdmissionDecision, RateLimitStoreError> {
        self.events.lock().unwrap().push("admit");
        self.seen_admits.lock().unwrap().push(command.clone());
        let attempt = self.admit_attempts.fetch_add(1, Ordering::SeqCst);
        match self.admit_script.action(attempt) {
            FaultAction::Pass => self.inner.admit(command).await,
            FaultAction::DropUnknown => Err(Self::outcome_unknown()),
            FaultAction::ApplyUnknown => {
                self.inner.admit(command).await?;
                Err(Self::outcome_unknown())
            }
        }
    }

    async fn settle(
        &self,
        command: SettleCommand,
    ) -> Result<SettlementResult, RateLimitStoreError> {
        self.events.lock().unwrap().push("settle");
        self.seen_settlements.lock().unwrap().push(command.clone());
        let attempt = self.settle_attempts.fetch_add(1, Ordering::SeqCst);
        match self.settle_script.action(attempt) {
            FaultAction::Pass => self.inner.settle(command).await,
            FaultAction::DropUnknown => Err(Self::outcome_unknown()),
            FaultAction::ApplyUnknown => {
                self.inner.settle(command).await?;
                Err(Self::outcome_unknown())
            }
        }
    }

    async fn inspect(&self, query: InspectQuery) -> Result<InspectResult, RateLimitStoreError> {
        self.events.lock().unwrap().push(match &query {
            InspectQuery::Current { .. } => "inspect_current",
            InspectQuery::Admission { .. } => "inspect_admission",
            InspectQuery::Settlement { .. } => "inspect_settlement",
        });
        self.inner.inspect(query).await
    }

    fn stats(&self) -> RateLimitStoreStatsSnapshot {
        self.inner.stats()
    }
}

fn limits() -> QuotaLimits {
    QuotaLimits {
        requests: NonZeroU64::new(10),
        tokens: NonZeroU64::new(1_000),
    }
}

fn admit_command(request_id: &str) -> AdmitCommand {
    AdmitCommand {
        request_id: Arc::from(request_id),
        key: RateLimitKey::new(
            "orchestration-test",
            RateLimitSubject::VirtualKey(uuid::Uuid::from_u128(0xA1_003)),
        ),
        window: WindowSpec::fixed(Duration::from_secs(60)),
        limits: limits(),
        reserve: QuotaCharge {
            requests: 1,
            tokens: 100,
        },
    }
}

fn reservation(decision: AdmissionDecision) -> kong_ai::ratelimit::ReservationToken {
    match decision {
        AdmissionDecision::Allowed { reservation, .. } => reservation,
        other => panic!("测试准入必须成功，实际为 {other:?}"),
    }
}

#[tokio::test]
async fn memory_store_satisfies_orchestration_contract() {
    let store = MemoryRateLimitStore::with_defaults(Arc::new(ManualRateLimitClock::default()));
    let command = admit_command("memory-contract");
    let token = reservation(admit_with_recovery(&store, command.clone()).await.unwrap());
    let settlement = SettleCommand {
        operation_id: Arc::from("quota-settle:v1:memory-contract"),
        reservation: token,
        final_charge: QuotaCharge {
            requests: 1,
            tokens: 40,
        },
    };

    settle_with_recovery(&store, settlement).await.unwrap();
    let InspectResult::Current(snapshot) = store
        .inspect(InspectQuery::Current {
            key: command.key,
            window: command.window,
            limits: command.limits,
        })
        .await
        .unwrap()
    else {
        panic!("必须返回当前快照");
    };
    assert_eq!(snapshot.requests.unwrap().used, 1);
    assert_eq!(snapshot.tokens.unwrap().used, 40);
}

#[tokio::test]
async fn admission_applied_before_ack_loss_is_resolved_by_inspect_without_replay() {
    let store = DeterministicOutcomeStore::new(FaultScript::ApplyThenUnknown, FaultScript::Pass);
    let command = admit_command("admit-applied-unknown");

    let decision = admit_with_recovery(&store, command.clone()).await.unwrap();

    assert!(matches!(decision, AdmissionDecision::Allowed { .. }));
    assert_eq!(store.admit_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.events.lock().unwrap().as_slice(),
        ["admit", "inspect_admission"]
    );
    let InspectResult::Current(snapshot) = store
        .inspect(InspectQuery::Current {
            key: command.key,
            window: command.window,
            limits: command.limits,
        })
        .await
        .unwrap()
    else {
        panic!("必须返回当前快照");
    };
    assert_eq!(snapshot.requests.unwrap().used, 1);
    assert_eq!(snapshot.tokens.unwrap().used, 100);
}

#[tokio::test]
async fn admission_unknown_is_queried_replayed_once_and_never_double_charged() {
    let store =
        DeterministicOutcomeStore::new(FaultScript::DropThenApplyUnknown, FaultScript::Pass);
    let command = admit_command("admit-unknown");

    let decision = admit_with_recovery(&store, command.clone()).await.unwrap();

    assert!(matches!(decision, AdmissionDecision::Allowed { .. }));
    assert_eq!(store.admit_attempts.load(Ordering::SeqCst), 2);
    let seen = store.seen_admits.lock().unwrap();
    assert_eq!(seen.as_slice(), [command.clone(), command.clone()]);
    drop(seen);
    assert_eq!(
        store.events.lock().unwrap().as_slice(),
        ["admit", "inspect_admission", "admit", "inspect_admission"]
    );
    let InspectResult::Current(snapshot) = store
        .inspect(InspectQuery::Current {
            key: command.key,
            window: command.window,
            limits: command.limits,
        })
        .await
        .unwrap()
    else {
        panic!("必须返回当前快照");
    };
    assert_eq!(snapshot.requests.unwrap().used, 1);
    assert_eq!(snapshot.tokens.unwrap().used, 100);
}

#[tokio::test]
async fn settlement_unknown_replays_identical_command_without_double_adjustment() {
    let store =
        DeterministicOutcomeStore::new(FaultScript::Pass, FaultScript::DropThenApplyUnknown);
    let admit = admit_command("settle-unknown");
    let token = reservation(admit_with_recovery(&store, admit.clone()).await.unwrap());
    store.events.lock().unwrap().clear();
    let command = SettleCommand {
        operation_id: Arc::from("quota-settle:v1:settle-unknown"),
        reservation: token,
        final_charge: QuotaCharge {
            requests: 1,
            tokens: 25,
        },
    };

    settle_with_recovery(&store, command.clone()).await.unwrap();

    assert_eq!(store.settle_attempts.load(Ordering::SeqCst), 2);
    let seen = store.seen_settlements.lock().unwrap();
    assert_eq!(seen.as_slice(), [command.clone(), command]);
    drop(seen);
    assert_eq!(
        store.events.lock().unwrap().as_slice(),
        [
            "settle",
            "inspect_settlement",
            "settle",
            "inspect_settlement"
        ]
    );
    let InspectResult::Current(snapshot) = store
        .inspect(InspectQuery::Current {
            key: admit.key,
            window: admit.window,
            limits: admit.limits,
        })
        .await
        .unwrap()
    else {
        panic!("必须返回当前快照");
    };
    assert_eq!(snapshot.requests.unwrap().used, 1);
    assert_eq!(snapshot.tokens.unwrap().used, 25);
}

#[tokio::test]
async fn outcome_remaining_unknown_stops_after_one_replay() {
    let store = DeterministicOutcomeStore::new(FaultScript::AlwaysUnknown, FaultScript::Pass);
    let command = admit_command("still-unknown");

    let error = admit_with_recovery(&store, command.clone())
        .await
        .unwrap_err();

    assert_eq!(error.kind(), RateLimitStoreErrorKind::OutcomeUnknown);
    assert_eq!(store.admit_attempts.load(Ordering::SeqCst), 2);
    assert_eq!(
        store.seen_admits.lock().unwrap().as_slice(),
        [command.clone(), command]
    );
    assert_eq!(
        store.events.lock().unwrap().as_slice(),
        ["admit", "inspect_admission", "admit", "inspect_admission"]
    );
}

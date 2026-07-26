//! REQ-AI-003 RateLimitStore 契约与 Memory 后端聚焦测试。

use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use kong_ai::ratelimit::{
    AdmissionDecision, AdmitCommand, ExceededDimension, InspectQuery, InspectResult,
    ManualRateLimitClock, MemoryRateLimitConfig, MemoryRateLimitStore, QuotaCharge, QuotaLimits,
    RateLimitKey, RateLimitStore, RateLimitStoreErrorKind, RateLimitSubject, ReservationToken,
    SettleCommand, SettlementDisposition, WindowSpec,
};

fn quota(requests: Option<u64>, tokens: Option<u64>) -> QuotaLimits {
    QuotaLimits {
        requests: requests.map(|value| NonZeroU64::new(value).unwrap()),
        tokens: tokens.map(|value| NonZeroU64::new(value).unwrap()),
    }
}

fn key(name: u128) -> RateLimitKey {
    RateLimitKey::new(
        "contract-suite",
        RateLimitSubject::VirtualKey(uuid::Uuid::from_u128(name)),
    )
}

fn command(
    request_id: &str,
    key: RateLimitKey,
    duration: Duration,
    limits: QuotaLimits,
    requests: u64,
    tokens: u64,
) -> AdmitCommand {
    AdmitCommand {
        request_id: Arc::from(request_id),
        key,
        window: WindowSpec::fixed(duration),
        limits,
        reserve: QuotaCharge { requests, tokens },
    }
}

fn default_store() -> (Arc<ManualRateLimitClock>, MemoryRateLimitStore) {
    let clock = Arc::new(ManualRateLimitClock::new(
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000),
    ));
    let store = MemoryRateLimitStore::new(MemoryRateLimitConfig::default(), clock.clone()).unwrap();
    (clock, store)
}

fn compact_config() -> MemoryRateLimitConfig {
    MemoryRateLimitConfig {
        max_buckets: 32,
        max_idempotency_records: 128,
        max_records_per_bucket: 32,
        max_live_reservations: 64,
        recovery_record_headroom: 8,
        max_request_lifetime: Duration::from_secs(5),
        settlement_retry_grace: Duration::from_secs(5),
        cleanup_interval: Duration::from_secs(1),
        cleanup_scan_batch: 64,
    }
}

fn allowed(
    decision: AdmissionDecision,
) -> (ReservationToken, kong_ai::ratelimit::RateLimitSnapshot) {
    match decision {
        AdmissionDecision::Allowed {
            reservation,
            snapshot,
            replayed: false,
        } => (reservation, snapshot),
        other => panic!("expected a new allowed decision, got {other:?}"),
    }
}

#[tokio::test]
async fn prospective_inspect_does_not_create_a_bucket_or_window() {
    let (_clock, store) = default_store();
    let result = store
        .inspect(InspectQuery::Current {
            key: key(1),
            window: WindowSpec::fixed(Duration::from_secs(60)),
            limits: quota(Some(10), Some(1_000)),
        })
        .await
        .unwrap();

    let InspectResult::Current(snapshot) = result else {
        panic!("expected current snapshot");
    };
    assert!(snapshot.window.identity.is_none());
    assert_eq!(snapshot.window.reset_after, Duration::from_secs(60));
    assert_eq!(snapshot.requests.unwrap().used, 0);
    assert_eq!(snapshot.tokens.unwrap().used, 0);
    assert_eq!(store.stats().buckets, 0);
}

#[tokio::test]
async fn rpm_and_tpm_are_admitted_or_rejected_together() {
    let (_clock, store) = default_store();
    let bucket = key(2);
    let (_, first) = allowed(
        store
            .admit(command(
                "request-1",
                bucket.clone(),
                Duration::from_secs(60),
                quota(Some(1), Some(10)),
                1,
                8,
            ))
            .await
            .unwrap(),
    );
    assert_eq!(first.requests.unwrap().used, 1);
    assert_eq!(first.tokens.unwrap().used, 8);

    let second = store
        .admit(command(
            "request-2",
            bucket,
            Duration::from_secs(60),
            quota(Some(1), Some(10)),
            1,
            3,
        ))
        .await
        .unwrap();
    let AdmissionDecision::Rejected {
        reason,
        snapshot,
        replayed: false,
    } = second
    else {
        panic!("expected rejection");
    };
    assert_eq!(reason, ExceededDimension::RequestsAndTokens);
    assert_eq!(snapshot.requests.unwrap().used, 1);
    assert_eq!(snapshot.tokens.unwrap().used, 8);
}

#[tokio::test]
async fn admit_replay_is_stable_and_conflicting_payload_is_corrupt() {
    let (_clock, store) = default_store();
    let original = command(
        "stable-request",
        key(3),
        Duration::from_secs(60),
        quota(Some(2), Some(20)),
        1,
        5,
    );
    let (token, original_snapshot) = allowed(store.admit(original.clone()).await.unwrap());

    let replay = store.admit(original.clone()).await.unwrap();
    let AdmissionDecision::Allowed {
        reservation,
        snapshot,
        replayed: true,
    } = replay
    else {
        panic!("expected allowed replay");
    };
    assert_eq!(reservation, token);
    assert_eq!(snapshot, original_snapshot);

    let inspected = store
        .inspect(InspectQuery::Admission {
            key: original.key.clone(),
            request_id: original.request_id.clone(),
        })
        .await
        .unwrap();
    assert!(matches!(
        inspected,
        InspectResult::Admission(AdmissionDecision::Allowed { replayed: true, .. })
    ));

    let mut conflict = original;
    conflict.reserve.tokens = 6;
    let error = store.admit(conflict).await.unwrap_err();
    assert_eq!(error.kind(), RateLimitStoreErrorKind::Corrupt);
    assert_eq!(store.stats().idempotency_records, 1);
}

#[tokio::test]
async fn rejected_admission_is_also_idempotent() {
    let (_clock, store) = default_store();
    let reject = command(
        "rejected-request",
        key(4),
        Duration::from_secs(60),
        quota(Some(1), Some(5)),
        2,
        6,
    );
    let first = store.admit(reject.clone()).await.unwrap();
    assert!(matches!(
        first,
        AdmissionDecision::Rejected {
            reason: ExceededDimension::RequestsAndTokens,
            replayed: false,
            ..
        }
    ));
    let replay = store.admit(reject).await.unwrap();
    assert!(matches!(
        replay,
        AdmissionDecision::Rejected {
            reason: ExceededDimension::RequestsAndTokens,
            replayed: true,
            ..
        }
    ));
    assert_eq!(store.stats().idempotency_records, 1);
}

#[tokio::test]
async fn settlement_adjusts_both_dimensions_once_and_can_be_inspected() {
    let (_clock, store) = default_store();
    let (reservation, _) = allowed(
        store
            .admit(command(
                "settle-request",
                key(5),
                Duration::from_secs(60),
                quota(Some(10), Some(1_000)),
                1,
                100,
            ))
            .await
            .unwrap(),
    );
    let settle = SettleCommand {
        operation_id: Arc::from("quota-settle:v1:settle-request"),
        reservation: reservation.clone(),
        final_charge: QuotaCharge {
            requests: 1,
            tokens: 40,
        },
    };

    let applied = store.settle(settle.clone()).await.unwrap();
    assert_eq!(applied.disposition, SettlementDisposition::Applied);
    let snapshot = applied.snapshot.as_ref().unwrap();
    assert_eq!(snapshot.requests.unwrap().used, 1);
    assert_eq!(snapshot.tokens.unwrap().used, 40);

    let inspected = store
        .inspect(InspectQuery::Settlement {
            reservation: reservation.clone(),
            operation_id: settle.operation_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(inspected, InspectResult::Settlement(applied.clone()));

    let replay = store.settle(settle.clone()).await.unwrap();
    assert_eq!(replay.disposition, SettlementDisposition::Replayed);
    assert_eq!(replay.snapshot, applied.snapshot);

    let mut conflict = settle;
    conflict.final_charge.tokens = 41;
    let error = store.settle(conflict).await.unwrap_err();
    assert_eq!(error.kind(), RateLimitStoreErrorKind::Corrupt);
    let stats = store.stats();
    assert_eq!(stats.live_reservations, 0);
    assert_eq!(stats.recovery_records, 1);
}

#[tokio::test]
async fn full_refund_and_actual_charge_above_limit_are_terminal_adjustments() {
    let (_clock, store) = default_store();
    let bucket = key(6);
    let (refund, _) = allowed(
        store
            .admit(command(
                "refund",
                bucket.clone(),
                Duration::from_secs(60),
                quota(Some(2), Some(100)),
                1,
                80,
            ))
            .await
            .unwrap(),
    );
    let refunded = store
        .settle(SettleCommand {
            operation_id: Arc::from("quota-settle:v1:refund"),
            reservation: refund,
            final_charge: QuotaCharge::default(),
        })
        .await
        .unwrap();
    assert_eq!(refunded.snapshot.unwrap().tokens.unwrap().used, 0);

    let (increase, _) = allowed(
        store
            .admit(command(
                "increase",
                bucket.clone(),
                Duration::from_secs(60),
                quota(Some(2), Some(100)),
                1,
                20,
            ))
            .await
            .unwrap(),
    );
    let increased = store
        .settle(SettleCommand {
            operation_id: Arc::from("quota-settle:v1:increase"),
            reservation: increase,
            final_charge: QuotaCharge {
                requests: 1,
                tokens: 120,
            },
        })
        .await
        .unwrap();
    let final_snapshot = increased.snapshot.unwrap();
    assert_eq!(final_snapshot.tokens.unwrap().used, 120);
    assert_eq!(final_snapshot.tokens.unwrap().remaining, 0);

    let rejected = store
        .admit(command(
            "after-increase",
            bucket,
            Duration::from_secs(60),
            quota(Some(2), Some(100)),
            1,
            1,
        ))
        .await
        .unwrap();
    assert!(matches!(
        rejected,
        AdmissionDecision::Rejected {
            reason: ExceededDimension::Tokens,
            ..
        }
    ));
}

#[tokio::test]
async fn stale_settlement_never_mutates_the_next_window() {
    let (clock, store) = default_store();
    let bucket = key(7);
    let (old_reservation, old_snapshot) = allowed(
        store
            .admit(command(
                "old-window",
                bucket.clone(),
                Duration::from_secs(60),
                quota(Some(10), Some(100)),
                1,
                50,
            ))
            .await
            .unwrap(),
    );
    let old_generation = old_snapshot.window.identity.unwrap().generation;

    clock.advance(Duration::from_secs(60));
    let (_, new_snapshot) = allowed(
        store
            .admit(command(
                "new-window",
                bucket.clone(),
                Duration::from_secs(60),
                quota(Some(10), Some(100)),
                1,
                7,
            ))
            .await
            .unwrap(),
    );
    assert!(new_snapshot.window.identity.unwrap().generation > old_generation);

    let stale = store
        .settle(SettleCommand {
            operation_id: Arc::from("quota-settle:v1:old-window"),
            reservation: old_reservation,
            final_charge: QuotaCharge {
                requests: 1,
                tokens: 99,
            },
        })
        .await
        .unwrap();
    assert_eq!(stale.disposition, SettlementDisposition::StaleWindowNoop);
    assert!(stale.snapshot.is_none());

    let current = store
        .inspect(InspectQuery::Current {
            key: bucket,
            window: WindowSpec::fixed(Duration::from_secs(60)),
            limits: quota(Some(10), Some(100)),
        })
        .await
        .unwrap();
    let InspectResult::Current(current) = current else {
        panic!("expected current snapshot");
    };
    assert_eq!(current.requests.unwrap().used, 1);
    assert_eq!(current.tokens.unwrap().used, 7);
}

#[tokio::test]
async fn limit_null_and_spec_changes_follow_active_window_rules() {
    let (clock, store) = default_store();
    let bucket = key(8);
    let (_, initial) = allowed(
        store
            .admit(command(
                "initial",
                bucket.clone(),
                Duration::from_secs(60),
                quota(Some(5), Some(100)),
                1,
                10,
            ))
            .await
            .unwrap(),
    );
    let generation = initial.window.identity.unwrap().generation;

    let (_, disabled_rpm) = allowed(
        store
            .admit(command(
                "rpm-disabled",
                bucket.clone(),
                Duration::from_secs(10),
                quota(None, Some(100)),
                50,
                10,
            ))
            .await
            .unwrap(),
    );
    assert_eq!(disabled_rpm.window.duration, Duration::from_secs(60));
    assert!(disabled_rpm.requests.is_none());
    assert_eq!(disabled_rpm.tokens.unwrap().used, 20);

    let restored = store
        .inspect(InspectQuery::Current {
            key: bucket.clone(),
            window: WindowSpec::fixed(Duration::from_secs(10)),
            limits: quota(Some(5), None),
        })
        .await
        .unwrap();
    let InspectResult::Current(restored) = restored else {
        panic!("expected current snapshot");
    };
    assert_eq!(restored.requests.unwrap().used, 1);
    assert!(restored.tokens.is_none());
    assert_eq!(restored.window.duration, Duration::from_secs(60));

    let lowered = store
        .admit(command(
            "lowered",
            bucket.clone(),
            Duration::from_secs(10),
            quota(Some(1), None),
            1,
            0,
        ))
        .await
        .unwrap();
    assert!(matches!(
        lowered,
        AdmissionDecision::Rejected {
            reason: ExceededDimension::Requests,
            ..
        }
    ));

    clock.advance(Duration::from_secs(60));
    let (_, next) = allowed(
        store
            .admit(command(
                "next-generation",
                bucket,
                Duration::from_secs(10),
                quota(Some(5), Some(100)),
                1,
                1,
            ))
            .await
            .unwrap(),
    );
    assert_eq!(next.window.duration, Duration::from_secs(10));
    assert!(next.window.identity.unwrap().generation > generation);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_same_key_admission_has_an_exact_limit() {
    let store = Arc::new(default_store().1);
    let bucket = key(9);
    let mut tasks = Vec::new();
    for index in 0..200 {
        let store = Arc::clone(&store);
        let bucket = bucket.clone();
        tasks.push(tokio::spawn(async move {
            store
                .admit(command(
                    &format!("concurrent-{index}"),
                    bucket,
                    Duration::from_secs(60),
                    quota(Some(50), None),
                    1,
                    0,
                ))
                .await
                .unwrap()
        }));
    }

    let mut allowed_count = 0;
    for task in tasks {
        if matches!(task.await.unwrap(), AdmissionDecision::Allowed { .. }) {
            allowed_count += 1;
        }
    }
    assert_eq!(allowed_count, 50);
    let current = store
        .inspect(InspectQuery::Current {
            key: bucket,
            window: WindowSpec::fixed(Duration::from_secs(60)),
            limits: quota(Some(50), None),
        })
        .await
        .unwrap();
    let InspectResult::Current(current) = current else {
        panic!("expected current snapshot");
    };
    assert_eq!(current.requests.unwrap().used, 50);
}

#[tokio::test]
async fn per_key_capacity_isolated_and_does_not_break_replay_or_settlement() {
    let clock = Arc::new(ManualRateLimitClock::default());
    let mut config = compact_config();
    config.max_records_per_bucket = 2;
    let store = MemoryRateLimitStore::new(config, clock).unwrap();
    let attack_key = key(10);
    let other_key = key(11);

    let (first_token, _) = allowed(
        store
            .admit(command(
                "attack-1",
                attack_key.clone(),
                Duration::from_secs(60),
                quota(Some(100), None),
                1,
                0,
            ))
            .await
            .unwrap(),
    );
    let original = command(
        "attack-2",
        attack_key.clone(),
        Duration::from_secs(60),
        quota(Some(100), None),
        1,
        0,
    );
    allowed(store.admit(original.clone()).await.unwrap());

    let overloaded = store
        .admit(command(
            "attack-3",
            attack_key,
            Duration::from_secs(60),
            quota(Some(100), None),
            1,
            0,
        ))
        .await
        .unwrap_err();
    assert_eq!(overloaded.kind(), RateLimitStoreErrorKind::Overloaded);
    assert_eq!(store.stats().idempotency_records, 2);
    assert!(matches!(
        store.admit(original).await.unwrap(),
        AdmissionDecision::Allowed { replayed: true, .. }
    ));

    let settled = store
        .settle(SettleCommand {
            operation_id: Arc::from("quota-settle:v1:attack-1"),
            reservation: first_token,
            final_charge: QuotaCharge::default(),
        })
        .await
        .unwrap();
    assert_eq!(settled.disposition, SettlementDisposition::Applied);

    allowed(
        store
            .admit(command(
                "other-key",
                other_key,
                Duration::from_secs(60),
                quota(Some(100), None),
                1,
                0,
            ))
            .await
            .unwrap(),
    );
    assert_eq!(store.stats().idempotency_records, 3);
}

#[tokio::test]
async fn global_headroom_and_live_capacity_fail_closed_without_new_records() {
    let clock = Arc::new(ManualRateLimitClock::default());
    let mut config = compact_config();
    config.max_idempotency_records = 4;
    config.recovery_record_headroom = 1;
    config.max_live_reservations = 3;
    let store = MemoryRateLimitStore::new(config, clock).unwrap();

    let mut tokens = Vec::new();
    for index in 0..3 {
        let (token, _) = allowed(
            store
                .admit(command(
                    &format!("global-{index}"),
                    key(20 + index),
                    Duration::from_secs(60),
                    quota(Some(10), None),
                    1,
                    0,
                ))
                .await
                .unwrap(),
        );
        tokens.push(token);
    }
    let error = store
        .admit(command(
            "global-overload",
            key(30),
            Duration::from_secs(60),
            quota(Some(10), None),
            1,
            0,
        ))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), RateLimitStoreErrorKind::Overloaded);
    assert_eq!(store.stats().idempotency_records, 3);
    assert_eq!(store.stats().live_reservations, 3);

    store
        .settle(SettleCommand {
            operation_id: Arc::from("quota-settle:v1:global-0"),
            reservation: tokens.remove(0),
            final_charge: QuotaCharge::default(),
        })
        .await
        .unwrap();
    assert_eq!(store.stats().idempotency_records, 3);
    assert_eq!(store.stats().recovery_records, 1);
}

#[tokio::test]
async fn live_reservation_capacity_does_not_block_rejected_record_or_existing_settle() {
    let clock = Arc::new(ManualRateLimitClock::default());
    let mut config = compact_config();
    config.max_live_reservations = 1;
    let store = MemoryRateLimitStore::new(config, clock).unwrap();
    let (reservation, _) = allowed(
        store
            .admit(command(
                "only-live",
                key(31),
                Duration::from_secs(60),
                quota(Some(10), None),
                1,
                0,
            ))
            .await
            .unwrap(),
    );

    let overload = store
        .admit(command(
            "second-live",
            key(32),
            Duration::from_secs(60),
            quota(Some(10), None),
            1,
            0,
        ))
        .await
        .unwrap_err();
    assert_eq!(overload.kind(), RateLimitStoreErrorKind::Overloaded);
    assert_eq!(store.stats().idempotency_records, 1);

    let rejected = store
        .admit(command(
            "rejected-with-live-full",
            key(33),
            Duration::from_secs(60),
            quota(Some(1), None),
            2,
            0,
        ))
        .await
        .unwrap();
    assert!(matches!(
        rejected,
        AdmissionDecision::Rejected {
            reason: ExceededDimension::Requests,
            ..
        }
    ));
    assert_eq!(store.stats().rejected_records, 1);

    store
        .settle(SettleCommand {
            operation_id: Arc::from("quota-settle:v1:only-live"),
            reservation,
            final_charge: QuotaCharge::default(),
        })
        .await
        .unwrap();
    assert_eq!(store.stats().live_reservations, 0);
}

#[tokio::test]
async fn cleanup_keeps_live_reservations_and_retains_terminal_tombstones() {
    let clock = Arc::new(ManualRateLimitClock::default());
    let store = MemoryRateLimitStore::new(compact_config(), clock.clone()).unwrap();
    let (reservation, _) = allowed(
        store
            .admit(command(
                "long-request",
                key(40),
                Duration::from_secs(5),
                quota(Some(10), Some(100)),
                1,
                20,
            ))
            .await
            .unwrap(),
    );

    // 记录最低保留期为 5s window + 5s request + 5s retry。
    clock.advance(Duration::from_secs(20));
    store.cleanup_now();
    assert_eq!(store.stats().live_reservations, 1);
    assert_eq!(store.stats().idempotency_records, 1);
    assert_eq!(store.stats().buckets, 1);

    let stale = store
        .settle(SettleCommand {
            operation_id: Arc::from("quota-settle:v1:long-request"),
            reservation,
            final_charge: QuotaCharge {
                requests: 1,
                tokens: 25,
            },
        })
        .await
        .unwrap();
    assert_eq!(stale.disposition, SettlementDisposition::StaleWindowNoop);

    clock.advance(Duration::from_secs(4));
    store.cleanup_now();
    assert_eq!(store.stats().idempotency_records, 1);
    clock.advance(Duration::from_secs(1));
    let report = store.cleanup_now();
    assert_eq!(report.removed_records, 1);
    assert_eq!(report.removed_buckets, 1);
    assert_eq!(store.stats().idempotency_records, 0);
    assert_eq!(store.stats().buckets, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cleanup_racing_first_acquire_never_detaches_an_active_bucket() {
    let clock = Arc::new(ManualRateLimitClock::default());
    let mut config = compact_config();
    config.max_request_lifetime = Duration::ZERO;
    config.settlement_retry_grace = Duration::ZERO;
    config.cleanup_interval = Duration::from_secs(3_600);
    let store = Arc::new(MemoryRateLimitStore::new(config, clock.clone()).unwrap());
    let bucket = key(45);

    for index in 0..50 {
        if index != 0 {
            clock.advance(Duration::from_secs(1));
        }
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let cleanup_store = Arc::clone(&store);
        let cleanup_barrier = Arc::clone(&barrier);
        let cleanup = tokio::spawn(async move {
            cleanup_barrier.wait().await;
            cleanup_store.cleanup_now();
        });
        let admit_store = Arc::clone(&store);
        let admit_barrier = Arc::clone(&barrier);
        let bucket_for_admit = bucket.clone();
        let request_id = format!("cleanup-race-{index}");
        let admit = tokio::spawn(async move {
            admit_barrier.wait().await;
            admit_store
                .admit(command(
                    &request_id,
                    bucket_for_admit,
                    Duration::from_secs(1),
                    quota(Some(1), None),
                    1,
                    0,
                ))
                .await
                .unwrap()
        });

        cleanup.await.unwrap();
        let (reservation, snapshot) = allowed(admit.await.unwrap());
        assert_eq!(snapshot.requests.unwrap().used, 1);
        store
            .settle(SettleCommand {
                operation_id: Arc::from(format!("quota-settle:v1:cleanup-race-{index}")),
                reservation,
                final_charge: QuotaCharge {
                    requests: 1,
                    tokens: 0,
                },
            })
            .await
            .unwrap();
    }

    let current = store
        .inspect(InspectQuery::Current {
            key: bucket,
            window: WindowSpec::fixed(Duration::from_secs(1)),
            limits: quota(Some(1), None),
        })
        .await
        .unwrap();
    let InspectResult::Current(current) = current else {
        panic!("expected current snapshot");
    };
    assert_eq!(current.requests.unwrap().used, 1);
}

#[tokio::test]
async fn bounded_cleanup_rotates_across_all_buckets() {
    let clock = Arc::new(ManualRateLimitClock::default());
    let mut config = compact_config();
    config.max_request_lifetime = Duration::ZERO;
    config.settlement_retry_grace = Duration::ZERO;
    config.cleanup_scan_batch = 1;
    config.cleanup_interval = Duration::from_secs(3_600);
    let store = MemoryRateLimitStore::new(config, clock.clone()).unwrap();

    for index in 0..3 {
        let (reservation, _) = allowed(
            store
                .admit(command(
                    &format!("rotation-{index}"),
                    key(70 + index),
                    Duration::from_secs(1),
                    quota(Some(10), None),
                    1,
                    0,
                ))
                .await
                .unwrap(),
        );
        store
            .settle(SettleCommand {
                operation_id: Arc::from(format!("quota-settle:v1:rotation-{index}")),
                reservation,
                final_charge: QuotaCharge::default(),
            })
            .await
            .unwrap();
    }
    clock.advance(Duration::from_secs(1));

    for expected_remaining in [2, 1, 0] {
        let report = store.cleanup_now();
        assert_eq!(report.scanned_buckets, 1);
        assert_eq!(report.removed_buckets, 1);
        assert_eq!(store.stats().buckets, expected_remaining);
    }
}

#[tokio::test]
async fn reservation_is_bound_to_the_issuing_backend() {
    let (_clock_a, store_a) = default_store();
    let (_clock_b, store_b) = default_store();
    let (reservation, _) = allowed(
        store_a
            .admit(command(
                "backend-bound",
                key(50),
                Duration::from_secs(60),
                quota(Some(10), None),
                1,
                0,
            ))
            .await
            .unwrap(),
    );
    assert_eq!(format!("{reservation:?}"), "ReservationToken(<opaque>)");

    let error = store_b
        .settle(SettleCommand {
            operation_id: Arc::from("quota-settle:v1:backend-bound"),
            reservation,
            final_charge: QuotaCharge::default(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), RateLimitStoreErrorKind::Corrupt);
}

#[tokio::test]
async fn checked_counter_overflow_is_reported_without_partial_commit() {
    let (_clock, store) = default_store();
    let bucket = key(60);
    allowed(
        store
            .admit(command(
                "max",
                bucket.clone(),
                Duration::from_secs(60),
                quota(Some(u64::MAX), None),
                u64::MAX,
                0,
            ))
            .await
            .unwrap(),
    );
    let error = store
        .admit(command(
            "overflow",
            bucket.clone(),
            Duration::from_secs(60),
            quota(Some(u64::MAX), None),
            1,
            0,
        ))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), RateLimitStoreErrorKind::Corrupt);
    assert_eq!(store.stats().idempotency_records, 1);

    let current = store
        .inspect(InspectQuery::Current {
            key: bucket,
            window: WindowSpec::fixed(Duration::from_secs(60)),
            limits: quota(Some(u64::MAX), None),
        })
        .await
        .unwrap();
    let InspectResult::Current(current) = current else {
        panic!("expected current snapshot");
    };
    assert_eq!(current.requests.unwrap().used, u64::MAX);
}

#[test]
fn invalid_capacity_configuration_fails_at_startup() {
    let mut config = compact_config();
    config.recovery_record_headroom = config.max_idempotency_records;
    let error = match MemoryRateLimitStore::new(config, Arc::new(ManualRateLimitClock::default())) {
        Ok(_) => panic!("invalid config must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.field(), "recovery_record_headroom");
}

//! MemoryRateLimitStore 的有界容量与结果统计。

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Store 当前状态和累计操作统计。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RateLimitStoreStatsSnapshot {
    pub buckets: usize,
    pub idempotency_records: usize,
    pub live_reservations: usize,
    pub rejected_records: usize,
    pub recovery_records: usize,
    pub admissions_allowed: u64,
    pub admissions_rejected: u64,
    pub admission_replays: u64,
    pub settlements_applied: u64,
    pub settlements_stale: u64,
    pub settlement_replays: u64,
    pub overloads: u64,
    pub cleanup_runs: u64,
    pub cleanup_records_removed: u64,
    pub cleanup_buckets_removed: u64,
}

#[derive(Default)]
pub(crate) struct RateLimitStoreStats {
    pub buckets: AtomicUsize,
    pub idempotency_records: AtomicUsize,
    pub live_reservations: AtomicUsize,
    pub rejected_records: AtomicUsize,
    pub recovery_records: AtomicUsize,
    pub admissions_allowed: AtomicU64,
    pub admissions_rejected: AtomicU64,
    pub admission_replays: AtomicU64,
    pub settlements_applied: AtomicU64,
    pub settlements_stale: AtomicU64,
    pub settlement_replays: AtomicU64,
    pub overloads: AtomicU64,
    pub cleanup_runs: AtomicU64,
    pub cleanup_records_removed: AtomicU64,
    pub cleanup_buckets_removed: AtomicU64,
}

impl RateLimitStoreStats {
    pub fn snapshot(&self) -> RateLimitStoreStatsSnapshot {
        RateLimitStoreStatsSnapshot {
            buckets: self.buckets.load(Ordering::Relaxed),
            idempotency_records: self.idempotency_records.load(Ordering::Relaxed),
            live_reservations: self.live_reservations.load(Ordering::Relaxed),
            rejected_records: self.rejected_records.load(Ordering::Relaxed),
            recovery_records: self.recovery_records.load(Ordering::Relaxed),
            admissions_allowed: self.admissions_allowed.load(Ordering::Relaxed),
            admissions_rejected: self.admissions_rejected.load(Ordering::Relaxed),
            admission_replays: self.admission_replays.load(Ordering::Relaxed),
            settlements_applied: self.settlements_applied.load(Ordering::Relaxed),
            settlements_stale: self.settlements_stale.load(Ordering::Relaxed),
            settlement_replays: self.settlement_replays.load(Ordering::Relaxed),
            overloads: self.overloads.load(Ordering::Relaxed),
            cleanup_runs: self.cleanup_runs.load(Ordering::Relaxed),
            cleanup_records_removed: self.cleanup_records_removed.load(Ordering::Relaxed),
            cleanup_buckets_removed: self.cleanup_buckets_removed.load(Ordering::Relaxed),
        }
    }
}

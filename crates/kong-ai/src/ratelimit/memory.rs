//! 有界、进程内的固定窗口配额 Store。

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use uuid::Uuid;

use super::clock::{MonoTime, RateLimitClock, RateLimitNow, SystemRateLimitClock};
use super::metrics::{RateLimitStoreStats, RateLimitStoreStatsSnapshot};
use super::store::{RateLimitStore, RateLimitStoreError};
use super::types::{
    AdmissionDecision, AdmitCommand, BackendInstanceId, DimensionSnapshot, ExceededDimension,
    InspectQuery, InspectResult, QuotaCharge, QuotaLimits, RateLimitBackendDescriptor,
    RateLimitBackendScope, RateLimitKey, RateLimitSnapshot, ReservationEnvelope, ReservationToken,
    SettleCommand, SettlementDisposition, SettlementResult, WindowIdentity, WindowSnapshot,
    WindowSpec, RATE_LIMIT_SCHEMA_VERSION,
};

/// Memory Store 容量与保留策略。
#[derive(Clone, Debug)]
pub struct MemoryRateLimitConfig {
    pub max_buckets: usize,
    pub max_idempotency_records: usize,
    pub max_records_per_bucket: usize,
    pub max_live_reservations: usize,
    pub recovery_record_headroom: usize,
    pub max_request_lifetime: Duration,
    pub settlement_retry_grace: Duration,
    pub cleanup_interval: Duration,
    pub cleanup_scan_batch: usize,
}

impl Default for MemoryRateLimitConfig {
    fn default() -> Self {
        Self {
            max_buckets: 100_000,
            max_idempotency_records: 2_000_000,
            max_records_per_bucket: 100_000,
            max_live_reservations: 200_000,
            recovery_record_headroom: 50_000,
            max_request_lifetime: Duration::from_secs(15 * 60),
            settlement_retry_grace: Duration::from_secs(5 * 60),
            cleanup_interval: Duration::from_secs(30),
            cleanup_scan_batch: 4096,
        }
    }
}

/// 启动时拒绝不安全的 Memory Store 配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryRateLimitConfigError {
    field: &'static str,
    message: &'static str,
}

impl MemoryRateLimitConfigError {
    pub fn field(&self) -> &'static str {
        self.field
    }
}

impl fmt::Display for MemoryRateLimitConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for MemoryRateLimitConfigError {}

impl MemoryRateLimitConfig {
    pub fn validate(&self) -> Result<(), MemoryRateLimitConfigError> {
        for (field, value) in [
            ("max_buckets", self.max_buckets),
            ("max_idempotency_records", self.max_idempotency_records),
            ("max_records_per_bucket", self.max_records_per_bucket),
            ("max_live_reservations", self.max_live_reservations),
            ("cleanup_scan_batch", self.cleanup_scan_batch),
        ] {
            if value == 0 {
                return Err(MemoryRateLimitConfigError {
                    field,
                    message: "必须大于 0",
                });
            }
        }
        if self.recovery_record_headroom >= self.max_idempotency_records {
            return Err(MemoryRateLimitConfigError {
                field: "recovery_record_headroom",
                message: "必须小于 max_idempotency_records",
            });
        }
        if self.cleanup_interval.is_zero() {
            return Err(MemoryRateLimitConfigError {
                field: "cleanup_interval",
                message: "必须大于 0",
            });
        }
        self.max_request_lifetime
            .checked_add(self.settlement_retry_grace)
            .ok_or(MemoryRateLimitConfigError {
                field: "max_request_lifetime",
                message: "与 settlement_retry_grace 相加后溢出",
            })?;
        Ok(())
    }
}

/// 一次显式清理的结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryCleanupReport {
    pub scanned_buckets: usize,
    pub removed_records: usize,
    pub removed_buckets: usize,
}

/// 进程内 RateLimitStore。
pub struct MemoryRateLimitStore {
    instance_id: BackendInstanceId,
    buckets: DashMap<RateLimitKey, Arc<BucketCell>>,
    clock: Arc<dyn RateLimitClock>,
    config: MemoryRateLimitConfig,
    stats: Arc<RateLimitStoreStats>,
    next_cleanup_at: Mutex<MonoTime>,
    cleanup_queue: Mutex<VecDeque<CleanupTicket>>,
}

struct BucketCell {
    active_ops: AtomicUsize,
    cleanup_queued: AtomicBool,
    state: Mutex<BucketState>,
}

struct CleanupTicket {
    key: RateLimitKey,
    cell: Weak<BucketCell>,
}

struct BucketState {
    active_window: Option<WindowState>,
    generation: u64,
    admissions: HashMap<Arc<str>, AdmissionRecord>,
    last_touched_mono: MonoTime,
}

struct WindowState {
    identity: WindowIdentity,
    spec: WindowSpec,
    started_wall: SystemTime,
    reset_mono: MonoTime,
    reset_wall: SystemTime,
    requests_used: u64,
    tokens_used: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdmitFingerprint {
    window: WindowSpec,
    limits: QuotaLimits,
    reserve: QuotaCharge,
}

struct AdmissionRecord {
    fingerprint: AdmitFingerprint,
    decision: AdmissionDecision,
    settlement: Option<SettlementRecord>,
    retain_until: MonoTime,
}

struct SettlementRecord {
    operation_id: Arc<str>,
    final_charge: QuotaCharge,
    result: SettlementResult,
}

struct BucketLease {
    cell: Arc<BucketCell>,
}

impl Drop for BucketLease {
    fn drop(&mut self) {
        self.cell.active_ops.fetch_sub(1, Ordering::Release);
    }
}

enum AdmitAttempt {
    Decision(AdmissionDecision),
    Capacity,
}

impl MemoryRateLimitStore {
    pub fn new(
        config: MemoryRateLimitConfig,
        clock: Arc<dyn RateLimitClock>,
    ) -> Result<Self, MemoryRateLimitConfigError> {
        config.validate()?;
        let now = clock.now();
        let next_cleanup_at =
            now.mono
                .checked_add(config.cleanup_interval)
                .ok_or(MemoryRateLimitConfigError {
                    field: "cleanup_interval",
                    message: "与当前单调时间相加后溢出",
                })?;
        Ok(Self {
            instance_id: BackendInstanceId::new(),
            buckets: DashMap::new(),
            clock,
            config,
            stats: Arc::new(RateLimitStoreStats::default()),
            next_cleanup_at: Mutex::new(next_cleanup_at),
            cleanup_queue: Mutex::new(VecDeque::new()),
        })
    }

    pub fn with_defaults(clock: Arc<dyn RateLimitClock>) -> Self {
        Self::new(MemoryRateLimitConfig::default(), clock)
            .expect("default memory rate limit config must be valid")
    }

    /// 执行一次有界清理。主要供后台任务和确定性测试调用。
    pub fn cleanup_now(&self) -> MemoryCleanupReport {
        let now = self.clock.now();
        self.cleanup_at(now)
    }

    fn validate_key(key: &RateLimitKey) -> Result<(), RateLimitStoreError> {
        if key.schema_version != RATE_LIMIT_SCHEMA_VERSION {
            return Err(RateLimitStoreError::corrupt(
                "unsupported rate limit key schema version",
            ));
        }
        if key.deployment_namespace.is_empty() {
            return Err(RateLimitStoreError::corrupt(
                "deployment namespace must not be empty",
            ));
        }
        Ok(())
    }

    fn validate_window(window: WindowSpec) -> Result<(), RateLimitStoreError> {
        if window.duration.is_zero() {
            return Err(RateLimitStoreError::corrupt(
                "rate limit window duration must be positive",
            ));
        }
        Ok(())
    }

    fn validate_admit(command: &AdmitCommand) -> Result<(), RateLimitStoreError> {
        Self::validate_key(&command.key)?;
        Self::validate_window(command.window)?;
        if command.request_id.is_empty() {
            return Err(RateLimitStoreError::corrupt(
                "rate limit request id must not be empty",
            ));
        }
        Ok(())
    }

    fn maybe_cleanup(&self, now: RateLimitNow) {
        let due = {
            let mut next = match self.next_cleanup_at.lock() {
                Ok(next) => next,
                Err(_) => return,
            };
            if now.mono < *next {
                false
            } else {
                *next = now
                    .mono
                    .checked_add(self.config.cleanup_interval)
                    .unwrap_or(now.mono);
                true
            }
        };
        if due {
            self.cleanup_at(now);
        }
    }

    fn cleanup_at(&self, now: RateLimitNow) -> MemoryCleanupReport {
        self.stats.cleanup_runs.fetch_add(1, Ordering::Relaxed);
        let mut tickets = Vec::new();
        {
            let Ok(mut queue) = self.cleanup_queue.lock() else {
                return MemoryCleanupReport::default();
            };
            let scan_count = queue.len().min(self.config.cleanup_scan_batch);
            tickets.extend((0..scan_count).filter_map(|_| queue.pop_front()));
        }
        let mut report = MemoryCleanupReport {
            scanned_buckets: tickets.len(),
            ..MemoryCleanupReport::default()
        };
        let mut requeue = Vec::new();

        for ticket in tickets {
            let Some(ticket_cell) = ticket.cell.upgrade() else {
                continue;
            };
            let mut removed_from_bucket = 0;
            let mut still_current = false;
            let mut needs_requeue = false;
            let removed = self.buckets.remove_if(&ticket.key, |_, cell| {
                if !Arc::ptr_eq(cell, &ticket_cell) {
                    return false;
                }
                still_current = true;
                if cell.active_ops.load(Ordering::Acquire) != 0 {
                    needs_requeue = true;
                    return false;
                }
                let Ok(mut state) = cell.state.lock() else {
                    needs_requeue = true;
                    return false;
                };
                if cell.active_ops.load(Ordering::Acquire) != 0 {
                    needs_requeue = true;
                    return false;
                }
                removed_from_bucket = self.purge_expired(&mut state, now.mono);
                let removable = state.active_window.is_none() && state.admissions.is_empty();
                needs_requeue = !removable;
                removable
            });
            report.removed_records += removed_from_bucket;
            if removed.is_some() {
                self.stats.buckets.fetch_sub(1, Ordering::Relaxed);
                report.removed_buckets += 1;
                ticket_cell.cleanup_queued.store(false, Ordering::Release);
            } else if still_current && needs_requeue {
                requeue.push(ticket);
            } else {
                ticket_cell.cleanup_queued.store(false, Ordering::Release);
            }
        }
        if !requeue.is_empty() {
            if let Ok(mut queue) = self.cleanup_queue.lock() {
                queue.extend(requeue);
            }
        }

        self.stats
            .cleanup_records_removed
            .fetch_add(report.removed_records as u64, Ordering::Relaxed);
        self.stats
            .cleanup_buckets_removed
            .fetch_add(report.removed_buckets as u64, Ordering::Relaxed);
        report
    }

    fn purge_expired(&self, state: &mut BucketState, now: MonoTime) -> usize {
        if state
            .active_window
            .as_ref()
            .is_some_and(|window| now >= window.reset_mono)
        {
            state.active_window = None;
        }

        let mut removed_rejected = 0usize;
        let mut removed_recovery = 0usize;
        state.admissions.retain(|_, record| {
            let removable = record.settlement.is_some()
                || matches!(record.decision, AdmissionDecision::Rejected { .. });
            if !removable || now < record.retain_until {
                return true;
            }
            if record.settlement.is_some() {
                removed_recovery += 1;
            } else {
                removed_rejected += 1;
            }
            false
        });
        let removed = removed_rejected + removed_recovery;
        if removed != 0 {
            self.stats
                .idempotency_records
                .fetch_sub(removed, Ordering::Relaxed);
            self.stats
                .rejected_records
                .fetch_sub(removed_rejected, Ordering::Relaxed);
            self.stats
                .recovery_records
                .fetch_sub(removed_recovery, Ordering::Relaxed);
        }
        removed
    }

    fn acquire_bucket(
        &self,
        key: &RateLimitKey,
        create: bool,
    ) -> Result<Option<BucketLease>, RateLimitStoreError> {
        if let Some(entry) = self.buckets.get(key) {
            entry.active_ops.fetch_add(1, Ordering::Acquire);
            return Ok(Some(BucketLease {
                cell: Arc::clone(entry.value()),
            }));
        }
        if !create {
            return Ok(None);
        }
        if !try_reserve(&self.stats.buckets, self.config.max_buckets) {
            return Err(RateLimitStoreError::overloaded(
                "memory rate limit bucket capacity exhausted",
            ));
        }

        match self.buckets.entry(key.clone()) {
            Entry::Occupied(entry) => {
                self.stats.buckets.fetch_sub(1, Ordering::Relaxed);
                entry.get().active_ops.fetch_add(1, Ordering::Acquire);
                Ok(Some(BucketLease {
                    cell: Arc::clone(entry.get()),
                }))
            }
            Entry::Vacant(entry) => {
                let cell = Arc::new(BucketCell {
                    active_ops: AtomicUsize::new(1),
                    cleanup_queued: AtomicBool::new(false),
                    state: Mutex::new(BucketState {
                        active_window: None,
                        generation: 0,
                        admissions: HashMap::new(),
                        last_touched_mono: Duration::ZERO,
                    }),
                });
                entry.insert(Arc::clone(&cell));
                Ok(Some(BucketLease { cell }))
            }
        }
    }

    fn lock_state<'a>(
        &self,
        lease: &'a BucketLease,
    ) -> Result<MutexGuard<'a, BucketState>, RateLimitStoreError> {
        lease
            .cell
            .state
            .lock()
            .map_err(|_| RateLimitStoreError::corrupt("rate limit bucket lock poisoned"))
    }

    fn remove_empty_bucket(&self, key: &RateLimitKey) {
        let removed = self.buckets.remove_if(key, |_, cell| {
            if cell.active_ops.load(Ordering::Acquire) != 0 {
                return false;
            }
            let Ok(state) = cell.state.lock() else {
                return false;
            };
            cell.active_ops.load(Ordering::Acquire) == 0
                && state.active_window.is_none()
                && state.admissions.is_empty()
        });
        if let Some((_, cell)) = removed {
            cell.cleanup_queued.store(false, Ordering::Release);
            self.stats.buckets.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn schedule_cleanup(
        &self,
        key: &RateLimitKey,
        cell: &Arc<BucketCell>,
    ) -> Result<(), RateLimitStoreError> {
        if cell
            .cleanup_queued
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        let mut queue = match self.cleanup_queue.lock() {
            Ok(queue) => queue,
            Err(_) => {
                cell.cleanup_queued.store(false, Ordering::Release);
                return Err(RateLimitStoreError::corrupt(
                    "rate limit cleanup queue lock poisoned",
                ));
            }
        };
        queue.push_back(CleanupTicket {
            key: key.clone(),
            cell: Arc::downgrade(cell),
        });
        Ok(())
    }

    fn admit_once(
        &self,
        command: &AdmitCommand,
        now: RateLimitNow,
    ) -> Result<AdmitAttempt, RateLimitStoreError> {
        let Some(lease) = self.acquire_bucket(&command.key, true)? else {
            unreachable!("create=true always returns a bucket");
        };
        self.schedule_cleanup(&command.key, &lease.cell)?;
        let mut state = self.lock_state(&lease)?;
        state.last_touched_mono = now.mono;
        self.purge_expired(&mut state, now.mono);

        let fingerprint = AdmitFingerprint {
            window: command.window,
            limits: command.limits,
            reserve: command.reserve,
        };
        if let Some(record) = state.admissions.get(&command.request_id) {
            if record.fingerprint != fingerprint {
                return Err(RateLimitStoreError::corrupt(
                    "admit idempotency payload conflict",
                ));
            }
            self.stats.admission_replays.fetch_add(1, Ordering::Relaxed);
            return Ok(AdmitAttempt::Decision(record.decision.as_replayed()));
        }

        let effective_window = state
            .active_window
            .as_ref()
            .map_or(command.window, |window| window.spec);
        let retention = retention_duration(effective_window, &self.config)?;
        let retain_until = now
            .mono
            .checked_add(retention)
            .ok_or_else(|| RateLimitStoreError::corrupt("idempotency retention overflow"))?;
        let admission_capacity = self
            .config
            .max_idempotency_records
            .saturating_sub(self.config.recovery_record_headroom);
        if state.admissions.len() >= self.config.max_records_per_bucket
            || !try_reserve(&self.stats.idempotency_records, admission_capacity)
        {
            return Ok(AdmitAttempt::Capacity);
        }

        let created_window = state.active_window.is_none();
        let window = match ensure_window(&mut state, command.window, now) {
            Ok(window) => window,
            Err(error) => {
                self.stats
                    .idempotency_records
                    .fetch_sub(1, Ordering::Relaxed);
                return Err(error);
            }
        };
        let requests_candidate = if command.limits.requests.is_some() {
            match window.requests_used.checked_add(command.reserve.requests) {
                Some(candidate) => candidate,
                None => {
                    self.stats
                        .idempotency_records
                        .fetch_sub(1, Ordering::Relaxed);
                    rollback_new_window(&mut state, created_window);
                    return Err(RateLimitStoreError::corrupt(
                        "request quota counter overflow",
                    ));
                }
            }
        } else {
            window.requests_used
        };
        let tokens_candidate = if command.limits.tokens.is_some() {
            match window.tokens_used.checked_add(command.reserve.tokens) {
                Some(candidate) => candidate,
                None => {
                    self.stats
                        .idempotency_records
                        .fetch_sub(1, Ordering::Relaxed);
                    rollback_new_window(&mut state, created_window);
                    return Err(RateLimitStoreError::corrupt("token quota counter overflow"));
                }
            }
        } else {
            window.tokens_used
        };

        let requests_exceeded = command
            .limits
            .requests
            .is_some_and(|limit| requests_candidate > limit.get());
        let tokens_exceeded = command
            .limits
            .tokens
            .is_some_and(|limit| tokens_candidate > limit.get());

        if requests_exceeded || tokens_exceeded {
            let reason = match (requests_exceeded, tokens_exceeded) {
                (true, true) => ExceededDimension::RequestsAndTokens,
                (true, false) => ExceededDimension::Requests,
                (false, true) => ExceededDimension::Tokens,
                (false, false) => unreachable!(),
            };
            let decision = AdmissionDecision::Rejected {
                reason,
                snapshot: snapshot(window, command.limits, now),
                replayed: false,
            };
            state.admissions.insert(
                Arc::clone(&command.request_id),
                AdmissionRecord {
                    fingerprint,
                    decision: decision.clone(),
                    settlement: None,
                    retain_until,
                },
            );
            self.stats.rejected_records.fetch_add(1, Ordering::Relaxed);
            self.stats
                .admissions_rejected
                .fetch_add(1, Ordering::Relaxed);
            return Ok(AdmitAttempt::Decision(decision));
        }

        if !try_reserve(
            &self.stats.live_reservations,
            self.config.max_live_reservations,
        ) {
            self.stats
                .idempotency_records
                .fetch_sub(1, Ordering::Relaxed);
            rollback_new_window(&mut state, created_window);
            return Ok(AdmitAttempt::Capacity);
        }

        window.requests_used = requests_candidate;
        window.tokens_used = tokens_candidate;
        let token = ReservationToken(Arc::new(ReservationEnvelope {
            schema_version: RATE_LIMIT_SCHEMA_VERSION,
            backend_instance_id: self.instance_id,
            reservation_id: Uuid::new_v4(),
            request_id: Arc::clone(&command.request_id),
            key: command.key.clone(),
            window: window.identity,
            limits_at_admission: command.limits,
            reserved: command.reserve,
        }));
        let decision = AdmissionDecision::Allowed {
            reservation: token,
            snapshot: snapshot(window, command.limits, now),
            replayed: false,
        };
        state.admissions.insert(
            Arc::clone(&command.request_id),
            AdmissionRecord {
                fingerprint,
                decision: decision.clone(),
                settlement: None,
                retain_until,
            },
        );
        self.stats
            .admissions_allowed
            .fetch_add(1, Ordering::Relaxed);
        Ok(AdmitAttempt::Decision(decision))
    }

    fn settle_inner(
        &self,
        command: SettleCommand,
        now: RateLimitNow,
    ) -> Result<SettlementResult, RateLimitStoreError> {
        if command.operation_id.is_empty() {
            return Err(RateLimitStoreError::corrupt(
                "settlement operation id must not be empty",
            ));
        }
        let envelope = command.reservation.envelope();
        self.validate_envelope(envelope)?;
        let Some(lease) = self.acquire_bucket(&envelope.key, false)? else {
            return Err(RateLimitStoreError::corrupt(
                "reservation bucket no longer exists",
            ));
        };
        let mut state = self.lock_state(&lease)?;
        state.last_touched_mono = now.mono;
        self.purge_expired(&mut state, now.mono);

        let record = state
            .admissions
            .get(&envelope.request_id)
            .ok_or_else(|| RateLimitStoreError::corrupt("reservation admission not found"))?;
        let stored_token = match &record.decision {
            AdmissionDecision::Allowed { reservation, .. } => reservation,
            AdmissionDecision::Rejected { .. } => {
                return Err(RateLimitStoreError::corrupt(
                    "rejected admission has no reservation",
                ));
            }
        };
        if stored_token != &command.reservation {
            return Err(RateLimitStoreError::corrupt(
                "reservation token does not match admission",
            ));
        }
        if let Some(settlement) = &record.settlement {
            if settlement.operation_id != command.operation_id
                || settlement.final_charge != command.final_charge
            {
                return Err(RateLimitStoreError::corrupt(
                    "settlement idempotency payload conflict",
                ));
            }
            self.stats
                .settlement_replays
                .fetch_add(1, Ordering::Relaxed);
            return Ok(SettlementResult {
                disposition: SettlementDisposition::Replayed,
                snapshot: settlement.result.snapshot.clone(),
            });
        }

        let same_window = state.active_window.as_ref().is_some_and(|window| {
            window.identity == envelope.window && now.mono < window.reset_mono
        });
        let result = if same_window {
            let window = state
                .active_window
                .as_mut()
                .expect("same_window requires an active window");
            let requests = adjusted_count(
                window.requests_used,
                envelope.reserved.requests,
                command.final_charge.requests,
                envelope.limits_at_admission.requests.is_some(),
                "request quota settlement arithmetic invalid",
            )?;
            let tokens = adjusted_count(
                window.tokens_used,
                envelope.reserved.tokens,
                command.final_charge.tokens,
                envelope.limits_at_admission.tokens.is_some(),
                "token quota settlement arithmetic invalid",
            )?;
            window.requests_used = requests;
            window.tokens_used = tokens;
            SettlementResult {
                disposition: SettlementDisposition::Applied,
                snapshot: Some(snapshot(window, envelope.limits_at_admission, now)),
            }
        } else {
            SettlementResult {
                disposition: SettlementDisposition::StaleWindowNoop,
                snapshot: None,
            }
        };

        let retry_until = now
            .mono
            .checked_add(self.config.settlement_retry_grace)
            .ok_or_else(|| RateLimitStoreError::corrupt("settlement retention overflow"))?;
        let record = state
            .admissions
            .get_mut(&envelope.request_id)
            .expect("admission was checked above");
        record.retain_until = record.retain_until.max(retry_until);
        record.settlement = Some(SettlementRecord {
            operation_id: command.operation_id,
            final_charge: command.final_charge,
            result: result.clone(),
        });
        self.stats.live_reservations.fetch_sub(1, Ordering::Relaxed);
        self.stats.recovery_records.fetch_add(1, Ordering::Relaxed);
        match result.disposition {
            SettlementDisposition::Applied => {
                self.stats
                    .settlements_applied
                    .fetch_add(1, Ordering::Relaxed);
            }
            SettlementDisposition::StaleWindowNoop => {
                self.stats.settlements_stale.fetch_add(1, Ordering::Relaxed);
            }
            SettlementDisposition::Replayed => unreachable!(),
        }
        Ok(result)
    }

    fn validate_envelope(&self, envelope: &ReservationEnvelope) -> Result<(), RateLimitStoreError> {
        if envelope.schema_version != RATE_LIMIT_SCHEMA_VERSION {
            return Err(RateLimitStoreError::corrupt(
                "unsupported reservation schema version",
            ));
        }
        if envelope.backend_instance_id != self.instance_id {
            return Err(RateLimitStoreError::corrupt(
                "reservation belongs to a different backend instance",
            ));
        }
        Self::validate_key(&envelope.key)
    }

    fn prospective_snapshot(
        &self,
        window: WindowSpec,
        limits: QuotaLimits,
        now: RateLimitNow,
    ) -> Result<RateLimitSnapshot, RateLimitStoreError> {
        let reset_at = now
            .wall
            .checked_add(window.duration)
            .ok_or_else(|| RateLimitStoreError::corrupt("window wall time overflow"))?;
        Ok(RateLimitSnapshot {
            window: WindowSnapshot {
                identity: None,
                algorithm: window.algorithm,
                duration: window.duration,
                started_at: now.wall,
                reset_at,
                reset_after: window.duration,
            },
            requests: dimension_snapshot(limits.requests, 0),
            tokens: dimension_snapshot(limits.tokens, 0),
        })
    }

    fn inspect_inner(
        &self,
        query: InspectQuery,
        now: RateLimitNow,
    ) -> Result<InspectResult, RateLimitStoreError> {
        match query {
            InspectQuery::Current {
                key,
                window,
                limits,
            } => {
                Self::validate_key(&key)?;
                Self::validate_window(window)?;
                let Some(lease) = self.acquire_bucket(&key, false)? else {
                    return Ok(InspectResult::Current(
                        self.prospective_snapshot(window, limits, now)?,
                    ));
                };
                let mut state = self.lock_state(&lease)?;
                state.last_touched_mono = now.mono;
                self.purge_expired(&mut state, now.mono);
                match state.active_window.as_ref() {
                    Some(active) => Ok(InspectResult::Current(snapshot(active, limits, now))),
                    None => Ok(InspectResult::Current(
                        self.prospective_snapshot(window, limits, now)?,
                    )),
                }
            }
            InspectQuery::Admission { key, request_id } => {
                Self::validate_key(&key)?;
                if request_id.is_empty() {
                    return Err(RateLimitStoreError::corrupt(
                        "rate limit request id must not be empty",
                    ));
                }
                let Some(lease) = self.acquire_bucket(&key, false)? else {
                    return Ok(InspectResult::NotFound);
                };
                let mut state = self.lock_state(&lease)?;
                state.last_touched_mono = now.mono;
                self.purge_expired(&mut state, now.mono);
                Ok(match state.admissions.get(&request_id) {
                    Some(record) => InspectResult::Admission(record.decision.as_replayed()),
                    None => InspectResult::NotFound,
                })
            }
            InspectQuery::Settlement {
                reservation,
                operation_id,
            } => {
                if operation_id.is_empty() {
                    return Err(RateLimitStoreError::corrupt(
                        "settlement operation id must not be empty",
                    ));
                }
                let envelope = reservation.envelope();
                self.validate_envelope(envelope)?;
                let Some(lease) = self.acquire_bucket(&envelope.key, false)? else {
                    return Ok(InspectResult::NotFound);
                };
                let mut state = self.lock_state(&lease)?;
                state.last_touched_mono = now.mono;
                self.purge_expired(&mut state, now.mono);
                let Some(record) = state.admissions.get(&envelope.request_id) else {
                    return Ok(InspectResult::NotFound);
                };
                let stored_token = match &record.decision {
                    AdmissionDecision::Allowed { reservation, .. } => reservation,
                    AdmissionDecision::Rejected { .. } => {
                        return Err(RateLimitStoreError::corrupt(
                            "rejected admission has no reservation",
                        ));
                    }
                };
                if stored_token != &reservation {
                    return Err(RateLimitStoreError::corrupt(
                        "reservation token does not match admission",
                    ));
                }
                Ok(match &record.settlement {
                    Some(settlement) if settlement.operation_id == operation_id => {
                        InspectResult::Settlement(settlement.result.clone())
                    }
                    _ => InspectResult::NotFound,
                })
            }
        }
    }
}

impl Default for MemoryRateLimitStore {
    fn default() -> Self {
        Self::with_defaults(Arc::new(SystemRateLimitClock::new()))
    }
}

#[async_trait]
impl RateLimitStore for MemoryRateLimitStore {
    fn descriptor(&self) -> RateLimitBackendDescriptor {
        RateLimitBackendDescriptor {
            backend: Arc::from("memory"),
            instance_id: self.instance_id,
            scope: RateLimitBackendScope::ProcessLocal,
            ephemeral: true,
        }
    }

    async fn admit(&self, command: AdmitCommand) -> Result<AdmissionDecision, RateLimitStoreError> {
        Self::validate_admit(&command)?;
        let now = self.clock.now();
        self.maybe_cleanup(now);

        for attempt in 0..2 {
            match self.admit_once(&command, now) {
                Ok(AdmitAttempt::Decision(decision)) => return Ok(decision),
                Ok(AdmitAttempt::Capacity) if attempt == 0 => {
                    self.cleanup_at(now);
                }
                Err(error)
                    if error.kind() == super::store::RateLimitStoreErrorKind::Overloaded
                        && attempt == 0 =>
                {
                    self.cleanup_at(now);
                }
                Err(error) if error.kind() == super::store::RateLimitStoreErrorKind::Overloaded => {
                    self.stats.overloads.fetch_add(1, Ordering::Relaxed);
                    return Err(error);
                }
                Err(error) => return Err(error),
                Ok(AdmitAttempt::Capacity) => {
                    self.stats.overloads.fetch_add(1, Ordering::Relaxed);
                    self.remove_empty_bucket(&command.key);
                    return Err(RateLimitStoreError::overloaded(
                        "memory rate limit admission capacity exhausted",
                    ));
                }
            }
        }
        unreachable!("admission capacity retry loop always returns")
    }

    async fn settle(
        &self,
        command: SettleCommand,
    ) -> Result<SettlementResult, RateLimitStoreError> {
        let now = self.clock.now();
        self.maybe_cleanup(now);
        self.settle_inner(command, now)
    }

    async fn inspect(&self, query: InspectQuery) -> Result<InspectResult, RateLimitStoreError> {
        let now = self.clock.now();
        self.maybe_cleanup(now);
        self.inspect_inner(query, now)
    }

    fn stats(&self) -> RateLimitStoreStatsSnapshot {
        self.stats.snapshot()
    }
}

fn rollback_new_window(state: &mut BucketState, created_window: bool) {
    if created_window {
        state.active_window = None;
        state.generation -= 1;
    }
}

fn try_reserve(counter: &AtomicUsize, limit: usize) -> bool {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        if current >= limit {
            return false;
        }
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(actual) => current = actual,
        }
    }
}

fn ensure_window(
    state: &mut BucketState,
    spec: WindowSpec,
    now: RateLimitNow,
) -> Result<&mut WindowState, RateLimitStoreError> {
    if state.active_window.is_none() {
        let generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| RateLimitStoreError::corrupt("window generation overflow"))?;
        let reset_mono = now
            .mono
            .checked_add(spec.duration)
            .ok_or_else(|| RateLimitStoreError::corrupt("window monotonic time overflow"))?;
        let reset_wall = now
            .wall
            .checked_add(spec.duration)
            .ok_or_else(|| RateLimitStoreError::corrupt("window wall time overflow"))?;
        state.generation = generation;
        state.active_window = Some(WindowState {
            identity: WindowIdentity {
                id: super::types::WindowId::new(),
                generation,
            },
            spec,
            started_wall: now.wall,
            reset_mono,
            reset_wall,
            requests_used: 0,
            tokens_used: 0,
        });
    }
    Ok(state
        .active_window
        .as_mut()
        .expect("window is initialized above"))
}

fn retention_duration(
    window: WindowSpec,
    config: &MemoryRateLimitConfig,
) -> Result<Duration, RateLimitStoreError> {
    window
        .duration
        .checked_add(config.max_request_lifetime)
        .and_then(|duration| duration.checked_add(config.settlement_retry_grace))
        .ok_or_else(|| RateLimitStoreError::corrupt("idempotency retention duration overflow"))
}

fn adjusted_count(
    current: u64,
    reserved: u64,
    final_charge: u64,
    enabled: bool,
    error: &'static str,
) -> Result<u64, RateLimitStoreError> {
    if !enabled {
        return Ok(current);
    }
    current
        .checked_sub(reserved)
        .and_then(|count| count.checked_add(final_charge))
        .ok_or_else(|| RateLimitStoreError::corrupt(error))
}

fn dimension_snapshot(limit: Option<std::num::NonZeroU64>, used: u64) -> Option<DimensionSnapshot> {
    limit.map(|limit| DimensionSnapshot {
        limit: limit.get(),
        used,
        remaining: limit.get().saturating_sub(used),
    })
}

fn snapshot(window: &WindowState, limits: QuotaLimits, now: RateLimitNow) -> RateLimitSnapshot {
    RateLimitSnapshot {
        window: WindowSnapshot {
            identity: Some(window.identity),
            algorithm: window.spec.algorithm,
            duration: window.spec.duration,
            started_at: window.started_wall,
            reset_at: window.reset_wall,
            reset_after: window.reset_mono.saturating_sub(now.mono),
        },
        requests: dimension_snapshot(limits.requests, window.requests_used),
        tokens: dimension_snapshot(limits.tokens, window.tokens_used),
    }
}

// 以下旧实现仅用于尚未迁移到 RateLimitStore 的 ai-rate-limit 插件。

/// 内存固定窗口限流器。
pub struct MemoryRateLimiter {
    windows: DashMap<String, WindowEntry>,
    window_duration: Duration,
}

struct WindowEntry {
    start: Instant,
    count: AtomicU64,
}

impl MemoryRateLimiter {
    /// 创建旧同步内存限流器。
    pub fn new(window_duration: Duration) -> Self {
        Self {
            windows: DashMap::new(),
            window_duration,
        }
    }

    fn get_or_reset(
        &self,
        key: &str,
        now: Instant,
    ) -> dashmap::mapref::one::Ref<'_, String, WindowEntry> {
        if let Some(entry) = self.windows.get(key) {
            if now.duration_since(entry.start) < self.window_duration {
                return entry;
            }
        }
        self.windows
            .entry(key.to_string())
            .and_modify(|entry| {
                if now.duration_since(entry.start) >= self.window_duration {
                    entry.start = now;
                    entry.count.store(0, Ordering::Relaxed);
                }
            })
            .or_insert_with(|| WindowEntry {
                start: now,
                count: AtomicU64::new(0),
            });
        self.windows
            .get(key)
            .expect("legacy rate limit window was inserted above")
    }
}

impl super::RateLimiter for MemoryRateLimiter {
    fn check(&self, key: &str, limit: u64) -> (bool, u64) {
        let entry = self.get_or_reset(key, Instant::now());
        let current = entry.count.load(Ordering::Relaxed);
        (current < limit, current)
    }

    fn check_and_increment(&self, key: &str, limit: u64, amount: u64) -> (bool, u64) {
        let entry = self.get_or_reset(key, Instant::now());
        loop {
            let current = entry.count.load(Ordering::Relaxed);
            if current.saturating_add(amount) > limit {
                return (false, current);
            }
            match entry.count.compare_exchange_weak(
                current,
                current + amount,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return (true, current + amount),
                Err(_) => continue,
            }
        }
    }

    fn decrement(&self, key: &str, amount: u64) {
        let entry = self.get_or_reset(key, Instant::now());
        loop {
            let current = entry.count.load(Ordering::Relaxed);
            let new_value = current.saturating_sub(amount);
            match entry.count.compare_exchange_weak(
                current,
                new_value,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(_) => continue,
            }
        }
    }

    fn increment(&self, key: &str, amount: u64) {
        let entry = self.get_or_reset(key, Instant::now());
        entry.count.fetch_add(amount, Ordering::Relaxed);
    }
}

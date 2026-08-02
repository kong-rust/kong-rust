//! 有界、非阻塞 usage writer 及批量 flush runner。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, Sleep};
use uuid::Uuid;

use super::model::{AiUsageError, AiUsageFact};
use super::store::AiUsageStore;

pub const DEFAULT_QUEUE_CAPACITY: usize = 8192;
pub const DEFAULT_BATCH_SIZE: usize = 256;
pub const MAX_QUEUE_CAPACITY: usize = 1_000_000;
pub const MAX_BATCH_SIZE: usize = 1_129;
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_millis(500);
pub const SHUTDOWN_DRAIN_WINDOW: Duration = Duration::from_secs(5);
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
];

#[derive(Default)]
pub struct AiUsageWriterStats {
    enqueued: AtomicU64,
    written: AtomicU64,
    duplicate: AtomicU64,
    dropped: AtomicU64,
    write_failures: AtomicU64,
    retries: AtomicU64,
    write_outcome_unknown: AtomicU64,
    dbless_evicted: AtomicU64,
    dropped_queue_full: AtomicU64,
    dropped_writer_closed: AtomicU64,
    dropped_write_retries_exhausted: AtomicU64,
    dropped_shutdown_timeout: AtomicU64,
    queue_depth: AtomicU64,
    queue_capacity: AtomicU64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiUsageWriterStatsSnapshot {
    pub enqueued: u64,
    pub written: u64,
    pub duplicate: u64,
    pub dropped: u64,
    pub write_failures: u64,
    pub retries: u64,
    pub write_outcome_unknown: u64,
    pub dbless_evicted: u64,
    pub dropped_queue_full: u64,
    pub dropped_writer_closed: u64,
    pub dropped_write_retries_exhausted: u64,
    pub dropped_shutdown_timeout: u64,
    pub queue_depth: u64,
    pub queue_capacity: u64,
}

impl AiUsageWriterStats {
    pub fn snapshot(&self) -> AiUsageWriterStatsSnapshot {
        AiUsageWriterStatsSnapshot {
            enqueued: self.enqueued.load(Ordering::Relaxed),
            written: self.written.load(Ordering::Relaxed),
            duplicate: self.duplicate.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            write_failures: self.write_failures.load(Ordering::Relaxed),
            retries: self.retries.load(Ordering::Relaxed),
            write_outcome_unknown: self.write_outcome_unknown.load(Ordering::Relaxed),
            dbless_evicted: self.dbless_evicted.load(Ordering::Relaxed),
            dropped_queue_full: self.dropped_queue_full.load(Ordering::Relaxed),
            dropped_writer_closed: self.dropped_writer_closed.load(Ordering::Relaxed),
            dropped_write_retries_exhausted: self
                .dropped_write_retries_exhausted
                .load(Ordering::Relaxed),
            dropped_shutdown_timeout: self.dropped_shutdown_timeout.load(Ordering::Relaxed),
            queue_depth: self.queue_depth.load(Ordering::Relaxed),
            queue_capacity: self.queue_capacity.load(Ordering::Relaxed),
        }
    }

    pub fn set_dbless_evicted(&self, value: u64) {
        self.dbless_evicted.store(value, Ordering::Relaxed);
    }

    fn dropped(&self, reason: &AtomicU64, count: u64) {
        reason.fetch_add(count, Ordering::Relaxed);
        self.dropped.fetch_add(count, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub struct AiUsageWriter {
    sender: mpsc::Sender<Arc<AiUsageFact>>,
    accepting: Arc<AtomicBool>,
    stats: Arc<AiUsageWriterStats>,
}

pub struct AiUsageWriterRunner {
    receiver: mpsc::Receiver<Arc<AiUsageFact>>,
    store: Arc<dyn AiUsageStore>,
    accepting: Arc<AtomicBool>,
    stats: Arc<AiUsageWriterStats>,
    batch_size: usize,
    flush_interval: Duration,
    shutdown_window: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlushResult {
    Persisted,
    ConfirmedFailure,
    OutcomeUnknown,
}

impl AiUsageWriter {
    pub fn channel(
        store: Arc<dyn AiUsageStore>,
        queue_capacity: usize,
        batch_size: usize,
        flush_interval: Duration,
    ) -> Result<(Self, AiUsageWriterRunner), String> {
        Self::channel_with_shutdown(
            store,
            queue_capacity,
            batch_size,
            flush_interval,
            SHUTDOWN_DRAIN_WINDOW,
        )
    }

    pub fn channel_with_shutdown(
        store: Arc<dyn AiUsageStore>,
        queue_capacity: usize,
        batch_size: usize,
        flush_interval: Duration,
        shutdown_window: Duration,
    ) -> Result<(Self, AiUsageWriterRunner), String> {
        if queue_capacity == 0 {
            return Err("ai_usage_queue_capacity 必须大于 0".to_string());
        }
        if queue_capacity > MAX_QUEUE_CAPACITY {
            return Err(format!(
                "ai_usage_queue_capacity 不能大于 {MAX_QUEUE_CAPACITY}"
            ));
        }
        if batch_size == 0 {
            return Err("ai_usage_batch_size 必须大于 0".to_string());
        }
        if batch_size > MAX_BATCH_SIZE {
            return Err(format!("ai_usage_batch_size 不能大于 {MAX_BATCH_SIZE}"));
        }
        if batch_size > queue_capacity {
            return Err("ai_usage_batch_size 不能大于 ai_usage_queue_capacity".to_string());
        }
        if flush_interval.is_zero() {
            return Err("ai_usage_flush_interval 必须大于 0".to_string());
        }
        if shutdown_window.is_zero() {
            return Err("ai_usage_shutdown_timeout 必须大于 0".to_string());
        }
        let (sender, receiver) = mpsc::channel(queue_capacity);
        let accepting = Arc::new(AtomicBool::new(true));
        let stats = Arc::new(AiUsageWriterStats::default());
        stats
            .queue_capacity
            .store(queue_capacity as u64, Ordering::Relaxed);
        Ok((
            Self {
                sender,
                accepting: Arc::clone(&accepting),
                stats: Arc::clone(&stats),
            },
            AiUsageWriterRunner {
                receiver,
                store,
                accepting,
                stats,
                batch_size,
                flush_interval,
                shutdown_window,
            },
        ))
    }

    pub fn try_enqueue(&self, fact: Arc<AiUsageFact>) -> bool {
        if !self.accepting.load(Ordering::Acquire) {
            self.stats.dropped(&self.stats.dropped_writer_closed, 1);
            return false;
        }
        self.stats.queue_depth.fetch_add(1, Ordering::Relaxed);
        match self.sender.try_send(fact) {
            Ok(()) => {
                self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.stats.queue_depth.fetch_sub(1, Ordering::Relaxed);
                self.stats.dropped(&self.stats.dropped_queue_full, 1);
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.stats.queue_depth.fetch_sub(1, Ordering::Relaxed);
                self.stats.dropped(&self.stats.dropped_writer_closed, 1);
                false
            }
        }
    }

    pub fn stats(&self) -> Arc<AiUsageWriterStats> {
        Arc::clone(&self.stats)
    }

    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
    }
}

impl AiUsageWriterRunner {
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) {
        let mut batch = Vec::with_capacity(self.batch_size);
        let mut next_flush: Option<Instant> = None;
        let mut shutdown_deadline: Option<Instant> = None;

        loop {
            tokio::select! {
                message = self.receiver.recv() => {
                    match message {
                        Some(fact) => {
                            self.stats.queue_depth.fetch_sub(1, Ordering::Relaxed);
                            if batch.is_empty() {
                                next_flush = Some(Instant::now() + self.flush_interval);
                            }
                            batch.push(fact);
                            if batch.len() >= self.batch_size {
                                if !self.flush_with_deadline(&mut batch, shutdown_deadline).await {
                                    self.stop_and_count_remaining(&mut batch);
                                    break;
                                }
                                next_flush = None;
                            }
                        }
                        None => {
                            self.flush_with_deadline(&mut batch, shutdown_deadline).await;
                            self.accepting.store(false, Ordering::Release);
                            break;
                        }
                    }
                }
                _ = sleep_until(next_flush), if next_flush.is_some() => {
                    if !self.flush_with_deadline(&mut batch, shutdown_deadline).await {
                        self.stop_and_count_remaining(&mut batch);
                        break;
                    }
                    next_flush = None;
                }
                changed = shutdown.changed(), if shutdown_deadline.is_none() => {
                    if changed.is_err() || *shutdown.borrow() {
                        shutdown_deadline = Some(Instant::now() + self.shutdown_window);
                    }
                }
                _ = sleep_until(shutdown_deadline), if shutdown_deadline.is_some() => {
                    self.stop_and_count_remaining(&mut batch);
                    break;
                }
            }
        }
    }

    async fn flush_with_deadline(
        &self,
        batch: &mut Vec<Arc<AiUsageFact>>,
        deadline: Option<Instant>,
    ) -> bool {
        if batch.is_empty() {
            return true;
        }
        let facts: Vec<_> = batch.iter().map(|fact| fact.as_ref().clone()).collect();
        let flush = self.flush(&facts);
        let result = if let Some(deadline) = deadline {
            match tokio::time::timeout_at(deadline, flush).await {
                Ok(result) => result,
                Err(_) => {
                    self.stats
                        .write_outcome_unknown
                        .fetch_add(1, Ordering::Relaxed);
                    batch.clear();
                    return false;
                }
            }
        } else {
            flush.await
        };
        match result {
            FlushResult::Persisted | FlushResult::OutcomeUnknown => batch.clear(),
            FlushResult::ConfirmedFailure => {
                self.stats.dropped(
                    &self.stats.dropped_write_retries_exhausted,
                    batch.len() as u64,
                );
                batch.clear();
            }
        }
        true
    }

    async fn flush(&self, facts: &[AiUsageFact]) -> FlushResult {
        let mut saw_unknown_outcome = false;
        let mut retry_delays = RETRY_DELAYS.iter().copied();
        let mut attempts = 0;
        loop {
            attempts += 1;
            match self.store.insert_batch(facts).await {
                Ok(result) => {
                    self.stats
                        .written
                        .fetch_add(result.inserted, Ordering::Relaxed);
                    self.stats
                        .duplicate
                        .fetch_add(result.duplicate, Ordering::Relaxed);
                    return FlushResult::Persisted;
                }
                Err(error) => {
                    self.stats.write_failures.fetch_add(1, Ordering::Relaxed);
                    let outcome_unknown = matches!(&error, AiUsageError::WriteOutcomeUnknown(_));
                    if outcome_unknown {
                        saw_unknown_outcome = true;
                        self.stats
                            .write_outcome_unknown
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    let retry_delay = if is_transient(&error) {
                        retry_delays.next()
                    } else {
                        None
                    };
                    let Some(retry_delay) = retry_delay else {
                        tracing::warn!(
                            backend = %self.store.mode(),
                            batch_size = facts.len(),
                            attempt = attempts,
                            error_category = error_category(&error),
                            dropped_total = self.stats.dropped.load(Ordering::Relaxed),
                            "AI usage batch 写入失败"
                        );
                        return if saw_unknown_outcome {
                            FlushResult::OutcomeUnknown
                        } else {
                            FlushResult::ConfirmedFailure
                        };
                    };
                    self.stats.retries.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(retry_delay).await;
                }
            }
        }
    }

    fn stop_and_count_remaining(&mut self, batch: &mut Vec<Arc<AiUsageFact>>) {
        self.accepting.store(false, Ordering::Release);
        self.receiver.close();
        let mut remaining = batch.len() as u64;
        batch.clear();
        while self.receiver.try_recv().is_ok() {
            remaining += 1;
            self.stats.queue_depth.fetch_sub(1, Ordering::Relaxed);
        }
        if remaining > 0 {
            self.stats
                .dropped(&self.stats.dropped_shutdown_timeout, remaining);
        }
    }
}

fn sleep_until(deadline: Option<Instant>) -> Sleep {
    tokio::time::sleep_until(
        deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(31_536_000)),
    )
}

fn is_transient(error: &AiUsageError) -> bool {
    match error {
        AiUsageError::QueryUnavailable(_)
        | AiUsageError::QueryTimeout(_)
        | AiUsageError::WriteOutcomeUnknown(_) => true,
        AiUsageError::Internal(message) => {
            let message = message.to_ascii_lowercase();
            [
                "connection",
                "pool timed out",
                "serialization",
                "could not serialize",
                "deadlock",
                "canceling statement due to statement timeout",
                "sqlstate 40001",
                "sqlstate 40p01",
                "sqlstate 57014",
            ]
            .iter()
            .any(|needle| message.contains(needle))
        }
        AiUsageError::InvalidQuery(_) | AiUsageError::SnapshotExpired(_) => false,
    }
}

fn error_category(error: &AiUsageError) -> &'static str {
    match error {
        AiUsageError::QueryUnavailable(_) => "unavailable",
        AiUsageError::QueryTimeout(_) => "timeout",
        AiUsageError::WriteOutcomeUnknown(_) => "outcome_unknown",
        AiUsageError::Internal(_) => "database",
        AiUsageError::InvalidQuery(_) => "invalid",
        AiUsageError::SnapshotExpired(_) => "snapshot",
    }
}

#[derive(Clone)]
pub enum AiUsageRuntime {
    Supported {
        store: Arc<dyn AiUsageStore>,
        default_workspace_id: Uuid,
        stats: Arc<AiUsageWriterStats>,
    },
    UnsupportedHybrid,
}

impl AiUsageRuntime {
    pub fn unsupported_hybrid() -> Self {
        Self::UnsupportedHybrid
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use chrono::Utc;
    use rust_decimal::Decimal;

    use super::*;
    use crate::usage::model::{
        AiUsageFilter, AiUsageListQuery, AiUsageMode, AiUsageOutcome, AiUsagePage, AiUsageSnapshot,
        AiUsageSummary, AiUsageSummaryQuery, BatchWriteResult, CacheStatus, CostStatus,
        PricingStatus, UsageSource,
    };

    struct RecordingStore {
        written: AtomicU64,
    }

    struct UnknownOutcomeStore;

    #[async_trait]
    impl AiUsageStore for RecordingStore {
        fn mode(&self) -> AiUsageMode {
            AiUsageMode::Dbless
        }

        async fn insert_batch(
            &self,
            rows: &[AiUsageFact],
        ) -> super::super::model::AiUsageResult<BatchWriteResult> {
            self.written.fetch_add(rows.len() as u64, Ordering::Relaxed);
            Ok(BatchWriteResult {
                inserted: rows.len() as u64,
                duplicate: 0,
            })
        }

        async fn snapshot(
            &self,
            _filter: &AiUsageFilter,
        ) -> super::super::model::AiUsageResult<AiUsageSnapshot> {
            unreachable!()
        }

        async fn list(
            &self,
            _query: &AiUsageListQuery,
        ) -> super::super::model::AiUsageResult<AiUsagePage> {
            unreachable!()
        }

        async fn summary(
            &self,
            _query: &AiUsageSummaryQuery,
        ) -> super::super::model::AiUsageResult<AiUsageSummary> {
            unreachable!()
        }
    }

    #[async_trait]
    impl AiUsageStore for UnknownOutcomeStore {
        fn mode(&self) -> AiUsageMode {
            AiUsageMode::Postgres
        }

        async fn insert_batch(
            &self,
            _rows: &[AiUsageFact],
        ) -> super::super::model::AiUsageResult<BatchWriteResult> {
            Err(AiUsageError::WriteOutcomeUnknown(
                "test commit outcome is unknown".to_string(),
            ))
        }

        async fn snapshot(
            &self,
            _filter: &AiUsageFilter,
        ) -> super::super::model::AiUsageResult<AiUsageSnapshot> {
            unreachable!()
        }

        async fn list(
            &self,
            _query: &AiUsageListQuery,
        ) -> super::super::model::AiUsageResult<AiUsagePage> {
            unreachable!()
        }

        async fn summary(
            &self,
            _query: &AiUsageSummaryQuery,
        ) -> super::super::model::AiUsageResult<AiUsageSummary> {
            unreachable!()
        }
    }

    fn fact(index: u64) -> Arc<AiUsageFact> {
        let now = Utc::now();
        Arc::new(AiUsageFact {
            id: Uuid::new_v4(),
            ingest_seq: None,
            request_id: format!("{index:032x}"),
            node_id: Uuid::new_v4(),
            started_at: now,
            finished_at: now,
            recorded_at: None,
            workspace_id: Some(Uuid::nil()),
            route_id: None,
            route_name: None,
            service_id: None,
            service_name: None,
            provider_id: None,
            provider_name: None,
            provider_type: None,
            model_id: None,
            requested_model: None,
            model_group: None,
            actual_model: None,
            attempt_count: 0,
            virtual_key_id: None,
            virtual_key_name: None,
            virtual_key_prefix: None,
            consumer_id: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            reasoning_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            usage_source: UsageSource::Unavailable,
            usage_unavailable_reasons: vec!["not_attempted".to_string()],
            input_price: None,
            output_price: None,
            pricing_fingerprint: None,
            pricing_status: PricingStatus::NotApplicable,
            pricing_unsupported_reasons: Vec::new(),
            cost_usd: Some(Decimal::ZERO),
            cost_status: CostStatus::NotIncurred,
            cost_unavailable_reasons: Vec::new(),
            status_code: Some(401),
            upstream_status_code: None,
            outcome: AiUsageOutcome::GatewayRejected,
            e2e_ms: 1,
            ttft_ms: None,
            upstream_attempted: false,
            stream: None,
            cache_status: CacheStatus::NotConfigured,
            context_compression: None,
        })
    }

    #[tokio::test(start_paused = true)]
    async fn flushes_by_batch_and_drains_for_fixed_shutdown_window() {
        let store = Arc::new(RecordingStore {
            written: AtomicU64::new(0),
        });
        let store_trait: Arc<dyn AiUsageStore> = store.clone();
        let (writer, runner) =
            AiUsageWriter::channel(store_trait, 8, 2, Duration::from_millis(500)).unwrap();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(runner.run(shutdown_rx));
        assert!(writer.try_enqueue(fact(1)));
        assert!(writer.try_enqueue(fact(2)));
        tokio::task::yield_now().await;
        assert_eq!(store.written.load(Ordering::Relaxed), 2);

        shutdown_tx.send(true).unwrap();
        tokio::time::advance(Duration::from_secs(4)).await;
        assert!(writer.try_enqueue(fact(3)));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(500)).await;
        tokio::task::yield_now().await;
        assert_eq!(store.written.load(Ordering::Relaxed), 3);
        tokio::time::advance(Duration::from_millis(500)).await;
        task.await.unwrap();
        assert!(!writer.is_accepting());
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_commit_outcome_is_retried_without_confirmed_drop() {
        let store: Arc<dyn AiUsageStore> = Arc::new(UnknownOutcomeStore);
        let (writer, runner) =
            AiUsageWriter::channel(store, 8, 1, Duration::from_millis(500)).unwrap();
        let stats = writer.stats();
        let row = fact(1).as_ref().clone();
        let flush = tokio::spawn(async move { runner.flush(&[row]).await });

        tokio::task::yield_now().await;
        for delay in RETRY_DELAYS {
            tokio::time::advance(delay).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(flush.await.unwrap(), FlushResult::OutcomeUnknown);
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.write_failures, 4);
        assert_eq!(snapshot.retries, 3);
        assert_eq!(snapshot.write_outcome_unknown, 4);
        assert_eq!(snapshot.dropped, 0);
        assert_eq!(snapshot.dropped_write_retries_exhausted, 0);
    }

    #[test]
    fn channel_rejects_capacities_that_cannot_be_executed_safely() {
        let store: Arc<dyn AiUsageStore> = Arc::new(RecordingStore {
            written: AtomicU64::new(0),
        });
        assert!(AiUsageWriter::channel(
            Arc::clone(&store),
            MAX_QUEUE_CAPACITY + 1,
            1,
            Duration::from_millis(1),
        )
        .is_err());
        assert!(
            AiUsageWriter::channel(store, 2048, MAX_BATCH_SIZE + 1, Duration::from_millis(1),)
                .is_err()
        );
    }
}

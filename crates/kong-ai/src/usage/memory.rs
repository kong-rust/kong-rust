//! DB-less 模式的有界内存 ring Store。

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use rand::RngCore;
use tokio::sync::{RwLock, Semaphore};
use uuid::Uuid;

use super::cursor::{
    encode_offset, encode_snapshot, filter_hash, validate_offset, validate_snapshot_binding,
    validate_snapshot_watermark, CURSOR_VERSION,
};
use super::model::{
    AiUsageError, AiUsageFact, AiUsageListQuery, AiUsageMeta, AiUsageMode, AiUsageOffset,
    AiUsagePage, AiUsageRecord, AiUsageResult, AiUsageSnapshot, AiUsageSummary,
    AiUsageSummaryQuery, BatchWriteResult,
};
use super::store::{aggregate, build_breakdown, AiUsageStore};

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const QUERY_CONCURRENCY: usize = 4;
pub const MAX_DBLESS_CAPACITY: usize = 1_000_000;

#[derive(Default)]
struct RingState {
    rows: VecDeque<Arc<AiUsageFact>>,
    next_seq: i64,
    generation: u64,
}

#[derive(Clone)]
pub struct MemoryAiUsageStore {
    state: Arc<RwLock<RingState>>,
    ring_instance_id: Uuid,
    node_id: Uuid,
    capacity: usize,
    evicted: Arc<AtomicU64>,
    writer_stats: Arc<std::sync::RwLock<Option<Arc<super::writer::AiUsageWriterStats>>>>,
    query_slots: Arc<Semaphore>,
}

impl MemoryAiUsageStore {
    pub fn new(node_id: Uuid, capacity: usize) -> Result<Self, String> {
        if capacity == 0 {
            return Err("ai_usage_dbless_capacity 必须大于 0".to_string());
        }
        if capacity > MAX_DBLESS_CAPACITY {
            return Err(format!(
                "ai_usage_dbless_capacity 不能大于 {MAX_DBLESS_CAPACITY}"
            ));
        }
        let mut random = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut random);
        Ok(Self {
            state: Arc::new(RwLock::new(RingState::default())),
            ring_instance_id: Uuid::from_bytes(random),
            node_id,
            capacity,
            evicted: Arc::new(AtomicU64::new(0)),
            writer_stats: Arc::new(std::sync::RwLock::new(None)),
            query_slots: Arc::new(Semaphore::new(QUERY_CONCURRENCY)),
        })
    }

    pub fn evicted_total(&self) -> u64 {
        self.evicted.load(Ordering::Relaxed)
    }

    pub fn attach_writer_stats(&self, stats: Arc<super::writer::AiUsageWriterStats>) {
        *self.writer_stats.write().unwrap() = Some(stats);
    }

    async fn capture(
        &self,
        snapshot: &AiUsageSnapshot,
        filter: &super::model::AiUsageFilter,
    ) -> AiUsageResult<(Vec<Arc<AiUsageFact>>, AiUsageMeta)> {
        let state = self.state.read().await;
        let current_watermark = state.next_seq;
        validate_snapshot_binding(snapshot, filter, AiUsageMode::Dbless)?;
        if snapshot.ring_instance_id != Some(self.ring_instance_id)
            || snapshot.eviction_generation != Some(state.generation)
        {
            return Err(AiUsageError::SnapshotExpired(
                "Analytics snapshot has expired".to_string(),
            ));
        }
        validate_snapshot_watermark(snapshot, current_watermark)?;
        let mut earliest = None;
        let mut rows = Vec::with_capacity(state.rows.len());
        for (index, fact) in state.rows.iter().enumerate() {
            earliest = Some(
                earliest.map_or(fact.started_at, |value: chrono::DateTime<Utc>| {
                    value.min(fact.started_at)
                }),
            );
            rows.push(Arc::clone(fact));
            if (index + 1) % 4096 == 0 {
                tokio::task::yield_now().await;
            }
        }
        Ok((
            rows,
            AiUsageMeta {
                mode: AiUsageMode::Dbless,
                ephemeral: true,
                node_id: Some(self.node_id),
                capacity: Some(self.capacity),
                earliest_available_at: earliest,
                restart_clears: true,
            },
        ))
    }

    async fn query_permit(&self) -> AiUsageResult<tokio::sync::OwnedSemaphorePermit> {
        self.query_slots.clone().try_acquire_owned().map_err(|_| {
            AiUsageError::QueryUnavailable("Analytics query capacity is exhausted".to_string())
        })
    }
}

#[async_trait]
impl AiUsageStore for MemoryAiUsageStore {
    fn mode(&self) -> AiUsageMode {
        AiUsageMode::Dbless
    }

    async fn insert_batch(&self, rows: &[AiUsageFact]) -> AiUsageResult<BatchWriteResult> {
        let mut state = self.state.write().await;
        let mut request_ids: HashSet<String> = state
            .rows
            .iter()
            .map(|fact| fact.request_id.clone())
            .collect();
        let mut result = BatchWriteResult::default();
        for fact in rows {
            if !request_ids.insert(fact.request_id.clone()) {
                result.duplicate += 1;
                continue;
            }
            state.next_seq = state
                .next_seq
                .checked_add(1)
                .ok_or_else(|| AiUsageError::Internal("DB-less usage sequence 溢出".to_string()))?;
            let mut fact = fact.clone();
            fact.ingest_seq = Some(state.next_seq);
            fact.recorded_at.get_or_insert_with(Utc::now);
            state.rows.push_back(Arc::new(fact));
            result.inserted += 1;
            if state.rows.len() > self.capacity {
                let next_generation = state.generation.checked_add(1).ok_or_else(|| {
                    AiUsageError::Internal("DB-less usage eviction generation 溢出".to_string())
                })?;
                state.rows.pop_front();
                state.generation = next_generation;
                let evicted = self.evicted.fetch_add(1, Ordering::Relaxed) + 1;
                if let Some(stats) = self.writer_stats.read().unwrap().as_ref() {
                    stats.set_dbless_evicted(evicted);
                }
            }
        }
        Ok(result)
    }

    async fn snapshot(
        &self,
        filter: &super::model::AiUsageFilter,
    ) -> AiUsageResult<AiUsageSnapshot> {
        let state = tokio::time::timeout(QUERY_TIMEOUT, self.state.read())
            .await
            .map_err(|_| AiUsageError::QueryTimeout("Analytics query timed out".to_string()))?;
        Ok(AiUsageSnapshot {
            v: CURSOR_VERSION,
            backend: AiUsageMode::Dbless,
            workspace_id: filter.workspace_id,
            start: filter.start,
            end: filter.end,
            high_watermark: state.next_seq,
            eviction_generation: Some(state.generation),
            ring_instance_id: Some(self.ring_instance_id),
            filter_hash: filter_hash(filter)?,
        })
    }

    async fn list(&self, query: &AiUsageListQuery) -> AiUsageResult<AiUsagePage> {
        let deadline = tokio::time::Instant::now() + QUERY_TIMEOUT;
        let (permit, rows, meta) = tokio::time::timeout_at(deadline, async {
            let permit = self.query_permit().await?;
            let (rows, meta) = self.capture(&query.snapshot, &query.filter).await?;
            Ok::<_, AiUsageError>((permit, rows, meta))
        })
        .await
        .map_err(|_| AiUsageError::QueryTimeout("Analytics query timed out".to_string()))??;
        if let Some(offset) = &query.offset {
            validate_offset(offset, &query.snapshot)?;
        }
        let query = query.clone();
        let worker_query = query.clone();
        tokio::time::timeout_at(
            deadline,
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let mut rows: Vec<_> = rows
                    .into_iter()
                    .filter(|fact| {
                        fact.ingest_seq.is_some_and(|sequence| {
                            sequence <= worker_query.snapshot.high_watermark
                        }) && worker_query.filter.matches(fact)
                            && worker_query.offset.as_ref().is_none_or(|offset| {
                                (fact.started_at, fact.id)
                                    < (offset.last_started_at, offset.last_id)
                            })
                    })
                    .collect();
                rows.sort_unstable_by(|left, right| {
                    (right.started_at, right.id).cmp(&(left.started_at, left.id))
                });
                let has_more = rows.len() > worker_query.size;
                rows.truncate(worker_query.size);
                (rows, has_more)
            }),
        )
        .await
        .map_err(|_| AiUsageError::QueryTimeout("Analytics query timed out".to_string()))?
        .map_err(|error| AiUsageError::Internal(format!("Analytics task failed: {error}")))
        .and_then(|(rows, has_more)| {
            let offset = if has_more {
                rows.last()
                    .map(|fact| AiUsageOffset {
                        v: CURSOR_VERSION,
                        snapshot: query.snapshot.clone(),
                        last_started_at: fact.started_at,
                        last_id: fact.id,
                    })
                    .map(|offset| encode_offset(&offset))
                    .transpose()?
            } else {
                None
            };
            Ok(AiUsagePage {
                data: rows
                    .iter()
                    .map(|fact| AiUsageRecord::from(fact.as_ref()))
                    .collect(),
                offset,
                next: None,
                snapshot: encode_snapshot(&query.snapshot)?,
                meta,
            })
        })
    }

    async fn summary(&self, query: &AiUsageSummaryQuery) -> AiUsageResult<AiUsageSummary> {
        let deadline = tokio::time::Instant::now() + QUERY_TIMEOUT;
        let (permit, rows, meta) = tokio::time::timeout_at(deadline, async {
            let permit = self.query_permit().await?;
            let (rows, meta) = self.capture(&query.snapshot, &query.filter).await?;
            Ok::<_, AiUsageError>((permit, rows, meta))
        })
        .await
        .map_err(|_| AiUsageError::QueryTimeout("Analytics query timed out".to_string()))??;
        let query = query.clone();
        let worker_query = query.clone();
        tokio::time::timeout_at(
            deadline,
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let rows: Vec<_> = rows
                    .iter()
                    .filter(|fact| {
                        fact.ingest_seq.is_some_and(|sequence| {
                            sequence <= worker_query.snapshot.high_watermark
                        }) && worker_query.filter.matches(fact)
                    })
                    .map(Arc::as_ref)
                    .collect();
                let totals = aggregate(&rows);
                let breakdown = build_breakdown(&rows, &worker_query)?;
                Ok::<_, AiUsageError>((totals, breakdown))
            }),
        )
        .await
        .map_err(|_| AiUsageError::QueryTimeout("Analytics query timed out".to_string()))?
        .map_err(|error| AiUsageError::Internal(format!("Analytics task failed: {error}")))?
        .map(|(totals, breakdown)| AiUsageSummary {
            snapshot: encode_snapshot(&query.snapshot).expect("已验证的 snapshot 应始终可编码"),
            meta,
            totals,
            breakdown,
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use rust_decimal::Decimal;

    use super::*;
    use crate::usage::model::{
        AiUsageFilter, AiUsageOutcome, CacheStatus, CostStatus, PricingStatus, UsageSource,
    };

    fn fact(request_id: &str, workspace_id: Uuid) -> AiUsageFact {
        let started_at = Utc::now();
        AiUsageFact {
            id: Uuid::new_v4(),
            ingest_seq: None,
            request_id: request_id.to_string(),
            node_id: Uuid::new_v4(),
            started_at,
            finished_at: started_at,
            recorded_at: None,
            workspace_id: Some(workspace_id),
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
        }
    }

    fn filter(workspace_id: Uuid) -> AiUsageFilter {
        let now = Utc::now();
        AiUsageFilter {
            workspace_id,
            start: super::super::cursor::normalize_millis(now - Duration::hours(1)),
            end: super::super::cursor::normalize_millis(now + Duration::hours(1)),
            request_id: None,
            route_id: None,
            service_id: None,
            provider_id: None,
            provider_type: None,
            requested_model: None,
            model_group: None,
            actual_model: None,
            virtual_key_id: None,
            consumer_id: None,
            status_code: None,
            outcome: None,
            stream: None,
            cache_status: None,
            usage_source: None,
            pricing_status: None,
            cost_status: None,
        }
    }

    #[tokio::test]
    async fn eviction_invalidates_existing_snapshot() {
        let workspace_id = Uuid::nil();
        let store = MemoryAiUsageStore::new(Uuid::new_v4(), 1).unwrap();
        store
            .insert_batch(&[fact("00000000000000000000000000000001", workspace_id)])
            .await
            .unwrap();
        let filter = filter(workspace_id);
        let snapshot = store.snapshot(&filter).await.unwrap();
        store
            .insert_batch(&[fact("00000000000000000000000000000002", workspace_id)])
            .await
            .unwrap();
        let error = store
            .list(&AiUsageListQuery {
                filter,
                snapshot,
                offset: None,
                size: 100,
            })
            .await
            .unwrap_err();
        assert!(matches!(error, AiUsageError::SnapshotExpired(_)));
    }

    #[tokio::test]
    async fn invalid_snapshot_is_rejected_before_eviction_state() {
        let workspace_id = Uuid::nil();
        let store = MemoryAiUsageStore::new(Uuid::new_v4(), 1).unwrap();
        let filter = filter(workspace_id);
        let mut snapshot = store.snapshot(&filter).await.unwrap();
        snapshot.backend = AiUsageMode::Postgres;
        snapshot.ring_instance_id = None;
        let error = store
            .list(&AiUsageListQuery {
                filter,
                snapshot,
                offset: None,
                size: 100,
            })
            .await
            .unwrap_err();
        assert!(matches!(error, AiUsageError::InvalidQuery(_)));
    }

    #[tokio::test]
    async fn snapshot_from_restarted_store_expires_before_watermark_validation() {
        let workspace_id = Uuid::nil();
        let previous_store = MemoryAiUsageStore::new(Uuid::new_v4(), 10).unwrap();
        previous_store
            .insert_batch(&[fact("00000000000000000000000000000001", workspace_id)])
            .await
            .unwrap();
        let filter = filter(workspace_id);
        let snapshot = previous_store.snapshot(&filter).await.unwrap();

        let restarted_store = MemoryAiUsageStore::new(Uuid::new_v4(), 10).unwrap();
        let error = restarted_store
            .list(&AiUsageListQuery {
                filter,
                snapshot,
                offset: None,
                size: 100,
            })
            .await
            .unwrap_err();

        assert!(matches!(error, AiUsageError::SnapshotExpired(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn query_deadline_includes_ring_lock_wait() {
        let workspace_id = Uuid::nil();
        let store = MemoryAiUsageStore::new(Uuid::new_v4(), 10).unwrap();
        let filter = filter(workspace_id);
        let snapshot = store.snapshot(&filter).await.unwrap();
        let guard = store.state.write().await;
        let query_store = store.clone();
        let task = tokio::spawn(async move {
            query_store
                .list(&AiUsageListQuery {
                    filter,
                    snapshot,
                    offset: None,
                    size: 100,
                })
                .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(QUERY_TIMEOUT + std::time::Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        let error = task.await.unwrap().unwrap_err();
        drop(guard);
        assert!(matches!(error, AiUsageError::QueryTimeout(_)));
    }

    #[test]
    fn constructor_rejects_unbounded_ring_capacity() {
        assert!(MemoryAiUsageStore::new(Uuid::new_v4(), MAX_DBLESS_CAPACITY + 1,).is_err());
    }
}

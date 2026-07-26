//! PostgreSQL usage 事实批写、稳定分页与汇总 Store。

use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use num_bigint::BigInt;
use rust_decimal::{Decimal, RoundingStrategy};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder, Transaction};

use super::cursor::{
    encode_offset, encode_snapshot, filter_hash, validate_offset, validate_snapshot, CURSOR_VERSION,
};
use super::model::{
    AggregateMetrics, AiUsageBreakdown, AiUsageError, AiUsageFact, AiUsageListQuery, AiUsageMeta,
    AiUsageMode, AiUsageOffset, AiUsagePage, AiUsageRecord, AiUsageResult, AiUsageSnapshot,
    AiUsageSummary, AiUsageSummaryQuery, BatchWriteResult, BreakdownItem, BreakdownType,
    CostStatusCounts, DimensionRef, OutcomeCounts, PriceSnapshot, PricingStatusCounts,
    SummaryOrder, TokenAggregate, TokenField,
};
use super::store::{AiUsageStore, TimeBucketPlan};

const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct PgAiUsageStore {
    pool: PgPool,
}

impl PgAiUsageStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    async fn current_high_watermark(
        transaction: &mut Transaction<'_, Postgres>,
    ) -> AiUsageResult<i64> {
        let value: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(ingest_seq), 0) FROM ai_usage_logs")
                .fetch_one(&mut **transaction)
                .await?;
        Ok(value)
    }

    async fn begin_read(&self) -> AiUsageResult<Transaction<'_, Postgres>> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("SET LOCAL statement_timeout = '5s'")
            .execute(&mut *transaction)
            .await?;
        Ok(transaction)
    }
}

#[async_trait]
impl AiUsageStore for PgAiUsageStore {
    fn mode(&self) -> AiUsageMode {
        AiUsageMode::Postgres
    }

    async fn insert_batch(&self, rows: &[AiUsageFact]) -> AiUsageResult<BatchWriteResult> {
        if rows.is_empty() {
            return Ok(BatchWriteResult::default());
        }
        let pool = self.pool.clone();
        let operation = async move {
            let mut transaction = pool.begin().await?;
            sqlx::query("SET LOCAL statement_timeout = '2s'")
                .execute(&mut *transaction)
                .await?;
            sqlx::query(
                "SELECT pg_advisory_xact_lock(\
                 hashtextextended(current_database() || ':' || current_schema() || ':ai_usage_logs', 0))",
            )
            .execute(&mut *transaction)
            .await?;

            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO ai_usage_logs (\
                 id, request_id, node_id, started_at, finished_at, workspace_id, \
                 route_id, route_name, service_id, service_name, provider_id, provider_name, \
                 provider_type, model_id, requested_model, model_group, actual_model, \
                 attempt_count, virtual_key_id, virtual_key_name, virtual_key_prefix, consumer_id, \
                 prompt_tokens, completion_tokens, total_tokens, reasoning_tokens, \
                 cache_read_input_tokens, cache_write_input_tokens, prompt_tokens_source, \
                 completion_tokens_source, total_tokens_source, usage_source, \
                 usage_unavailable_reasons, input_price_per_million, input_price_source, \
                 input_price_version, input_price_snapshot_date, input_price_effective_from, \
                 input_price_effective_to, output_price_per_million, output_price_source, \
                 output_price_version, output_price_snapshot_date, output_price_effective_from, \
                 output_price_effective_to, pricing_status, pricing_unsupported_reasons, \
                 cost_usd, cost_status, cost_unavailable_reasons, status_code, \
                 upstream_status_code, outcome, e2e_ms, ttft_ms, upstream_attempted, stream, \
                 cache_status) ",
            );
            query.push_values(rows, |mut values, fact| {
                let input = fact.input_price.as_ref();
                let output = fact.output_price.as_ref();
                values
                    .push_bind(fact.id)
                    .push_bind(&fact.request_id)
                    .push_bind(fact.node_id)
                    .push_bind(fact.started_at)
                    .push_bind(fact.finished_at)
                    .push_bind(fact.workspace_id)
                    .push_bind(fact.route_id)
                    .push_bind(&fact.route_name)
                    .push_bind(fact.service_id)
                    .push_bind(&fact.service_name)
                    .push_bind(fact.provider_id)
                    .push_bind(&fact.provider_name)
                    .push_bind(&fact.provider_type)
                    .push_bind(fact.model_id)
                    .push_bind(&fact.requested_model)
                    .push_bind(&fact.model_group)
                    .push_bind(&fact.actual_model)
                    .push_bind(fact.attempt_count)
                    .push_bind(fact.virtual_key_id)
                    .push_bind(&fact.virtual_key_name)
                    .push_bind(&fact.virtual_key_prefix)
                    .push_bind(fact.consumer_id)
                    .push_bind(fact.prompt_tokens.map(|field| field.value))
                    .push_bind(fact.completion_tokens.map(|field| field.value))
                    .push_bind(fact.total_tokens.map(|field| field.value))
                    .push_bind(fact.reasoning_tokens)
                    .push_bind(fact.cache_read_input_tokens)
                    .push_bind(fact.cache_write_input_tokens)
                    .push_bind(fact.prompt_tokens.map(|field| field.source.as_str()))
                    .push_bind(fact.completion_tokens.map(|field| field.source.as_str()))
                    .push_bind(fact.total_tokens.map(|field| field.source.as_str()))
                    .push_bind(fact.usage_source.as_str())
                    .push_bind(&fact.usage_unavailable_reasons)
                    .push_bind(input.map(|price| price.usd_per_million))
                    .push_bind(input.map(|price| price.source.as_str()))
                    .push_bind(input.map(|price| price.version.as_str()))
                    .push_bind(input.map(|price| price.snapshot_date))
                    .push_bind(input.map(|price| price.effective_from))
                    .push_bind(input.and_then(|price| price.effective_to))
                    .push_bind(output.map(|price| price.usd_per_million))
                    .push_bind(output.map(|price| price.source.as_str()))
                    .push_bind(output.map(|price| price.version.as_str()))
                    .push_bind(output.map(|price| price.snapshot_date))
                    .push_bind(output.map(|price| price.effective_from))
                    .push_bind(output.and_then(|price| price.effective_to))
                    .push_bind(fact.pricing_status.as_str())
                    .push_bind(&fact.pricing_unsupported_reasons)
                    .push_bind(fact.cost_usd)
                    .push_bind(fact.cost_status.as_str())
                    .push_bind(&fact.cost_unavailable_reasons)
                    .push_bind(fact.status_code)
                    .push_bind(fact.upstream_status_code)
                    .push_bind(fact.outcome.as_str())
                    .push_bind(fact.e2e_ms)
                    .push_bind(fact.ttft_ms)
                    .push_bind(fact.upstream_attempted)
                    .push_bind(fact.stream)
                    .push_bind(fact.cache_status.as_str());
            });
            query.push(" ON CONFLICT (request_id) DO NOTHING");
            let inserted = query
                .build()
                .execute(&mut *transaction)
                .await?
                .rows_affected();
            if let Err(error) = transaction.commit().await {
                let sqlstate = error
                    .as_database_error()
                    .and_then(|database_error| database_error.code())
                    .map(|code| code.into_owned());
                let outcome_unknown = sqlstate.as_deref().is_none_or(|code| {
                    code.starts_with("08") || matches!(code, "57P01" | "57P02" | "57P03")
                });
                if outcome_unknown {
                    return Err(AiUsageError::WriteOutcomeUnknown(format!(
                        "AI usage commit outcome is unknown: {error}"
                    )));
                }
                return Err(AiUsageError::from(error));
            }
            Ok::<_, AiUsageError>(BatchWriteResult {
                inserted,
                duplicate: rows.len() as u64 - inserted,
            })
        };
        tokio::time::timeout(WRITE_TIMEOUT, operation)
            .await
            .map_err(|_| {
                AiUsageError::WriteOutcomeUnknown(
                    "AI usage batch write timed out; commit outcome is unknown".to_string(),
                )
            })?
    }

    async fn snapshot(
        &self,
        filter: &super::model::AiUsageFilter,
    ) -> AiUsageResult<AiUsageSnapshot> {
        tokio::time::timeout(QUERY_TIMEOUT, async {
            let mut transaction = self.begin_read().await?;
            let high_watermark = Self::current_high_watermark(&mut transaction).await?;
            transaction.commit().await?;
            Ok(AiUsageSnapshot {
                v: CURSOR_VERSION,
                backend: AiUsageMode::Postgres,
                workspace_id: filter.workspace_id,
                start: filter.start,
                end: filter.end,
                high_watermark,
                eviction_generation: None,
                ring_instance_id: None,
                filter_hash: filter_hash(filter)?,
            })
        })
        .await
        .map_err(|_| AiUsageError::QueryTimeout("Analytics query timed out".to_string()))?
    }

    async fn list(&self, query: &AiUsageListQuery) -> AiUsageResult<AiUsagePage> {
        tokio::time::timeout(QUERY_TIMEOUT, async {
            let mut transaction = self.begin_read().await?;
            let current_high_watermark = Self::current_high_watermark(&mut transaction).await?;
            validate_snapshot(
                &query.snapshot,
                &query.filter,
                AiUsageMode::Postgres,
                current_high_watermark,
            )?;
            if let Some(offset) = &query.offset {
                validate_offset(offset, &query.snapshot)?;
            }
            let mut sql = QueryBuilder::<Postgres>::new(format!(
                "SELECT {} FROM ai_usage_logs WHERE workspace_id = ",
                SELECT_COLUMNS
            ));
            push_filters(&mut sql, query);
            sql.push(" ORDER BY started_at DESC, id DESC LIMIT ")
                .push_bind((query.size + 1) as i64);
            let mut facts: Vec<_> = sql
                .build_query_as::<UsageRow>()
                .fetch_all(&mut *transaction)
                .await?
                .into_iter()
                .map(UsageRow::into_fact)
                .collect::<AiUsageResult<Vec<_>>>()?;
            transaction.commit().await?;

            let has_more = facts.len() > query.size;
            facts.truncate(query.size);
            let offset = if has_more {
                facts
                    .last()
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
                data: facts.iter().map(AiUsageRecord::from).collect(),
                offset,
                next: None,
                snapshot: encode_snapshot(&query.snapshot)?,
                meta: postgres_meta(),
            })
        })
        .await
        .map_err(|_| AiUsageError::QueryTimeout("Analytics query timed out".to_string()))?
    }

    async fn summary(&self, query: &AiUsageSummaryQuery) -> AiUsageResult<AiUsageSummary> {
        tokio::time::timeout(QUERY_TIMEOUT, async {
            let mut transaction = self.begin_read().await?;
            let current_high_watermark = Self::current_high_watermark(&mut transaction).await?;
            validate_snapshot(
                &query.snapshot,
                &query.filter,
                AiUsageMode::Postgres,
                current_high_watermark,
            )?;
            let totals = fetch_aggregate(&mut transaction, query).await?;
            let breakdown = match query.breakdown {
                None => None,
                Some(kind) if kind.is_time() => {
                    Some(fetch_time_breakdown(&mut transaction, query, kind).await?)
                }
                Some(kind) => Some(fetch_category_breakdown(&mut transaction, query, kind).await?),
            };
            transaction.commit().await?;
            Ok(AiUsageSummary {
                snapshot: encode_snapshot(&query.snapshot)?,
                meta: postgres_meta(),
                totals,
                breakdown,
            })
        })
        .await
        .map_err(|_| AiUsageError::QueryTimeout("Analytics query timed out".to_string()))?
    }
}

async fn fetch_aggregate(
    transaction: &mut Transaction<'_, Postgres>,
    query: &AiUsageSummaryQuery,
) -> AiUsageResult<AggregateMetrics> {
    let mut sql = filtered_query(query);
    sql.push(") SELECT ")
        .push(AGGREGATE_SELECT)
        .push(" FROM filtered");
    sql.build_query_as::<AggregateSqlRow>()
        .fetch_one(&mut **transaction)
        .await?
        .into_metrics()
}

async fn fetch_time_breakdown(
    transaction: &mut Transaction<'_, Postgres>,
    query: &AiUsageSummaryQuery,
    breakdown_type: BreakdownType,
) -> AiUsageResult<AiUsageBreakdown> {
    let timezone = query.timezone.unwrap_or(chrono_tz::UTC);
    let plan = TimeBucketPlan::new(
        query.filter.start,
        query.filter.end,
        timezone,
        breakdown_type,
    )?;
    let starts: Vec<_> = plan.buckets.iter().map(|bucket| bucket.start).collect();
    let mut sql = filtered_query(query);
    sql.push(
        "), bucketed AS MATERIALIZED (\
         SELECT filtered.*, width_bucket(started_at, ",
    )
    .push_bind(starts)
    .push(
        "::timestamptz[]) AS bucket_ordinal FROM filtered\
         ) SELECT bucket_ordinal, ",
    )
    .push(AGGREGATE_SELECT)
    .push(
        " FROM bucketed \
         GROUP BY bucket_ordinal \
         ORDER BY bucket_ordinal",
    );
    let rows = sql
        .build_query_as::<TimeAggregateSqlRow>()
        .fetch_all(&mut **transaction)
        .await?;
    let mut metrics_by_bucket = HashMap::with_capacity(rows.len());
    for row in rows {
        if row.bucket_ordinal < 1 || row.bucket_ordinal as usize > plan.buckets.len() {
            return Err(AiUsageError::Internal(format!(
                "数据库返回无效的时间 bucket ordinal: {}",
                row.bucket_ordinal
            )));
        }
        metrics_by_bucket.insert(row.bucket_ordinal, row.metrics.into_metrics()?);
    }
    let items = plan
        .buckets
        .into_iter()
        .enumerate()
        .map(|(index, bucket)| BreakdownItem {
            key: Some(bucket.start.to_rfc3339()),
            label: Some(bucket.label),
            is_other: false,
            bucket_start: Some(bucket.start),
            bucket_end: Some(bucket.end),
            dimension: None,
            metrics: metrics_by_bucket
                .remove(&((index + 1) as i32))
                .unwrap_or_default(),
        })
        .collect();
    Ok(AiUsageBreakdown {
        breakdown_type,
        timezone: Some(timezone.name().to_string()),
        order_by: None,
        limit: None,
        items,
        other: None,
    })
}

async fn fetch_category_breakdown(
    transaction: &mut Transaction<'_, Postgres>,
    query: &AiUsageSummaryQuery,
    breakdown_type: BreakdownType,
) -> AiUsageResult<AiUsageBreakdown> {
    let spec = CategorySqlSpec::for_type(breakdown_type)?;
    let order_by = query.order_by.unwrap_or_default();
    let limit = query.limit.unwrap_or(10);
    let order_metric = match order_by {
        SummaryOrder::CostUsd => {
            "COALESCE(SUM(cost_usd) FILTER (WHERE cost_status IN \
             ('calculated', 'estimated')), 0::numeric)"
        }
        SummaryOrder::TotalTokens => {
            "COALESCE(SUM(total_tokens) FILTER (WHERE upstream_attempted \
             AND total_tokens IS NOT NULL), 0::numeric)"
        }
        SummaryOrder::Requests => "COUNT(*)::numeric",
    };
    let key_expression = format!(
        "CASE \
         WHEN group_id IS NOT NULL THEN 'id:' || group_id::text \
         WHEN group_value1 IS NULL AND group_value2 IS NULL THEN NULL \
         ELSE 'snapshot:' || encode(sha256(convert_to(\
         '[\"{}\",[' || {} || ']]', 'UTF8')), 'hex') \
         END",
        spec.kind, spec.canonical_values
    );
    let mut sql = filtered_query(query);
    sql.push(format!(
        "), category_values AS MATERIALIZED (\
         SELECT filtered.*, {} AS group_id, {} AS group_value1, \
         {} AS group_value2 FROM filtered\
         ), categorized AS MATERIALIZED (\
         SELECT category_values.*, {} AS group_key FROM category_values\
         ), grouped AS (\
         SELECT group_id, group_value1, group_value2, group_key, \
         {} AS label, group_id AS dimension_id, {} AS dimension_name, \
         {} AS dimension_type, {} AS dimension_prefix, ",
        spec.id,
        spec.value1,
        spec.value2,
        key_expression,
        spec.latest_name,
        spec.latest_name,
        spec.latest_type,
        spec.latest_prefix,
    ))
    .push(AGGREGATE_SELECT)
    .push(format!(
        ", {} AS order_metric \
         FROM categorized \
         GROUP BY group_id, group_value1, group_value2, group_key \
         ), top_groups AS MATERIALIZED (\
         SELECT grouped.*, ROW_NUMBER() OVER (\
         ORDER BY order_metric DESC, requests DESC, group_key ASC NULLS LAST\
         ) AS sort_rank \
         FROM grouped \
         ORDER BY order_metric DESC, requests DESC, group_key ASC NULLS LAST \
         LIMIT ",
        order_metric
    ))
    .push_bind(limit as i64)
    .push(
        "), other_metrics AS (\
         SELECT ",
    )
    .push(AGGREGATE_SELECT)
    .push(
        " FROM categorized \
         WHERE NOT EXISTS (\
         SELECT 1 FROM top_groups \
         WHERE categorized.group_key IS NOT DISTINCT FROM top_groups.group_key\
         ) \
         HAVING COUNT(*) > 0 \
         ) \
         SELECT FALSE AS is_other, sort_rank, group_key AS key, label, \
         dimension_id, dimension_name, dimension_type, dimension_prefix, ",
    )
    .push(AGGREGATE_COLUMNS)
    .push(
        " FROM top_groups \
         UNION ALL \
         SELECT TRUE AS is_other, 9223372036854775807::bigint AS sort_rank, \
         NULL::text AS key, 'Other'::text AS label, NULL::uuid AS dimension_id, \
         NULL::text AS dimension_name, NULL::text AS dimension_type, \
         NULL::text AS dimension_prefix, ",
    )
    .push(AGGREGATE_COLUMNS)
    .push(
        " FROM other_metrics \
         ORDER BY sort_rank",
    );
    let rows = sql
        .build_query_as::<CategoryAggregateSqlRow>()
        .fetch_all(&mut **transaction)
        .await?;
    let mut items = Vec::with_capacity(rows.len().min(limit));
    let mut other = None;
    for row in rows {
        let dimension = DimensionRef {
            id: row.dimension_id,
            name: row.dimension_name,
            dimension_type: row.dimension_type,
            prefix: row.dimension_prefix,
        };
        let item = BreakdownItem {
            key: row.key,
            label: row.label,
            is_other: row.is_other,
            bucket_start: None,
            bucket_end: None,
            dimension: (!row.is_other
                && (dimension.id.is_some()
                    || dimension.name.is_some()
                    || dimension.dimension_type.is_some()
                    || dimension.prefix.is_some()))
            .then_some(dimension),
            metrics: row.metrics.into_metrics()?,
        };
        if row.is_other {
            other = Some(item);
        } else {
            items.push(item);
        }
    }
    Ok(AiUsageBreakdown {
        breakdown_type,
        timezone: None,
        order_by: Some(order_by),
        limit: Some(limit),
        items,
        other,
    })
}

fn filtered_query<'args>(query: &'args AiUsageSummaryQuery) -> QueryBuilder<'args, Postgres> {
    let mut sql = QueryBuilder::<Postgres>::new(
        "WITH filtered AS MATERIALIZED (\
         SELECT * FROM ai_usage_logs WHERE workspace_id = ",
    );
    push_summary_filters(&mut sql, query);
    sql
}

const AGGREGATE_SELECT: &str = "\
COUNT(*)::bigint AS requests, \
COUNT(*) FILTER (WHERE outcome = 'success')::bigint AS outcome_success, \
COUNT(*) FILTER (WHERE outcome = 'gateway_rejected')::bigint AS outcome_gateway_rejected, \
COUNT(*) FILTER (WHERE outcome = 'gateway_error')::bigint AS outcome_gateway_error, \
COUNT(*) FILTER (WHERE outcome = 'upstream_error')::bigint AS outcome_upstream_error, \
COUNT(*) FILTER (WHERE outcome = 'client_disconnected')::bigint AS outcome_client_disconnected, \
COUNT(*) FILTER (WHERE outcome = 'stream_interrupted')::bigint AS outcome_stream_interrupted, \
COALESCE(SUM(prompt_tokens) FILTER (WHERE upstream_attempted AND prompt_tokens IS NOT NULL), \
0::numeric)::text AS prompt_known_sum, \
COUNT(prompt_tokens) FILTER (WHERE upstream_attempted)::bigint AS prompt_known_requests, \
COALESCE(SUM(completion_tokens) FILTER (WHERE upstream_attempted AND completion_tokens IS NOT NULL), \
0::numeric)::text AS completion_known_sum, \
COUNT(completion_tokens) FILTER (WHERE upstream_attempted)::bigint AS completion_known_requests, \
COALESCE(SUM(total_tokens) FILTER (WHERE upstream_attempted AND total_tokens IS NOT NULL), \
0::numeric)::text AS total_known_sum, \
COUNT(total_tokens) FILTER (WHERE upstream_attempted)::bigint AS total_known_requests, \
COUNT(*) FILTER (WHERE upstream_attempted)::bigint AS attempted_requests, \
COALESCE(SUM(cost_usd) FILTER (WHERE cost_status IN ('calculated', 'estimated')), \
0::numeric)::text AS cost_usd_calculable_sum, \
COUNT(*) FILTER (WHERE pricing_status = 'matched')::bigint AS pricing_matched, \
COUNT(*) FILTER (WHERE pricing_status = 'unmatched')::bigint AS pricing_unmatched, \
COUNT(*) FILTER (WHERE pricing_status = 'unsupported')::bigint AS pricing_unsupported, \
COUNT(*) FILTER (WHERE pricing_status = 'not_applicable')::bigint AS pricing_not_applicable, \
COUNT(*) FILTER (WHERE cost_status = 'calculated')::bigint AS cost_calculated, \
COUNT(*) FILTER (WHERE cost_status = 'estimated')::bigint AS cost_estimated, \
COUNT(*) FILTER (WHERE cost_status = 'not_incurred')::bigint AS cost_not_incurred, \
COUNT(*) FILTER (WHERE cost_status = 'unavailable')::bigint AS cost_unavailable, \
CASE WHEN COUNT(*) FILTER (WHERE upstream_attempted AND \
(prompt_tokens IS NOT NULL OR completion_tokens IS NOT NULL OR total_tokens IS NOT NULL)) = 0 \
THEN NULL ELSE ROUND(\
(COUNT(*) FILTER (WHERE upstream_attempted AND usage_source IN ('estimated', 'mixed') AND \
(prompt_tokens IS NOT NULL OR completion_tokens IS NOT NULL OR total_tokens IS NOT NULL)))::numeric / \
(COUNT(*) FILTER (WHERE upstream_attempted AND \
(prompt_tokens IS NOT NULL OR completion_tokens IS NOT NULL OR total_tokens IS NOT NULL)))::numeric, \
6)::text END AS estimated_usage_ratio, \
CASE WHEN COUNT(*) FILTER (WHERE upstream_attempted) = 0 THEN NULL ELSE ROUND(\
(COUNT(*) FILTER (WHERE upstream_attempted AND pricing_status = 'matched'))::numeric / \
(COUNT(*) FILTER (WHERE upstream_attempted))::numeric, 6)::text \
END AS pricing_coverage, \
CASE WHEN COUNT(*) FILTER (WHERE upstream_attempted) = 0 THEN NULL ELSE ROUND(\
(COUNT(*) FILTER (WHERE upstream_attempted AND \
cost_status IN ('calculated', 'estimated')))::numeric / \
(COUNT(*) FILTER (WHERE upstream_attempted))::numeric, 6)::text \
END AS cost_calculable_coverage, \
ROUND(AVG(e2e_ms)::numeric, 3)::text AS avg_e2e_ms, \
ROUND((percentile_cont(0.95) WITHIN GROUP (ORDER BY e2e_ms))::numeric, 3)::text \
AS p95_e2e_ms, \
ROUND(AVG(ttft_ms)::numeric, 3)::text AS avg_ttft_ms, \
COUNT(*) FILTER (WHERE cache_status = 'hit')::bigint AS cache_hits";

const AGGREGATE_COLUMNS: &str = "\
requests, outcome_success, outcome_gateway_rejected, outcome_gateway_error, \
outcome_upstream_error, outcome_client_disconnected, outcome_stream_interrupted, \
prompt_known_sum, prompt_known_requests, completion_known_sum, \
completion_known_requests, total_known_sum, total_known_requests, attempted_requests, \
cost_usd_calculable_sum, pricing_matched, pricing_unmatched, pricing_unsupported, \
pricing_not_applicable, cost_calculated, cost_estimated, cost_not_incurred, \
cost_unavailable, estimated_usage_ratio, pricing_coverage, \
cost_calculable_coverage, avg_e2e_ms, p95_e2e_ms, avg_ttft_ms, cache_hits";

#[derive(FromRow)]
struct AggregateSqlRow {
    requests: i64,
    outcome_success: i64,
    outcome_gateway_rejected: i64,
    outcome_gateway_error: i64,
    outcome_upstream_error: i64,
    outcome_client_disconnected: i64,
    outcome_stream_interrupted: i64,
    prompt_known_sum: String,
    prompt_known_requests: i64,
    completion_known_sum: String,
    completion_known_requests: i64,
    total_known_sum: String,
    total_known_requests: i64,
    attempted_requests: i64,
    cost_usd_calculable_sum: String,
    pricing_matched: i64,
    pricing_unmatched: i64,
    pricing_unsupported: i64,
    pricing_not_applicable: i64,
    cost_calculated: i64,
    cost_estimated: i64,
    cost_not_incurred: i64,
    cost_unavailable: i64,
    estimated_usage_ratio: Option<String>,
    pricing_coverage: Option<String>,
    cost_calculable_coverage: Option<String>,
    avg_e2e_ms: Option<String>,
    p95_e2e_ms: Option<String>,
    avg_ttft_ms: Option<String>,
    cache_hits: i64,
}

impl AggregateSqlRow {
    fn into_metrics(self) -> AiUsageResult<AggregateMetrics> {
        let requests = sql_count(self.requests, "requests")?;
        let successful_requests = sql_count(self.outcome_success, "successful_requests")?;
        let attempted_requests = sql_count(self.attempted_requests, "attempted_requests")?;
        let prompt_known_requests = sql_count(self.prompt_known_requests, "prompt_known_requests")?;
        let completion_known_requests =
            sql_count(self.completion_known_requests, "completion_known_requests")?;
        let total_known_requests = sql_count(self.total_known_requests, "total_known_requests")?;
        Ok(AggregateMetrics {
            requests,
            successful_requests,
            failed_requests: requests.checked_sub(successful_requests).ok_or_else(|| {
                AiUsageError::Internal("数据库 successful_requests 超过 requests".to_string())
            })?,
            outcomes: OutcomeCounts {
                success: successful_requests,
                gateway_rejected: sql_count(
                    self.outcome_gateway_rejected,
                    "outcome.gateway_rejected",
                )?,
                gateway_error: sql_count(self.outcome_gateway_error, "outcome.gateway_error")?,
                upstream_error: sql_count(self.outcome_upstream_error, "outcome.upstream_error")?,
                client_disconnected: sql_count(
                    self.outcome_client_disconnected,
                    "outcome.client_disconnected",
                )?,
                stream_interrupted: sql_count(
                    self.outcome_stream_interrupted,
                    "outcome.stream_interrupted",
                )?,
            },
            prompt_tokens: TokenAggregate {
                known_sum: sql_integer(&self.prompt_known_sum, "prompt_known_sum")?,
                known_requests: prompt_known_requests,
                unknown_requests: attempted_requests
                    .checked_sub(prompt_known_requests)
                    .ok_or_else(|| {
                        AiUsageError::Internal(
                            "数据库 prompt known_requests 超过 attempted_requests".to_string(),
                        )
                    })?,
                coverage: ratio_from_counts(prompt_known_requests, attempted_requests),
            },
            completion_tokens: TokenAggregate {
                known_sum: sql_integer(&self.completion_known_sum, "completion_known_sum")?,
                known_requests: completion_known_requests,
                unknown_requests: attempted_requests
                    .checked_sub(completion_known_requests)
                    .ok_or_else(|| {
                        AiUsageError::Internal(
                            "数据库 completion known_requests 超过 attempted_requests".to_string(),
                        )
                    })?,
                coverage: ratio_from_counts(completion_known_requests, attempted_requests),
            },
            total_tokens: TokenAggregate {
                known_sum: sql_integer(&self.total_known_sum, "total_known_sum")?,
                known_requests: total_known_requests,
                unknown_requests: attempted_requests
                    .checked_sub(total_known_requests)
                    .ok_or_else(|| {
                        AiUsageError::Internal(
                            "数据库 total known_requests 超过 attempted_requests".to_string(),
                        )
                    })?,
                coverage: ratio_from_counts(total_known_requests, attempted_requests),
            },
            cost_usd_calculable_sum: sql_fixed_decimal(
                &self.cost_usd_calculable_sum,
                12,
                "cost_usd_calculable_sum",
            )?,
            pricing_status: PricingStatusCounts {
                matched: sql_count(self.pricing_matched, "pricing.matched")?,
                unmatched: sql_count(self.pricing_unmatched, "pricing.unmatched")?,
                unsupported: sql_count(self.pricing_unsupported, "pricing.unsupported")?,
                not_applicable: sql_count(self.pricing_not_applicable, "pricing.not_applicable")?,
            },
            cost_status: CostStatusCounts {
                calculated: sql_count(self.cost_calculated, "cost.calculated")?,
                estimated: sql_count(self.cost_estimated, "cost.estimated")?,
                not_incurred: sql_count(self.cost_not_incurred, "cost.not_incurred")?,
                unavailable: sql_count(self.cost_unavailable, "cost.unavailable")?,
            },
            estimated_usage_ratio: sql_optional_fixed_decimal(
                self.estimated_usage_ratio.as_deref(),
                6,
                "estimated_usage_ratio",
            )?,
            pricing_coverage: sql_optional_fixed_decimal(
                self.pricing_coverage.as_deref(),
                6,
                "pricing_coverage",
            )?,
            cost_calculable_coverage: sql_optional_fixed_decimal(
                self.cost_calculable_coverage.as_deref(),
                6,
                "cost_calculable_coverage",
            )?,
            avg_e2e_ms: sql_optional_fixed_decimal(self.avg_e2e_ms.as_deref(), 3, "avg_e2e_ms")?,
            p95_e2e_ms: sql_optional_fixed_decimal(self.p95_e2e_ms.as_deref(), 3, "p95_e2e_ms")?,
            avg_ttft_ms: sql_optional_fixed_decimal(self.avg_ttft_ms.as_deref(), 3, "avg_ttft_ms")?,
            cache_hits: sql_count(self.cache_hits, "cache_hits")?,
        })
    }
}

#[derive(FromRow)]
struct TimeAggregateSqlRow {
    bucket_ordinal: i32,
    #[sqlx(flatten)]
    metrics: AggregateSqlRow,
}

#[derive(FromRow)]
struct CategoryAggregateSqlRow {
    is_other: bool,
    key: Option<String>,
    label: Option<String>,
    dimension_id: Option<uuid::Uuid>,
    dimension_name: Option<String>,
    dimension_type: Option<String>,
    dimension_prefix: Option<String>,
    #[sqlx(flatten)]
    metrics: AggregateSqlRow,
}

fn sql_count(value: i64, field: &str) -> AiUsageResult<u64> {
    u64::try_from(value)
        .map_err(|_| AiUsageError::Internal(format!("数据库聚合字段 {field} 为负数")))
}

fn sql_integer(value: &str, field: &str) -> AiUsageResult<String> {
    let value = BigInt::from_str(value).map_err(|error| {
        AiUsageError::Internal(format!("数据库聚合字段 {field} 不是整数: {error}"))
    })?;
    if value < BigInt::from(0u8) {
        return Err(AiUsageError::Internal(format!(
            "数据库聚合字段 {field} 为负数"
        )));
    }
    Ok(value.to_string())
}

fn sql_fixed_decimal(value: &str, scale: i64, field: &str) -> AiUsageResult<String> {
    let value = BigDecimal::from_str(value).map_err(|error| {
        AiUsageError::Internal(format!("数据库聚合字段 {field} 不是十进制数: {error}"))
    })?;
    Ok(value.with_scale(scale).to_string())
}

fn sql_optional_fixed_decimal(
    value: Option<&str>,
    scale: i64,
    field: &str,
) -> AiUsageResult<Option<String>> {
    value
        .map(|value| sql_fixed_decimal(value, scale, field))
        .transpose()
}

fn ratio_from_counts(numerator: u64, denominator: u64) -> Option<String> {
    if denominator == 0 {
        return None;
    }
    Decimal::from(numerator)
        .checked_div(Decimal::from(denominator))
        .map(|value| {
            format!(
                "{:.6}",
                value.round_dp_with_strategy(6, RoundingStrategy::MidpointAwayFromZero,)
            )
        })
}

struct CategorySqlSpec {
    kind: &'static str,
    id: &'static str,
    value1: &'static str,
    value2: &'static str,
    canonical_values: &'static str,
    latest_name: &'static str,
    latest_type: &'static str,
    latest_prefix: &'static str,
}

impl CategorySqlSpec {
    fn for_type(breakdown_type: BreakdownType) -> AiUsageResult<Self> {
        let latest = |column| match column {
            "provider_name" => "(ARRAY_AGG(provider_name ORDER BY started_at DESC, id DESC))[1]",
            "provider_type" => "(ARRAY_AGG(provider_type ORDER BY started_at DESC, id DESC))[1]",
            "actual_model" => "(ARRAY_AGG(actual_model ORDER BY started_at DESC, id DESC))[1]",
            "model_group" => "(ARRAY_AGG(model_group ORDER BY started_at DESC, id DESC))[1]",
            "virtual_key_name" => {
                "(ARRAY_AGG(virtual_key_name ORDER BY started_at DESC, id DESC))[1]"
            }
            "virtual_key_prefix" => {
                "(ARRAY_AGG(virtual_key_prefix ORDER BY started_at DESC, id DESC))[1]"
            }
            "route_name" => "(ARRAY_AGG(route_name ORDER BY started_at DESC, id DESC))[1]",
            "service_name" => "(ARRAY_AGG(service_name ORDER BY started_at DESC, id DESC))[1]",
            _ => "NULL::text",
        };
        Ok(match breakdown_type {
            BreakdownType::Provider => Self {
                kind: "provider",
                id: "provider_id",
                value1: "CASE WHEN provider_id IS NULL THEN provider_name ELSE NULL::text END",
                value2: "CASE WHEN provider_id IS NULL THEN provider_type ELSE NULL::text END",
                canonical_values: "COALESCE(to_jsonb(group_value1)::text, 'null') || ',' || COALESCE(to_jsonb(group_value2)::text, 'null')",
                latest_name: latest("provider_name"),
                latest_type: latest("provider_type"),
                latest_prefix: "NULL::text",
            },
            BreakdownType::ActualModel => Self {
                kind: "actual_model",
                id: "NULL::uuid",
                value1: "provider_type",
                value2: "actual_model",
                canonical_values: "COALESCE(to_jsonb(group_value1)::text, 'null') || ',' || COALESCE(to_jsonb(group_value2)::text, 'null')",
                latest_name: latest("actual_model"),
                latest_type: latest("provider_type"),
                latest_prefix: "NULL::text",
            },
            BreakdownType::ModelGroup => Self {
                kind: "model_group",
                id: "NULL::uuid",
                value1: "model_group",
                value2: "NULL::text",
                canonical_values: "COALESCE(to_jsonb(group_value1)::text, 'null')",
                latest_name: latest("model_group"),
                latest_type: "NULL::text",
                latest_prefix: "NULL::text",
            },
            BreakdownType::VirtualKey => Self {
                kind: "virtual_key",
                id: "virtual_key_id",
                value1:
                    "CASE WHEN virtual_key_id IS NULL THEN virtual_key_name ELSE NULL::text END",
                value2:
                    "CASE WHEN virtual_key_id IS NULL THEN virtual_key_prefix ELSE NULL::text END",
                canonical_values: "COALESCE(to_jsonb(group_value1)::text, 'null') || ',' || COALESCE(to_jsonb(group_value2)::text, 'null')",
                latest_name: latest("virtual_key_name"),
                latest_type: "NULL::text",
                latest_prefix: latest("virtual_key_prefix"),
            },
            BreakdownType::Route => Self {
                kind: "route",
                id: "route_id",
                value1: "CASE WHEN route_id IS NULL THEN route_name ELSE NULL::text END",
                value2: "NULL::text",
                canonical_values: "COALESCE(to_jsonb(group_value1)::text, 'null')",
                latest_name: latest("route_name"),
                latest_type: "NULL::text",
                latest_prefix: "NULL::text",
            },
            BreakdownType::Service => Self {
                kind: "service",
                id: "service_id",
                value1: "CASE WHEN service_id IS NULL THEN service_name ELSE NULL::text END",
                value2: "NULL::text",
                canonical_values: "COALESCE(to_jsonb(group_value1)::text, 'null')",
                latest_name: latest("service_name"),
                latest_type: "NULL::text",
                latest_prefix: "NULL::text",
            },
            BreakdownType::Hour | BreakdownType::Day => {
                return Err(AiUsageError::Internal(
                    "时间 breakdown 不能使用分类 SQL".to_string(),
                ))
            }
        })
    }
}

fn push_filters<'args>(sql: &mut QueryBuilder<'args, Postgres>, query: &'args AiUsageListQuery) {
    push_common_filters(sql, &query.filter, query.snapshot.high_watermark);
    if let Some(offset) = &query.offset {
        sql.push(" AND (started_at, id) < (")
            .push_bind(offset.last_started_at)
            .push(", ")
            .push_bind(offset.last_id)
            .push(")");
    }
}

fn push_summary_filters<'args>(
    sql: &mut QueryBuilder<'args, Postgres>,
    query: &'args AiUsageSummaryQuery,
) {
    push_common_filters(sql, &query.filter, query.snapshot.high_watermark);
}

fn push_common_filters<'args>(
    sql: &mut QueryBuilder<'args, Postgres>,
    filter: &'args super::model::AiUsageFilter,
    high_watermark: i64,
) {
    sql.push_bind(filter.workspace_id)
        .push(" AND ingest_seq <= ")
        .push_bind(high_watermark)
        .push(" AND started_at >= ")
        .push_bind(filter.start)
        .push(" AND started_at < ")
        .push_bind(filter.end);
    macro_rules! optional_filter {
        ($field:expr, $column:literal) => {
            if let Some(value) = $field {
                sql.push(concat!(" AND ", $column, " = ")).push_bind(value);
            }
        };
    }
    optional_filter!(filter.request_id.as_deref(), "request_id");
    optional_filter!(filter.route_id, "route_id");
    optional_filter!(filter.service_id, "service_id");
    optional_filter!(filter.provider_id, "provider_id");
    optional_filter!(filter.provider_type.as_deref(), "provider_type");
    optional_filter!(filter.requested_model.as_deref(), "requested_model");
    optional_filter!(filter.model_group.as_deref(), "model_group");
    optional_filter!(filter.actual_model.as_deref(), "actual_model");
    optional_filter!(filter.virtual_key_id, "virtual_key_id");
    optional_filter!(filter.consumer_id, "consumer_id");
    optional_filter!(filter.status_code, "status_code");
    optional_filter!(filter.outcome.map(|value| value.as_str()), "outcome");
    optional_filter!(filter.stream, "stream");
    optional_filter!(
        filter.cache_status.map(|value| value.as_str()),
        "cache_status"
    );
    optional_filter!(
        filter.usage_source.map(|value| value.as_str()),
        "usage_source"
    );
    optional_filter!(
        filter.pricing_status.map(|value| value.as_str()),
        "pricing_status"
    );
    optional_filter!(
        filter.cost_status.map(|value| value.as_str()),
        "cost_status"
    );
}

fn postgres_meta() -> AiUsageMeta {
    AiUsageMeta {
        mode: AiUsageMode::Postgres,
        ephemeral: false,
        node_id: None,
        capacity: None,
        earliest_available_at: None,
        restart_clears: false,
    }
}

const SELECT_COLUMNS: &str = "\
id, ingest_seq, request_id, node_id, started_at, finished_at, recorded_at, workspace_id, \
route_id, route_name, service_id, service_name, provider_id, provider_name, provider_type, \
model_id, requested_model, model_group, actual_model, attempt_count, virtual_key_id, \
virtual_key_name, virtual_key_prefix, consumer_id, prompt_tokens, completion_tokens, \
total_tokens, reasoning_tokens, cache_read_input_tokens, cache_write_input_tokens, \
prompt_tokens_source, completion_tokens_source, total_tokens_source, usage_source, \
usage_unavailable_reasons, input_price_per_million, input_price_source, input_price_version, \
input_price_snapshot_date, input_price_effective_from, input_price_effective_to, \
output_price_per_million, output_price_source, output_price_version, \
output_price_snapshot_date, output_price_effective_from, output_price_effective_to, \
pricing_status, pricing_unsupported_reasons, cost_usd, cost_status, \
cost_unavailable_reasons, status_code, upstream_status_code, outcome, e2e_ms, ttft_ms, \
upstream_attempted, stream, cache_status";

#[derive(FromRow)]
struct UsageRow {
    id: uuid::Uuid,
    ingest_seq: i64,
    request_id: String,
    node_id: uuid::Uuid,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    recorded_at: DateTime<Utc>,
    workspace_id: Option<uuid::Uuid>,
    route_id: Option<uuid::Uuid>,
    route_name: Option<String>,
    service_id: Option<uuid::Uuid>,
    service_name: Option<String>,
    provider_id: Option<uuid::Uuid>,
    provider_name: Option<String>,
    provider_type: Option<String>,
    model_id: Option<uuid::Uuid>,
    requested_model: Option<String>,
    model_group: Option<String>,
    actual_model: Option<String>,
    attempt_count: i16,
    virtual_key_id: Option<uuid::Uuid>,
    virtual_key_name: Option<String>,
    virtual_key_prefix: Option<String>,
    consumer_id: Option<uuid::Uuid>,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    total_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_write_input_tokens: Option<i64>,
    prompt_tokens_source: Option<String>,
    completion_tokens_source: Option<String>,
    total_tokens_source: Option<String>,
    usage_source: String,
    usage_unavailable_reasons: Vec<String>,
    input_price_per_million: Option<Decimal>,
    input_price_source: Option<String>,
    input_price_version: Option<String>,
    input_price_snapshot_date: Option<NaiveDate>,
    input_price_effective_from: Option<DateTime<Utc>>,
    input_price_effective_to: Option<DateTime<Utc>>,
    output_price_per_million: Option<Decimal>,
    output_price_source: Option<String>,
    output_price_version: Option<String>,
    output_price_snapshot_date: Option<NaiveDate>,
    output_price_effective_from: Option<DateTime<Utc>>,
    output_price_effective_to: Option<DateTime<Utc>>,
    pricing_status: String,
    pricing_unsupported_reasons: Vec<String>,
    cost_usd: Option<Decimal>,
    cost_status: String,
    cost_unavailable_reasons: Vec<String>,
    status_code: Option<i16>,
    upstream_status_code: Option<i16>,
    outcome: String,
    e2e_ms: i64,
    ttft_ms: Option<i64>,
    upstream_attempted: bool,
    stream: Option<bool>,
    cache_status: String,
}

impl UsageRow {
    fn into_fact(self) -> AiUsageResult<AiUsageFact> {
        Ok(AiUsageFact {
            id: self.id,
            ingest_seq: Some(self.ingest_seq),
            request_id: self.request_id,
            node_id: self.node_id,
            started_at: self.started_at,
            finished_at: self.finished_at,
            recorded_at: Some(self.recorded_at),
            workspace_id: self.workspace_id,
            route_id: self.route_id,
            route_name: self.route_name,
            service_id: self.service_id,
            service_name: self.service_name,
            provider_id: self.provider_id,
            provider_name: self.provider_name,
            provider_type: self.provider_type,
            model_id: self.model_id,
            requested_model: self.requested_model,
            model_group: self.model_group,
            actual_model: self.actual_model,
            attempt_count: self.attempt_count,
            virtual_key_id: self.virtual_key_id,
            virtual_key_name: self.virtual_key_name,
            virtual_key_prefix: self.virtual_key_prefix,
            consumer_id: self.consumer_id,
            prompt_tokens: token_field(self.prompt_tokens, self.prompt_tokens_source)?,
            completion_tokens: token_field(self.completion_tokens, self.completion_tokens_source)?,
            total_tokens: token_field(self.total_tokens, self.total_tokens_source)?,
            reasoning_tokens: self.reasoning_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            cache_write_input_tokens: self.cache_write_input_tokens,
            usage_source: parse_enum(&self.usage_source, "usage_source")?,
            usage_unavailable_reasons: self.usage_unavailable_reasons,
            input_price: price_snapshot(
                self.input_price_per_million,
                self.input_price_source,
                self.input_price_version,
                self.input_price_snapshot_date,
                self.input_price_effective_from,
                self.input_price_effective_to,
            )?,
            output_price: price_snapshot(
                self.output_price_per_million,
                self.output_price_source,
                self.output_price_version,
                self.output_price_snapshot_date,
                self.output_price_effective_from,
                self.output_price_effective_to,
            )?,
            pricing_status: parse_enum(&self.pricing_status, "pricing_status")?,
            pricing_unsupported_reasons: self.pricing_unsupported_reasons,
            cost_usd: self.cost_usd,
            cost_status: parse_enum(&self.cost_status, "cost_status")?,
            cost_unavailable_reasons: self.cost_unavailable_reasons,
            status_code: self.status_code,
            upstream_status_code: self.upstream_status_code,
            outcome: parse_enum(&self.outcome, "outcome")?,
            e2e_ms: self.e2e_ms,
            ttft_ms: self.ttft_ms,
            upstream_attempted: self.upstream_attempted,
            stream: self.stream,
            cache_status: parse_enum(&self.cache_status, "cache_status")?,
        })
    }
}

fn token_field(value: Option<i64>, source: Option<String>) -> AiUsageResult<Option<TokenField>> {
    match (value, source) {
        (None, None) => Ok(None),
        (Some(value), Some(source)) => Ok(Some(TokenField {
            value,
            source: parse_enum(&source, "token source")?,
            derived: false,
        })),
        _ => Err(AiUsageError::Internal(
            "数据库 token/source 原子约束被破坏".to_string(),
        )),
    }
}

fn price_snapshot(
    rate: Option<Decimal>,
    source: Option<String>,
    version: Option<String>,
    snapshot_date: Option<NaiveDate>,
    effective_from: Option<DateTime<Utc>>,
    effective_to: Option<DateTime<Utc>>,
) -> AiUsageResult<Option<PriceSnapshot>> {
    match (rate, source, version, snapshot_date, effective_from) {
        (None, None, None, None, None) => Ok(None),
        (Some(rate), Some(source), Some(version), Some(snapshot_date), Some(effective_from)) => {
            Ok(Some(PriceSnapshot {
                usd_per_million: rate,
                source,
                version,
                snapshot_date,
                effective_from,
                effective_to,
            }))
        }
        _ => Err(AiUsageError::Internal(
            "数据库价格 bundle 原子约束被破坏".to_string(),
        )),
    }
}

fn parse_enum<T: FromStr<Err = String>>(value: &str, field: &str) -> AiUsageResult<T> {
    value
        .parse()
        .map_err(|error| AiUsageError::Internal(format!("数据库 {field} 枚举无效: {error}")))
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use sha2::{Digest, Sha256};
    use sqlx::postgres::PgPoolOptions;
    use uuid::Uuid;

    use super::*;
    use crate::usage::model::{
        AiUsageFilter, AiUsageOutcome, CacheStatus, CostStatus, PricingStatus, TokenFieldSource,
        UsageSource,
    };

    #[test]
    fn pg_time_bucket_rejects_chrono_boundary_without_panicking() {
        let end = DateTime::<Utc>::MAX_UTC;
        let start = end.checked_sub_signed(Duration::hours(1)).unwrap();
        assert!(matches!(
            TimeBucketPlan::new(start, end, chrono_tz::UTC, BreakdownType::Hour,),
            Err(AiUsageError::InvalidQuery(_))
        ));
    }

    fn attempted_fact(
        request_id: &str,
        workspace_id: Uuid,
        provider_id: Option<Uuid>,
        provider_name: &str,
        started_at: DateTime<Utc>,
        source: TokenFieldSource,
        usage_source: UsageSource,
        cost_status: CostStatus,
        cost_usd: Decimal,
        e2e_ms: i64,
    ) -> AiUsageFact {
        let price = PriceSnapshot {
            usd_per_million: Decimal::ONE,
            source: "test".to_string(),
            version: "test-v1".to_string(),
            snapshot_date: started_at.date_naive(),
            effective_from: started_at - Duration::days(1),
            effective_to: None,
        };
        AiUsageFact {
            id: Uuid::new_v4(),
            ingest_seq: None,
            request_id: request_id.to_string(),
            node_id: Uuid::new_v4(),
            started_at,
            finished_at: started_at + Duration::milliseconds(e2e_ms),
            recorded_at: None,
            workspace_id: Some(workspace_id),
            route_id: None,
            route_name: None,
            service_id: None,
            service_name: None,
            provider_id,
            provider_name: Some(provider_name.to_string()),
            provider_type: Some("openai".to_string()),
            model_id: None,
            requested_model: Some("gpt-test".to_string()),
            model_group: Some("test".to_string()),
            actual_model: Some("gpt-test".to_string()),
            attempt_count: 1,
            virtual_key_id: None,
            virtual_key_name: None,
            virtual_key_prefix: None,
            consumer_id: None,
            prompt_tokens: Some(TokenField {
                value: 100,
                source,
                derived: false,
            }),
            completion_tokens: Some(TokenField {
                value: 20,
                source,
                derived: false,
            }),
            total_tokens: Some(TokenField {
                value: 120,
                source,
                derived: false,
            }),
            reasoning_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            usage_source,
            usage_unavailable_reasons: Vec::new(),
            input_price: Some(price.clone()),
            output_price: Some(price),
            pricing_status: PricingStatus::Matched,
            pricing_unsupported_reasons: Vec::new(),
            cost_usd: Some(cost_usd),
            cost_status,
            cost_unavailable_reasons: Vec::new(),
            status_code: Some(200),
            upstream_status_code: Some(200),
            outcome: AiUsageOutcome::Success,
            e2e_ms,
            ttft_ms: Some(e2e_ms / 2),
            upstream_attempted: true,
            stream: Some(false),
            cache_status: CacheStatus::Hit,
        }
    }

    fn rejected_fact(
        request_id: &str,
        workspace_id: Uuid,
        started_at: DateTime<Utc>,
    ) -> AiUsageFact {
        AiUsageFact {
            id: Uuid::new_v4(),
            ingest_seq: None,
            request_id: request_id.to_string(),
            node_id: Uuid::new_v4(),
            started_at,
            finished_at: started_at + Duration::milliseconds(50),
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
            pricing_status: PricingStatus::NotApplicable,
            pricing_unsupported_reasons: Vec::new(),
            cost_usd: Some(Decimal::ZERO),
            cost_status: CostStatus::NotIncurred,
            cost_unavailable_reasons: Vec::new(),
            status_code: Some(401),
            upstream_status_code: None,
            outcome: AiUsageOutcome::GatewayRejected,
            e2e_ms: 50,
            ttft_ms: None,
            upstream_attempted: false,
            stream: None,
            cache_status: CacheStatus::NotConfigured,
        }
    }

    fn filter(workspace_id: Uuid, start: DateTime<Utc>, end: DateTime<Utc>) -> AiUsageFilter {
        AiUsageFilter {
            workspace_id,
            start,
            end,
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
    async fn postgres_summary_aggregates_in_database() {
        let Ok(database_url) = std::env::var("KONG_AI_USAGE_PG_TEST_URL") else {
            return;
        };
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let store = PgAiUsageStore::new(pool.clone());
        let workspace_id = Uuid::new_v4();
        let first_provider = Uuid::new_v4();
        let now = Utc::now();
        let first_request_id = Uuid::new_v4().simple().to_string();
        let second_request_id = Uuid::new_v4().simple().to_string();
        let rejected_request_id = Uuid::new_v4().simple().to_string();
        store
            .insert_batch(&[
                attempted_fact(
                    &first_request_id,
                    workspace_id,
                    Some(first_provider),
                    "first",
                    now - Duration::minutes(20),
                    TokenFieldSource::Provider,
                    UsageSource::Provider,
                    CostStatus::Calculated,
                    Decimal::new(1, 3),
                    100,
                ),
                attempted_fact(
                    &second_request_id,
                    workspace_id,
                    None,
                    "second, provider",
                    now - Duration::minutes(10),
                    TokenFieldSource::Estimated,
                    UsageSource::Estimated,
                    CostStatus::Estimated,
                    Decimal::new(2, 3),
                    200,
                ),
                rejected_fact(&rejected_request_id, workspace_id, now),
            ])
            .await
            .unwrap();
        let filter = filter(
            workspace_id,
            super::super::cursor::normalize_millis(now - Duration::hours(2)),
            super::super::cursor::normalize_millis(now + Duration::hours(2)),
        );
        let snapshot = store.snapshot(&filter).await.unwrap();
        let category = store
            .summary(&AiUsageSummaryQuery {
                filter: filter.clone(),
                snapshot: snapshot.clone(),
                breakdown: Some(BreakdownType::Provider),
                timezone: None,
                limit: Some(2),
                order_by: Some(SummaryOrder::Requests),
            })
            .await
            .unwrap();
        assert_eq!(category.totals.requests, 3);
        assert_eq!(category.totals.successful_requests, 2);
        assert_eq!(category.totals.prompt_tokens.known_sum, "200");
        assert_eq!(category.totals.prompt_tokens.known_requests, 2);
        assert_eq!(category.totals.prompt_tokens.unknown_requests, 0);
        assert_eq!(category.totals.cost_usd_calculable_sum, "0.003000000000");
        assert_eq!(category.totals.p95_e2e_ms.as_deref(), Some("190.000"));
        let category = category.breakdown.unwrap();
        let canonical =
            serde_json::to_vec(&("provider", [Some("second, provider"), Some("openai")])).unwrap();
        let expected_snapshot_key = format!("snapshot:{:x}", Sha256::digest(canonical));
        assert_eq!(category.items.len(), 2);
        assert!(category
            .items
            .iter()
            .any(|item| item.key.as_deref() == Some(expected_snapshot_key.as_str())));
        assert_eq!(category.other.as_ref().unwrap().metrics.requests, 1);

        let time = store
            .summary(&AiUsageSummaryQuery {
                filter,
                snapshot,
                breakdown: Some(BreakdownType::Hour),
                timezone: Some("America/New_York".parse().unwrap()),
                limit: None,
                order_by: None,
            })
            .await
            .unwrap()
            .breakdown
            .unwrap();
        assert_eq!(
            time.items
                .iter()
                .map(|item| item.metrics.requests)
                .sum::<u64>(),
            3
        );
        pool.close().await;
    }
}

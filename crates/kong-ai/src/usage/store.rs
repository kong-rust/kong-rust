//! Usage Store 抽象以及 PostgreSQL/DB-less 共用的聚合口径。

use std::cmp::Ordering;
use std::collections::HashMap;
use std::str::FromStr;

use async_trait::async_trait;
use bigdecimal::{BigDecimal, RoundingMode};
use chrono::{DateTime, Duration, NaiveDate, Offset, Timelike, Utc};
use num_bigint::BigInt;
use rust_decimal::{Decimal, RoundingStrategy};
use sha2::{Digest, Sha256};

use super::model::{
    AggregateMetrics, AiUsageBreakdown, AiUsageFact, AiUsageListQuery, AiUsageMode, AiUsagePage,
    AiUsageResult, AiUsageSnapshot, AiUsageSummary, AiUsageSummaryQuery, BatchWriteResult,
    BreakdownItem, BreakdownType, CacheStatus, CostStatus, DimensionRef, PricingStatus,
    SummaryOrder, TokenAggregate, UsageSource,
};

#[async_trait]
pub trait AiUsageStore: Send + Sync {
    fn mode(&self) -> AiUsageMode;

    async fn insert_batch(&self, rows: &[AiUsageFact]) -> AiUsageResult<BatchWriteResult>;

    async fn snapshot(
        &self,
        filter: &super::model::AiUsageFilter,
    ) -> AiUsageResult<AiUsageSnapshot>;

    async fn list(&self, query: &AiUsageListQuery) -> AiUsageResult<AiUsagePage>;

    async fn summary(&self, query: &AiUsageSummaryQuery) -> AiUsageResult<AiUsageSummary>;
}

pub(crate) fn aggregate(rows: &[&AiUsageFact]) -> AggregateMetrics {
    if rows.is_empty() {
        return AggregateMetrics::default();
    }

    let requests = rows.len() as u64;
    let successful_requests = rows
        .iter()
        .filter(|fact| fact.outcome == super::model::AiUsageOutcome::Success)
        .count() as u64;
    let mut metrics = AggregateMetrics {
        requests,
        successful_requests,
        failed_requests: requests - successful_requests,
        ..Default::default()
    };
    let mut prompt_sum = BigInt::from(0u8);
    let mut completion_sum = BigInt::from(0u8);
    let mut total_sum = BigInt::from(0u8);
    let mut attempted = 0u64;
    let mut prompt_known = 0u64;
    let mut completion_known = 0u64;
    let mut total_known = 0u64;
    let mut usage_known = 0u64;
    let mut estimated_usage = 0u64;
    let mut pricing_matched = 0u64;
    let mut cost_calculable = 0u64;
    let mut cost_sum = BigDecimal::from(0);
    let mut e2e_sum = BigInt::from(0u8);
    let mut e2e_values = Vec::with_capacity(rows.len());
    let mut ttft_sum = BigInt::from(0u8);
    let mut ttft_count = 0u64;

    for fact in rows {
        match fact.outcome {
            super::model::AiUsageOutcome::Success => metrics.outcomes.success += 1,
            super::model::AiUsageOutcome::GatewayRejected => metrics.outcomes.gateway_rejected += 1,
            super::model::AiUsageOutcome::GatewayError => metrics.outcomes.gateway_error += 1,
            super::model::AiUsageOutcome::UpstreamError => metrics.outcomes.upstream_error += 1,
            super::model::AiUsageOutcome::ClientDisconnected => {
                metrics.outcomes.client_disconnected += 1
            }
            super::model::AiUsageOutcome::StreamInterrupted => {
                metrics.outcomes.stream_interrupted += 1
            }
        }
        match fact.pricing_status {
            PricingStatus::Matched => metrics.pricing_status.matched += 1,
            PricingStatus::Unmatched => metrics.pricing_status.unmatched += 1,
            PricingStatus::Unsupported => metrics.pricing_status.unsupported += 1,
            PricingStatus::NotApplicable => metrics.pricing_status.not_applicable += 1,
        }
        match fact.cost_status {
            CostStatus::Calculated => metrics.cost_status.calculated += 1,
            CostStatus::Estimated => metrics.cost_status.estimated += 1,
            CostStatus::NotIncurred => metrics.cost_status.not_incurred += 1,
            CostStatus::Unavailable => metrics.cost_status.unavailable += 1,
        }
        if fact.cache_status == CacheStatus::Hit {
            metrics.cache_hits += 1;
        }
        if fact.upstream_attempted {
            attempted += 1;
            if let Some(field) = fact.prompt_tokens {
                prompt_sum += BigInt::from(field.value);
                prompt_known += 1;
            }
            if let Some(field) = fact.completion_tokens {
                completion_sum += BigInt::from(field.value);
                completion_known += 1;
            }
            if let Some(field) = fact.total_tokens {
                total_sum += BigInt::from(field.value);
                total_known += 1;
            }
            if fact.prompt_tokens.is_some()
                || fact.completion_tokens.is_some()
                || fact.total_tokens.is_some()
            {
                usage_known += 1;
                if matches!(
                    fact.usage_source,
                    UsageSource::Estimated | UsageSource::Mixed
                ) {
                    estimated_usage += 1;
                }
            }
            if fact.pricing_status == PricingStatus::Matched {
                pricing_matched += 1;
            }
            if matches!(
                fact.cost_status,
                CostStatus::Calculated | CostStatus::Estimated
            ) {
                cost_calculable += 1;
            }
        }
        if matches!(
            fact.cost_status,
            CostStatus::Calculated | CostStatus::Estimated
        ) {
            if let Some(value) = fact.cost_usd {
                if let Ok(value) = BigDecimal::from_str(&value.to_string()) {
                    cost_sum += value;
                }
            }
        }
        e2e_sum += BigInt::from(fact.e2e_ms);
        e2e_values.push(fact.e2e_ms);
        if let Some(ttft) = fact.ttft_ms {
            ttft_sum += BigInt::from(ttft);
            ttft_count += 1;
        }
    }

    metrics.prompt_tokens = token_aggregate(prompt_sum, prompt_known, attempted);
    metrics.completion_tokens = token_aggregate(completion_sum, completion_known, attempted);
    metrics.total_tokens = token_aggregate(total_sum, total_known, attempted);
    metrics.cost_usd_calculable_sum = decimal_big_12(cost_sum);
    metrics.estimated_usage_ratio = ratio_6(estimated_usage, usage_known);
    metrics.pricing_coverage = ratio_6(pricing_matched, attempted);
    metrics.cost_calculable_coverage = ratio_6(cost_calculable, attempted);
    metrics.avg_e2e_ms = average_3(e2e_sum, requests);
    metrics.p95_e2e_ms = percentile_95_3(&mut e2e_values);
    metrics.avg_ttft_ms = if ttft_count == 0 {
        None
    } else {
        average_3(ttft_sum, ttft_count)
    };
    metrics
}

fn token_aggregate(sum: BigInt, known: u64, attempted: u64) -> TokenAggregate {
    TokenAggregate {
        known_sum: sum.to_string(),
        known_requests: known,
        unknown_requests: attempted.saturating_sub(known),
        coverage: ratio_6(known, attempted),
    }
}

fn ratio_6(numerator: u64, denominator: u64) -> Option<String> {
    if denominator == 0 {
        return None;
    }
    Decimal::from(numerator)
        .checked_div(Decimal::from(denominator))
        .map(|value| {
            format!(
                "{:.6}",
                value.round_dp_with_strategy(6, RoundingStrategy::MidpointAwayFromZero)
            )
        })
}

fn average_3(sum: BigInt, count: u64) -> Option<String> {
    if count == 0 {
        None
    } else {
        Some(
            (BigDecimal::from(sum) / BigDecimal::from(count))
                .with_scale_round(3, RoundingMode::HalfUp)
                .to_string(),
        )
    }
}

fn decimal_3(value: Decimal) -> String {
    format!(
        "{:.3}",
        value.round_dp_with_strategy(3, RoundingStrategy::MidpointAwayFromZero)
    )
}

fn decimal_big_12(value: BigDecimal) -> String {
    value.with_scale(12).to_string()
}

fn percentile_95_3(values: &mut [i64]) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    if values.len() == 1 {
        return Some(format!("{:.3}", Decimal::from(values[0])));
    }
    // percentile_cont: rank=(n-1)*0.95，在相邻点之间线性插值。
    let scaled_rank = (values.len() - 1) * 95;
    let lower = scaled_rank / 100;
    let remainder = scaled_rank % 100;
    let lower_value = Decimal::from(values[lower]);
    let result = if remainder == 0 {
        lower_value
    } else {
        let upper_value = Decimal::from(values[lower + 1]);
        let fraction = Decimal::new(remainder as i64, 2);
        lower_value + (upper_value - lower_value) * fraction
    };
    Some(decimal_3(result))
}

pub(crate) fn build_breakdown(
    rows: &[&AiUsageFact],
    query: &AiUsageSummaryQuery,
) -> AiUsageResult<Option<AiUsageBreakdown>> {
    let Some(breakdown_type) = query.breakdown else {
        return Ok(None);
    };
    if breakdown_type.is_time() {
        return build_time_breakdown(rows, query, breakdown_type).map(Some);
    }
    Ok(Some(build_category_breakdown(rows, query, breakdown_type)))
}

fn build_time_breakdown(
    rows: &[&AiUsageFact],
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
    let items = plan
        .buckets
        .iter()
        .map(|bucket| {
            let bucket_rows: Vec<_> = rows
                .iter()
                .copied()
                .filter(|fact| fact.started_at >= bucket.start && fact.started_at < bucket.end)
                .collect();
            BreakdownItem {
                key: Some(bucket.start.to_rfc3339()),
                label: Some(bucket.label.clone()),
                is_other: false,
                bucket_start: Some(bucket.start),
                bucket_end: Some(bucket.end),
                dimension: None,
                metrics: aggregate(&bucket_rows),
            }
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

fn build_category_breakdown(
    rows: &[&AiUsageFact],
    query: &AiUsageSummaryQuery,
    breakdown_type: BreakdownType,
) -> AiUsageBreakdown {
    let mut groups: HashMap<Option<String>, Vec<&AiUsageFact>> = HashMap::new();
    for fact in rows {
        groups
            .entry(category_key(fact, breakdown_type))
            .or_default()
            .push(*fact);
    }
    let mut ranked: Vec<_> = groups
        .into_iter()
        .map(|(key, group_rows)| {
            let latest = group_rows
                .iter()
                .max_by_key(|fact| (fact.started_at, fact.id))
                .copied()
                .unwrap();
            let dimension = category_dimension(latest, breakdown_type);
            let label = category_label(latest, breakdown_type);
            let metrics = aggregate(&group_rows);
            (key, label, dimension, metrics, group_rows)
        })
        .collect();
    let order_by = query.order_by.unwrap_or_default();
    ranked.sort_by(|left, right| {
        compare_metric(&right.3, &left.3, order_by)
            .then_with(|| right.3.requests.cmp(&left.3.requests))
            .then_with(|| match (&left.0, &right.0) {
                (Some(left), Some(right)) => left.cmp(right),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            })
    });

    let limit = query.limit.unwrap_or(10);
    let remainder = ranked.split_off(ranked.len().min(limit));
    let items = ranked
        .into_iter()
        .map(|(key, label, dimension, metrics, _)| BreakdownItem {
            key,
            label,
            is_other: false,
            bucket_start: None,
            bucket_end: None,
            dimension,
            metrics,
        })
        .collect();
    let other_rows: Vec<_> = remainder
        .into_iter()
        .flat_map(|(_, _, _, _, rows)| rows)
        .collect();
    let other = (!other_rows.is_empty()).then(|| BreakdownItem {
        key: None,
        label: Some("Other".to_string()),
        is_other: true,
        bucket_start: None,
        bucket_end: None,
        dimension: None,
        metrics: aggregate(&other_rows),
    });
    AiUsageBreakdown {
        breakdown_type,
        timezone: None,
        order_by: Some(order_by),
        limit: Some(limit),
        items,
        other,
    }
}

fn compare_metric(
    left: &AggregateMetrics,
    right: &AggregateMetrics,
    order: SummaryOrder,
) -> Ordering {
    match order {
        SummaryOrder::Requests => left.requests.cmp(&right.requests),
        SummaryOrder::TotalTokens => {
            let left = BigInt::from_str(&left.total_tokens.known_sum).unwrap_or_default();
            let right = BigInt::from_str(&right.total_tokens.known_sum).unwrap_or_default();
            left.cmp(&right)
        }
        SummaryOrder::CostUsd => {
            let left = BigDecimal::from_str(&left.cost_usd_calculable_sum).unwrap_or_default();
            let right = BigDecimal::from_str(&right.cost_usd_calculable_sum).unwrap_or_default();
            left.partial_cmp(&right).unwrap_or(Ordering::Equal)
        }
    }
}

fn category_key(fact: &AiUsageFact, kind: BreakdownType) -> Option<String> {
    match kind {
        BreakdownType::Provider => stable_or_snapshot(
            fact.provider_id,
            kind,
            &[fact.provider_name.as_deref(), fact.provider_type.as_deref()],
        ),
        BreakdownType::ActualModel => snapshot_key(
            kind,
            &[fact.provider_type.as_deref(), fact.actual_model.as_deref()],
        ),
        BreakdownType::ModelGroup => snapshot_key(kind, &[fact.model_group.as_deref()]),
        BreakdownType::VirtualKey => stable_or_snapshot(
            fact.virtual_key_id,
            kind,
            &[
                fact.virtual_key_name.as_deref(),
                fact.virtual_key_prefix.as_deref(),
            ],
        ),
        BreakdownType::Route => {
            stable_or_snapshot(fact.route_id, kind, &[fact.route_name.as_deref()])
        }
        BreakdownType::Service => {
            stable_or_snapshot(fact.service_id, kind, &[fact.service_name.as_deref()])
        }
        BreakdownType::Hour | BreakdownType::Day => None,
    }
}

fn stable_or_snapshot(
    id: Option<uuid::Uuid>,
    kind: BreakdownType,
    values: &[Option<&str>],
) -> Option<String> {
    id.map(|id| format!("id:{id}"))
        .or_else(|| snapshot_key(kind, values))
}

fn snapshot_key(kind: BreakdownType, values: &[Option<&str>]) -> Option<String> {
    if values.iter().all(Option::is_none) {
        return None;
    }
    let canonical = serde_json::to_vec(&(kind.as_str(), values)).ok()?;
    Some(format!("snapshot:{:x}", Sha256::digest(canonical)))
}

fn category_label(fact: &AiUsageFact, kind: BreakdownType) -> Option<String> {
    match kind {
        BreakdownType::Provider => fact.provider_name.clone(),
        BreakdownType::ActualModel => fact.actual_model.clone(),
        BreakdownType::ModelGroup => fact.model_group.clone(),
        BreakdownType::VirtualKey => fact.virtual_key_name.clone(),
        BreakdownType::Route => fact.route_name.clone(),
        BreakdownType::Service => fact.service_name.clone(),
        BreakdownType::Hour | BreakdownType::Day => None,
    }
}

fn category_dimension(fact: &AiUsageFact, kind: BreakdownType) -> Option<DimensionRef> {
    let dimension = match kind {
        BreakdownType::Provider => DimensionRef {
            id: fact.provider_id,
            name: fact.provider_name.clone(),
            dimension_type: fact.provider_type.clone(),
            prefix: None,
        },
        BreakdownType::ActualModel => DimensionRef {
            id: None,
            name: fact.actual_model.clone(),
            dimension_type: fact.provider_type.clone(),
            prefix: None,
        },
        BreakdownType::ModelGroup => DimensionRef {
            id: None,
            name: fact.model_group.clone(),
            dimension_type: None,
            prefix: None,
        },
        BreakdownType::VirtualKey => DimensionRef {
            id: fact.virtual_key_id,
            name: fact.virtual_key_name.clone(),
            dimension_type: None,
            prefix: fact.virtual_key_prefix.clone(),
        },
        BreakdownType::Route => DimensionRef {
            id: fact.route_id,
            name: fact.route_name.clone(),
            dimension_type: None,
            prefix: None,
        },
        BreakdownType::Service => DimensionRef {
            id: fact.service_id,
            name: fact.service_name.clone(),
            dimension_type: None,
            prefix: None,
        },
        BreakdownType::Hour | BreakdownType::Day => return None,
    };
    (dimension.id.is_some()
        || dimension.name.is_some()
        || dimension.dimension_type.is_some()
        || dimension.prefix.is_some())
    .then_some(dimension)
}

pub(crate) struct TimeBucket {
    pub(crate) start: DateTime<Utc>,
    pub(crate) end: DateTime<Utc>,
    pub(crate) label: String,
}

pub(crate) struct TimeBucketPlan {
    pub(crate) buckets: Vec<TimeBucket>,
}

impl TimeBucketPlan {
    pub(crate) fn new(
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        timezone: chrono_tz::Tz,
        kind: BreakdownType,
    ) -> AiUsageResult<Self> {
        let maximum_duration = if kind == BreakdownType::Hour {
            Duration::days(31)
        } else {
            Duration::days(90)
        };
        if end - start > maximum_duration {
            return Err(super::model::AiUsageError::InvalidQuery(format!(
                "{} breakdown 时间窗口超限",
                kind.as_str()
            )));
        }

        let padding = if kind == BreakdownType::Hour {
            Duration::hours(3)
        } else {
            // 日期变更线历史回拨可让同一当地日期持续接近 48 小时。
            Duration::hours(72)
        };
        let rounded_start = start
            .with_second(0)
            .and_then(|value| value.with_nanosecond(0))
            .unwrap_or(start);
        let mut cursor = rounded_start.checked_sub_signed(padding).ok_or_else(|| {
            super::model::AiUsageError::InvalidQuery("breakdown 起始时间超出可计算范围".to_string())
        })?;
        let scan_end = end.checked_add_signed(padding).ok_or_else(|| {
            super::model::AiUsageError::InvalidQuery("breakdown 结束时间超出可计算范围".to_string())
        })?;
        let mut boundaries = Vec::new();
        let mut identity = time_bucket_identity(cursor, timezone, kind);
        while cursor < scan_end {
            let next = cursor
                .checked_add_signed(Duration::minutes(1))
                .map(|value| value.min(scan_end))
                .ok_or_else(|| {
                    super::model::AiUsageError::InvalidQuery(
                        "breakdown 时间超出可计算范围".to_string(),
                    )
                })?;
            let next_identity = time_bucket_identity(next, timezone, kind);
            if next_identity != identity {
                let mut probe =
                    cursor
                        .checked_add_signed(Duration::seconds(1))
                        .ok_or_else(|| {
                            super::model::AiUsageError::InvalidQuery(
                                "breakdown 时间超出可计算范围".to_string(),
                            )
                        })?;
                while probe <= next {
                    if time_bucket_identity(probe, timezone, kind) != identity {
                        boundaries.push(probe);
                        break;
                    }
                    probe = probe
                        .checked_add_signed(Duration::seconds(1))
                        .ok_or_else(|| {
                            super::model::AiUsageError::InvalidQuery(
                                "breakdown 时间超出可计算范围".to_string(),
                            )
                        })?;
                }
                identity = next_identity;
            }
            cursor = next;
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        let first = boundaries
            .iter()
            .rposition(|boundary| *boundary <= start)
            .ok_or_else(|| {
                super::model::AiUsageError::Internal("无法生成时区 bucket 起点".to_string())
            })?;
        let selected = &boundaries[first..];
        let mut buckets = Vec::new();
        for window in selected.windows(2) {
            if window[0] >= end {
                break;
            }
            if window[1] <= start {
                continue;
            }
            let local = window[0].with_timezone(&timezone);
            let offset_seconds = local.offset().fix().local_minus_utc();
            let label = if kind == BreakdownType::Hour {
                if offset_seconds % 60 == 0 {
                    local.format("%Y-%m-%d %H:00 %:z").to_string()
                } else {
                    local.format("%Y-%m-%d %H:00 %::z").to_string()
                }
            } else if offset_seconds % 60 == 0 {
                local.format("%Y-%m-%d %:z").to_string()
            } else {
                local.format("%Y-%m-%d %::z").to_string()
            };
            buckets.push(TimeBucket {
                start: window[0],
                end: window[1],
                label,
            });
        }
        let limit = if kind == BreakdownType::Hour { 744 } else { 90 };
        if buckets.len() > limit {
            return Err(super::model::AiUsageError::InvalidQuery(format!(
                "{} breakdown bucket 数量超过 {limit}",
                kind.as_str()
            )));
        }
        Ok(Self { buckets })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimeBucketIdentity {
    Hour {
        date: NaiveDate,
        hour: u32,
        offset_seconds: i32,
    },
    Day(NaiveDate),
}

fn time_bucket_identity(
    instant: DateTime<Utc>,
    timezone: chrono_tz::Tz,
    kind: BreakdownType,
) -> TimeBucketIdentity {
    let local = instant.with_timezone(&timezone);
    if kind == BreakdownType::Hour {
        TimeBucketIdentity::Hour {
            date: local.date_naive(),
            hour: local.hour(),
            offset_seconds: local.offset().fix().local_minus_utc(),
        }
    } else {
        TimeBucketIdentity::Day(local.date_naive())
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn percentile_matches_percentile_cont_interpolation() {
        let mut values = [100, 200];
        assert_eq!(percentile_95_3(&mut values), Some("195.000".to_string()));
    }

    #[test]
    fn average_rounds_half_up_without_integer_overflow() {
        assert_eq!(average_3(BigInt::from(5u8), 3), Some("1.667".to_string()));
        assert_eq!(
            average_3(BigInt::from(i64::MAX) * BigInt::from(2u8), 2),
            Some("9223372036854775807.000".to_string())
        );
    }

    #[test]
    fn dst_fall_back_produces_two_distinct_one_oclock_buckets() {
        let timezone: chrono_tz::Tz = "America/New_York".parse().unwrap();
        let start = Utc.with_ymd_and_hms(2025, 11, 2, 4, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 11, 2, 8, 0, 0).unwrap();
        let plan = TimeBucketPlan::new(start, end, timezone, BreakdownType::Hour).unwrap();
        let labels: Vec<_> = plan.buckets.iter().map(|bucket| &bucket.label).collect();
        assert!(labels.iter().any(|label| label.contains("01:00 -04:00")));
        assert!(labels.iter().any(|label| label.contains("01:00 -05:00")));
    }

    #[test]
    fn day_buckets_use_one_boundary_for_repeated_or_missing_midnight() {
        let timezone: chrono_tz::Tz = "America/Havana".parse().unwrap();

        let spring_start = Utc.with_ymd_and_hms(2026, 3, 7, 5, 0, 0).unwrap();
        let spring_end = Utc.with_ymd_and_hms(2026, 3, 10, 4, 0, 0).unwrap();
        let spring =
            TimeBucketPlan::new(spring_start, spring_end, timezone, BreakdownType::Day).unwrap();
        let spring_starts: Vec<_> = spring.buckets.iter().map(|bucket| bucket.start).collect();
        assert_eq!(
            spring_starts,
            vec![
                Utc.with_ymd_and_hms(2026, 3, 7, 5, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 3, 8, 5, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 3, 9, 4, 0, 0).unwrap(),
            ]
        );
        assert_eq!(
            spring.buckets[1].end - spring.buckets[1].start,
            Duration::hours(23)
        );

        let fall_start = Utc.with_ymd_and_hms(2026, 10, 31, 4, 0, 0).unwrap();
        let fall_end = Utc.with_ymd_and_hms(2026, 11, 3, 5, 0, 0).unwrap();
        let fall = TimeBucketPlan::new(fall_start, fall_end, timezone, BreakdownType::Day).unwrap();
        let fall_starts: Vec<_> = fall.buckets.iter().map(|bucket| bucket.start).collect();
        assert_eq!(
            fall_starts,
            vec![
                Utc.with_ymd_and_hms(2026, 10, 31, 4, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 11, 1, 4, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 11, 2, 5, 0, 0).unwrap(),
            ]
        );
        assert_eq!(
            fall.buckets[1].end - fall.buckets[1].start,
            Duration::hours(25)
        );
        assert!(!fall_starts.contains(&Utc.with_ymd_and_hms(2026, 11, 1, 5, 0, 0).unwrap()));
    }

    #[test]
    fn historical_second_offset_produces_exact_hour_and_day_boundaries() {
        let timezone: chrono_tz::Tz = "Africa/Monrovia".parse().unwrap();
        let start = Utc.with_ymd_and_hms(1960, 1, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(1960, 1, 2, 2, 0, 0).unwrap();
        let expected = Utc.with_ymd_and_hms(1960, 1, 1, 0, 44, 30).unwrap();

        for kind in [BreakdownType::Hour, BreakdownType::Day] {
            let plan = TimeBucketPlan::new(start, end, timezone, kind).unwrap();
            assert!(
                plan.buckets.iter().any(|bucket| bucket.start == expected),
                "{} 缺少精确历史边界",
                kind.as_str()
            );
            assert!(plan.buckets.windows(2).all(
                |window| window[0].start < window[1].start && window[0].end == window[1].start
            ));
        }
    }

    #[test]
    fn date_line_rollback_still_has_a_day_boundary_before_the_window() {
        let timezone: chrono_tz::Tz = "Pacific/Kwajalein".parse().unwrap();
        let start = Utc.with_ymd_and_hms(1969, 10, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(1969, 10, 2, 12, 0, 0).unwrap();
        let plan = TimeBucketPlan::new(start, end, timezone, BreakdownType::Day).unwrap();

        assert!(plan
            .buckets
            .iter()
            .any(|bucket| bucket.start <= start && bucket.end > start));
        assert!(plan
            .buckets
            .windows(2)
            .all(|window| window[0].end == window[1].start));
    }

    #[test]
    fn time_bucket_rejects_chrono_boundary_without_panicking() {
        let end = DateTime::<Utc>::MAX_UTC;
        let start = end.checked_sub_signed(Duration::hours(1)).unwrap();
        assert!(matches!(
            TimeBucketPlan::new(start, end, chrono_tz::UTC, BreakdownType::Hour),
            Err(super::super::model::AiUsageError::InvalidQuery(_))
        ));
    }
}

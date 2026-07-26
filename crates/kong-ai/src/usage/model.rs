//! AI 调用事实、查询条件与 Admin API 数据契约。

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! string_enum {
    ($(#[$meta:meta])* pub enum $name:ident { $($(#[$variant_meta:meta])* $variant:ident => $value:literal),+ $(,)? }) => {
        $(#[$meta])*
        pub enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str((*self).as_str())
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant)),+,
                    _ => Err(format!("不支持的 {} 值: {value}", stringify!($name))),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str((*self).as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::from_str(&value).map_err(serde::de::Error::custom)
            }
        }
    };
}

string_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub enum TokenFieldSource {
        #[default]
        Provider => "provider",
        Estimated => "estimated",
        Mixed => "mixed",
    }
}

string_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub enum UsageSource {
        Provider => "provider",
        Estimated => "estimated",
        Mixed => "mixed",
        #[default]
        Unavailable => "unavailable",
    }
}

string_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub enum PricingStatus {
        Matched => "matched",
        Unmatched => "unmatched",
        Unsupported => "unsupported",
        #[default]
        NotApplicable => "not_applicable",
    }
}

string_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub enum CostStatus {
        Calculated => "calculated",
        Estimated => "estimated",
        #[default]
        NotIncurred => "not_incurred",
        Unavailable => "unavailable",
    }
}

string_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub enum AiUsageOutcome {
        #[default]
        Success => "success",
        GatewayRejected => "gateway_rejected",
        GatewayError => "gateway_error",
        UpstreamError => "upstream_error",
        ClientDisconnected => "client_disconnected",
        StreamInterrupted => "stream_interrupted",
    }
}

string_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub enum CacheStatus {
        #[default]
        NotConfigured => "not_configured",
        Unavailable => "unavailable",
        Bypass => "bypass",
        Miss => "miss",
        Hit => "hit",
    }
}

string_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub enum AiUsageMode {
        #[default]
        Postgres => "postgres",
        Dbless => "dbless",
    }
}

string_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum BreakdownType {
        Hour => "hour",
        Day => "day",
        Provider => "provider",
        ActualModel => "actual_model",
        ModelGroup => "model_group",
        VirtualKey => "virtual_key",
        Route => "route",
        Service => "service",
    }
}

impl BreakdownType {
    pub fn is_time(self) -> bool {
        matches!(self, Self::Hour | Self::Day)
    }
}

string_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub enum SummaryOrder {
        #[default]
        CostUsd => "cost_usd",
        TotalTokens => "total_tokens",
        Requests => "requests",
    }
}

/// 单个 token 字段及其来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenField {
    pub value: i64,
    pub source: TokenFieldSource,
    pub derived: bool,
}

/// 请求发生时固化的某一方向价格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceSnapshot {
    pub usd_per_million: Decimal,
    pub source: String,
    pub version: String,
    pub snapshot_date: NaiveDate,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
}

/// 一次 AI 网关请求的不可变事实。
#[derive(Debug, Clone)]
pub struct AiUsageFact {
    pub id: Uuid,
    pub ingest_seq: Option<i64>,
    pub request_id: String,
    pub node_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub recorded_at: Option<DateTime<Utc>>,
    pub workspace_id: Option<Uuid>,
    pub route_id: Option<Uuid>,
    pub route_name: Option<String>,
    pub service_id: Option<Uuid>,
    pub service_name: Option<String>,
    pub provider_id: Option<Uuid>,
    pub provider_name: Option<String>,
    pub provider_type: Option<String>,
    pub model_id: Option<Uuid>,
    pub requested_model: Option<String>,
    pub model_group: Option<String>,
    pub actual_model: Option<String>,
    pub attempt_count: i16,
    pub virtual_key_id: Option<Uuid>,
    pub virtual_key_name: Option<String>,
    pub virtual_key_prefix: Option<String>,
    pub consumer_id: Option<Uuid>,
    pub prompt_tokens: Option<TokenField>,
    pub completion_tokens: Option<TokenField>,
    pub total_tokens: Option<TokenField>,
    pub reasoning_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_write_input_tokens: Option<i64>,
    pub usage_source: UsageSource,
    pub usage_unavailable_reasons: Vec<String>,
    pub input_price: Option<PriceSnapshot>,
    pub output_price: Option<PriceSnapshot>,
    pub pricing_status: PricingStatus,
    pub pricing_unsupported_reasons: Vec<String>,
    pub cost_usd: Option<Decimal>,
    pub cost_status: CostStatus,
    pub cost_unavailable_reasons: Vec<String>,
    pub status_code: Option<i16>,
    pub upstream_status_code: Option<i16>,
    pub outcome: AiUsageOutcome,
    pub e2e_ms: i64,
    pub ttft_ms: Option<i64>,
    pub upstream_attempted: bool,
    pub stream: Option<bool>,
    pub cache_status: CacheStatus,
}

/// 查询事实集合的公共过滤器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiUsageFilter {
    pub workspace_id: Uuid,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub request_id: Option<String>,
    pub route_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
    pub provider_id: Option<Uuid>,
    pub provider_type: Option<String>,
    pub requested_model: Option<String>,
    pub model_group: Option<String>,
    pub actual_model: Option<String>,
    pub virtual_key_id: Option<Uuid>,
    pub consumer_id: Option<Uuid>,
    pub status_code: Option<i16>,
    pub outcome: Option<AiUsageOutcome>,
    pub stream: Option<bool>,
    pub cache_status: Option<CacheStatus>,
    pub usage_source: Option<UsageSource>,
    pub pricing_status: Option<PricingStatus>,
    pub cost_status: Option<CostStatus>,
}

impl AiUsageFilter {
    pub fn matches(&self, fact: &AiUsageFact) -> bool {
        fact.workspace_id == Some(self.workspace_id)
            && fact.started_at >= self.start
            && fact.started_at < self.end
            && self
                .request_id
                .as_ref()
                .is_none_or(|value| fact.request_id == *value)
            && self
                .route_id
                .is_none_or(|value| fact.route_id == Some(value))
            && self
                .service_id
                .is_none_or(|value| fact.service_id == Some(value))
            && self
                .provider_id
                .is_none_or(|value| fact.provider_id == Some(value))
            && self
                .provider_type
                .as_ref()
                .is_none_or(|value| fact.provider_type.as_ref() == Some(value))
            && self
                .requested_model
                .as_ref()
                .is_none_or(|value| fact.requested_model.as_ref() == Some(value))
            && self
                .model_group
                .as_ref()
                .is_none_or(|value| fact.model_group.as_ref() == Some(value))
            && self
                .actual_model
                .as_ref()
                .is_none_or(|value| fact.actual_model.as_ref() == Some(value))
            && self
                .virtual_key_id
                .is_none_or(|value| fact.virtual_key_id == Some(value))
            && self
                .consumer_id
                .is_none_or(|value| fact.consumer_id == Some(value))
            && self
                .status_code
                .is_none_or(|value| fact.status_code == Some(value))
            && self.outcome.is_none_or(|value| fact.outcome == value)
            && self.stream.is_none_or(|value| fact.stream == Some(value))
            && self
                .cache_status
                .is_none_or(|value| fact.cache_status == value)
            && self
                .usage_source
                .is_none_or(|value| fact.usage_source == value)
            && self
                .pricing_status
                .is_none_or(|value| fact.pricing_status == value)
            && self
                .cost_status
                .is_none_or(|value| fact.cost_status == value)
    }
}

/// Store 生成的稳定读取水位。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiUsageSnapshot {
    pub v: u8,
    pub backend: AiUsageMode,
    pub workspace_id: Uuid,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub high_watermark: i64,
    pub eviction_generation: Option<u64>,
    pub ring_instance_id: Option<Uuid>,
    pub filter_hash: String,
}

/// 明细分页游标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiUsageOffset {
    pub v: u8,
    pub snapshot: AiUsageSnapshot,
    pub last_started_at: DateTime<Utc>,
    pub last_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct AiUsageListQuery {
    pub filter: AiUsageFilter,
    pub snapshot: AiUsageSnapshot,
    pub offset: Option<AiUsageOffset>,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub struct AiUsageSummaryQuery {
    pub filter: AiUsageFilter,
    pub snapshot: AiUsageSnapshot,
    pub breakdown: Option<BreakdownType>,
    pub timezone: Option<chrono_tz::Tz>,
    pub limit: Option<usize>,
    pub order_by: Option<SummaryOrder>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiUsageMeta {
    pub mode: AiUsageMode,
    pub ephemeral: bool,
    pub node_id: Option<Uuid>,
    pub capacity: Option<usize>,
    pub earliest_available_at: Option<DateTime<Utc>>,
    pub restart_clears: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityRef {
    pub id: Option<Uuid>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderRef {
    pub id: Option<Uuid>,
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub provider_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelRef {
    pub id: Option<Uuid>,
    pub requested: Option<String>,
    pub group: Option<String>,
    pub actual: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VirtualKeyRef {
    pub id: Option<Uuid>,
    pub name: Option<String>,
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GatewayRecord {
    pub route: Option<EntityRef>,
    pub service: Option<EntityRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiRecord {
    pub provider: Option<ProviderRef>,
    pub model: Option<ModelRef>,
    pub attempt_count: i16,
}

#[derive(Debug, Clone, Serialize)]
pub struct IdentityRecord {
    pub virtual_key: Option<VirtualKeyRef>,
    pub consumer_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageRecord {
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub prompt_source: Option<TokenFieldSource>,
    pub completion_source: Option<TokenFieldSource>,
    pub total_source: Option<TokenFieldSource>,
    pub reasoning_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_write_input_tokens: Option<i64>,
    pub source: UsageSource,
    pub unavailable_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PriceRecord {
    pub usd_per_million: String,
    pub source: String,
    pub version: String,
    pub snapshot_date: NaiveDate,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
}

impl From<&PriceSnapshot> for PriceRecord {
    fn from(value: &PriceSnapshot) -> Self {
        Self {
            usd_per_million: decimal_12(value.usd_per_million),
            source: value.source.clone(),
            version: value.version.clone(),
            snapshot_date: value.snapshot_date,
            effective_from: value.effective_from,
            effective_to: value.effective_to,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PricingRecord {
    pub status: PricingStatus,
    pub currency: &'static str,
    pub input: Option<PriceRecord>,
    pub output: Option<PriceRecord>,
    pub unsupported_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CostRecord {
    pub usd: Option<String>,
    pub status: CostStatus,
    pub unavailable_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultRecord {
    pub status_code: Option<i16>,
    pub upstream_status_code: Option<i16>,
    pub outcome: AiUsageOutcome,
    pub e2e_ms: i64,
    pub ttft_ms: Option<i64>,
    pub upstream_attempted: bool,
    pub stream: Option<bool>,
    pub cache_status: CacheStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiUsageRecord {
    pub id: Uuid,
    pub request_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub gateway: GatewayRecord,
    pub ai: AiRecord,
    pub identity: IdentityRecord,
    pub usage: UsageRecord,
    pub pricing: PricingRecord,
    pub cost: CostRecord,
    pub result: ResultRecord,
}

impl From<&AiUsageFact> for AiUsageRecord {
    fn from(fact: &AiUsageFact) -> Self {
        let route = optional_entity(fact.route_id, fact.route_name.clone());
        let service = optional_entity(fact.service_id, fact.service_name.clone());
        let provider = if fact.provider_id.is_some()
            || fact.provider_name.is_some()
            || fact.provider_type.is_some()
        {
            Some(ProviderRef {
                id: fact.provider_id,
                name: fact.provider_name.clone(),
                provider_type: fact.provider_type.clone(),
            })
        } else {
            None
        };
        let model = if fact.model_id.is_some()
            || fact.requested_model.is_some()
            || fact.model_group.is_some()
            || fact.actual_model.is_some()
        {
            Some(ModelRef {
                id: fact.model_id,
                requested: fact.requested_model.clone(),
                group: fact.model_group.clone(),
                actual: fact.actual_model.clone(),
            })
        } else {
            None
        };
        let virtual_key = if fact.virtual_key_id.is_some()
            || fact.virtual_key_name.is_some()
            || fact.virtual_key_prefix.is_some()
        {
            Some(VirtualKeyRef {
                id: fact.virtual_key_id,
                name: fact.virtual_key_name.clone(),
                prefix: fact.virtual_key_prefix.clone(),
            })
        } else {
            None
        };

        Self {
            id: fact.id,
            request_id: fact.request_id.clone(),
            started_at: fact.started_at,
            finished_at: fact.finished_at,
            gateway: GatewayRecord { route, service },
            ai: AiRecord {
                provider,
                model,
                attempt_count: fact.attempt_count,
            },
            identity: IdentityRecord {
                virtual_key,
                consumer_id: fact.consumer_id,
            },
            usage: UsageRecord {
                prompt_tokens: fact.prompt_tokens.map(|field| field.value),
                completion_tokens: fact.completion_tokens.map(|field| field.value),
                total_tokens: fact.total_tokens.map(|field| field.value),
                prompt_source: fact.prompt_tokens.map(|field| field.source),
                completion_source: fact.completion_tokens.map(|field| field.source),
                total_source: fact.total_tokens.map(|field| field.source),
                reasoning_tokens: fact.reasoning_tokens,
                cache_read_input_tokens: fact.cache_read_input_tokens,
                cache_write_input_tokens: fact.cache_write_input_tokens,
                source: fact.usage_source,
                unavailable_reasons: fact.usage_unavailable_reasons.clone(),
            },
            pricing: PricingRecord {
                status: fact.pricing_status,
                currency: "USD",
                input: fact.input_price.as_ref().map(PriceRecord::from),
                output: fact.output_price.as_ref().map(PriceRecord::from),
                unsupported_reasons: fact.pricing_unsupported_reasons.clone(),
            },
            cost: CostRecord {
                usd: fact.cost_usd.map(decimal_12),
                status: fact.cost_status,
                unavailable_reasons: fact.cost_unavailable_reasons.clone(),
            },
            result: ResultRecord {
                status_code: fact.status_code,
                upstream_status_code: fact.upstream_status_code,
                outcome: fact.outcome,
                e2e_ms: fact.e2e_ms,
                ttft_ms: fact.ttft_ms,
                upstream_attempted: fact.upstream_attempted,
                stream: fact.stream,
                cache_status: fact.cache_status,
            },
        }
    }
}

fn optional_entity(id: Option<Uuid>, name: Option<String>) -> Option<EntityRef> {
    if id.is_some() || name.is_some() {
        Some(EntityRef { id, name })
    } else {
        None
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AiUsagePage {
    pub data: Vec<AiUsageRecord>,
    pub offset: Option<String>,
    pub next: Option<String>,
    pub snapshot: String,
    pub meta: AiUsageMeta,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct OutcomeCounts {
    pub success: u64,
    pub gateway_rejected: u64,
    pub gateway_error: u64,
    pub upstream_error: u64,
    pub client_disconnected: u64,
    pub stream_interrupted: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PricingStatusCounts {
    pub matched: u64,
    pub unmatched: u64,
    pub unsupported: u64,
    pub not_applicable: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CostStatusCounts {
    pub calculated: u64,
    pub estimated: u64,
    pub not_incurred: u64,
    pub unavailable: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenAggregate {
    pub known_sum: String,
    pub known_requests: u64,
    pub unknown_requests: u64,
    pub coverage: Option<String>,
}

impl Default for TokenAggregate {
    fn default() -> Self {
        Self {
            known_sum: "0".to_string(),
            known_requests: 0,
            unknown_requests: 0,
            coverage: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AggregateMetrics {
    pub requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub outcomes: OutcomeCounts,
    pub prompt_tokens: TokenAggregate,
    pub completion_tokens: TokenAggregate,
    pub total_tokens: TokenAggregate,
    pub cost_usd_calculable_sum: String,
    pub pricing_status: PricingStatusCounts,
    pub cost_status: CostStatusCounts,
    pub estimated_usage_ratio: Option<String>,
    pub pricing_coverage: Option<String>,
    pub cost_calculable_coverage: Option<String>,
    pub avg_e2e_ms: Option<String>,
    pub p95_e2e_ms: Option<String>,
    pub avg_ttft_ms: Option<String>,
    pub cache_hits: u64,
}

impl Default for AggregateMetrics {
    fn default() -> Self {
        Self {
            requests: 0,
            successful_requests: 0,
            failed_requests: 0,
            outcomes: OutcomeCounts::default(),
            prompt_tokens: TokenAggregate::default(),
            completion_tokens: TokenAggregate::default(),
            total_tokens: TokenAggregate::default(),
            cost_usd_calculable_sum: "0.000000000000".to_string(),
            pricing_status: PricingStatusCounts::default(),
            cost_status: CostStatusCounts::default(),
            estimated_usage_ratio: None,
            pricing_coverage: None,
            cost_calculable_coverage: None,
            avg_e2e_ms: None,
            p95_e2e_ms: None,
            avg_ttft_ms: None,
            cache_hits: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DimensionRef {
    pub id: Option<Uuid>,
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub dimension_type: Option<String>,
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BreakdownItem {
    pub key: Option<String>,
    pub label: Option<String>,
    pub is_other: bool,
    pub bucket_start: Option<DateTime<Utc>>,
    pub bucket_end: Option<DateTime<Utc>>,
    pub dimension: Option<DimensionRef>,
    pub metrics: AggregateMetrics,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiUsageBreakdown {
    #[serde(rename = "type")]
    pub breakdown_type: BreakdownType,
    pub timezone: Option<String>,
    pub order_by: Option<SummaryOrder>,
    pub limit: Option<usize>,
    pub items: Vec<BreakdownItem>,
    pub other: Option<BreakdownItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiUsageSummary {
    pub snapshot: String,
    pub meta: AiUsageMeta,
    pub totals: AggregateMetrics,
    pub breakdown: Option<AiUsageBreakdown>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BatchWriteResult {
    pub inserted: u64,
    pub duplicate: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum AiUsageError {
    #[error("{0}")]
    InvalidQuery(String),
    #[error("{0}")]
    SnapshotExpired(String),
    #[error("{0}")]
    QueryTimeout(String),
    #[error("{0}")]
    QueryUnavailable(String),
    #[error("{0}")]
    WriteOutcomeUnknown(String),
    #[error("{0}")]
    Internal(String),
}

impl From<sqlx::Error> for AiUsageError {
    fn from(error: sqlx::Error) -> Self {
        let message = error.to_string();
        if matches!(
            &error,
            sqlx::Error::Io(_)
                | sqlx::Error::Tls(_)
                | sqlx::Error::Protocol(_)
                | sqlx::Error::PoolTimedOut
                | sqlx::Error::PoolClosed
                | sqlx::Error::WorkerCrashed
                | sqlx::Error::BeginFailed
        ) {
            return Self::QueryUnavailable(message);
        }
        let sqlstate = error
            .as_database_error()
            .and_then(|database_error| database_error.code())
            .map(|code| code.into_owned());
        match sqlstate.as_deref() {
            Some("57014") => Self::QueryTimeout(message),
            Some(sqlstate) if sqlstate_is_query_unavailable(sqlstate) => {
                Self::QueryUnavailable(message)
            }
            Some(sqlstate) => Self::Internal(format!("sqlstate {sqlstate}: {message}")),
            None => Self::Internal(message),
        }
    }
}

fn sqlstate_is_query_unavailable(sqlstate: &str) -> bool {
    sqlstate.starts_with("08")
        || sqlstate.starts_with("53")
        || sqlstate.starts_with("57P")
        || matches!(sqlstate, "40001" | "40P01" | "55P03" | "58000" | "58030")
}

pub type AiUsageResult<T> = Result<T, AiUsageError>;

pub fn decimal_12(value: Decimal) -> String {
    format!("{:.12}", value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_contract_uses_stable_snake_case_values() {
        assert_eq!(UsageSource::Mixed.to_string(), "mixed");
        assert_eq!(
            serde_json::to_value(PricingStatus::NotApplicable).unwrap(),
            "not_applicable"
        );
        assert_eq!(
            "stream_interrupted".parse::<AiUsageOutcome>().unwrap(),
            AiUsageOutcome::StreamInterrupted
        );
    }

    #[test]
    fn transient_sqlx_pool_and_io_errors_are_query_unavailable() {
        for error in [
            sqlx::Error::PoolTimedOut,
            sqlx::Error::PoolClosed,
            sqlx::Error::WorkerCrashed,
            sqlx::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "connection reset",
            )),
            sqlx::Error::Protocol("connection protocol failure".to_string()),
            sqlx::Error::BeginFailed,
        ] {
            assert!(matches!(
                AiUsageError::from(error),
                AiUsageError::QueryUnavailable(_)
            ));
        }
        for sqlstate in [
            "08006", "53300", "57P01", "57P05", "40001", "40P01", "55P03", "58000", "58030",
        ] {
            assert!(sqlstate_is_query_unavailable(sqlstate), "{sqlstate}");
        }
        assert!(!sqlstate_is_query_unavailable("23505"));
        assert!(!sqlstate_is_query_unavailable("57014"));
    }

    #[test]
    fn decimal_api_projection_always_has_twelve_places() {
        assert_eq!(decimal_12(Decimal::new(11, 4)), "0.001100000000");
        assert_eq!(decimal_12(Decimal::ZERO), "0.000000000000");
    }
}

//! AI Gateway usage 明细、汇总与 writer 可观测性端点。

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::Arc;

use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use kong_ai::usage::cursor::{decode_offset, decode_snapshot, normalize_millis, validate_window};
use kong_ai::usage::{
    AiUsageError, AiUsageFilter, AiUsageListQuery, AiUsageOffset, AiUsageRuntime, AiUsageSnapshot,
    AiUsageStore, AiUsageSummaryQuery, AiUsageWriterStatsSnapshot, BreakdownType, SummaryOrder,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::AdminState;

const DEFAULT_WINDOW: Duration = Duration::hours(24);
const MAX_WINDOW: Duration = Duration::days(90);
const MAX_HOUR_BREAKDOWN_WINDOW: Duration = Duration::days(31);
const MAX_RAW_QUERY_LEN: usize = 64 * 1024;

const COMMON_KEYS: &[&str] = &[
    "start",
    "end",
    "snapshot",
    "request_id",
    "route_id",
    "service_id",
    "provider_id",
    "provider_type",
    "requested_model",
    "model_group",
    "actual_model",
    "virtual_key_id",
    "consumer_id",
    "status_code",
    "outcome",
    "stream",
    "cache_status",
    "usage_source",
    "pricing_status",
    "cost_status",
];

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUsageParams {
    start: Option<String>,
    end: Option<String>,
    snapshot: Option<String>,
    request_id: Option<String>,
    route_id: Option<String>,
    service_id: Option<String>,
    provider_id: Option<String>,
    provider_type: Option<String>,
    requested_model: Option<String>,
    model_group: Option<String>,
    actual_model: Option<String>,
    virtual_key_id: Option<String>,
    consumer_id: Option<String>,
    status_code: Option<String>,
    outcome: Option<String>,
    stream: Option<String>,
    cache_status: Option<String>,
    usage_source: Option<String>,
    pricing_status: Option<String>,
    cost_status: Option<String>,
    size: Option<String>,
    offset: Option<String>,
    breakdown: Option<String>,
    timezone: Option<String>,
    limit: Option<String>,
    order_by: Option<String>,
}

struct ParsedRawQuery {
    params: RawUsageParams,
    values: BTreeMap<String, String>,
}

/// GET /ai-usage — 请求级 AI usage 明细。
pub async fn list(State(state): State<AdminState>, RawQuery(raw_query): RawQuery) -> Response {
    let (store, default_workspace_id) = match supported_runtime(&state.ai_usage) {
        Ok(runtime) => runtime,
        Err(response) => return response,
    };
    let mut allowed = COMMON_KEYS.to_vec();
    allowed.extend(["size", "offset"]);
    let parsed = match parse_raw_query(raw_query.as_deref(), &allowed) {
        Ok(parsed) => parsed,
        Err(error) => return analytics_error_response(error),
    };

    let explicit_snapshot = match parsed
        .params
        .snapshot
        .as_deref()
        .map(decode_snapshot)
        .transpose()
    {
        Ok(snapshot) => snapshot,
        Err(error) => return analytics_error_response(error),
    };
    let offset = match parsed
        .params
        .offset
        .as_deref()
        .map(decode_offset)
        .transpose()
    {
        Ok(offset) => offset,
        Err(error) => return analytics_error_response(error),
    };
    let token_snapshot = match reconcile_snapshot(explicit_snapshot, offset.as_ref()) {
        Ok(snapshot) => snapshot,
        Err(error) => return analytics_error_response(error),
    };
    let filter = match build_filter(
        &parsed.params,
        default_workspace_id,
        token_snapshot.as_ref(),
        Utc::now(),
    ) {
        Ok(filter) => filter,
        Err(error) => return analytics_error_response(error),
    };
    let size = match parse_bounded_usize("size", parsed.params.size.as_deref(), 100, 1, 1000) {
        Ok(size) => size,
        Err(error) => return analytics_error_response(error),
    };
    let snapshot = match token_snapshot {
        Some(snapshot) => snapshot,
        None => match store.snapshot(&filter).await {
            Ok(snapshot) => snapshot,
            Err(error) => return analytics_error_response(error),
        },
    };
    let query = AiUsageListQuery {
        filter,
        snapshot,
        offset,
        size,
    };
    let mut page = match store.list(&query).await {
        Ok(page) => page,
        Err(error) => return analytics_error_response(error),
    };
    page.next = page
        .offset
        .as_deref()
        .map(|offset| next_link(parsed.values, &page.snapshot, offset));
    Json(page).into_response()
}

/// GET /ai-usage/summary — AI usage 汇总与单一 breakdown。
pub async fn summary(State(state): State<AdminState>, RawQuery(raw_query): RawQuery) -> Response {
    let (store, default_workspace_id) = match supported_runtime(&state.ai_usage) {
        Ok(runtime) => runtime,
        Err(response) => return response,
    };
    let mut allowed = COMMON_KEYS.to_vec();
    allowed.extend(["breakdown", "timezone", "limit", "order_by"]);
    let parsed = match parse_raw_query(raw_query.as_deref(), &allowed) {
        Ok(parsed) => parsed,
        Err(error) => return analytics_error_response(error),
    };
    let snapshot = match parsed
        .params
        .snapshot
        .as_deref()
        .map(decode_snapshot)
        .transpose()
    {
        Ok(snapshot) => snapshot,
        Err(error) => return analytics_error_response(error),
    };
    let filter = match build_filter(
        &parsed.params,
        default_workspace_id,
        snapshot.as_ref(),
        Utc::now(),
    ) {
        Ok(filter) => filter,
        Err(error) => return analytics_error_response(error),
    };
    let (breakdown, timezone, limit, order_by) =
        match parse_summary_options(&parsed.params, &filter) {
            Ok(options) => options,
            Err(error) => return analytics_error_response(error),
        };
    let snapshot = match snapshot {
        Some(snapshot) => snapshot,
        None => match store.snapshot(&filter).await {
            Ok(snapshot) => snapshot,
            Err(error) => return analytics_error_response(error),
        },
    };
    let query = AiUsageSummaryQuery {
        filter,
        snapshot,
        breakdown,
        timezone,
        limit,
        order_by,
    };
    match store.summary(&query).await {
        Ok(summary) => Json(summary).into_response(),
        Err(error) => analytics_error_response(error),
    }
}

fn supported_runtime(runtime: &AiUsageRuntime) -> Result<(Arc<dyn AiUsageStore>, Uuid), Response> {
    match runtime {
        AiUsageRuntime::Supported {
            store,
            default_workspace_id,
            ..
        } => Ok((Arc::clone(store), *default_workspace_id)),
        AiUsageRuntime::UnsupportedHybrid => Err(analytics_response(
            StatusCode::NOT_IMPLEMENTED,
            "Analytics are unsupported in hybrid mode",
            "analytics_unsupported_in_hybrid",
        )),
    }
}

fn parse_raw_query(
    raw_query: Option<&str>,
    allowed: &[&str],
) -> Result<ParsedRawQuery, AiUsageError> {
    let raw_query = raw_query.unwrap_or_default();
    if raw_query.len() > MAX_RAW_QUERY_LEN {
        return invalid_query("查询字符串过长");
    }

    let mut values = BTreeMap::new();
    for pair in raw_query.split('&').filter(|pair| !pair.is_empty()) {
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode_form_component(raw_key)?;
        let value = decode_form_component(raw_value)?;
        if key.is_empty() || !allowed.contains(&key.as_str()) {
            return invalid_query(format!("未知查询参数: {key}"));
        }
        if values.insert(key.clone(), value).is_some() {
            return invalid_query(format!("查询参数不能重复: {key}"));
        }
    }

    let object: Map<String, Value> = values
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    let params = serde_json::from_value(Value::Object(object))
        .map_err(|_| AiUsageError::InvalidQuery("查询参数格式无效".to_string()))?;
    Ok(ParsedRawQuery { params, values })
}

fn decode_form_component(value: &str) -> Result<String, AiUsageError> {
    let input = value.as_bytes();
    let mut decoded = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        match input[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' => {
                if index + 2 >= input.len() {
                    return invalid_query("查询参数包含无效百分号编码");
                }
                let high = decode_hex(input[index + 1]).ok_or_else(|| {
                    AiUsageError::InvalidQuery("查询参数包含无效百分号编码".to_string())
                })?;
                let low = decode_hex(input[index + 2]).ok_or_else(|| {
                    AiUsageError::InvalidQuery("查询参数包含无效百分号编码".to_string())
                })?;
                decoded.push((high << 4) | low);
                index += 3;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded)
        .map_err(|_| AiUsageError::InvalidQuery("查询参数不是有效 UTF-8".to_string()))
}

fn decode_hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn reconcile_snapshot(
    explicit: Option<AiUsageSnapshot>,
    offset: Option<&AiUsageOffset>,
) -> Result<Option<AiUsageSnapshot>, AiUsageError> {
    match (explicit, offset) {
        (Some(snapshot), Some(offset)) if snapshot != offset.snapshot => {
            invalid_query("offset 与 snapshot 不一致")
        }
        (Some(snapshot), _) => Ok(Some(snapshot)),
        (None, Some(offset)) => Ok(Some(offset.snapshot.clone())),
        (None, None) => Ok(None),
    }
}

fn build_filter(
    params: &RawUsageParams,
    workspace_id: Uuid,
    snapshot: Option<&AiUsageSnapshot>,
    now: DateTime<Utc>,
) -> Result<AiUsageFilter, AiUsageError> {
    let (start, end) = resolve_window(params, snapshot, now)?;
    Ok(AiUsageFilter {
        workspace_id,
        start,
        end,
        request_id: parse_request_id(params.request_id.as_deref())?,
        route_id: parse_uuid("route_id", params.route_id.as_deref())?,
        service_id: parse_uuid("service_id", params.service_id.as_deref())?,
        provider_id: parse_uuid("provider_id", params.provider_id.as_deref())?,
        provider_type: parse_string("provider_type", params.provider_type.as_deref())?,
        requested_model: parse_string("requested_model", params.requested_model.as_deref())?,
        model_group: parse_string("model_group", params.model_group.as_deref())?,
        actual_model: parse_string("actual_model", params.actual_model.as_deref())?,
        virtual_key_id: parse_uuid("virtual_key_id", params.virtual_key_id.as_deref())?,
        consumer_id: parse_uuid("consumer_id", params.consumer_id.as_deref())?,
        status_code: parse_status(params.status_code.as_deref())?,
        outcome: parse_enum("outcome", params.outcome.as_deref())?,
        stream: parse_bool("stream", params.stream.as_deref())?,
        cache_status: parse_enum("cache_status", params.cache_status.as_deref())?,
        usage_source: parse_enum("usage_source", params.usage_source.as_deref())?,
        pricing_status: parse_enum("pricing_status", params.pricing_status.as_deref())?,
        cost_status: parse_enum("cost_status", params.cost_status.as_deref())?,
    })
}

fn resolve_window(
    params: &RawUsageParams,
    snapshot: Option<&AiUsageSnapshot>,
    now: DateTime<Utc>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), AiUsageError> {
    let window = match (params.start.as_deref(), params.end.as_deref()) {
        (Some(start), Some(end)) => (
            parse_timestamp("start", start)?,
            parse_timestamp("end", end)?,
        ),
        (None, None) => snapshot
            .map(|snapshot| (snapshot.start, snapshot.end))
            .unwrap_or_else(|| {
                let end = normalize_millis(now);
                (end - DEFAULT_WINDOW, end)
            }),
        _ => return invalid_query("start 和 end 必须同时提供"),
    };
    validate_exact_window(window.0, window.1)?;
    Ok(window)
}

fn parse_timestamp(name: &str, value: &str) -> Result<DateTime<Utc>, AiUsageError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| normalize_millis(timestamp.with_timezone(&Utc)))
        .map_err(|_| AiUsageError::InvalidQuery(format!("{name} 必须是 RFC3339 时间")))
}

fn validate_exact_window(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<(), AiUsageError> {
    validate_window(start, end)?;
    if end - start > MAX_WINDOW {
        return invalid_query("时间窗口不能超过 90 天");
    }
    Ok(())
}

fn parse_request_id(value: Option<&str>) -> Result<Option<String>, AiUsageError> {
    value
        .map(|value| {
            if value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                Ok(value.to_string())
            } else {
                invalid_query("request_id 必须是 32 位小写十六进制")
            }
        })
        .transpose()
}

fn parse_uuid(name: &str, value: Option<&str>) -> Result<Option<Uuid>, AiUsageError> {
    value
        .map(|value| {
            Uuid::parse_str(value)
                .map_err(|_| AiUsageError::InvalidQuery(format!("{name} 必须是合法 UUID")))
        })
        .transpose()
}

fn parse_string(name: &str, value: Option<&str>) -> Result<Option<String>, AiUsageError> {
    value
        .map(|value| {
            if value.is_empty() {
                invalid_query(format!("{name} 不能为空"))
            } else if value.contains('\0') {
                invalid_query(format!("{name} 不能包含 NUL 字符"))
            } else {
                Ok(value.to_string())
            }
        })
        .transpose()
}

fn parse_status(value: Option<&str>) -> Result<Option<i16>, AiUsageError> {
    value
        .map(|value| {
            let status = parse_digits("status_code", value)?;
            if !(100..=599).contains(&status) {
                return invalid_query("status_code 必须在 100 到 599 之间");
            }
            Ok(status as i16)
        })
        .transpose()
}

fn parse_bool(name: &str, value: Option<&str>) -> Result<Option<bool>, AiUsageError> {
    value
        .map(|value| match value {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => invalid_query(format!("{name} 必须是 true 或 false")),
        })
        .transpose()
}

fn parse_enum<T>(name: &str, value: Option<&str>) -> Result<Option<T>, AiUsageError>
where
    T: std::str::FromStr,
{
    value
        .map(|value| {
            value
                .parse()
                .map_err(|_| AiUsageError::InvalidQuery(format!("{name} 的值无效")))
        })
        .transpose()
}

fn parse_digits(name: &str, value: &str) -> Result<usize, AiUsageError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return invalid_query(format!("{name} 必须是整数"));
    }
    value
        .parse()
        .map_err(|_| AiUsageError::InvalidQuery(format!("{name} 超出范围")))
}

fn parse_bounded_usize(
    name: &str,
    value: Option<&str>,
    default: usize,
    minimum: usize,
    maximum: usize,
) -> Result<usize, AiUsageError> {
    let value = match value {
        Some(value) => parse_digits(name, value)?,
        None => default,
    };
    if !(minimum..=maximum).contains(&value) {
        return invalid_query(format!("{name} 必须在 {minimum} 到 {maximum} 之间"));
    }
    Ok(value)
}

fn parse_summary_options(
    params: &RawUsageParams,
    filter: &AiUsageFilter,
) -> Result<
    (
        Option<BreakdownType>,
        Option<chrono_tz::Tz>,
        Option<usize>,
        Option<SummaryOrder>,
    ),
    AiUsageError,
> {
    let breakdown: Option<BreakdownType> = parse_enum("breakdown", params.breakdown.as_deref())?;
    let Some(breakdown) = breakdown else {
        if params.timezone.is_some() || params.limit.is_some() || params.order_by.is_some() {
            return invalid_query("未指定 breakdown 时不能使用 timezone、limit 或 order_by");
        }
        return Ok((None, None, None, None));
    };

    if breakdown.is_time() {
        if params.limit.is_some() || params.order_by.is_some() {
            return invalid_query("时间 breakdown 不支持 limit 或 order_by");
        }
        if breakdown == BreakdownType::Hour && filter.end - filter.start > MAX_HOUR_BREAKDOWN_WINDOW
        {
            return invalid_query("hour breakdown 时间窗口不能超过 31 天");
        }
        let timezone = params
            .timezone
            .as_deref()
            .unwrap_or("UTC")
            .parse::<chrono_tz::Tz>()
            .map_err(|_| AiUsageError::InvalidQuery("timezone 必须是合法 IANA 时区".to_string()))?;
        Ok((Some(breakdown), Some(timezone), None, None))
    } else {
        if params.timezone.is_some() {
            return invalid_query("分类 breakdown 不支持 timezone");
        }
        let limit = parse_bounded_usize("limit", params.limit.as_deref(), 10, 1, 100)?;
        let order_by =
            parse_enum("order_by", params.order_by.as_deref())?.unwrap_or(SummaryOrder::CostUsd);
        Ok((Some(breakdown), None, Some(limit), Some(order_by)))
    }
}

fn next_link(mut values: BTreeMap<String, String>, snapshot: &str, offset: &str) -> String {
    values.insert("snapshot".to_string(), snapshot.to_string());
    values.insert("offset".to_string(), offset.to_string());
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in values {
        serializer.append_pair(&key, &value);
    }
    format!("/ai-usage?{}", serializer.finish())
}

fn invalid_query<T>(message: impl Into<String>) -> Result<T, AiUsageError> {
    Err(AiUsageError::InvalidQuery(message.into()))
}

fn analytics_error_response(error: AiUsageError) -> Response {
    match error {
        AiUsageError::InvalidQuery(message) => {
            analytics_response(StatusCode::BAD_REQUEST, &message, "analytics_invalid_query")
        }
        AiUsageError::SnapshotExpired(message) => {
            analytics_response(StatusCode::CONFLICT, &message, "analytics_snapshot_expired")
        }
        AiUsageError::QueryTimeout(message) => {
            tracing::warn!(error = %message, "AI usage 查询超时");
            analytics_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Analytics query timed out",
                "analytics_query_timeout",
            )
        }
        AiUsageError::QueryUnavailable(message) => {
            tracing::warn!(error = %message, "AI usage Store 暂时不可用");
            analytics_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Analytics are temporarily unavailable",
                "analytics_query_unavailable",
            )
        }
        AiUsageError::WriteOutcomeUnknown(message) => {
            tracing::error!(error = %message, "AI usage 查询意外遇到写入结果未知错误");
            analytics_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "Analytics are temporarily unavailable",
                "analytics_query_unavailable",
            )
        }
        AiUsageError::Internal(message) => {
            tracing::error!(error = %message, "AI usage 查询发生内部错误");
            analytics_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal analytics error",
                "analytics_internal_error",
            )
        }
    }
}

fn analytics_response(status: StatusCode, message: &str, error_code: &str) -> Response {
    (
        status,
        Json(json!({
            "message": message,
            "name": "analytics error",
            "code": 5,
            "error_code": error_code,
            "fields": {},
        })),
    )
        .into_response()
}

pub(crate) fn writer_stats_snapshot(
    runtime: &AiUsageRuntime,
) -> Option<AiUsageWriterStatsSnapshot> {
    match runtime {
        AiUsageRuntime::Supported { stats, .. } => Some(stats.snapshot()),
        AiUsageRuntime::UnsupportedHybrid => None,
    }
}

pub(crate) fn writer_prometheus_metrics(runtime: &AiUsageRuntime) -> String {
    let Some(stats) = writer_stats_snapshot(runtime) else {
        return String::new();
    };
    let mut output = String::new();
    let counters = [
        ("enqueued", stats.enqueued),
        ("written", stats.written),
        ("duplicate", stats.duplicate),
        ("dropped", stats.dropped),
        ("write_failures", stats.write_failures),
        ("retries", stats.retries),
        ("write_outcome_unknown", stats.write_outcome_unknown),
        ("dbless_evicted", stats.dbless_evicted),
        ("dropped_queue_full", stats.dropped_queue_full),
        ("dropped_writer_closed", stats.dropped_writer_closed),
        (
            "dropped_write_retries_exhausted",
            stats.dropped_write_retries_exhausted,
        ),
        ("dropped_shutdown_timeout", stats.dropped_shutdown_timeout),
    ];
    for (name, value) in counters {
        let metric = format!("kong_ai_usage_writer_{name}_total");
        let _ = writeln!(output, "# HELP {metric} AI usage writer {name} total.");
        let _ = writeln!(output, "# TYPE {metric} counter");
        let _ = writeln!(output, "{metric} {value}");
    }
    for (name, value) in [
        ("queue_depth", stats.queue_depth),
        ("queue_capacity", stats.queue_capacity),
    ] {
        let metric = format!("kong_ai_usage_writer_{name}");
        let _ = writeln!(output, "# HELP {metric} AI usage writer {name}.");
        let _ = writeln!(output, "# TYPE {metric} gauge");
        let _ = writeln!(output, "{metric} {value}");
    }
    output
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn raw_query_rejects_unknown_duplicate_and_invalid_encoding() {
        let allowed = ["size"];
        assert!(parse_raw_query(Some("workspace_id=x"), &allowed).is_err());
        assert!(parse_raw_query(Some("size=1&size=2"), &allowed).is_err());
        assert!(parse_raw_query(Some("size=%ZZ"), &allowed).is_err());
    }

    #[test]
    fn default_window_is_exactly_24_hours_and_millisecond_aligned() {
        let now = Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap()
            + Duration::nanoseconds(123_456_789);
        let params = RawUsageParams::default();
        let (start, end) = resolve_window(&params, None, now).unwrap();

        assert_eq!(end - start, Duration::hours(24));
        assert_eq!(end.timestamp_subsec_nanos(), 123_000_000);
    }

    #[test]
    fn explicit_window_is_paired_and_capped_at_exactly_90_days() {
        let start = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert!(validate_exact_window(start, start + Duration::days(90)).is_ok());
        assert!(validate_exact_window(
            start,
            start + Duration::days(90) + Duration::milliseconds(1)
        )
        .is_err());

        let params = RawUsageParams {
            start: Some(start.to_rfc3339()),
            ..Default::default()
        };
        assert!(resolve_window(&params, None, Utc::now()).is_err());
    }

    #[test]
    fn summary_parameter_scopes_are_strict() {
        let mut params = RawUsageParams {
            timezone: Some("UTC".to_string()),
            ..Default::default()
        };
        let filter = build_filter(&params, Uuid::nil(), None, Utc::now()).unwrap();
        assert!(parse_summary_options(&params, &filter).is_err());

        params.breakdown = Some("provider".to_string());
        assert!(parse_summary_options(&params, &filter).is_err());

        params.timezone = None;
        let (_, timezone, limit, order_by) = parse_summary_options(&params, &filter).unwrap();
        assert!(timezone.is_none());
        assert_eq!(limit, Some(10));
        assert_eq!(order_by, Some(SummaryOrder::CostUsd));
    }

    #[test]
    fn response_status_filters_and_request_ids_are_strict() {
        assert!(parse_status(Some("99")).is_err());
        assert!(parse_status(Some("600")).is_err());
        assert!(parse_status(Some("200")).is_ok());
        assert!(parse_request_id(Some("ABCDEF0123456789abcdef0123456789")).is_err());
        assert!(parse_request_id(Some("abcdef0123456789abcdef0123456789")).is_ok());
        assert!(parse_string("provider_type", Some("\0")).is_err());
        assert!(parse_string("provider_type", Some("openai")).is_ok());
    }

    #[tokio::test]
    async fn store_failures_do_not_expose_internal_diagnostics() {
        for (error, expected_code, expected_message) in [
            (
                AiUsageError::QueryTimeout("secret SQL and host".to_string()),
                "analytics_query_timeout",
                "Analytics query timed out",
            ),
            (
                AiUsageError::QueryUnavailable("secret SQL and host".to_string()),
                "analytics_query_unavailable",
                "Analytics are temporarily unavailable",
            ),
        ] {
            let response = analytics_error_response(error);
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["error_code"], expected_code);
            assert_eq!(body["message"], expected_message);
            assert!(!body.to_string().contains("secret SQL and host"));
        }
    }
}

//! 版本化 snapshot/offset token 的编解码与不可信输入校验。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Timelike, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::model::{
    AiUsageError, AiUsageFilter, AiUsageMode, AiUsageOffset, AiUsageResult, AiUsageSnapshot,
};

pub const CURSOR_VERSION: u8 = 1;
const MAX_TOKEN_LENGTH: usize = 8 * 1024;
const MAX_QUERY_WINDOW_DAYS: i64 = 90;

pub fn normalize_millis(value: DateTime<Utc>) -> DateTime<Utc> {
    value
        .with_nanosecond(value.timestamp_subsec_millis() * 1_000_000)
        .expect("毫秒精度始终是合法纳秒值")
}

pub fn filter_hash(filter: &AiUsageFilter) -> AiUsageResult<String> {
    let encoded = serde_json::to_vec(filter)
        .map_err(|error| AiUsageError::Internal(format!("过滤器编码失败: {error}")))?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

pub fn encode_snapshot(snapshot: &AiUsageSnapshot) -> AiUsageResult<String> {
    encode(snapshot)
}

pub fn decode_snapshot(token: &str) -> AiUsageResult<AiUsageSnapshot> {
    decode(token, "snapshot")
}

pub fn encode_offset(offset: &AiUsageOffset) -> AiUsageResult<String> {
    encode(offset)
}

pub fn decode_offset(token: &str) -> AiUsageResult<AiUsageOffset> {
    decode(token, "offset")
}

pub fn validate_snapshot(
    snapshot: &AiUsageSnapshot,
    filter: &AiUsageFilter,
    backend: AiUsageMode,
    current_high_watermark: i64,
) -> AiUsageResult<()> {
    validate_snapshot_binding(snapshot, filter, backend)?;
    validate_snapshot_watermark(snapshot, current_high_watermark)
}

pub fn validate_snapshot_binding(
    snapshot: &AiUsageSnapshot,
    filter: &AiUsageFilter,
    backend: AiUsageMode,
) -> AiUsageResult<()> {
    if snapshot.v != CURSOR_VERSION {
        return invalid("snapshot 版本不受支持");
    }
    if snapshot.backend != backend {
        return invalid("snapshot backend 与当前运行模式不一致");
    }
    if snapshot.workspace_id != filter.workspace_id {
        return invalid("snapshot workspace 与当前查询不一致");
    }
    if snapshot.start != filter.start || snapshot.end != filter.end {
        return invalid("snapshot 时间窗口与当前查询不一致");
    }
    validate_window(snapshot.start, snapshot.end)?;
    if snapshot.high_watermark < 0 {
        return invalid("snapshot 写入水位无效");
    }
    match snapshot.backend {
        AiUsageMode::Postgres
            if snapshot.eviction_generation.is_some() || snapshot.ring_instance_id.is_some() =>
        {
            return invalid("PostgreSQL snapshot 不应包含 DB-less ring 状态");
        }
        AiUsageMode::Dbless
            if snapshot.eviction_generation.is_none() || snapshot.ring_instance_id.is_none() =>
        {
            return invalid("DB-less snapshot 缺少 ring 状态");
        }
        _ => {}
    }
    if snapshot.filter_hash != filter_hash(filter)? {
        return invalid("snapshot 过滤条件与当前查询不一致");
    }
    Ok(())
}

pub fn validate_snapshot_watermark(
    snapshot: &AiUsageSnapshot,
    current_high_watermark: i64,
) -> AiUsageResult<()> {
    if snapshot.high_watermark > current_high_watermark {
        return invalid("snapshot 写入水位无效");
    }
    Ok(())
}

pub fn validate_offset(offset: &AiUsageOffset, snapshot: &AiUsageSnapshot) -> AiUsageResult<()> {
    if offset.v != CURSOR_VERSION {
        return invalid("offset 版本不受支持");
    }
    if &offset.snapshot != snapshot {
        return invalid("offset 与 snapshot 不一致");
    }
    if offset.last_started_at < snapshot.start || offset.last_started_at >= snapshot.end {
        return invalid("offset 排序键不在 snapshot 时间窗口内");
    }
    Ok(())
}

pub fn validate_window(start: DateTime<Utc>, end: DateTime<Utc>) -> AiUsageResult<()> {
    if start >= end {
        return invalid("start 必须早于 end");
    }
    if end.signed_duration_since(start) > chrono::Duration::days(MAX_QUERY_WINDOW_DAYS) {
        return invalid("时间窗口不能超过 90 天");
    }
    if start != normalize_millis(start) || end != normalize_millis(end) {
        return invalid("时间边界必须规范化为毫秒精度");
    }
    Ok(())
}

fn encode<T: Serialize>(value: &T) -> AiUsageResult<String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| AiUsageError::Internal(format!("cursor 编码失败: {error}")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode<T: DeserializeOwned>(token: &str, name: &str) -> AiUsageResult<T> {
    if token.is_empty() || token.len() > MAX_TOKEN_LENGTH {
        return invalid(format!("{name} 长度无效"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| AiUsageError::InvalidQuery(format!("{name} 不是合法 base64url")))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| AiUsageError::InvalidQuery(format!("{name} 内容无效")))
}

fn invalid<T>(message: impl Into<String>) -> AiUsageResult<T> {
    Err(AiUsageError::InvalidQuery(message.into()))
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::usage::model::AiUsageFilter;

    fn filter() -> AiUsageFilter {
        AiUsageFilter {
            workspace_id: Uuid::nil(),
            start: Utc.with_ymd_and_hms(2026, 7, 26, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 7, 27, 0, 0, 0).unwrap(),
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

    #[test]
    fn snapshot_round_trip_and_filter_binding() {
        let filter = filter();
        let snapshot = AiUsageSnapshot {
            v: CURSOR_VERSION,
            backend: AiUsageMode::Postgres,
            workspace_id: filter.workspace_id,
            start: filter.start,
            end: filter.end,
            high_watermark: 9,
            eviction_generation: None,
            ring_instance_id: None,
            filter_hash: filter_hash(&filter).unwrap(),
        };
        let encoded = encode_snapshot(&snapshot).unwrap();
        let decoded = decode_snapshot(&encoded).unwrap();
        assert_eq!(decoded, snapshot);
        validate_snapshot(&decoded, &filter, AiUsageMode::Postgres, 9).unwrap();

        let mut changed = filter;
        changed.provider_type = Some("openai".to_string());
        assert!(validate_snapshot(&decoded, &changed, AiUsageMode::Postgres, 9).is_err());
    }
}

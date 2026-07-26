//! AI 执行错误体与实时配额响应头。

use std::time::Duration;

use kong_core::traits::RequestCtx;

use crate::ratelimit::{ExceededDimension, RateLimitSnapshot};

use super::policy::AiClientProtocol;

const QUOTA_HEADERS: [&str; 7] = [
    "x-ratelimit-limit-requests",
    "x-ratelimit-remaining-requests",
    "x-ratelimit-reset-requests",
    "x-ratelimit-limit-tokens",
    "x-ratelimit-remaining-tokens",
    "x-ratelimit-reset-tokens",
    "retry-after",
];

/// quota header 的响应场景。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaHeaderMode {
    Allowed,
    Rejected(ExceededDimension),
}

/// 使用已解析客户端协议生成固定错误体并短路请求。
pub fn reject_with_protocol_error(
    ctx: &mut RequestCtx,
    protocol: AiClientProtocol,
    status: u16,
    code: &str,
    message: &str,
) {
    let error_type = match status {
        401 => "invalid_request_error",
        403 => "insufficient_quota",
        429 => "rate_limit_error",
        500..=599 => "server_error",
        _ => "gateway_error",
    };
    let body = match protocol {
        AiClientProtocol::OpenAi => serde_json::json!({
            "error": {
                "message": message,
                "type": error_type,
                "param": null,
                "code": code,
            }
        }),
        AiClientProtocol::Anthropic => serde_json::json!({
            "type": "error",
            "error": {
                "type": code,
                "message": message,
            },
            "request_id": ctx.lifecycle.request_id.clone(),
        }),
    };
    ctx.short_circuited = true;
    ctx.exit_status = Some(status);
    ctx.exit_body = Some(body.to_string());
}

/// 清除之前暂存的 quota headers。
///
/// backend 结果不确定时必须调用本函数，避免把准入快照伪装成退款后的最终值。
pub fn clear_quota_headers(ctx: &mut RequestCtx) {
    ctx.response_headers_to_set.retain(|(name, _)| {
        !QUOTA_HEADERS
            .iter()
            .any(|header| name.eq_ignore_ascii_case(header))
    });
    for name in QUOTA_HEADERS {
        if !ctx
            .response_headers_to_remove
            .iter()
            .any(|value| value.eq_ignore_ascii_case(name))
        {
            ctx.response_headers_to_remove.push(name.to_string());
        }
    }
}

/// 从 Store 返回的同一原子快照写入 quota headers。
pub fn apply_quota_headers(
    ctx: &mut RequestCtx,
    snapshot: &RateLimitSnapshot,
    mode: QuotaHeaderMode,
) {
    clear_quota_headers(ctx);
    ctx.response_headers_to_remove.retain(|name| {
        !QUOTA_HEADERS
            .iter()
            .any(|header| name.eq_ignore_ascii_case(header))
    });

    if let Some(requests) = snapshot.requests {
        upsert_header(
            ctx,
            "X-RateLimit-Limit-Requests",
            requests.limit.to_string(),
        );
        upsert_header(
            ctx,
            "X-RateLimit-Remaining-Requests",
            requests.remaining.to_string(),
        );
        upsert_header(
            ctx,
            "X-RateLimit-Reset-Requests",
            reset_seconds(snapshot.window.reset_after).to_string(),
        );
    }
    if let Some(tokens) = snapshot.tokens {
        upsert_header(ctx, "X-RateLimit-Limit-Tokens", tokens.limit.to_string());
        upsert_header(
            ctx,
            "X-RateLimit-Remaining-Tokens",
            tokens.remaining.to_string(),
        );
        upsert_header(
            ctx,
            "X-RateLimit-Reset-Tokens",
            reset_seconds(snapshot.window.reset_after).to_string(),
        );
    }

    if let QuotaHeaderMode::Rejected(reason) = mode {
        let retry_after = match reason {
            ExceededDimension::Requests if snapshot.requests.is_some() => {
                Some(reset_seconds(snapshot.window.reset_after))
            }
            ExceededDimension::Tokens if snapshot.tokens.is_some() => {
                Some(reset_seconds(snapshot.window.reset_after))
            }
            ExceededDimension::RequestsAndTokens
                if snapshot.requests.is_some() || snapshot.tokens.is_some() =>
            {
                // v1 两个维度共享一个窗口；保留分支便于未来多窗口 adapter 扩展。
                Some(reset_seconds(snapshot.window.reset_after))
            }
            _ => None,
        };
        if let Some(retry_after) = retry_after {
            upsert_header(ctx, "Retry-After", retry_after.to_string());
        }
    }
}

fn reset_seconds(duration: Duration) -> u64 {
    let millis = duration.as_millis();
    let seconds = millis.saturating_add(999) / 1_000;
    u64::try_from(seconds.max(1)).unwrap_or(u64::MAX)
}

fn upsert_header(ctx: &mut RequestCtx, name: &str, value: String) {
    ctx.response_headers_to_set
        .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
    ctx.response_headers_to_set.push((name.to_string(), value));
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use crate::ratelimit::{DimensionSnapshot, RateLimitSnapshot, WindowAlgorithm, WindowSnapshot};

    use super::*;

    fn snapshot(reset_after: Duration) -> RateLimitSnapshot {
        RateLimitSnapshot {
            window: WindowSnapshot {
                identity: None,
                algorithm: WindowAlgorithm::FixedFirstHit,
                duration: Duration::from_secs(60),
                started_at: SystemTime::UNIX_EPOCH,
                reset_at: SystemTime::UNIX_EPOCH + Duration::from_secs(60),
                reset_after,
            },
            requests: Some(DimensionSnapshot {
                limit: 10,
                used: 4,
                remaining: 6,
            }),
            tokens: Some(DimensionSnapshot {
                limit: 1_000,
                used: 250,
                remaining: 750,
            }),
        }
    }

    #[test]
    fn quota_headers_use_authoritative_snapshot_and_ceil_reset() {
        let mut ctx = RequestCtx::new();

        apply_quota_headers(
            &mut ctx,
            &snapshot(Duration::from_millis(1_001)),
            QuotaHeaderMode::Rejected(ExceededDimension::Tokens),
        );

        assert!(ctx.response_headers_to_set.contains(&(
            "X-RateLimit-Remaining-Requests".to_string(),
            "6".to_string()
        )));
        assert!(ctx.response_headers_to_set.contains(&(
            "X-RateLimit-Remaining-Tokens".to_string(),
            "750".to_string()
        )));
        assert!(ctx
            .response_headers_to_set
            .contains(&("X-RateLimit-Reset-Tokens".to_string(), "2".to_string())));
        assert!(ctx
            .response_headers_to_set
            .contains(&("Retry-After".to_string(), "2".to_string())));
    }

    #[test]
    fn protocol_errors_do_not_leak_openai_shape_to_anthropic_clients() {
        let mut ctx = RequestCtx::new();

        reject_with_protocol_error(
            &mut ctx,
            AiClientProtocol::Anthropic,
            503,
            "quota_backend_unavailable",
            "Quota backend unavailable",
        );

        let body: serde_json::Value =
            serde_json::from_str(ctx.exit_body.as_deref().unwrap()).unwrap();
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "quota_backend_unavailable");
        assert!(body["error"].get("code").is_none());
    }

    #[test]
    fn openai_error_type_follows_status_contract() {
        let mut ctx = RequestCtx::new();

        reject_with_protocol_error(
            &mut ctx,
            AiClientProtocol::OpenAi,
            403,
            "budget_exhausted",
            "The virtual key budget has been exhausted.",
        );

        let body: serde_json::Value =
            serde_json::from_str(ctx.exit_body.as_deref().unwrap()).unwrap();
        assert_eq!(body["error"]["type"], "insufficient_quota");
        assert_eq!(body["error"]["code"], "budget_exhausted");
        assert!(body["error"]["param"].is_null());
    }
}

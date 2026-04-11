//! gRPC proxy support — gRPC 代理支持
//!
//! Provides gRPC-specific error response formatting and HTTP-to-gRPC status code mapping.
//! Kong returns gRPC Trailers-Only responses when a gRPC request hits a framework-level
//! error (no route, service unavailable, plugin short-circuit, etc.).
//! 提供 gRPC 特定的错误响应格式化和 HTTP → gRPC 状态码映射。
//! 当 gRPC 请求命中框架级错误时，Kong 返回 gRPC Trailers-Only 响应。

use bytes::Bytes;
use percent_encoding::{percent_encode, NON_ALPHANUMERIC};
use pingora_http::ResponseHeader;
use pingora_proxy::Session;

/// gRPC status codes (subset used by Kong) — gRPC 状态码（Kong 使用的子集）
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub enum GrpcStatus {
    // Ok = 0,
    Cancelled = 1,
    // Unknown = 2,
    InvalidArgument = 3,
    DeadlineExceeded = 4,
    NotFound = 5,
    AlreadyExists = 6,
    PermissionDenied = 7,
    ResourceExhausted = 8,
    // FailedPrecondition = 9,
    // Aborted = 10,
    // OutOfRange = 11,
    Unimplemented = 12,
    Internal = 13,
    Unavailable = 14,
    // DataLoss = 15,
    Unauthenticated = 16,
}

/// Map HTTP status code to gRPC status code (Kong-compatible) — HTTP 状态码映射到 gRPC 状态码（与 Kong 兼容）
pub fn http_status_to_grpc(http_status: u16) -> GrpcStatus {
    match http_status {
        400 => GrpcStatus::InvalidArgument,
        401 => GrpcStatus::Unauthenticated,
        403 => GrpcStatus::PermissionDenied,
        404 => GrpcStatus::Unimplemented,
        405 => GrpcStatus::Unimplemented,
        408 => GrpcStatus::DeadlineExceeded,
        409 => GrpcStatus::AlreadyExists,
        429 => GrpcStatus::ResourceExhausted,
        499 => GrpcStatus::Cancelled,
        500 => GrpcStatus::Internal,
        501 => GrpcStatus::Unimplemented,
        502 => GrpcStatus::Unavailable,
        503 => GrpcStatus::Unavailable,
        504 => GrpcStatus::DeadlineExceeded,
        _ if http_status < 400 => GrpcStatus::Internal,
        _ => GrpcStatus::Internal,
    }
}

/// Check if the request is a gRPC request (content-type starts with "application/grpc")
/// 检查请求是否为 gRPC 请求（content-type 以 "application/grpc" 开头）
pub fn is_grpc_request(session: &Session) -> bool {
    session
        .req_header()
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.starts_with("application/grpc"))
        .unwrap_or(false)
}

/// Send a gRPC Trailers-Only error response (HTTP 200 + grpc-status/grpc-message headers).
/// Kong uses this format for framework-level errors on gRPC requests.
/// 发送 gRPC Trailers-Only 错误响应（HTTP 200 + grpc-status/grpc-message 头）。
/// Kong 在 gRPC 请求遇到框架级错误时使用此格式。
pub async fn send_grpc_error(
    session: &mut Session,
    http_status: u16,
    message: &str,
    request_id: Option<&str>,
    include_request_id: bool,
) -> pingora_core::Result<bool> {
    let grpc_status = http_status_to_grpc(http_status);
    let mut resp = ResponseHeader::build(200, Some(6))?;
    resp.insert_header("content-type", "application/grpc")?;
    resp.insert_header("grpc-status", (grpc_status as u32).to_string())?;
    // gRPC spec requires Percent-Encoded grpc-message (Kong uses ngx.escape_uri) — gRPC 规范要求 grpc-message 百分比编码
    let encoded_message = percent_encode(message.as_bytes(), NON_ALPHANUMERIC).to_string();
    resp.insert_header("grpc-message", &encoded_message)?;

    if include_request_id {
        if let Some(rid) = request_id {
            let _ = resp.insert_header("x-kong-request-id", rid);
        }
    }

    session
        .write_response_header(Box::new(resp), false)
        .await?;
    // gRPC Trailers-Only: empty body with end_of_stream=true — gRPC Trailers-Only：空 body + end_of_stream=true
    session.write_response_body(Some(Bytes::new()), true).await?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_to_grpc_status_mapping() {
        // Standard mappings — 标准映射
        assert!(matches!(http_status_to_grpc(400), GrpcStatus::InvalidArgument));
        assert!(matches!(http_status_to_grpc(401), GrpcStatus::Unauthenticated));
        assert!(matches!(http_status_to_grpc(403), GrpcStatus::PermissionDenied));
        assert!(matches!(http_status_to_grpc(404), GrpcStatus::Unimplemented));
        assert!(matches!(http_status_to_grpc(408), GrpcStatus::DeadlineExceeded));
        assert!(matches!(http_status_to_grpc(409), GrpcStatus::AlreadyExists));
        assert!(matches!(http_status_to_grpc(429), GrpcStatus::ResourceExhausted));
        assert!(matches!(http_status_to_grpc(499), GrpcStatus::Cancelled));
        assert!(matches!(http_status_to_grpc(500), GrpcStatus::Internal));
        assert!(matches!(http_status_to_grpc(501), GrpcStatus::Unimplemented));
        assert!(matches!(http_status_to_grpc(502), GrpcStatus::Unavailable));
        assert!(matches!(http_status_to_grpc(503), GrpcStatus::Unavailable));
        assert!(matches!(http_status_to_grpc(504), GrpcStatus::DeadlineExceeded));

        // Unknown 4xx/5xx → Internal — 未知 4xx/5xx → Internal
        assert!(matches!(http_status_to_grpc(418), GrpcStatus::Internal));
        assert!(matches!(http_status_to_grpc(599), GrpcStatus::Internal));

        // 2xx/3xx → Internal (should not happen but handles gracefully) — 2xx/3xx → Internal（不应发生但优雅处理）
        assert!(matches!(http_status_to_grpc(200), GrpcStatus::Internal));
    }

    #[test]
    fn test_grpc_status_code_values() {
        // Verify numeric values match gRPC spec — 验证数值与 gRPC 规范一致
        assert_eq!(GrpcStatus::Cancelled as u32, 1);
        assert_eq!(GrpcStatus::InvalidArgument as u32, 3);
        assert_eq!(GrpcStatus::DeadlineExceeded as u32, 4);
        assert_eq!(GrpcStatus::NotFound as u32, 5);
        assert_eq!(GrpcStatus::AlreadyExists as u32, 6);
        assert_eq!(GrpcStatus::PermissionDenied as u32, 7);
        assert_eq!(GrpcStatus::ResourceExhausted as u32, 8);
        assert_eq!(GrpcStatus::Unimplemented as u32, 12);
        assert_eq!(GrpcStatus::Internal as u32, 13);
        assert_eq!(GrpcStatus::Unavailable as u32, 14);
        assert_eq!(GrpcStatus::Unauthenticated as u32, 16);
    }
}

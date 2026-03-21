//! Mock upstream server for Kong spec tests — Kong spec 测试用 mock 上游服务器
//!
//! Implements a subset of httpbin-like endpoints that Kong's official spec tests
//! depend on. Runs on port 15555 (HTTP) by default.
//! 实现 Kong 官方 spec 测试依赖的 httpbin 风格端点，默认运行在 15555 (HTTP) 端口。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{any, delete, get, post};
use axum::Router;
use base64::Engine;
use serde::Serialize;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Utilities — 工具函数
// ---------------------------------------------------------------------------

/// Simple percent-decoding (no external crate needed) — 简单百分号解码
fn percent_decode(input: &str) -> String {
    let mut output = Vec::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().unwrap_or(b'0');
            let lo = chars.next().unwrap_or(b'0');
            let hex = [hi, lo];
            if let Ok(s) = std::str::from_utf8(&hex) {
                if let Ok(byte) = u8::from_str_radix(s, 16) {
                    output.push(byte);
                    continue;
                }
            }
            output.push(b'%');
            output.push(hi);
            output.push(lo);
        } else if b == b'+' {
            output.push(b' ');
        } else {
            output.push(b);
        }
    }
    String::from_utf8_lossy(&output).to_string()
}

// ---------------------------------------------------------------------------
// Shared state — 共享状态
// ---------------------------------------------------------------------------

/// Log storage: each named logger has a list of entries and a counter.
/// 日志存储：每个命名 logger 有一个条目列表和一个计数器。
#[derive(Clone)]
pub struct MockState {
    logs: Arc<RwLock<HashMap<String, LogStore>>>,
}

struct LogStore {
    entries: Vec<String>,
    count: usize,
}

impl MockState {
    fn new() -> Self {
        Self {
            logs: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

// ---------------------------------------------------------------------------
// Echo response — 回显响应结构体
// ---------------------------------------------------------------------------

/// Mirrors the JSON structure returned by Kong's mock_upstream.lua
/// 与 Kong mock_upstream.lua 返回的 JSON 结构保持一致
#[derive(Serialize)]
struct EchoResponse {
    headers: HashMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    post_data: Option<PostData>,
    url: String,
    uri_args: HashMap<String, serde_json::Value>,
    vars: EchoVars,
    // Optional extra fields per endpoint — 每个端点的可选额外字段
    #[serde(skip_serializing_if = "Option::is_none")]
    valid_routes: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delay: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    authenticated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
}

#[derive(Serialize)]
struct PostData {
    text: String,
    kind: String,
    params: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct EchoVars {
    uri: String,
    host: String,
    hostname: String,
    https: String,
    scheme: String,
    is_args: String,
    server_addr: String,
    server_port: String,
    server_name: String,
    server_protocol: String,
    remote_addr: String,
    request: String,
    request_uri: String,
    request_method: String,
}

// ---------------------------------------------------------------------------
// Helper: build echo response from request parts — 从请求部分构建回显响应
// ---------------------------------------------------------------------------

fn build_echo(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
    port: u16,
) -> EchoResponse {
    // headers — 请求头（小写键）
    let mut hdr_map: HashMap<String, serde_json::Value> = HashMap::new();
    for (name, value) in headers.iter() {
        let key = name.as_str().to_lowercase();
        let val = value.to_str().unwrap_or("").to_string();
        // Kong 的 mock upstream 对于多值 header 用逗号连接
        if let Some(existing) = hdr_map.get(&key) {
            if let Some(s) = existing.as_str() {
                hdr_map.insert(key, serde_json::Value::String(format!("{}, {}", s, val)));
            }
        } else {
            hdr_map.insert(key, serde_json::Value::String(val));
        }
    }

    // uri_args — 查询参数
    let mut uri_args: HashMap<String, serde_json::Value> = HashMap::new();
    if let Some(query) = uri.query() {
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                let k_decoded = percent_decode(k);
                let v_decoded = percent_decode(v);
                // Kong: duplicate keys become arrays — 重复 key 变数组
                if let Some(existing) = uri_args.get(&k_decoded) {
                    match existing {
                        serde_json::Value::Array(arr) => {
                            let mut arr = arr.clone();
                            arr.push(serde_json::Value::String(v_decoded));
                            uri_args.insert(k_decoded, serde_json::Value::Array(arr));
                        }
                        _ => {
                            uri_args.insert(
                                k_decoded,
                                serde_json::Value::Array(vec![
                                    existing.clone(),
                                    serde_json::Value::String(v_decoded),
                                ]),
                            );
                        }
                    }
                } else {
                    uri_args.insert(k_decoded, serde_json::Value::String(v_decoded));
                }
            } else if !pair.is_empty() {
                let k_decoded = percent_decode(pair);
                uri_args.insert(k_decoded, serde_json::Value::Bool(true));
            }
        }
    }

    // post_data — 请求体
    let post_data = if !body.is_empty() || method == Method::POST || method == Method::PUT || method == Method::PATCH {
        let text = String::from_utf8_lossy(body).to_string();
        let content_type = headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let (kind, params, error) = if content_type.contains("application/json") {
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(v) => ("json".to_string(), v, None),
                Err(e) => (
                    "json (error)".to_string(),
                    serde_json::Value::Null,
                    Some(e.to_string()),
                ),
            }
        } else if content_type.contains("application/x-www-form-urlencoded") {
            let mut params_map = serde_json::Map::new();
            for pair in text.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    let k_decoded = percent_decode(k);
                    let v_decoded = percent_decode(v);
                    params_map.insert(k_decoded, serde_json::Value::String(v_decoded));
                }
            }
            (
                "form".to_string(),
                serde_json::Value::Object(params_map),
                None,
            )
        } else if content_type.contains("multipart/form-data") {
            // Simplified multipart: Kong tests usually don't rely on deep multipart parsing
            // 简化 multipart：Kong 测试通常不依赖深度 multipart 解析
            ("multipart-form".to_string(), serde_json::Value::Null, None)
        } else {
            ("unknown".to_string(), serde_json::Value::Null, None)
        };

        Some(PostData {
            text,
            kind,
            params,
            error,
        })
    } else {
        Some(PostData {
            text: String::new(),
            kind: "unknown".to_string(),
            params: serde_json::Value::Null,
            error: None,
        })
    };

    // vars — Nginx 风格变量
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost")
        .to_string();
    let scheme = if port == 15556 { "https" } else { "http" };
    let request_uri = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    let is_args = if uri.query().is_some() { "?" } else { "" };
    let url = format!("{}://{}:{}{}", scheme, host.split(':').next().unwrap_or(&host), port, request_uri);

    EchoResponse {
        headers: hdr_map,
        post_data,
        url,
        uri_args,
        vars: EchoVars {
            uri: uri.path().to_string(),
            host: host.split(':').next().unwrap_or(&host).to_string(),
            hostname: gethostname::gethostname()
                .to_string_lossy()
                .to_string(),
            https: if scheme == "https" { "on" } else { "off" }.to_string(),
            scheme: scheme.to_string(),
            is_args: is_args.to_string(),
            server_addr: "127.0.0.1".to_string(),
            server_port: port.to_string(),
            server_name: "mock_upstream".to_string(),
            server_protocol: "HTTP/1.1".to_string(),
            remote_addr: "127.0.0.1".to_string(),
            request: format!("{} {} HTTP/1.1", method, request_uri),
            request_uri: request_uri.to_string(),
            request_method: method.to_string(),
        },
        valid_routes: None,
        code: None,
        delay: None,
        authenticated: None,
        user: None,
    }
}

/// Build a JSON response with standard mock upstream headers — 构建带标准 mock upstream 头的 JSON 响应
fn echo_response(status: StatusCode, echo: &EchoResponse) -> Response {
    let json = serde_json::to_string(echo).unwrap_or_else(|_| "{}".to_string());
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("X-Powered-By", "mock_upstream")
        .header("Server", "mock-upstream/1.0.0")
        .body(Body::from(json))
        .unwrap()
}

fn text_response(status: StatusCode, text: &str) -> Response {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/plain")
        .header("X-Powered-By", "mock_upstream")
        .header("Server", "mock-upstream/1.0.0")
        .body(Body::from(text.to_string()))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Route handlers — 路由处理器
// ---------------------------------------------------------------------------

/// GET / — root: return valid routes listing — 根路径：返回可用路由列表
async fn handle_root(method: Method, uri: Uri, headers: HeaderMap) -> Response {
    if method != Method::GET {
        return text_response(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed for the requested URL");
    }
    let mut echo = build_echo(&method, &uri, &headers, &[], 15555);
    echo.valid_routes = Some(serde_json::json!({
        "/ws": "Websocket echo server",
        "/get": "Accepts a GET request and returns it in JSON format",
        "/xml": "Returns a simple XML document",
        "/post": "Accepts a POST request and returns it in JSON format",
        "/response-headers?:key=:val": "Returns given response headers",
        "/cache/:n": "Sets a Cache-Control header for n seconds",
        "/anything": "Accepts any request and returns it in JSON format",
        "/request": "Alias to /anything",
        "/delay/:duration": "Delay the response for <duration> seconds",
        "/basic-auth/:user/:pass": "Performs HTTP basic authentication with the given credentials",
        "/status/:code": "Returns a response with the specified status code",
        "/stream/:num": "Stream <num> chunks of JSON data via chunked Transfer Encoding",
        "/timestamp": "Returns server timestamp in header"
    }));
    echo_response(StatusCode::OK, &echo)
}

/// GET /get — echo GET request only — 仅回显 GET 请求
async fn handle_get(method: Method, uri: Uri, headers: HeaderMap) -> Response {
    if method != Method::GET {
        return text_response(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed for the requested URL");
    }
    let echo = build_echo(&method, &uri, &headers, &[], 15555);
    echo_response(StatusCode::OK, &echo)
}

/// POST /post — echo POST request only — 仅回显 POST 请求
async fn handle_post(method: Method, uri: Uri, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    if method != Method::POST {
        return text_response(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed for the requested URL");
    }
    let echo = build_echo(&method, &uri, &headers, &body, 15555);
    echo_response(StatusCode::OK, &echo)
}

/// ANY /anything or /request — echo any request — 回显任意请求
async fn handle_anything(method: Method, uri: Uri, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let echo = build_echo(&method, &uri, &headers, &body, 15555);
    echo_response(StatusCode::OK, &echo)
}

/// GET /xml — return XML document — 返回 XML 文档
async fn handle_xml() -> Response {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<note>
  <body>Kong, Monolith destroyer.</body>
</note>"#;
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/xml")
        .header("X-Powered-By", "mock_upstream")
        .header("Server", "mock-upstream/1.0.0")
        .body(Body::from(xml))
        .unwrap()
}

/// ANY /status/{code} — return specified status code — 返回指定状态码
async fn handle_status(
    Path(code): Path<u16>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST);
    let mut echo = build_echo(&method, &uri, &headers, &body, 15555);
    echo.code = Some(code);
    echo_response(status, &echo)
}

/// ANY /delay/{seconds} — delay then respond — 延迟后响应
async fn handle_delay(
    Path(seconds): Path<f64>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let duration = std::time::Duration::from_secs_f64(seconds.max(0.0).min(60.0));
    tokio::time::sleep(duration).await;
    let mut echo = build_echo(&method, &uri, &headers, &body, 15555);
    echo.delay = Some(seconds);
    echo_response(StatusCode::OK, &echo)
}

/// GET /response-headers — set custom response headers from query params — 从查询参数设置自定义响应头
async fn handle_response_headers(
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let echo = build_echo(&method, &uri, &headers, &[], 15555);
    let json = serde_json::to_string(&echo).unwrap_or_else(|_| "{}".to_string());
    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("X-Powered-By", "mock_upstream")
        .header("Server", "mock-upstream/1.0.0");
    for (k, v) in &params {
        if let Ok(hv) = HeaderValue::from_str(v) {
            resp = resp.header(k.as_str(), hv);
        }
    }
    resp.body(Body::from(json)).unwrap()
}

/// GET /cache/{n} — set Cache-Control header — 设置缓存控制头
async fn handle_cache(
    Path(n): Path<u64>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let echo = build_echo(&method, &uri, &headers, &[], 15555);
    let json = serde_json::to_string(&echo).unwrap_or_else(|_| "{}".to_string());
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("X-Powered-By", "mock_upstream")
        .header("Server", "mock-upstream/1.0.0")
        .header("Cache-Control", format!("public, max-age={}", n))
        .body(Body::from(json))
        .unwrap()
}

/// ANY /basic-auth/{user}/{pass} — HTTP basic auth verification — HTTP 基本认证验证
async fn handle_basic_auth(
    Path((expected_user, expected_pass)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // Check Proxy-Authorization first, then Authorization — 先检查 Proxy-Authorization，再检查 Authorization
    let auth_header = headers
        .get("proxy-authorization")
        .or_else(|| headers.get("authorization"))
        .and_then(|v| v.to_str().ok());

    let authenticated = if let Some(auth) = auth_header {
        if let Some(encoded) = auth.strip_prefix("Basic ").or_else(|| auth.strip_prefix("basic ")) {
            if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) {
                let decoded_str = String::from_utf8_lossy(&decoded);
                // Split on first colon only (password may contain colons) — 只在第一个冒号处分割（密码可能包含冒号）
                if let Some((user, pass)) = decoded_str.split_once(':') {
                    user == expected_user && pass == expected_pass
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };

    if !authenticated {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header("WWW-Authenticate", "mock_upstream")
            .header("X-Powered-By", "mock_upstream")
            .header("Server", "mock-upstream/1.0.0")
            .body(Body::empty())
            .unwrap();
    }

    let mut echo = build_echo(&method, &uri, &headers, &body, 15555);
    echo.authenticated = Some(true);
    echo.user = Some(expected_user);
    echo_response(StatusCode::OK, &echo)
}

/// ANY /stream/{n} — chunked streaming response — 分块流式响应
async fn handle_stream(
    Path(n): Path<usize>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let n = n.min(100); // Cap at 100 chunks — 限制最多 100 块
    let mut chunks = Vec::with_capacity(n);
    for _ in 0..n {
        let echo = build_echo(&method, &uri, &headers, &body, 15555);
        let json = serde_json::to_string(&echo).unwrap_or_else(|_| "{}".to_string());
        chunks.push(format!("{}\n", json));
    }
    let full = chunks.join("");
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("X-Powered-By", "mock_upstream")
        .header("Server", "mock-upstream/1.0.0")
        .body(Body::from(full))
        .unwrap()
}

/// ANY /timestamp — return server timestamp in header — 在 header 中返回服务器时间戳
async fn handle_timestamp(method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let echo = build_echo(&method, &uri, &headers, &[], 15555);
    let json = serde_json::to_string(&echo).unwrap_or_else(|_| "{}".to_string());
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("X-Powered-By", "mock_upstream")
        .header("Server", "mock-upstream/1.0.0")
        .header("Server-Time", format!("{:.3}", now))
        .body(Body::from(json))
        .unwrap()
}

/// ANY /hop-by-hop — return hop-by-hop headers for proxy testing — 返回 hop-by-hop 头用于代理测试
async fn handle_hop_by_hop() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("X-Powered-By", "mock_upstream")
        .header("Server", "mock-upstream/1.0.0")
        .header("Keep-Alive", "timeout=5, max=1000")
        .header("Proxy", "Remove-Me")
        .header("Proxy-Connection", "close")
        .header("Proxy-Authenticate", "Basic")
        .header(
            "Proxy-Authorization",
            "Basic YWxhZGRpbjpvcGVuc2VzYW1l",
        )
        .header("TE", "trailers, deflate;q=0.5")
        .header("Trailer", "Expires")
        .header("Upgrade", "example/1, foo/2")
        .body(Body::from("hello\r\n\r\nExpires: Wed, 21 Oct 2015 07:28:00 GMT\r\n\r\n"))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Log endpoints — 日志端点
// ---------------------------------------------------------------------------

/// POST /post_log/{name} — store request body as log entries — 存储请求体作为日志条目
async fn handle_post_log(
    Path(name): Path<String>,
    State(state): State<MockState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let body_str = String::from_utf8_lossy(&body).to_string();

    // Collect request headers — 收集请求头
    let mut req_headers = serde_json::Map::new();
    for (k, v) in headers.iter() {
        req_headers.insert(
            k.as_str().to_string(),
            serde_json::Value::String(v.to_str().unwrap_or("").to_string()),
        );
    }
    let log_req_headers = serde_json::Value::Object(req_headers);

    // Parse body as JSON array or single object — 解析 body 为 JSON 数组或单个对象
    let entries: Vec<serde_json::Value> = match serde_json::from_str::<serde_json::Value>(&body_str)
    {
        Ok(serde_json::Value::Array(arr)) => arr,
        Ok(v) => vec![v],
        Err(_) => vec![serde_json::Value::String(body_str)],
    };

    let mut logs = state.logs.write().await;
    let store = logs.entry(name).or_insert_with(|| LogStore {
        entries: Vec::new(),
        count: 0,
    });

    for entry in entries {
        let wrapped = serde_json::json!({
            "entry": entry,
            "log_req_headers": log_req_headers,
        });
        store.entries.push(serde_json::to_string(&wrapped).unwrap_or_default());
        store.count += 1;
    }

    text_response(StatusCode::OK, "")
}

/// GET /read_log/{name} — read stored log entries — 读取存储的日志条目
async fn handle_read_log(
    Path(name): Path<String>,
    State(state): State<MockState>,
) -> Response {
    let logs = state.logs.read().await;
    if let Some(store) = logs.get(&name) {
        let entries: Vec<serde_json::Value> = store
            .entries
            .iter()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect();
        let resp = serde_json::json!({
            "entries": entries,
            "count": store.count,
        });
        let json = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".to_string());
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("X-Powered-By", "mock_upstream")
            .header("Server", "mock-upstream/1.0.0")
            .body(Body::from(json))
            .unwrap()
    } else {
        let resp = serde_json::json!({
            "entries": [],
            "count": 0,
        });
        let json = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".to_string());
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("X-Powered-By", "mock_upstream")
            .header("Server", "mock-upstream/1.0.0")
            .body(Body::from(json))
            .unwrap()
    }
}

/// GET /count_log/{name} — return log entry count — 返回日志条目计数
async fn handle_count_log(
    Path(name): Path<String>,
    State(state): State<MockState>,
) -> Response {
    let logs = state.logs.read().await;
    let count = logs.get(&name).map(|s| s.count).unwrap_or(0);
    text_response(StatusCode::OK, &count.to_string())
}

/// DELETE /reset_log/{name} — clear stored log entries — 清除存储的日志条目
async fn handle_reset_log(
    Path(name): Path<String>,
    State(state): State<MockState>,
) -> Response {
    let mut logs = state.logs.write().await;
    logs.remove(&name);
    text_response(StatusCode::OK, "")
}

// ---------------------------------------------------------------------------
// Router builder — 路由构建
// ---------------------------------------------------------------------------

pub fn build_mock_router() -> Router {
    let state = MockState::new();

    Router::new()
        .route("/", any(handle_root))
        .route("/get", any(handle_get))
        .route("/post", any(handle_post))
        .route("/anything", any(handle_anything))
        .route("/anything/{path}", any(handle_anything))
        .route("/request", any(handle_anything))
        .route("/request/{path}", any(handle_anything))
        .route("/xml", get(handle_xml))
        .route("/status/{code}", any(handle_status))
        .route("/delay/{seconds}", any(handle_delay))
        .route("/response-headers", get(handle_response_headers))
        .route("/cache/{n}", get(handle_cache))
        .route("/basic-auth/{user}/{pass}", any(handle_basic_auth))
        .route("/stream/{n}", any(handle_stream))
        .route("/timestamp", any(handle_timestamp))
        .route("/hop-by-hop", any(handle_hop_by_hop))
        // Log endpoints — 日志端点
        .route("/post_log/{name}", post(handle_post_log))
        .route("/read_log/{name}", get(handle_read_log))
        .route("/count_log/{name}", get(handle_count_log))
        .route("/reset_log/{name}", delete(handle_reset_log))
        .with_state(state)
}

/// Run the mock upstream server — 启动 mock upstream 服务器
pub async fn run(port: u16, _ssl_port: Option<u16>) -> anyhow::Result<()> {
    let app = build_mock_router();
    let addr = format!("0.0.0.0:{}", port);
    tracing::info!("Mock upstream 监听于: {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

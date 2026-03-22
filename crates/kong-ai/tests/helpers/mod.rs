//! Mock LLM Server — 模拟 OpenAI/Anthropic API 的 axum 测试服务器

use axum::{
    extract::State,
    http::StatusCode,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use tokio::sync::Mutex;

/// Mock LLM 响应模式
#[allow(dead_code)]
pub enum ResponseMode {
    /// 200 — 标准非流式响应
    Normal,
    /// 200 — SSE 流式响应
    Streaming,
    /// 返回指定 HTTP 错误码
    Error(u16),
    /// 延迟 N 毫秒后响应
    Slow(u64),
}

struct MockState {
    response_mode: Mutex<ResponseMode>,
    request_count: AtomicU32,
}

/// Mock LLM 服务器 — 在随机端口启动，模拟 OpenAI / Anthropic API
pub struct MockLlmServer {
    pub addr: String,
    pub port: u16,
    state: Arc<MockState>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MockLlmServer {
    /// 启动 mock 服务器（随机端口）
    pub async fn start() -> Self {
        let state = Arc::new(MockState {
            response_mode: Mutex::new(ResponseMode::Normal),
            request_count: AtomicU32::new(0),
        });

        let app_state = state.clone();
        let app = Router::new()
            .route("/v1/chat/completions", post(handle_openai_chat))
            .route("/v1/messages", post(handle_anthropic_messages))
            .with_state(app_state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = listener.local_addr().unwrap();
        let port = local_addr.port();
        let addr = format!("http://127.0.0.1:{}", port);

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });

        Self {
            addr,
            port,
            state,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    /// 切换响应模式
    #[allow(dead_code)]
    pub async fn set_mode(&self, mode: ResponseMode) {
        *self.state.response_mode.lock().await = mode;
    }

    /// 获取累计请求计数
    #[allow(dead_code)]
    pub fn request_count(&self) -> u32 {
        self.state.request_count.load(Ordering::Relaxed)
    }

    /// 关闭服务器
    #[allow(dead_code)]
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// OpenAI /v1/chat/completions 处理器
async fn handle_openai_chat(
    State(state): State<Arc<MockState>>,
    Json(body): Json<Value>,
) -> Result<axum::response::Response, StatusCode> {
    state.request_count.fetch_add(1, Ordering::Relaxed);

    let mode = state.response_mode.lock().await;
    match *mode {
        ResponseMode::Error(code) => {
            let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let error_body = json!({
                "error": {
                    "message": format!("mock error {}", code),
                    "type": "server_error",
                    "code": code
                }
            });
            Ok(axum::response::Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(error_body.to_string()))
                .unwrap())
        }
        ResponseMode::Slow(ms) => {
            drop(mode);
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            let resp = make_openai_response(&body);
            Ok(axum::response::Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(resp.to_string()))
                .unwrap())
        }
        ResponseMode::Streaming => {
            drop(mode);
            let model = body
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or("gpt-4");

            // 构建 SSE 流式响应
            let events = vec![
                make_stream_chunk(model, "Hi", false),
                make_stream_chunk(model, "!", true),
                "[DONE]".to_string(),
            ];

            let sse_body = events
                .into_iter()
                .map(|data| format!("data: {}\n\n", data))
                .collect::<String>();

            Ok(axum::response::Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from(sse_body))
                .unwrap())
        }
        ResponseMode::Normal => {
            drop(mode);
            let resp = make_openai_response(&body);
            Ok(axum::response::Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(resp.to_string()))
                .unwrap())
        }
    }
}

/// Anthropic /v1/messages 处理器
async fn handle_anthropic_messages(
    State(state): State<Arc<MockState>>,
    Json(body): Json<Value>,
) -> Result<axum::response::Response, StatusCode> {
    state.request_count.fetch_add(1, Ordering::Relaxed);

    let mode = state.response_mode.lock().await;
    match *mode {
        ResponseMode::Error(code) => {
            let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let error_body = json!({
                "type": "error",
                "error": {
                    "type": "server_error",
                    "message": format!("mock error {}", code)
                }
            });
            Ok(axum::response::Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(error_body.to_string()))
                .unwrap())
        }
        _ => {
            drop(mode);
            let model = body
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or("claude-3-opus-20240229");
            let resp = json!({
                "id": "msg_mock_123",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "Hi from Claude!"}],
                "model": model,
                "stop_reason": "end_turn",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5
                }
            });
            Ok(axum::response::Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(resp.to_string()))
                .unwrap())
        }
    }
}

/// 构建标准 OpenAI ChatCompletion 非流式响应
fn make_openai_response(request: &Value) -> Value {
    let model = request
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("gpt-4");
    json!({
        "id": "chatcmpl-mock-123",
        "object": "chat.completion",
        "created": 1700000000u64,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "Hi!"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    })
}

/// 构建 OpenAI 流式 chunk JSON
fn make_stream_chunk(model: &str, content: &str, include_usage: bool) -> String {
    let mut chunk = json!({
        "id": "chatcmpl-mock-stream",
        "object": "chat.completion.chunk",
        "created": 1700000000u64,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {
                "content": content
            },
            "finish_reason": null
        }]
    });

    if include_usage {
        chunk["usage"] = json!({
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        });
    }

    chunk.to_string()
}

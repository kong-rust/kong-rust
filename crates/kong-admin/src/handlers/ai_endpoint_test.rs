use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use kong_core::traits::PageParams;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

use crate::AdminState;

const MANAGED_ENDPOINT_TAG: &str = "kr-ai-endpoint-v1";

#[derive(Debug, Deserialize)]
pub struct TestEndpointRequest {
    path: String,
    request: Value,
}

#[derive(Debug, Serialize)]
pub struct TestEndpointResponse {
    status: u16,
    model: Option<String>,
    route_type: Option<String>,
    body: String,
}

fn is_safe_endpoint_path(path: &str) -> bool {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();

    segments.len() == 5
        && segments[0] == "ai"
        && !segments[1].is_empty()
        && segments[1]
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && segments[2..] == ["v1", "chat", "completions"]
}

fn proxy_base_url(state: &AdminState) -> Option<String> {
    let listener = state
        .config
        .proxy_listen
        .iter()
        .find(|listener| listener.port > 0 && !listener.ssl)
        .or_else(|| {
            state
                .config
                .proxy_listen
                .iter()
                .find(|listener| listener.port > 0)
        })?;
    let scheme = if listener.ssl { "https" } else { "http" };
    let host = match listener.ip.as_str() {
        "0.0.0.0" | "::" | "" => "127.0.0.1".to_string(),
        ip if ip.contains(':') => format!("[{ip}]"),
        ip => ip.to_string(),
    };

    Some(format!("{scheme}://{host}:{}", listener.port))
}

pub async fn test_endpoint(
    State(state): State<AdminState>,
    Json(payload): Json<TestEndpointRequest>,
) -> Result<Json<TestEndpointResponse>, (StatusCode, Json<Value>)> {
    if !is_safe_endpoint_path(&payload.path) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "message": "invalid managed AI endpoint path" })),
        ));
    }

    let routes = state
        .routes
        .page(&PageParams {
            size: 10_000,
            ..Default::default()
        })
        .await
        .map_err(|error| {
            tracing::error!("Unable to load AI endpoint routes for test request: {error}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "message": "unable to load managed AI endpoints" })),
            )
        })?;
    let managed_route_exists = routes.data.iter().any(|route| {
        route
            .tags
            .as_ref()
            .is_some_and(|tags| tags.iter().any(|tag| tag == MANAGED_ENDPOINT_TAG))
            && route
                .paths
                .as_ref()
                .is_some_and(|paths| paths.iter().any(|path| path == &payload.path))
    });

    if !managed_route_exists {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "managed AI endpoint not found" })),
        ));
    }

    let base_url = proxy_base_url(&state).ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "message": "no proxy listener is available" })),
        )
    })?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|error| {
            tracing::error!("Unable to create AI endpoint test client: {error}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "message": "unable to create endpoint test client" })),
            )
        })?;
    let response = client
        .post(format!("{base_url}{}", payload.path))
        .json(&payload.request)
        .send()
        .await
        .map_err(|error| {
            tracing::warn!("Managed AI endpoint test request failed: {error}");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "message": format!("endpoint request failed: {error}") })),
            )
        })?;
    let status = response.status().as_u16();
    let model = response
        .headers()
        .get("x-kong-llm-model")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let route_type = response
        .headers()
        .get("x-kong-ai-route-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response.text().await.map_err(|error| {
        tracing::warn!("Unable to read managed AI endpoint test response: {error}");
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "message": "unable to read endpoint response" })),
        )
    })?;

    Ok(Json(TestEndpointResponse {
        status,
        model,
        route_type,
        body,
    }))
}

#[cfg(test)]
mod tests {
    use super::is_safe_endpoint_path;

    #[test]
    fn only_accepts_managed_chat_endpoint_shape() {
        assert!(is_safe_endpoint_path(
            "/ai/customer-support/v1/chat/completions"
        ));
        assert!(!is_safe_endpoint_path("/services"));
        assert!(!is_safe_endpoint_path("/ai/../status/v1/chat/completions"));
        assert!(!is_safe_endpoint_path("/ai/UPPER/v1/chat/completions"));
        assert!(!is_safe_endpoint_path(
            "/ai/customer-support/v1/chat/completions/extra"
        ));
    }
}

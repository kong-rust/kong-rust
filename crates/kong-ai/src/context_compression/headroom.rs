use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::Url;
use tokio::sync::Mutex;

use super::{
    CompressionBackendDescriptor, CompressionBackendError, CompressionBodyTransform,
    CompressionProtocol, CompressionRoute, CompressionStoreScope, ContextCompressionBackend,
    ProviderTarget,
};

/// Headroom 官方 proxy adapter 的进程级配置。
#[derive(Clone)]
pub struct HeadroomProxyConfig {
    pub base_url: String,
    pub health_timeout: Duration,
    pub health_ttl: Duration,
    pub store_scope: CompressionStoreScope,
}

impl std::fmt::Debug for HeadroomProxyConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeadroomProxyConfig")
            .field("base_url", &self.base_url)
            .field("health_timeout", &self.health_timeout)
            .field("health_ttl", &self.health_ttl)
            .field("store_scope", &self.store_scope)
            .finish()
    }
}

#[derive(Debug)]
struct HealthSnapshot {
    checked_at: Option<Instant>,
    healthy: bool,
}

/// 通过 Headroom 官方 proxy 完成压缩与透明 CCR 的 adapter。
pub struct HeadroomProxyAdapter {
    endpoint: Url,
    client: reqwest::Client,
    health_timeout: Duration,
    health_ttl: Duration,
    health: Mutex<HealthSnapshot>,
    store_scope: CompressionStoreScope,
}

impl std::fmt::Debug for HeadroomProxyAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeadroomProxyAdapter")
            .field("endpoint", &self.endpoint.as_str())
            .field("health_timeout", &self.health_timeout)
            .field("health_ttl", &self.health_ttl)
            .field("store_scope", &self.store_scope)
            .finish_non_exhaustive()
    }
}

impl HeadroomProxyAdapter {
    pub fn new(config: HeadroomProxyConfig) -> Result<Self, CompressionBackendError> {
        let endpoint = parse_endpoint(&config.base_url)?;
        let client = reqwest::Client::builder()
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| CompressionBackendError::Unavailable)?;
        Ok(Self {
            endpoint,
            client,
            health_timeout: config.health_timeout,
            health_ttl: config.health_ttl,
            health: Mutex::new(HealthSnapshot {
                checked_at: None,
                healthy: false,
            }),
            store_scope: config.store_scope,
        })
    }

    async fn ensure_healthy(&self) -> Result<(), CompressionBackendError> {
        let mut snapshot = self.health.lock().await;
        if snapshot
            .checked_at
            .is_some_and(|checked_at| checked_at.elapsed() <= self.health_ttl)
        {
            return if snapshot.healthy {
                Ok(())
            } else {
                Err(CompressionBackendError::Unavailable)
            };
        }

        let health_url = append_endpoint_path(&self.endpoint, "/readyz")?;
        let request = self.client.get(health_url).timeout(self.health_timeout);
        let healthy = request
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false);
        snapshot.checked_at = Some(Instant::now());
        snapshot.healthy = healthy;
        if healthy {
            Ok(())
        } else {
            Err(CompressionBackendError::Unavailable)
        }
    }

    fn route_for(
        &self,
        protocol: CompressionProtocol,
        provider: ProviderTarget,
    ) -> Result<CompressionRoute, CompressionBackendError> {
        validate_provider_target(&provider)?;
        let provider_origin = provider_origin(&provider)?;

        let (headroom_path, base_url, original_path, body_transform) = match protocol {
            // Headroom 0.33.0 的 direct OpenAI Chat transport 会注入 retrieve tool，
            // 但不会拦截响应并续调。安全起见必须旁路，不能把内部 tool call 暴露给客户端。
            CompressionProtocol::OpenAiChat => {
                return Err(CompressionBackendError::UnsupportedProtocol);
            }
            CompressionProtocol::OpenAiResponses => {
                if !provider.path.ends_with("/responses") {
                    return Err(CompressionBackendError::UnsupportedTarget);
                }
                (
                    "/v1/responses",
                    provider_origin,
                    Some(provider.path),
                    Some(CompressionBodyTransform::InjectOpenAiResponsesCcrTool),
                )
            }
            CompressionProtocol::AnthropicMessages => {
                let Some(prefix) = provider.path.strip_suffix("/v1/messages") else {
                    return Err(CompressionBackendError::UnsupportedTarget);
                };
                (
                    "/v1/messages",
                    format!("{}{}", provider_origin, prefix.trim_end_matches('/')),
                    None,
                    None,
                )
            }
        };

        let mut control_headers = vec![
            ("x-headroom-base-url".to_string(), base_url),
            ("x-headroom-stack".to_string(), "kong-rust".to_string()),
        ];
        if let Some(path) = original_path {
            control_headers.push(("x-headroom-original-path".to_string(), path));
        }
        let port = self
            .endpoint
            .port_or_known_default()
            .ok_or(CompressionBackendError::Unavailable)?;
        Ok(CompressionRoute {
            scheme: self.endpoint.scheme().to_string(),
            host: self
                .endpoint
                .host_str()
                .ok_or(CompressionBackendError::Unavailable)?
                .to_string(),
            port,
            path: endpoint_path(&self.endpoint, headroom_path),
            control_headers,
            body_transform,
        })
    }
}

#[async_trait]
impl ContextCompressionBackend for HeadroomProxyAdapter {
    fn descriptor(&self) -> CompressionBackendDescriptor {
        CompressionBackendDescriptor {
            backend: "headroom_proxy",
            transparent_ccr: true,
            streaming: false,
            store_scope: self.store_scope,
        }
    }

    async fn prepare_route(
        &self,
        protocol: CompressionProtocol,
        provider: ProviderTarget,
    ) -> Result<CompressionRoute, CompressionBackendError> {
        self.ensure_healthy().await?;
        self.route_for(protocol, provider)
    }
}

fn parse_endpoint(value: &str) -> Result<Url, CompressionBackendError> {
    let mut url = Url::parse(value).map_err(|_| CompressionBackendError::Unavailable)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(CompressionBackendError::Unavailable);
    }
    let normalized = url.path().trim_end_matches('/').to_string();
    url.set_path(if normalized.is_empty() {
        "/"
    } else {
        &normalized
    });
    Ok(url)
}

fn append_endpoint_path(base: &Url, suffix: &str) -> Result<Url, CompressionBackendError> {
    let mut result = base.clone();
    result.set_path(&endpoint_path(base, suffix));
    Ok(result)
}

fn endpoint_path(base: &Url, suffix: &str) -> String {
    let prefix = base.path().trim_end_matches('/');
    format!("{}{}", prefix, suffix)
}

fn validate_provider_target(provider: &ProviderTarget) -> Result<(), CompressionBackendError> {
    if !matches!(provider.scheme.as_str(), "http" | "https")
        || provider.host.trim().is_empty()
        || !provider.path.starts_with('/')
        || provider.path.starts_with("//")
        || provider.path.contains('?')
        || provider.path.contains('#')
    {
        return Err(CompressionBackendError::UnsupportedTarget);
    }
    Ok(())
}

fn provider_origin(provider: &ProviderTarget) -> Result<String, CompressionBackendError> {
    validate_provider_target(provider)?;
    let host = if provider.host.contains(':') && !provider.host.starts_with('[') {
        format!("[{}]", provider.host)
    } else {
        provider.host.clone()
    };
    let default_port = (provider.scheme == "http" && provider.port == 80)
        || (provider.scheme == "https" && provider.port == 443);
    if default_port {
        Ok(format!("{}://{}", provider.scheme, host))
    } else {
        Ok(format!("{}://{}:{}", provider.scheme, host, provider.port))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::Router;

    use super::*;

    fn adapter(endpoint: &str) -> HeadroomProxyAdapter {
        HeadroomProxyAdapter::new(HeadroomProxyConfig {
            base_url: endpoint.to_string(),
            health_timeout: Duration::from_millis(100),
            health_ttl: Duration::from_secs(1),
            store_scope: CompressionStoreScope::Local,
        })
        .unwrap()
    }

    #[test]
    fn openai_responses_route_preserves_server_selected_target() {
        let route = adapter("http://127.0.0.1:8787")
            .route_for(
                CompressionProtocol::OpenAiResponses,
                ProviderTarget {
                    scheme: "https".to_string(),
                    host: "provider.example".to_string(),
                    port: 443,
                    path: "/custom/v1/responses".to_string(),
                },
            )
            .unwrap();
        assert_eq!(route.path, "/v1/responses");
        assert!(route.control_headers.contains(&(
            "x-headroom-base-url".to_string(),
            "https://provider.example".to_string()
        )));
        assert!(route.control_headers.contains(&(
            "x-headroom-original-path".to_string(),
            "/custom/v1/responses".to_string()
        )));
        assert_eq!(
            route.body_transform,
            Some(CompressionBodyTransform::InjectOpenAiResponsesCcrTool)
        );
    }

    #[test]
    fn anthropic_route_moves_prefix_into_base_url() {
        let route = adapter("https://headroom.example/internal")
            .route_for(
                CompressionProtocol::AnthropicMessages,
                ProviderTarget {
                    scheme: "http".to_string(),
                    host: "::1".to_string(),
                    port: 8080,
                    path: "/tenant-a/v1/messages".to_string(),
                },
            )
            .unwrap();
        assert_eq!(route.path, "/internal/v1/messages");
        assert!(route.control_headers.contains(&(
            "x-headroom-base-url".to_string(),
            "http://[::1]:8080/tenant-a".to_string()
        )));
        assert!(!route
            .control_headers
            .iter()
            .any(|(name, _)| name == "x-headroom-original-path"));
        assert_eq!(route.body_transform, None);
    }

    #[test]
    fn openai_chat_is_bypassed_until_direct_transport_handles_ccr_continuation() {
        let error = adapter("http://127.0.0.1:8787")
            .route_for(
                CompressionProtocol::OpenAiChat,
                ProviderTarget {
                    scheme: "https".to_string(),
                    host: "api.openai.com".to_string(),
                    port: 443,
                    path: "/v1/chat/completions".to_string(),
                },
            )
            .unwrap_err();
        assert_eq!(error, CompressionBackendError::UnsupportedProtocol);
    }

    #[test]
    fn rejects_ambiguous_or_unsupported_targets() {
        let adapter = adapter("http://127.0.0.1:8787");
        for target in [
            ProviderTarget {
                scheme: "file".to_string(),
                host: "provider.example".to_string(),
                port: 80,
                path: "/v1/chat/completions".to_string(),
            },
            ProviderTarget {
                scheme: "https".to_string(),
                host: "provider.example".to_string(),
                port: 443,
                path: "/v1/chat/completions?api-version=1".to_string(),
            },
        ] {
            assert_eq!(
                adapter
                    .route_for(CompressionProtocol::OpenAiResponses, target)
                    .unwrap_err(),
                CompressionBackendError::UnsupportedTarget
            );
        }
    }

    #[test]
    fn debug_output_contains_only_non_secret_configuration() {
        let config = HeadroomProxyConfig {
            base_url: "http://127.0.0.1:8787".to_string(),
            health_timeout: Duration::from_millis(100),
            health_ttl: Duration::from_secs(1),
            store_scope: CompressionStoreScope::Local,
        };
        let output = format!("{config:?}");
        assert!(output.contains("127.0.0.1:8787"));
        assert!(!output.contains("token"));
    }

    async fn start_health_server(status: StatusCode) -> (String, Arc<AtomicUsize>) {
        async fn health(
            State((status, calls)): State<(StatusCode, Arc<AtomicUsize>)>,
        ) -> StatusCode {
            calls.fetch_add(1, Ordering::SeqCst);
            status
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/readyz", get(health))
            .with_state((status, Arc::clone(&calls)));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), calls)
    }

    #[tokio::test]
    async fn health_result_is_cached_for_the_configured_ttl() {
        let (endpoint, calls) = start_health_server(StatusCode::OK).await;
        let adapter = HeadroomProxyAdapter::new(HeadroomProxyConfig {
            base_url: endpoint,
            health_timeout: Duration::from_secs(1),
            health_ttl: Duration::from_secs(60),
            store_scope: CompressionStoreScope::Local,
        })
        .unwrap();
        let target = ProviderTarget {
            scheme: "https".to_string(),
            host: "api.openai.com".to_string(),
            port: 443,
            path: "/v1/responses".to_string(),
        };
        adapter
            .prepare_route(CompressionProtocol::OpenAiResponses, target.clone())
            .await
            .unwrap();
        adapter
            .prepare_route(CompressionProtocol::OpenAiResponses, target)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unhealthy_proxy_is_reported_without_exposing_response_details() {
        let (endpoint, _) = start_health_server(StatusCode::SERVICE_UNAVAILABLE).await;
        let adapter = HeadroomProxyAdapter::new(HeadroomProxyConfig {
            base_url: endpoint,
            health_timeout: Duration::from_secs(1),
            health_ttl: Duration::from_secs(1),
            store_scope: CompressionStoreScope::Local,
        })
        .unwrap();
        let error = adapter
            .prepare_route(
                CompressionProtocol::OpenAiResponses,
                ProviderTarget {
                    scheme: "https".to_string(),
                    host: "api.openai.com".to_string(),
                    port: 443,
                    path: "/v1/responses".to_string(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error, CompressionBackendError::Unavailable);
        assert_eq!(
            error.to_string(),
            "context compression backend is unavailable"
        );
    }
}

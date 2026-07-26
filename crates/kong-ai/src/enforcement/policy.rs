//! AI 有效插件链的请求级快照。

use std::collections::HashSet;
use std::sync::Mutex;

use kong_core::traits::RequestCtx;
use kong_plugin_system::{DispatchFailureResponseFormat, RequestLifecycleObserver, ResolvedPlugin};
use uuid::Uuid;

use crate::plugins::ai_proxy::AiProxyConfig;

/// AI 实时限额的生效维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiRateLimitMode {
    Global,
    Route,
    Consumer,
    VirtualKey,
}

impl AiRateLimitMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "global" => Some(Self::Global),
            "route" => Some(Self::Route),
            "consumer" => Some(Self::Consumer),
            "virtual_key" => Some(Self::VirtualKey),
            _ => None,
        }
    }
}

/// 客户端使用的 AI 协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiClientProtocol {
    OpenAi,
    Anthropic,
}

impl AiClientProtocol {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "openai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            _ => None,
        }
    }
}

/// 有效链配置错误的稳定分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiPolicyConfigErrorCode {
    InvalidRateLimitMode,
    InvalidAiProxyConfig,
    InvalidClientProtocol,
    MissingAiProxy,
}

/// 请求内只保存低基数错误分类，不复制敏感插件配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiPolicyConfigError {
    pub plugin_name: &'static str,
    pub code: AiPolicyConfigErrorCode,
}

/// 由已解析插件链一次生成、供 access/dispatch/finalizer 共用的策略快照。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiPolicyChainSnapshot {
    pub has_ai_key_auth: bool,
    pub has_ai_proxy: bool,
    pub rate_limit_mode: Option<AiRateLimitMode>,
    pub client_protocol: Option<AiClientProtocol>,
    pub config_error: Option<AiPolicyConfigError>,
}

/// 同步解析有效插件链，不执行数据库或网络 I/O。
#[derive(Debug, PartialEq, Eq, Hash)]
struct DeprecatedConfigWarningKey {
    plugin_config_id: Uuid,
    plugin_version: String,
}

#[derive(Debug, Default)]
pub struct AiPolicyChainObserver {
    deprecated_config_warnings: Mutex<HashSet<DeprecatedConfigWarningKey>>,
}

impl AiPolicyChainObserver {
    pub fn new() -> Self {
        Self::default()
    }

    fn should_warn_deprecated_header_name(&self, plugin: &ResolvedPlugin) -> bool {
        if plugin.config.name != "ai-rate-limit"
            || plugin.config.config.get("header_name").is_none()
        {
            return false;
        }

        let key = DeprecatedConfigWarningKey {
            plugin_config_id: plugin.plugin_id,
            plugin_version: plugin.handler.version().to_owned(),
        };
        self.deprecated_config_warnings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key)
    }

    fn warn_deprecated_config(&self, plugins: &[ResolvedPlugin]) {
        for plugin in plugins {
            if self.should_warn_deprecated_header_name(plugin) {
                tracing::warn!(
                    plugin_config_id = %plugin.plugin_id,
                    plugin_version = plugin.handler.version(),
                    deprecated_field = "header_name",
                    "ai-rate-limit 配置包含已弃用字段"
                );
            }
        }
    }

    fn build_snapshot(plugins: &[ResolvedPlugin]) -> AiPolicyChainSnapshot {
        let mut snapshot = AiPolicyChainSnapshot::default();

        for plugin in plugins {
            match plugin.config.name.as_str() {
                "ai-key-auth" => snapshot.has_ai_key_auth = true,
                "ai-proxy" => {
                    snapshot.has_ai_proxy = true;
                    match serde_json::from_value::<AiProxyConfig>(plugin.config.config.clone()) {
                        Ok(config) => {
                            snapshot.client_protocol =
                                AiClientProtocol::parse(config.effective_client_protocol());
                            if snapshot.client_protocol.is_none() {
                                snapshot.config_error.get_or_insert(AiPolicyConfigError {
                                    plugin_name: "ai-proxy",
                                    code: AiPolicyConfigErrorCode::InvalidClientProtocol,
                                });
                            }
                        }
                        Err(_) => {
                            snapshot.config_error.get_or_insert(AiPolicyConfigError {
                                plugin_name: "ai-proxy",
                                code: AiPolicyConfigErrorCode::InvalidAiProxyConfig,
                            });
                        }
                    }
                }
                "ai-rate-limit" => {
                    let limit_by = plugin
                        .config
                        .config
                        .get("limit_by")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("consumer");
                    snapshot.rate_limit_mode = AiRateLimitMode::parse(limit_by);
                    if snapshot.rate_limit_mode.is_none() {
                        snapshot.config_error.get_or_insert(AiPolicyConfigError {
                            plugin_name: "ai-rate-limit",
                            code: AiPolicyConfigErrorCode::InvalidRateLimitMode,
                        });
                    }
                }
                _ => {}
            }
        }

        if snapshot.rate_limit_mode == Some(AiRateLimitMode::VirtualKey) && !snapshot.has_ai_proxy {
            snapshot.config_error.get_or_insert(AiPolicyConfigError {
                plugin_name: "ai-rate-limit",
                code: AiPolicyConfigErrorCode::MissingAiProxy,
            });
        }

        snapshot
    }
}

impl RequestLifecycleObserver for AiPolicyChainObserver {
    fn on_plugins_resolved(&self, plugins: &[ResolvedPlugin], ctx: &mut RequestCtx) {
        self.warn_deprecated_config(plugins);
        let snapshot = Self::build_snapshot(plugins);
        if let Some(protocol) = snapshot.client_protocol {
            ctx.extensions.insert(match protocol {
                AiClientProtocol::OpenAi => DispatchFailureResponseFormat::OpenAi,
                AiClientProtocol::Anthropic => DispatchFailureResponseFormat::Anthropic,
            });
        }
        ctx.extensions.insert(snapshot);
    }

    fn on_request_finalizing(&self, _plugins: &[ResolvedPlugin], _ctx: &mut RequestCtx) {}
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use kong_core::error::Result;
    use kong_core::traits::{PluginConfig, PluginHandler};
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    struct NoopPlugin {
        name: &'static str,
        version: &'static str,
    }

    #[async_trait]
    impl PluginHandler for NoopPlugin {
        fn priority(&self) -> i32 {
            0
        }

        fn version(&self) -> &str {
            self.version
        }

        fn name(&self) -> &str {
            self.name
        }

        async fn access(&self, _config: &PluginConfig, _ctx: &mut RequestCtx) -> Result<()> {
            Ok(())
        }
    }

    fn plugin(name: &'static str, config: serde_json::Value) -> ResolvedPlugin {
        plugin_with_identity(name, config, Uuid::new_v4(), "test")
    }

    fn plugin_with_identity(
        name: &'static str,
        config: serde_json::Value,
        plugin_id: Uuid,
        version: &'static str,
    ) -> ResolvedPlugin {
        ResolvedPlugin {
            handler: Arc::new(NoopPlugin { name, version }),
            config: PluginConfig {
                name: name.to_string(),
                config,
            },
            plugin_id,
            route_id: None,
            service_id: None,
            consumer_id: None,
        }
    }

    #[test]
    fn snapshots_virtual_key_chain_and_protocol() {
        let plugins = vec![
            plugin("ai-key-auth", json!({})),
            plugin("ai-proxy", json!({"client_protocol": "anthropic"})),
            plugin("ai-rate-limit", json!({"limit_by": "virtual_key"})),
        ];

        let snapshot = AiPolicyChainObserver::build_snapshot(&plugins);

        assert!(snapshot.has_ai_key_auth);
        assert!(snapshot.has_ai_proxy);
        assert_eq!(snapshot.rate_limit_mode, Some(AiRateLimitMode::VirtualKey));
        assert_eq!(snapshot.client_protocol, Some(AiClientProtocol::Anthropic));
        assert_eq!(snapshot.config_error, None);
    }

    #[test]
    fn virtual_key_chain_without_ai_proxy_is_invalid() {
        let plugins = vec![plugin("ai-rate-limit", json!({"limit_by": "virtual_key"}))];

        let snapshot = AiPolicyChainObserver::build_snapshot(&plugins);

        assert_eq!(
            snapshot.config_error,
            Some(AiPolicyConfigError {
                plugin_name: "ai-rate-limit",
                code: AiPolicyConfigErrorCode::MissingAiProxy,
            })
        );
    }

    #[test]
    fn deprecated_header_name_warns_once_per_config_id_and_plugin_version() {
        let observer = AiPolicyChainObserver::new();
        let plugin_id = Uuid::new_v4();
        let first = plugin_with_identity(
            "ai-rate-limit",
            json!({"header_name": "X-Do-Not-Log"}),
            plugin_id,
            "0.2.0",
        );
        let upgraded = plugin_with_identity(
            "ai-rate-limit",
            json!({"header_name": "X-Do-Not-Log"}),
            plugin_id,
            "0.3.0",
        );

        assert!(observer.should_warn_deprecated_header_name(&first));
        assert!(!observer.should_warn_deprecated_header_name(&first));
        assert!(observer.should_warn_deprecated_header_name(&upgraded));
        assert!(!observer.should_warn_deprecated_header_name(&upgraded));
    }

    #[test]
    fn absent_deprecated_header_name_does_not_consume_warning_key() {
        let observer = AiPolicyChainObserver::new();
        let plugin_id = Uuid::new_v4();
        let without_header = plugin_with_identity("ai-rate-limit", json!({}), plugin_id, "0.2.0");
        let with_header = plugin_with_identity(
            "ai-rate-limit",
            json!({"header_name": "X-Do-Not-Log"}),
            plugin_id,
            "0.2.0",
        );

        assert!(!observer.should_warn_deprecated_header_name(&without_header));
        assert!(observer.should_warn_deprecated_header_name(&with_header));
    }

    #[test]
    fn invalid_limit_by_is_not_silently_downgraded() {
        let plugins = vec![plugin("ai-rate-limit", json!({"limit_by": "key"}))];

        let snapshot = AiPolicyChainObserver::build_snapshot(&plugins);

        assert_eq!(snapshot.rate_limit_mode, None);
        assert_eq!(
            snapshot.config_error.map(|error| error.code),
            Some(AiPolicyConfigErrorCode::InvalidRateLimitMode)
        );
    }
}

//! ai-rate-limit 插件 — RPM/TPM 内存限流 + 预扣修正

use async_trait::async_trait;
use serde::Deserialize;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use kong_core::error::{KongError, Result};
use kong_core::traits::{PluginConfig, PluginHandler, RequestCtx};

use crate::auth::AiAuthContext;
use crate::enforcement::{
    apply_quota_headers, inspect_budget_before_quota, quota_error_contract,
    reject_with_protocol_error, AiClientProtocol, AiEnforcementRuntime, AiPolicyChainSnapshot,
    AiPolicyConfigErrorCode, AiRateLimitMode, AiRateLimitRequestContext, BudgetInspectionOutcome,
    QuotaHeaderMode,
};
use crate::plugins::context::AiRequestState;
use crate::ratelimit::{
    admit_with_recovery, AdmissionDecision, AdmitCommand, InspectQuery, InspectResult, QuotaCharge,
    QuotaLimits, RateLimitKey, RateLimitStoreErrorKind, RateLimitSubject, RateLimiter, WindowSpec,
};
use crate::token::{global_registry, TokenCounter, TokenizerRegistry};

// ============ 插件配置 ============

/// ai-rate-limit 插件配置
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiRateLimitConfig {
    /// 限流维度："virtual_key" | "consumer" | "route" | "global"
    pub limit_by: String,
    /// Token Per Minute 限制
    pub tpm_limit: Option<u64>,
    /// Request Per Minute 限制
    pub rpm_limit: Option<u64>,
    /// 读取 virtual key 的 header 名称
    pub header_name: String,
    /// 超限错误码
    pub error_code: u16,
    /// 超限错误消息
    pub error_message: String,
}

impl Default for AiRateLimitConfig {
    fn default() -> Self {
        Self {
            limit_by: "consumer".to_string(),
            tpm_limit: None,
            rpm_limit: None,
            header_name: "X-AI-Key".to_string(),
            error_code: 429,
            error_message: "AI rate limit exceeded".to_string(),
        }
    }
}

/// 存储在 ctx.extensions 中的限流上下文（跨阶段共享）
pub struct AiRateLimitContext {
    /// 限流键前缀
    pub rate_key: String,
    /// 预扣的 prompt token 估算值
    pub estimated_prompt_tokens: u64,
}

// ============ 插件结构体 ============

/// AI 速率限制插件
pub struct AiRateLimitPlugin {
    backend: AiRateLimitBackend,
}

enum AiRateLimitBackend {
    Runtime(Arc<AiEnforcementRuntime>),
    Legacy(Arc<dyn RateLimiter>),
}

impl AiRateLimitPlugin {
    /// 使用 server 构造的共享 enforcement runtime。
    pub fn new(runtime: Arc<AiEnforcementRuntime>) -> Self {
        Self {
            backend: AiRateLimitBackend::Runtime(runtime),
        }
    }

    /// 使用旧同步限流器创建插件，仅保留历史测试/嵌入方兼容。
    #[deprecated(note = "请使用 AiRateLimitPlugin::new(shared_runtime)")]
    pub fn with_limiter(limiter: Arc<dyn RateLimiter>) -> Self {
        Self {
            backend: AiRateLimitBackend::Legacy(limiter),
        }
    }

    fn validate_config(config: &PluginConfig) -> Result<()> {
        kong_plugin_system::config_validation::validate_ai_rate_limit_config(&config.config)
            .map_err(|error| KongError::PluginError {
                plugin_name: config.name.clone(),
                message: error.to_string(),
            })
    }

    fn protocol(ctx: &RequestCtx) -> AiClientProtocol {
        ctx.extensions
            .get::<AiAuthContext>()
            .map(|auth| auth.client_protocol)
            .or_else(|| {
                ctx.extensions
                    .get::<AiPolicyChainSnapshot>()
                    .and_then(|snapshot| snapshot.client_protocol)
            })
            .unwrap_or(AiClientProtocol::OpenAi)
    }

    fn reject_policy(
        ctx: &mut RequestCtx,
        protocol: AiClientProtocol,
        status: u16,
        code: &str,
        message: &str,
    ) {
        reject_with_protocol_error(ctx, protocol, status, code, message);
    }

    async fn access_runtime(
        runtime: &AiEnforcementRuntime,
        cfg: &AiRateLimitConfig,
        ctx: &mut RequestCtx,
    ) -> Result<()> {
        let protocol = Self::protocol(ctx);
        let mode = AiRateLimitMode::parse(&cfg.limit_by).ok_or_else(|| KongError::PluginError {
            plugin_name: "ai-rate-limit".to_string(),
            message: "invalid limit_by".to_string(),
        })?;

        if let Some(error) = ctx
            .extensions
            .get::<AiPolicyChainSnapshot>()
            .and_then(|snapshot| snapshot.config_error.clone())
        {
            let (status, code, message) = match error.code {
                AiPolicyConfigErrorCode::MissingAiProxy => (
                    500,
                    "ai_policy_chain_invalid",
                    "AI policy chain is missing ai-proxy",
                ),
                _ => (
                    500,
                    "ai_policy_chain_invalid",
                    "AI policy chain configuration is invalid",
                ),
            };
            Self::reject_policy(ctx, protocol, status, code, message);
            return Ok(());
        }

        let (subject, limits, budget_auth) = match mode {
            AiRateLimitMode::VirtualKey => {
                let snapshot = ctx.extensions.get::<AiPolicyChainSnapshot>();
                if snapshot.is_none_or(|snapshot| !snapshot.has_ai_proxy) {
                    Self::reject_policy(
                        ctx,
                        protocol,
                        500,
                        "ai_policy_chain_invalid",
                        "AI policy chain is missing ai-proxy",
                    );
                    return Ok(());
                }
                let Some(auth) = ctx.extensions.get::<AiAuthContext>().cloned() else {
                    Self::reject_policy(
                        ctx,
                        protocol,
                        401,
                        "virtual_key_required",
                        "A virtual key is required.",
                    );
                    return Ok(());
                };
                (
                    RateLimitSubject::VirtualKey(auth.virtual_key_id),
                    QuotaLimits {
                        requests: auth.policy.rpm_limit,
                        tokens: auth.policy.tpm_limit,
                    },
                    Some(auth),
                )
            }
            AiRateLimitMode::Global => (
                RateLimitSubject::Global,
                QuotaLimits {
                    requests: cfg.rpm_limit.and_then(NonZeroU64::new),
                    tokens: cfg.tpm_limit.and_then(NonZeroU64::new),
                },
                None,
            ),
            AiRateLimitMode::Route => {
                let Some(route_id) = ctx.route_id else {
                    Self::reject_policy(
                        ctx,
                        protocol,
                        500,
                        "ai_policy_chain_invalid",
                        "Route-scoped quota requires a matched route",
                    );
                    return Ok(());
                };
                (
                    RateLimitSubject::Route(route_id),
                    QuotaLimits {
                        requests: cfg.rpm_limit.and_then(NonZeroU64::new),
                        tokens: cfg.tpm_limit.and_then(NonZeroU64::new),
                    },
                    None,
                )
            }
            AiRateLimitMode::Consumer => (
                RateLimitSubject::Consumer(ctx.consumer_id),
                QuotaLimits {
                    requests: cfg.rpm_limit.and_then(NonZeroU64::new),
                    tokens: cfg.tpm_limit.and_then(NonZeroU64::new),
                },
                None,
            ),
        };

        if let Some(auth) = budget_auth.as_ref() {
            match inspect_budget_before_quota(runtime, auth, protocol, ctx).await {
                BudgetInspectionOutcome::Continue => {}
                BudgetInspectionOutcome::Rejected => return Ok(()),
                BudgetInspectionOutcome::Exhausted => {
                    if limits.requests.is_some() || limits.tokens.is_some() {
                        let quota_runtime = match runtime.quota_runtime() {
                            Ok(runtime) => runtime,
                            Err(_) => {
                                reject_quota_backend_error(
                                    ctx,
                                    protocol,
                                    RateLimitStoreErrorKind::Unsupported,
                                );
                                return Ok(());
                            }
                        };
                        let key = RateLimitKey::new(
                            quota_runtime.deployment_namespace.clone(),
                            subject.clone(),
                        );
                        match quota_runtime
                            .store
                            .inspect(InspectQuery::Current {
                                key: key.clone(),
                                window: WindowSpec::fixed(Duration::from_secs(60)),
                                limits,
                            })
                            .await
                        {
                            Ok(InspectResult::Current(snapshot)) => {
                                ctx.extensions
                                    .insert(AiRateLimitRequestContext::snapshot_only(
                                        key, limits, protocol, snapshot,
                                    ));
                            }
                            Ok(_) => {
                                reject_quota_backend_error(
                                    ctx,
                                    protocol,
                                    RateLimitStoreErrorKind::Corrupt,
                                );
                                return Ok(());
                            }
                            Err(error) => {
                                reject_quota_backend_error(ctx, protocol, error.kind());
                                return Ok(());
                            }
                        }
                    }
                    Self::reject_policy(
                        ctx,
                        protocol,
                        403,
                        "budget_exhausted",
                        "The virtual key budget has been exhausted.",
                    );
                    return Ok(());
                }
            }
        }

        // Virtual Key 可以只配置预算而没有 RPM/TPM；这种情况下 quota Store 零调用。
        if limits.requests.is_none() && limits.tokens.is_none() {
            return Ok(());
        }

        let quota_runtime = match runtime.quota_runtime() {
            Ok(runtime) => runtime,
            Err(_) => {
                reject_quota_backend_error(ctx, protocol, RateLimitStoreErrorKind::Unsupported);
                return Ok(());
            }
        };
        let key = RateLimitKey::new(quota_runtime.deployment_namespace.clone(), subject);
        let estimated = if limits.tokens.is_some() {
            compute_estimated_prompt_tokens(ctx).await
        } else {
            0
        };
        let reserved = QuotaCharge {
            requests: 1,
            tokens: estimated,
        };
        let command = AdmitCommand {
            request_id: Arc::from(ctx.lifecycle.request_id.clone()),
            key: key.clone(),
            window: WindowSpec::fixed(Duration::from_secs(60)),
            limits,
            reserve: reserved,
        };

        match admit_with_recovery(quota_runtime.store.as_ref(), command).await {
            Ok(AdmissionDecision::Allowed {
                reservation,
                snapshot,
                ..
            }) => {
                ctx.extensions.insert(AiRateLimitRequestContext::allowed(
                    key,
                    limits,
                    protocol,
                    reserved,
                    reservation,
                    snapshot,
                ));
            }
            Ok(AdmissionDecision::Rejected {
                reason, snapshot, ..
            }) => {
                ctx.extensions.insert(AiRateLimitRequestContext::rejected(
                    key, limits, protocol, reserved, reason, snapshot,
                ));
                if mode == AiRateLimitMode::VirtualKey {
                    let (code, message) = match reason {
                        crate::ratelimit::ExceededDimension::Tokens => (
                            "tokens_rate_limit_exceeded",
                            "Virtual key token rate limit exceeded.",
                        ),
                        crate::ratelimit::ExceededDimension::Requests
                        | crate::ratelimit::ExceededDimension::RequestsAndTokens => (
                            "requests_rate_limit_exceeded",
                            "Virtual key request rate limit exceeded.",
                        ),
                    };
                    Self::reject_policy(ctx, protocol, 429, code, message);
                } else {
                    Self::reject_policy(
                        ctx,
                        protocol,
                        cfg.error_code,
                        "rate_limit_exceeded",
                        &cfg.error_message,
                    );
                }
            }
            Err(error) => {
                reject_quota_backend_error(ctx, protocol, error.kind());
            }
        }

        Ok(())
    }

    async fn access_legacy(
        limiter: &dyn RateLimiter,
        cfg: &AiRateLimitConfig,
        ctx: &mut RequestCtx,
    ) -> Result<()> {
        // 旧路径只用于兼容既有嵌入测试，server 不再装配。
        let rate_key = match cfg.limit_by.as_str() {
            "global" => "global".to_string(),
            "route" => format!(
                "route:{}",
                ctx.route_id.map(|id| id.to_string()).unwrap_or_default()
            ),
            "consumer" => format!(
                "consumer:{}",
                ctx.consumer_id.map(|id| id.to_string()).unwrap_or_default()
            ),
            _ => "global".to_string(),
        };

        if let Some(rpm_limit) = cfg.rpm_limit {
            let rpm_key = format!("{}:rpm", rate_key);
            let (allowed, current) = limiter.check_and_increment(&rpm_key, rpm_limit, 1);
            if !allowed {
                ctx.short_circuited = true;
                ctx.exit_status = Some(cfg.error_code);
                ctx.exit_body = Some(
                    serde_json::json!({
                        "message": cfg.error_message,
                        "current_rpm": current,
                        "limit": rpm_limit
                    })
                    .to_string(),
                );
                return Ok(());
            }
        }

        if let Some(tpm_limit) = cfg.tpm_limit {
            let estimated = compute_estimated_prompt_tokens(ctx).await;
            let tpm_key = format!("{}:tpm", rate_key);
            let (allowed, current) = limiter.check_and_increment(&tpm_key, tpm_limit, estimated);
            if !allowed {
                ctx.short_circuited = true;
                ctx.exit_status = Some(cfg.error_code);
                ctx.exit_body = Some(
                    serde_json::json!({
                        "message": cfg.error_message,
                        "current_tpm": current,
                        "limit": tpm_limit
                    })
                    .to_string(),
                );
                return Ok(());
            }
            ctx.extensions.insert(AiRateLimitContext {
                rate_key,
                estimated_prompt_tokens: estimated,
            });
        }
        Ok(())
    }
}

// ============ PluginHandler 实现 ============

#[async_trait]
impl PluginHandler for AiRateLimitPlugin {
    fn name(&self) -> &str {
        "ai-rate-limit"
    }

    fn priority(&self) -> i32 {
        // 高于 ai-proxy (770)，先执行限流检查
        771
    }

    fn version(&self) -> &str {
        "0.2.0"
    }

    async fn access(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        Self::validate_config(config)?;
        let cfg: AiRateLimitConfig = crate::parse_plugin_config(config)?;
        match &self.backend {
            AiRateLimitBackend::Runtime(runtime) => Self::access_runtime(runtime, &cfg, ctx).await,
            AiRateLimitBackend::Legacy(limiter) => {
                Self::access_legacy(limiter.as_ref(), &cfg, ctx).await
            }
        }
    }

    async fn log(&self, config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let AiRateLimitBackend::Legacy(limiter) = &self.backend else {
            return Ok(());
        };
        // TPM 修正：根据实际 token 消耗量修正预扣值
        let rl_ctx = ctx.extensions.get::<AiRateLimitContext>();
        let ai_state = ctx.extensions.get::<AiRequestState>();

        if let (Some(rl_ctx), Some(ai_state)) = (rl_ctx, ai_state) {
            let cfg: AiRateLimitConfig = crate::parse_plugin_config(config)?;
            if cfg.tpm_limit.is_some() {
                let actual = ai_state.usage.total_tokens.unwrap_or(0);
                let estimated = rl_ctx.estimated_prompt_tokens;
                let tpm_key = format!("{}:tpm", rl_ctx.rate_key);
                if actual > estimated {
                    // 实际消耗 > 预扣：补扣差额
                    limiter.increment(&tpm_key, actual - estimated);
                } else if estimated > actual {
                    // 预扣 > 实际消耗：退还多扣的部分
                    limiter.decrement(&tpm_key, estimated - actual);
                }
            }
        }
        Ok(())
    }

    async fn header_filter(&self, _config: &PluginConfig, ctx: &mut RequestCtx) -> Result<()> {
        let Some(request) = ctx.extensions.get_mut::<AiRateLimitRequestContext>() else {
            return Ok(());
        };
        let snapshot = request
            .response_snapshot
            .as_ref()
            .or(request.admission_snapshot.as_ref())
            .cloned();
        let mode = request
            .rejection
            .map(QuotaHeaderMode::Rejected)
            .unwrap_or(QuotaHeaderMode::Allowed);
        request.headers_emitted = snapshot.is_some();
        if let Some(snapshot) = snapshot {
            apply_quota_headers(ctx, &snapshot, mode);
        }
        Ok(())
    }
}

fn reject_quota_backend_error(
    ctx: &mut RequestCtx,
    protocol: AiClientProtocol,
    kind: RateLimitStoreErrorKind,
) {
    let (code, message) = quota_error_contract(kind);
    reject_with_protocol_error(ctx, protocol, 503, code, message);
}

// ============ 辅助函数 / Helpers ============

/// 计算 prompt token 预扣值
/// Compute the prompt-token pre-debit value.
///
/// 优先级 / Priority:
/// 1. 上游已写入的 `AiRequestState.estimated_prompt_tokens`(ai-proxy 在更晚的 priority 执行,
///    本插件 priority=771 早于 ai-proxy 770,通常拿不到 — 仅当 priority 顺序被外部调换时生效)
///    Honor pre-existing AiRequestState (only present if priority order has been swapped externally)
/// 2. 全局 TokenizerRegistry → count_prompt_from_body(自己解析 body,启发式推断 provider)
///    Global TokenizerRegistry path
/// 3. 字符估算(byte_len / 4)— 与历史行为兼容
///    Char estimation fallback — preserves historical behavior
async fn compute_estimated_prompt_tokens(ctx: &kong_core::traits::RequestCtx) -> u64 {
    // 1. 上游可能已经把精确值写入 AiRequestState
    if let Some(state) = ctx.extensions.get::<AiRequestState>() {
        if state.estimated_prompt_tokens > 0 {
            return state.estimated_prompt_tokens;
        }
    }

    let body = match ctx.request_body.as_deref() {
        Some(b) if !b.is_empty() => b,
        _ => return 0,
    };

    // 2. registry 路径
    if let Some(registry) = global_registry() {
        // 从 body 中嗅出 model 名以决定 provider strategy(失败则空字符串)
        let model_name = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| {
                v.get("model")
                    .and_then(|m| m.as_str().map(|s| s.to_string()))
            })
            .unwrap_or_default();
        let provider_type = TokenizerRegistry::infer_provider_type(&model_name);
        return registry
            .count_prompt_from_body(provider_type, &model_name, body)
            .await;
    }

    // 3. 历史 byte/4 估算兜底
    TokenCounter::count_estimate(body)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use kong_core::traits::PluginHandler;
    use serde_json::json;

    use super::*;
    use crate::auth::VirtualKeyPolicySnapshot;
    use crate::enforcement::{BudgetCapability, QuotaCapability};
    use crate::ratelimit::{
        InspectQuery, InspectResult, ManualRateLimitClock, MemoryRateLimitStore,
        RateLimitBackendDescriptor, RateLimitStore, RateLimitStoreError,
        RateLimitStoreStatsSnapshot, SettleCommand, SettlementResult,
    };

    struct AlwaysUnknownAdmissionStore {
        inner: Arc<MemoryRateLimitStore>,
        commands: Mutex<Vec<AdmitCommand>>,
    }

    impl AlwaysUnknownAdmissionStore {
        fn new() -> Self {
            Self {
                inner: Arc::new(MemoryRateLimitStore::with_defaults(Arc::new(
                    ManualRateLimitClock::default(),
                ))),
                commands: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl RateLimitStore for AlwaysUnknownAdmissionStore {
        fn descriptor(&self) -> RateLimitBackendDescriptor {
            self.inner.descriptor()
        }

        async fn admit(
            &self,
            command: AdmitCommand,
        ) -> std::result::Result<AdmissionDecision, RateLimitStoreError> {
            self.commands.lock().unwrap().push(command);
            Err(RateLimitStoreError::new(
                RateLimitStoreErrorKind::OutcomeUnknown,
                "admission ACK is unknown",
            ))
        }

        async fn settle(
            &self,
            command: SettleCommand,
        ) -> std::result::Result<SettlementResult, RateLimitStoreError> {
            self.inner.settle(command).await
        }

        async fn inspect(
            &self,
            query: InspectQuery,
        ) -> std::result::Result<InspectResult, RateLimitStoreError> {
            if matches!(&query, InspectQuery::Admission { .. }) {
                Ok(InspectResult::NotFound)
            } else {
                self.inner.inspect(query).await
            }
        }

        fn stats(&self) -> RateLimitStoreStatsSnapshot {
            self.inner.stats()
        }
    }

    fn runtime() -> (Arc<AiEnforcementRuntime>, Arc<MemoryRateLimitStore>) {
        let store = Arc::new(MemoryRateLimitStore::with_defaults(Arc::new(
            ManualRateLimitClock::default(),
        )));
        let runtime = Arc::new(
            AiEnforcementRuntime::with_local_quota(
                Arc::clone(&store) as Arc<dyn RateLimitStore>,
                "test",
                false,
                BudgetCapability::UnsupportedDbLess,
            )
            .unwrap(),
        );
        assert_eq!(runtime.capability.quota, QuotaCapability::LocalMemory);
        (runtime, store)
    }

    #[tokio::test]
    async fn runtime_path_rejects_with_snapshot_headers() {
        let (runtime, _store) = runtime();
        let plugin = AiRateLimitPlugin::new(runtime);
        let config = PluginConfig {
            name: "ai-rate-limit".to_string(),
            config: json!({
                "limit_by": "global",
                "rpm_limit": 1,
                "tpm_limit": null
            }),
        };
        let mut first = RequestCtx::new();
        plugin.access(&config, &mut first).await.unwrap();
        assert!(!first.is_short_circuited());

        let mut second = RequestCtx::new();
        plugin.access(&config, &mut second).await.unwrap();
        assert_eq!(second.exit_status, Some(429));
        plugin.header_filter(&config, &mut second).await.unwrap();

        let body: serde_json::Value =
            serde_json::from_str(second.exit_body.as_deref().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "rate_limit_exceeded");
        assert!(second.response_headers_to_set.contains(&(
            "X-RateLimit-Remaining-Requests".to_string(),
            "0".to_string()
        )));
        assert!(second
            .response_headers_to_set
            .iter()
            .any(|(name, _)| name == "Retry-After"));
    }

    #[tokio::test]
    async fn virtual_key_mode_uses_authenticated_uuid_and_policy_limits() {
        let (runtime, store) = runtime();
        let plugin = AiRateLimitPlugin::new(runtime);
        let virtual_key_id = uuid::Uuid::new_v4();
        let config = PluginConfig {
            name: "ai-rate-limit".to_string(),
            config: json!({
                "limit_by": "virtual_key",
                "rpm_limit": null,
                "tpm_limit": null
            }),
        };
        let mut ctx = RequestCtx::new();
        ctx.extensions.insert(AiPolicyChainSnapshot {
            has_ai_key_auth: true,
            has_ai_proxy: true,
            rate_limit_mode: Some(AiRateLimitMode::VirtualKey),
            client_protocol: Some(AiClientProtocol::Anthropic),
            config_error: None,
        });
        ctx.extensions.insert(AiAuthContext {
            virtual_key_id,
            key_name: "key".to_string(),
            key_prefix: "sk-kr-test".to_string(),
            consumer_id: None,
            client_protocol: AiClientProtocol::Anthropic,
            policy: VirtualKeyPolicySnapshot {
                rpm_limit: NonZeroU64::new(2),
                tpm_limit: None,
                budget_guard_required: false,
                accounting_blocked: false,
            },
        });

        plugin.access(&config, &mut ctx).await.unwrap();

        assert!(!ctx.is_short_circuited());
        let current = store
            .inspect(InspectQuery::Current {
                key: RateLimitKey::new("test", RateLimitSubject::VirtualKey(virtual_key_id)),
                window: WindowSpec::fixed(Duration::from_secs(60)),
                limits: QuotaLimits {
                    requests: NonZeroU64::new(2),
                    tokens: None,
                },
            })
            .await
            .unwrap();
        let InspectResult::Current(snapshot) = current else {
            panic!("current quota snapshot expected");
        };
        assert_eq!(snapshot.requests.unwrap().used, 1);
    }

    #[tokio::test]
    async fn virtual_key_mode_without_auth_fails_before_store_call() {
        let (runtime, store) = runtime();
        let plugin = AiRateLimitPlugin::new(runtime);
        let config = PluginConfig {
            name: "ai-rate-limit".to_string(),
            config: json!({
                "limit_by": "virtual_key",
                "rpm_limit": null,
                "tpm_limit": null
            }),
        };
        let mut ctx = RequestCtx::new();
        ctx.extensions.insert(AiPolicyChainSnapshot {
            has_ai_key_auth: true,
            has_ai_proxy: true,
            rate_limit_mode: Some(AiRateLimitMode::VirtualKey),
            client_protocol: Some(AiClientProtocol::OpenAi),
            config_error: None,
        });

        plugin.access(&config, &mut ctx).await.unwrap();

        assert_eq!(ctx.exit_status, Some(401));
        assert_eq!(store.stats().admissions_allowed, 0);
        assert_eq!(store.stats().admissions_rejected, 0);
    }

    #[test]
    fn quota_backend_errors_use_exact_public_contract() {
        let cases = [
            (
                RateLimitStoreErrorKind::OutcomeUnknown,
                "quota_backend_unavailable",
                "Quota enforcement is temporarily unavailable.",
            ),
            (
                RateLimitStoreErrorKind::Corrupt,
                "quota_backend_state_invalid",
                "Quota enforcement state is invalid.",
            ),
            (
                RateLimitStoreErrorKind::Unsupported,
                "quota_backend_unsupported",
                "Quota enforcement is not supported in this deployment mode.",
            ),
        ];

        for (kind, code, message) in cases {
            let mut ctx = RequestCtx::new();
            reject_quota_backend_error(&mut ctx, AiClientProtocol::OpenAi, kind);

            assert_eq!(ctx.exit_status, Some(503));
            let body: serde_json::Value =
                serde_json::from_str(ctx.exit_body.as_deref().unwrap()).unwrap();
            assert_eq!(body["error"]["type"], "server_error");
            assert_eq!(body["error"]["code"], code);
            assert_eq!(body["error"]["message"], message);
            assert!(ctx.response_headers_to_set.is_empty());
        }
    }

    #[tokio::test]
    async fn admission_still_unknown_after_bounded_replay_returns_fixed_503() {
        let store = Arc::new(AlwaysUnknownAdmissionStore::new());
        let runtime = Arc::new(
            AiEnforcementRuntime::with_local_quota(
                store.clone() as Arc<dyn RateLimitStore>,
                "test",
                false,
                BudgetCapability::UnsupportedDbLess,
            )
            .unwrap(),
        );
        let plugin = AiRateLimitPlugin::new(runtime);
        let config = PluginConfig {
            name: "ai-rate-limit".to_string(),
            config: json!({
                "limit_by": "global",
                "rpm_limit": 10,
                "tpm_limit": null
            }),
        };
        let mut ctx = RequestCtx::new();

        plugin.access(&config, &mut ctx).await.unwrap();

        assert_eq!(ctx.exit_status, Some(503));
        let body: serde_json::Value =
            serde_json::from_str(ctx.exit_body.as_deref().unwrap()).unwrap();
        assert_eq!(body["error"]["type"], "server_error");
        assert_eq!(body["error"]["code"], "quota_backend_unavailable");
        assert_eq!(
            body["error"]["message"],
            "Quota enforcement is temporarily unavailable."
        );
        assert!(ctx.extensions.get::<AiRateLimitRequestContext>().is_none());
        assert!(ctx.response_headers_to_set.is_empty());
        let commands = store.commands.lock().unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0], commands[1]);
    }
}

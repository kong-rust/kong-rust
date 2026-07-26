//! 实时配额的请求级 reservation、补偿与最终修正。

use std::sync::Arc;

use async_trait::async_trait;
use kong_core::traits::RequestCtx;
use kong_plugin_system::{
    DispatchAbortCause, LifecycleHookError, RequestDispatchAbortHandler, RequestFinalizer,
    ResolvedPlugin,
};

use crate::plugins::context::AiRequestState;
use crate::ratelimit::{
    settle_with_recovery, ExceededDimension, QuotaCharge, QuotaLimits, RateLimitKey,
    RateLimitSnapshot, RateLimitStoreErrorKind, ReservationToken, SettleCommand, SettlementResult,
};
use crate::usage::AiUsageFact;

use super::policy::AiClientProtocol;
use super::response::{clear_quota_headers, reject_with_protocol_error};
use super::runtime::AiEnforcementRuntime;

/// 请求内配额修正状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaSettlementState {
    NotRequired,
    Pending,
    Settled,
    RetryRequired,
}

/// access、header_filter、dispatch abort 与 finalizer 共享的配额状态。
#[derive(Debug, Clone)]
pub struct AiRateLimitRequestContext {
    pub key: RateLimitKey,
    pub limits: QuotaLimits,
    pub protocol: AiClientProtocol,
    pub reserved: QuotaCharge,
    pub admission_snapshot: Option<RateLimitSnapshot>,
    pub response_snapshot: Option<RateLimitSnapshot>,
    pub rejection: Option<ExceededDimension>,
    pub reservation: Option<ReservationToken>,
    pub settlement_command: Option<SettleCommand>,
    pub settlement: QuotaSettlementState,
    pub headers_emitted: bool,
}

impl AiRateLimitRequestContext {
    pub fn allowed(
        key: RateLimitKey,
        limits: QuotaLimits,
        protocol: AiClientProtocol,
        reserved: QuotaCharge,
        reservation: ReservationToken,
        snapshot: RateLimitSnapshot,
    ) -> Self {
        Self {
            key,
            limits,
            protocol,
            reserved,
            admission_snapshot: Some(snapshot),
            response_snapshot: None,
            rejection: None,
            reservation: Some(reservation),
            settlement_command: None,
            settlement: QuotaSettlementState::Pending,
            headers_emitted: false,
        }
    }

    pub fn rejected(
        key: RateLimitKey,
        limits: QuotaLimits,
        protocol: AiClientProtocol,
        reserved: QuotaCharge,
        reason: ExceededDimension,
        snapshot: RateLimitSnapshot,
    ) -> Self {
        Self {
            key,
            limits,
            protocol,
            reserved,
            admission_snapshot: Some(snapshot),
            response_snapshot: None,
            rejection: Some(reason),
            reservation: None,
            settlement_command: None,
            settlement: QuotaSettlementState::NotRequired,
            headers_emitted: false,
        }
    }

    pub fn snapshot_only(
        key: RateLimitKey,
        limits: QuotaLimits,
        protocol: AiClientProtocol,
        snapshot: RateLimitSnapshot,
    ) -> Self {
        Self {
            key,
            limits,
            protocol,
            reserved: QuotaCharge::default(),
            admission_snapshot: Some(snapshot),
            response_snapshot: None,
            rejection: None,
            reservation: None,
            settlement_command: None,
            settlement: QuotaSettlementState::NotRequired,
            headers_emitted: false,
        }
    }
}

/// 客户端响应结束后修正 RPM/TPM reservation。
pub struct AiRateLimitFinalizer {
    runtime: Arc<AiEnforcementRuntime>,
}

impl AiRateLimitFinalizer {
    pub fn new(runtime: Arc<AiEnforcementRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl RequestFinalizer for AiRateLimitFinalizer {
    fn name(&self) -> &'static str {
        "ai-rate-limit-finalizer"
    }

    async fn finalize(
        &self,
        _plugins: &[ResolvedPlugin],
        ctx: &mut RequestCtx,
    ) -> Result<(), LifecycleHookError> {
        let command = prepare_final_settlement(ctx);
        let Some(command) = command else {
            return Ok(());
        };
        let store = self
            .runtime
            .quota_runtime()
            .map_err(|_| {
                LifecycleHookError::new("quota_backend_unsupported", "quota runtime is unsupported")
            })?
            .store
            .clone();

        match settle_with_recovery(store.as_ref(), command).await {
            Ok(result) => {
                record_settlement_result(ctx, result);
                Ok(())
            }
            Err(error) => {
                if let Some(request) = ctx.extensions.get_mut::<AiRateLimitRequestContext>() {
                    request.settlement = QuotaSettlementState::RetryRequired;
                }
                Err(LifecycleHookError::new(
                    quota_error_code(error.kind()),
                    error.to_string(),
                ))
            }
        }
    }
}

/// critical dispatch 失败后，在发送响应前把 quota reservation 全额退回。
pub struct AiQuotaDispatchAbortCompensator {
    runtime: Arc<AiEnforcementRuntime>,
}

impl AiQuotaDispatchAbortCompensator {
    pub fn new(runtime: Arc<AiEnforcementRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl RequestDispatchAbortHandler for AiQuotaDispatchAbortCompensator {
    fn name(&self) -> &'static str {
        "ai-quota-dispatch-abort"
    }

    fn compensation_domain(&self) -> &'static str {
        "ai-quota"
    }

    async fn compensate_before_response(
        &self,
        ctx: &mut RequestCtx,
        _cause: DispatchAbortCause,
    ) -> Result<(), LifecycleHookError> {
        let Some(command) = prepare_zero_settlement(ctx) else {
            return Ok(());
        };
        let runtime = self.runtime.quota_runtime().map_err(|_| {
            LifecycleHookError::new("quota_backend_unsupported", "quota runtime is unsupported")
        })?;

        match settle_with_recovery(runtime.store.as_ref(), command).await {
            Ok(result) => {
                record_settlement_result(ctx, result);
                Ok(())
            }
            Err(error) => {
                let protocol = ctx
                    .extensions
                    .get_mut::<AiRateLimitRequestContext>()
                    .map(|request| {
                        request.settlement = QuotaSettlementState::RetryRequired;
                        request.admission_snapshot = None;
                        request.response_snapshot = None;
                        request.protocol
                    })
                    .unwrap_or(AiClientProtocol::OpenAi);
                clear_quota_headers(ctx);
                let (code, message) = quota_error_contract(error.kind());
                // dispatch runner 可能已经冻结了更早、更精确的 budget/root failure。
                // quota 退款失败只负责隐藏不可信响应头并保留重试命令，不得覆盖根因。
                if !ctx.is_short_circuited() {
                    reject_with_protocol_error(ctx, protocol, 503, code, message);
                }
                Err(LifecycleHookError::new(code, error.to_string()))
            }
        }
    }
}

fn prepare_zero_settlement(ctx: &mut RequestCtx) -> Option<SettleCommand> {
    let request = ctx.extensions.get_mut::<AiRateLimitRequestContext>()?;
    if matches!(
        request.settlement,
        QuotaSettlementState::NotRequired | QuotaSettlementState::Settled
    ) {
        return None;
    }
    if let Some(command) = &request.settlement_command {
        return Some(command.clone());
    }
    let reservation = request.reservation.clone()?;
    let command = SettleCommand {
        operation_id: settlement_operation_id(&ctx.lifecycle.request_id),
        reservation,
        final_charge: QuotaCharge::default(),
    };
    request.settlement_command = Some(command.clone());
    Some(command)
}

fn prepare_final_settlement(ctx: &mut RequestCtx) -> Option<SettleCommand> {
    let (upstream_attempted, request_id) = (
        ctx.lifecycle.upstream_attempted,
        ctx.lifecycle.request_id.clone(),
    );
    let actual_total = observed_total_tokens(ctx);
    let request = ctx.extensions.get_mut::<AiRateLimitRequestContext>()?;
    if matches!(
        request.settlement,
        QuotaSettlementState::NotRequired | QuotaSettlementState::Settled
    ) {
        return None;
    }
    if let Some(command) = &request.settlement_command {
        return Some(command.clone());
    }
    let reservation = request.reservation.clone()?;
    let final_charge = if upstream_attempted {
        QuotaCharge {
            requests: 1,
            tokens: actual_total.unwrap_or(request.reserved.tokens),
        }
    } else {
        QuotaCharge {
            requests: 1,
            tokens: 0,
        }
    };
    let command = SettleCommand {
        operation_id: settlement_operation_id(&request_id),
        reservation,
        final_charge,
    };
    request.settlement_command = Some(command.clone());
    Some(command)
}

fn observed_total_tokens(ctx: &RequestCtx) -> Option<u64> {
    if let Some(fact) = ctx.extensions.get::<Arc<AiUsageFact>>() {
        if let Some(total) = fact.total_tokens {
            return u64::try_from(total.value).ok();
        }
    }
    ctx.extensions
        .get::<AiRequestState>()
        .and_then(|state| state.usage.total_tokens)
}

fn record_settlement_result(ctx: &mut RequestCtx, result: SettlementResult) {
    if let Some(request) = ctx.extensions.get_mut::<AiRateLimitRequestContext>() {
        request.response_snapshot = result.snapshot;
        request.settlement = QuotaSettlementState::Settled;
    }
}

fn settlement_operation_id(request_id: &str) -> Arc<str> {
    Arc::from(format!("quota-settle:v1:{request_id}"))
}

pub fn quota_error_contract(kind: RateLimitStoreErrorKind) -> (&'static str, &'static str) {
    match kind {
        RateLimitStoreErrorKind::Corrupt => (
            "quota_backend_state_invalid",
            "Quota enforcement state is invalid.",
        ),
        RateLimitStoreErrorKind::Unsupported => (
            "quota_backend_unsupported",
            "Quota enforcement is not supported in this deployment mode.",
        ),
        RateLimitStoreErrorKind::Unavailable
        | RateLimitStoreErrorKind::Timeout
        | RateLimitStoreErrorKind::OutcomeUnknown
        | RateLimitStoreErrorKind::Overloaded => (
            "quota_backend_unavailable",
            "Quota enforcement is temporarily unavailable.",
        ),
    }
}

pub fn quota_error_code(kind: RateLimitStoreErrorKind) -> &'static str {
    quota_error_contract(kind).0
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::Mutex;

    use crate::enforcement::runtime::BudgetCapability;
    use crate::ratelimit::{
        AdmissionDecision, AdmitCommand, InspectQuery, InspectResult, ManualRateLimitClock,
        MemoryRateLimitStore, RateLimitBackendDescriptor, RateLimitStore, RateLimitStoreError,
        RateLimitStoreErrorKind, RateLimitStoreStatsSnapshot, RateLimitSubject, WindowSpec,
    };

    use super::*;

    async fn admitted_context() -> (
        Arc<AiEnforcementRuntime>,
        RateLimitKey,
        QuotaLimits,
        RequestCtx,
    ) {
        let store: Arc<dyn RateLimitStore> = Arc::new(MemoryRateLimitStore::with_defaults(
            Arc::new(ManualRateLimitClock::default()),
        ));
        admitted_context_with_store(store).await
    }

    async fn admitted_context_with_store(
        store: Arc<dyn RateLimitStore>,
    ) -> (
        Arc<AiEnforcementRuntime>,
        RateLimitKey,
        QuotaLimits,
        RequestCtx,
    ) {
        let runtime = Arc::new(
            AiEnforcementRuntime::with_local_quota(
                Arc::clone(&store),
                "test",
                false,
                BudgetCapability::UnsupportedDbLess,
            )
            .unwrap(),
        );
        let key = RateLimitKey::new("test", RateLimitSubject::Global);
        let limits = QuotaLimits {
            requests: NonZeroU64::new(10),
            tokens: NonZeroU64::new(1_000),
        };
        let reserved = QuotaCharge {
            requests: 1,
            tokens: 100,
        };
        let mut ctx = RequestCtx::new();
        let decision = store
            .admit(AdmitCommand {
                request_id: Arc::from(ctx.lifecycle.request_id.clone()),
                key: key.clone(),
                window: WindowSpec::fixed(std::time::Duration::from_secs(60)),
                limits,
                reserve: reserved,
            })
            .await
            .unwrap();
        let AdmissionDecision::Allowed {
            reservation,
            snapshot,
            ..
        } = decision
        else {
            panic!("test admission must be allowed");
        };
        ctx.extensions.insert(AiRateLimitRequestContext::allowed(
            key.clone(),
            limits,
            AiClientProtocol::OpenAi,
            reserved,
            reservation,
            snapshot,
        ));
        (runtime, key, limits, ctx)
    }

    struct UnknownSettlementStore {
        inner: Arc<MemoryRateLimitStore>,
        commands: Mutex<Vec<SettleCommand>>,
    }

    impl UnknownSettlementStore {
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
    impl RateLimitStore for UnknownSettlementStore {
        fn descriptor(&self) -> RateLimitBackendDescriptor {
            self.inner.descriptor()
        }

        async fn admit(
            &self,
            command: AdmitCommand,
        ) -> Result<AdmissionDecision, RateLimitStoreError> {
            self.inner.admit(command).await
        }

        async fn settle(
            &self,
            command: SettleCommand,
        ) -> Result<SettlementResult, RateLimitStoreError> {
            self.commands.lock().unwrap().push(command);
            Err(RateLimitStoreError::new(
                RateLimitStoreErrorKind::OutcomeUnknown,
                "settlement ACK is unknown",
            ))
        }

        async fn inspect(&self, query: InspectQuery) -> Result<InspectResult, RateLimitStoreError> {
            self.inner.inspect(query).await
        }

        fn stats(&self) -> RateLimitStoreStatsSnapshot {
            self.inner.stats()
        }
    }

    #[tokio::test]
    async fn finalizer_keeps_rpm_and_refunds_prompt_when_upstream_was_not_attempted() {
        let (runtime, key, limits, mut ctx) = admitted_context().await;
        let finalizer = AiRateLimitFinalizer::new(Arc::clone(&runtime));

        finalizer.finalize(&[], &mut ctx).await.unwrap();

        let current = runtime
            .quota_runtime()
            .unwrap()
            .store
            .inspect(crate::ratelimit::InspectQuery::Current {
                key,
                window: WindowSpec::fixed(std::time::Duration::from_secs(60)),
                limits,
            })
            .await
            .unwrap();
        let crate::ratelimit::InspectResult::Current(snapshot) = current else {
            panic!("current snapshot expected");
        };
        assert_eq!(snapshot.requests.unwrap().used, 1);
        assert_eq!(snapshot.tokens.unwrap().used, 0);
    }

    #[tokio::test]
    async fn dispatch_abort_refunds_both_dimensions_before_response() {
        let (runtime, _key, _limits, mut ctx) = admitted_context().await;
        let compensator = AiQuotaDispatchAbortCompensator::new(runtime);

        compensator
            .compensate_before_response(
                &mut ctx,
                DispatchAbortCause::new("budget", kong_plugin_system::DispatchAbortKind::Error),
            )
            .await
            .unwrap();

        let request = ctx.extensions.get::<AiRateLimitRequestContext>().unwrap();
        let snapshot = request.response_snapshot.as_ref().unwrap();
        assert_eq!(snapshot.requests.unwrap().used, 0);
        assert_eq!(snapshot.tokens.unwrap().used, 0);
        assert_eq!(request.settlement, QuotaSettlementState::Settled);
    }

    #[tokio::test]
    async fn unknown_abort_keeps_identical_zero_command_for_finalizer_recovery() {
        let store = Arc::new(UnknownSettlementStore::new());
        let runtime_store: Arc<dyn RateLimitStore> = store.clone();
        let (runtime, _key, _limits, mut ctx) = admitted_context_with_store(runtime_store).await;
        let compensator = AiQuotaDispatchAbortCompensator::new(Arc::clone(&runtime));

        let abort_error = compensator
            .compensate_before_response(
                &mut ctx,
                DispatchAbortCause::new("budget", kong_plugin_system::DispatchAbortKind::Error),
            )
            .await
            .unwrap_err();

        assert_eq!(abort_error.code.as_ref(), "quota_backend_unavailable");
        let request = ctx.extensions.get::<AiRateLimitRequestContext>().unwrap();
        assert_eq!(request.settlement, QuotaSettlementState::RetryRequired);
        let saved = request.settlement_command.clone().unwrap();
        assert_eq!(saved.final_charge, QuotaCharge::default());
        let body: serde_json::Value =
            serde_json::from_str(ctx.exit_body.as_deref().unwrap()).unwrap();
        assert_eq!(
            body["error"]["message"],
            "Quota enforcement is temporarily unavailable."
        );

        let finalizer = AiRateLimitFinalizer::new(runtime);
        finalizer.finalize(&[], &mut ctx).await.unwrap_err();

        let commands = store.commands.lock().unwrap();
        assert_eq!(commands.len(), 4);
        assert!(commands.iter().all(|command| command == &saved));
        assert_eq!(
            ctx.extensions
                .get::<AiRateLimitRequestContext>()
                .unwrap()
                .settlement,
            QuotaSettlementState::RetryRequired
        );
    }
}

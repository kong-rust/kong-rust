//! Virtual Key 预算的准入、派发前 intent 与终态结算。

use std::sync::Arc;

use async_trait::async_trait;
use kong_core::traits::RequestCtx;
use kong_plugin_system::{
    DispatchAbortCause, DispatchFailurePolicy, DispatchFailureResponse, LifecycleHookError,
    RequestDispatchAbortHandler, RequestDispatchHook, RequestFinalizer, ResolvedPlugin,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth::AiAuthContext;
use crate::budget::{
    BudgetCostOutcome, BudgetEligibility, BudgetErrorKind, BudgetInspectCommand, BudgetIntent,
    BudgetIntentDisposition, BudgetIntentState, BudgetMetricOperation, BudgetMetricResult,
    BudgetPricingSnapshot, BudgetSettlement, BudgetSettlementDisposition, BudgetStoreError,
    BudgetUnitPriceSnapshot, CreateBudgetIntent, MarkBudgetDispatching, SettleBudgetIntent,
    BUDGET_SCHEMA_VERSION,
};
use crate::models::normalize_budget_amount;
use crate::plugins::context::AiRequestState;
use crate::usage::collector::freeze_pricing_snapshot;
use crate::usage::model::{AiUsageFact, CostStatus, FrozenPricingSnapshot, PricingStatus};
use crate::usage::pricing::{calculate_cost, model_price_overrides, ResolvedPricing};
use crate::usage::AiUsageContext;

use super::budget_registry::BudgetIntentGuard;
use super::policy::AiClientProtocol;
use super::response::reject_with_protocol_error;
use super::runtime::{AiEnforcementRuntime, BudgetRuntimeUnavailable, SupportedBudgetRuntime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetRequestState {
    InspectedEligible,
    Preparing,
    Prepared,
    DispatchCommitPending,
    Dispatching,
    Paused,
    RetryRequired,
    Settled,
}

/// 请求内预算状态。guard 必须在第一次数据库 await 前进入该对象。
pub struct AiBudgetRequestContext {
    pub virtual_key_id: Uuid,
    pub protocol: AiClientProtocol,
    pub state: BudgetRequestState,
    pub intent_id: Option<Uuid>,
    pub pricing_fingerprint: Option<Arc<str>>,
    pub create_command: Option<CreateBudgetIntent>,
    pub settlement_command: Option<SettleBudgetIntent>,
    runtime: Arc<SupportedBudgetRuntime>,
    guard: Option<BudgetIntentGuard>,
}

impl AiBudgetRequestContext {
    fn inspected(
        virtual_key_id: Uuid,
        protocol: AiClientProtocol,
        runtime: Arc<SupportedBudgetRuntime>,
    ) -> Self {
        Self {
            virtual_key_id,
            protocol,
            state: BudgetRequestState::InspectedEligible,
            intent_id: None,
            pricing_fingerprint: None,
            create_command: None,
            settlement_command: None,
            runtime,
            guard: None,
        }
    }

    fn acknowledge(&mut self) {
        if let Some(guard) = self.guard.take() {
            guard.acknowledge();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetInspectionOutcome {
    Continue,
    Exhausted,
    Rejected,
}

/// 在 quota 预扣前读取权威预算状态。
pub async fn inspect_budget_before_quota(
    runtime: &AiEnforcementRuntime,
    auth: &AiAuthContext,
    protocol: AiClientProtocol,
    ctx: &mut RequestCtx,
) -> BudgetInspectionOutcome {
    if !auth.policy.budget_guard_required {
        return BudgetInspectionOutcome::Continue;
    }

    let budget = match runtime.budget_runtime() {
        Ok(runtime) => runtime,
        Err(reason) => {
            let (code, message) = budget_runtime_error(reason);
            reject_with_protocol_error(ctx, protocol, 503, code, message);
            return BudgetInspectionOutcome::Rejected;
        }
    };
    let _admission_permit = match budget.acquire_admission_permit().await {
        Ok(permit) => permit,
        Err(reason) => {
            let (code, message) = budget_runtime_error(reason);
            reject_with_protocol_error(ctx, protocol, 503, code, message);
            return BudgetInspectionOutcome::Rejected;
        }
    };
    let inspection = budget
        .store
        .inspect(BudgetInspectCommand {
            virtual_key_id: auth.virtual_key_id,
        })
        .await;
    match inspection {
        Ok(inspection) => {
            budget.telemetry.record_operation(
                BudgetMetricOperation::Inspect,
                match inspection.eligibility {
                    BudgetEligibility::Eligible => BudgetMetricResult::Success,
                    BudgetEligibility::Paused | BudgetEligibility::Exhausted => {
                        BudgetMetricResult::Rejected
                    }
                },
                (inspection.eligibility == BudgetEligibility::Exhausted)
                    .then_some(BudgetErrorKind::Exhausted),
            );
            match inspection.eligibility {
                BudgetEligibility::Eligible => {
                    ctx.extensions.insert(AiBudgetRequestContext::inspected(
                        auth.virtual_key_id,
                        protocol,
                        Arc::clone(&budget),
                    ));
                    BudgetInspectionOutcome::Continue
                }
                BudgetEligibility::Paused => BudgetInspectionOutcome::Continue,
                BudgetEligibility::Exhausted => BudgetInspectionOutcome::Exhausted,
            }
        }
        Err(error) if error.kind() == BudgetErrorKind::Exhausted => {
            record_budget_error(&budget, BudgetMetricOperation::Inspect, &error);
            BudgetInspectionOutcome::Exhausted
        }
        Err(error) => {
            record_budget_error(&budget, BudgetMetricOperation::Inspect, &error);
            reject_store_error(ctx, protocol, &error);
            BudgetInspectionOutcome::Rejected
        }
    }
}

/// 模型选择完成后创建 intent 并提交 dispatch transition。
pub struct AiBudgetDispatchHook {
    _private: (),
}

impl AiBudgetDispatchHook {
    pub fn new(_runtime: Arc<AiEnforcementRuntime>) -> Self {
        Self { _private: () }
    }

    async fn execute(&self, ctx: &mut RequestCtx) -> Result<(), LifecycleHookError> {
        let Some((virtual_key_id, protocol, budget)) = ctx
            .extensions
            .get::<AiBudgetRequestContext>()
            .map(|request| {
                (
                    request.virtual_key_id,
                    request.protocol,
                    Arc::clone(&request.runtime),
                )
            })
        else {
            return Ok(());
        };
        if !budget.owner_available() {
            let (code, message) = budget_runtime_error(BudgetRuntimeUnavailable::OwnerFenced);
            reject_with_protocol_error(ctx, protocol, 503, code, message);
            ctx.forbid_upstream_dispatch();
            return Err(LifecycleHookError::new(code, message));
        }
        let _admission_permit = budget.acquire_admission_permit().await.map_err(|reason| {
            let (code, message) = budget_runtime_error(reason);
            reject_with_protocol_error(ctx, protocol, 503, code, message);
            ctx.forbid_upstream_dispatch();
            LifecycleHookError::new(code, message)
        })?;

        let pricing_snapshot = match build_pricing_snapshot(&budget, ctx) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                reject_with_protocol_error(
                    ctx,
                    protocol,
                    503,
                    "budget_pricing_unavailable",
                    "Budget pricing is unavailable for this request.",
                );
                ctx.forbid_upstream_dispatch();
                return Err(error);
            }
        };
        let pricing_json = serde_json::to_vec(&pricing_snapshot).map_err(|error| {
            LifecycleHookError::new("budget_pricing_unavailable", error.to_string())
        })?;
        let pricing_fingerprint: Arc<str> = Arc::from(sha256_hex(&pricing_json));
        freeze_pricing_snapshot(
            ctx,
            FrozenPricingSnapshot {
                fingerprint: Arc::clone(&pricing_fingerprint),
                provider_type: pricing_snapshot.provider_type.clone(),
                model: pricing_snapshot.model.clone(),
                input: pricing_snapshot.input.as_ref().map(Into::into),
                output: pricing_snapshot.output.as_ref().map(Into::into),
                max_prompt_tokens: pricing_snapshot.max_prompt_tokens,
            },
        )
        .map_err(|reason| {
            reject_with_protocol_error(
                ctx,
                protocol,
                503,
                "budget_pricing_unavailable",
                "Budget pricing is unavailable for this request.",
            );
            ctx.forbid_upstream_dispatch();
            LifecycleHookError::new("budget_pricing_context_invalid", reason)
        })?;
        let request_id: Arc<str> = Arc::from(ctx.lifecycle.request_id.clone());
        let intent_id = Uuid::now_v7();
        let operation_id: Arc<str> = Arc::from(format!("intent:v1:{request_id}"));
        let command_fingerprint: Arc<str> = Arc::from(sha256_hex(
            format!(
                "intent:v1\n{request_id}\n{virtual_key_id}\n{intent_id}\n{}\n{}\n{}",
                budget.node_id, budget.owner_session_id, pricing_fingerprint
            )
            .as_bytes(),
        ));
        let create_command = CreateBudgetIntent {
            intent_id,
            virtual_key_id,
            request_id: Arc::clone(&request_id),
            operation_id,
            command_fingerprint,
            pricing_fingerprint: Arc::clone(&pricing_fingerprint),
            pricing_snapshot,
            node_id: budget.node_id,
            owner_session_id: budget.owner_session_id,
            stale_after: budget.stale_after,
        };
        let guard = budget
            .registry
            .try_reserve(Arc::clone(&request_id))
            .map_err(|error| {
                reject_with_protocol_error(
                    ctx,
                    protocol,
                    503,
                    "budget_accounting_unavailable",
                    "Budget accounting is temporarily unavailable.",
                );
                ctx.forbid_upstream_dispatch();
                LifecycleHookError::new("budget_registry_overloaded", error.to_string())
            })?;
        guard
            .record_create_command(create_command.clone())
            .map_err(|error| {
                reject_with_protocol_error(
                    ctx,
                    protocol,
                    503,
                    "budget_accounting_unavailable",
                    "Budget accounting is temporarily unavailable.",
                );
                ctx.forbid_upstream_dispatch();
                LifecycleHookError::new("budget_registry_invalid", error.to_string())
            })?;

        {
            let request = ctx
                .extensions
                .get_mut::<AiBudgetRequestContext>()
                .expect("预算请求上下文必须仍然存在");
            request.state = BudgetRequestState::Preparing;
            request.pricing_fingerprint = Some(Arc::clone(&pricing_fingerprint));
            request.create_command = Some(create_command.clone());
            request.guard = Some(guard);
        }

        let intent = match budget.store.create_intent(create_command).await {
            Ok(intent) => {
                record_intent_result(&budget, BudgetMetricOperation::CreateIntent, &intent);
                intent
            }
            Err(error) => {
                record_budget_error(&budget, BudgetMetricOperation::CreateIntent, &error);
                if error.kind() != BudgetErrorKind::OutcomeUnknown {
                    acknowledge_budget_guard(ctx);
                    if let Some(request) = ctx.extensions.get_mut::<AiBudgetRequestContext>() {
                        request.create_command = None;
                    }
                }
                reject_store_error(ctx, protocol, &error);
                ctx.forbid_upstream_dispatch();
                return Err(LifecycleHookError::new(
                    budget_error_code(error.kind()),
                    error.to_string(),
                ));
            }
        };

        if intent.disposition == BudgetIntentDisposition::Paused {
            let request = ctx
                .extensions
                .get_mut::<AiBudgetRequestContext>()
                .expect("预算请求上下文必须仍然存在");
            request.state = BudgetRequestState::Paused;
            request.acknowledge();
            return Ok(());
        }
        let record = intent.record.ok_or_else(|| {
            LifecycleHookError::new("budget_accounting_invalid", "intent 缺少持久记录")
        })?;
        {
            let request = ctx
                .extensions
                .get_mut::<AiBudgetRequestContext>()
                .expect("预算请求上下文必须仍然存在");
            request
                .guard
                .as_ref()
                .ok_or_else(|| {
                    LifecycleHookError::new("budget_registry_missing", "预算 guard 丢失")
                })?
                .mark_prepared(record.id)
                .map_err(|error| {
                    LifecycleHookError::new("budget_registry_invalid", error.to_string())
                })?;
            request.intent_id = Some(record.id);
            request.state = BudgetRequestState::Prepared;
        }

        let dispatch_command = MarkBudgetDispatching {
            intent_id: record.id,
            virtual_key_id,
            request_id,
            operation_id: Arc::from(format!("budget-dispatch:v1:{}", ctx.lifecycle.request_id)),
            node_id: budget.node_id,
            owner_session_id: budget.owner_session_id,
        };
        {
            let request = ctx
                .extensions
                .get_mut::<AiBudgetRequestContext>()
                .expect("预算请求上下文必须仍然存在");
            request
                .guard
                .as_ref()
                .expect("prepared intent 必须持有 guard")
                .mark_dispatch_commit_pending()
                .map_err(|error| {
                    LifecycleHookError::new("budget_registry_invalid", error.to_string())
                })?;
            request.state = BudgetRequestState::DispatchCommitPending;
        }

        match budget.store.mark_dispatching(dispatch_command).await {
            Ok(intent) => {
                record_intent_result(&budget, BudgetMetricOperation::MarkDispatching, &intent);
                let request = ctx
                    .extensions
                    .get_mut::<AiBudgetRequestContext>()
                    .expect("预算请求上下文必须仍然存在");
                request
                    .guard
                    .as_ref()
                    .expect("dispatching intent 必须持有 guard")
                    .mark_dispatching()
                    .map_err(|error| {
                        LifecycleHookError::new("budget_registry_invalid", error.to_string())
                    })?;
                request.state = BudgetRequestState::Dispatching;
                Ok(())
            }
            Err(error) => {
                record_budget_error(&budget, BudgetMetricOperation::MarkDispatching, &error);
                reject_store_error(ctx, protocol, &error);
                ctx.forbid_upstream_dispatch();
                Err(LifecycleHookError::new(
                    budget_error_code(error.kind()),
                    error.to_string(),
                ))
            }
        }
    }
}

#[async_trait]
impl RequestDispatchHook for AiBudgetDispatchHook {
    fn name(&self) -> &'static str {
        "ai-budget-dispatch"
    }

    fn compensation_domain(&self) -> &'static str {
        "ai-budget"
    }

    fn failure_policy(&self) -> DispatchFailurePolicy {
        DispatchFailurePolicy::FailClosed(DispatchFailureResponse::new(
            503,
            "budget_accounting_unavailable",
            "Budget accounting is temporarily unavailable.",
        ))
    }

    async fn before_upstream_dispatch(
        &self,
        _plugins: &[ResolvedPlugin],
        ctx: &mut RequestCtx,
    ) -> Result<(), LifecycleHookError> {
        self.execute(ctx).await
    }
}

/// critical dispatch 失败后把 prepared/dispatching intent 结算为未发生。
pub struct AiBudgetDispatchAbortCompensator {
    _private: (),
}

impl AiBudgetDispatchAbortCompensator {
    pub fn new(_runtime: Arc<AiEnforcementRuntime>) -> Self {
        Self { _private: () }
    }
}

#[async_trait]
impl RequestDispatchAbortHandler for AiBudgetDispatchAbortCompensator {
    fn name(&self) -> &'static str {
        "ai-budget-dispatch-abort"
    }

    fn compensation_domain(&self) -> &'static str {
        "ai-budget"
    }

    async fn compensate_before_response(
        &self,
        ctx: &mut RequestCtx,
        _cause: DispatchAbortCause,
    ) -> Result<(), LifecycleHookError> {
        let Some(budget) = ctx
            .extensions
            .get::<AiBudgetRequestContext>()
            .map(|request| Arc::clone(&request.runtime))
        else {
            return Ok(());
        };

        ensure_intent_for_safe_zero(&budget, ctx).await?;
        let Some(command) = prepare_safe_zero_settlement(ctx)? else {
            return Ok(());
        };
        match budget.store.settle(command).await {
            Ok(settlement) => {
                record_settlement_result(&budget, &settlement);
                acknowledge_budget_guard(ctx);
                Ok(())
            }
            Err(error) => {
                record_budget_error(&budget, BudgetMetricOperation::Settle, &error);
                mark_budget_retry(ctx);
                Err(LifecycleHookError::new(
                    budget_error_code(error.kind()),
                    error.to_string(),
                ))
            }
        }
    }
}

/// 客户端响应完成后独立结算预算 intent。
pub struct AiBudgetFinalizer {
    _private: (),
}

impl AiBudgetFinalizer {
    pub fn new(_runtime: Arc<AiEnforcementRuntime>) -> Self {
        Self { _private: () }
    }
}

#[async_trait]
impl RequestFinalizer for AiBudgetFinalizer {
    fn name(&self) -> &'static str {
        "ai-budget-finalizer"
    }

    async fn finalize(
        &self,
        _plugins: &[ResolvedPlugin],
        ctx: &mut RequestCtx,
    ) -> Result<(), LifecycleHookError> {
        let Some(budget) = ctx
            .extensions
            .get::<AiBudgetRequestContext>()
            .map(|request| Arc::clone(&request.runtime))
        else {
            return Ok(());
        };
        let needs_safe_zero_replay = !ctx.lifecycle.upstream_attempted
            && ctx
                .extensions
                .get::<AiBudgetRequestContext>()
                .is_some_and(|request| {
                    request.intent_id.is_none()
                        && request.create_command.is_some()
                        && request.guard.is_some()
                });
        if needs_safe_zero_replay {
            ensure_intent_for_safe_zero(&budget, ctx).await?;
        }
        let command = if needs_safe_zero_replay {
            prepare_safe_zero_settlement(ctx)?
        } else {
            prepare_final_settlement(ctx)?
        };
        let Some(command) = command else {
            return Ok(());
        };
        match budget.store.settle(command).await {
            Ok(settlement) => {
                record_settlement_result(&budget, &settlement);
                acknowledge_budget_guard(ctx);
                if let Some(request) = ctx.extensions.get_mut::<AiBudgetRequestContext>() {
                    request.state = BudgetRequestState::Settled;
                }
                Ok(())
            }
            Err(error) => {
                record_budget_error(&budget, BudgetMetricOperation::Settle, &error);
                mark_budget_retry(ctx);
                Err(LifecycleHookError::new(
                    budget_error_code(error.kind()),
                    error.to_string(),
                ))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetRegistryRecoveryStats {
    pub scanned: u32,
    pub settled: u32,
    pub failed: u32,
}

/// 收口因 timeout、panic 或请求任务取消而失去 guard 的进程内 intent。
pub async fn recover_budget_registry_once(
    budget: &SupportedBudgetRuntime,
    max_entries: usize,
) -> BudgetRegistryRecoveryStats {
    let mut stats = BudgetRegistryRecoveryStats::default();
    for snapshot in budget.registry.snapshots().into_iter().take(max_entries) {
        if snapshot.guard_alive {
            continue;
        }
        stats.scanned = stats.scanned.saturating_add(1);

        let command = if let Some(command) = snapshot.settlement {
            Some(command)
        } else {
            match recovery_settlement_command(budget, &snapshot).await {
                Ok(command) => command,
                Err(error) => {
                    stats.failed = stats.failed.saturating_add(1);
                    tracing::warn!(
                        request_id = %snapshot.request_id,
                        "恢复预算 intent 失败: {error}"
                    );
                    continue;
                }
            }
        };
        let Some(command) = command else {
            budget.registry.acknowledge_request(&snapshot.request_id);
            stats.settled = stats.settled.saturating_add(1);
            continue;
        };
        match budget.store.settle(command).await {
            Ok(settlement) => {
                record_settlement_result(budget, &settlement);
                budget.registry.acknowledge_request(&snapshot.request_id);
                stats.settled = stats.settled.saturating_add(1);
            }
            Err(error) => {
                record_budget_error(budget, BudgetMetricOperation::Settle, &error);
                stats.failed = stats.failed.saturating_add(1);
                tracing::warn!(
                    request_id = %snapshot.request_id,
                    error_kind = ?error.kind(),
                    "重放预算 settlement 失败"
                );
            }
        }
    }
    budget
        .telemetry
        .record_registry_recovery(stats.scanned, stats.settled, stats.failed);
    stats
}

async fn recovery_settlement_command(
    budget: &SupportedBudgetRuntime,
    snapshot: &super::budget_registry::ActiveBudgetIntentSnapshot,
) -> Result<Option<SettleBudgetIntent>, LifecycleHookError> {
    let create = snapshot.create_command.clone().ok_or_else(|| {
        LifecycleHookError::new(
            "budget_recovery_missing_create",
            "恢复记录缺少 create command",
        )
    })?;
    let mut recovered_dispatching = false;
    let intent_id = match snapshot.intent_id {
        Some(intent_id) => intent_id,
        None => {
            let record = budget
                .store
                .lookup_intent((&create).into())
                .await
                .map_err(|error| {
                    record_budget_error(budget, BudgetMetricOperation::LookupIntent, &error);
                    LifecycleHookError::new(budget_error_code(error.kind()), error.to_string())
                })?;
            budget.telemetry.record_operation(
                BudgetMetricOperation::LookupIntent,
                if record.is_some() {
                    BudgetMetricResult::Replayed
                } else {
                    BudgetMetricResult::Success
                },
                None,
            );
            let Some(record) = record else {
                // create 未提交且请求从未进入 upstream，释放本地容量即可。若原
                // transaction 随后才提交，PG stale recovery 仍会以 prepared
                // intent 将其按零成本收口。
                return Ok(None);
            };
            recovered_dispatching = match record.state {
                BudgetIntentState::Prepared => false,
                BudgetIntentState::Dispatching => true,
                BudgetIntentState::Unresolved
                | BudgetIntentState::Settled
                | BudgetIntentState::Resolved => return Ok(None),
            };
            budget
                .registry
                .mark_recovered_intent(&snapshot.request_id, record.id, recovered_dispatching)
                .map_err(|error| {
                    LifecycleHookError::new("budget_registry_invalid", error.to_string())
                })?;
            record.id
        }
    };
    let unresolved = snapshot.state
        == super::budget_registry::ActiveBudgetIntentState::NeedsUnresolved
        || recovered_dispatching;
    let cost = if unresolved {
        BudgetCostOutcome {
            status: CostStatus::Unavailable,
            amount_usd: None,
            reasons: vec!["request_lifecycle_cancelled_after_dispatch".to_string()],
        }
    } else {
        BudgetCostOutcome {
            status: CostStatus::NotIncurred,
            amount_usd: Some(rust_decimal::Decimal::ZERO),
            reasons: vec!["request_lifecycle_cancelled_before_dispatch".to_string()],
        }
    };
    let command = settlement_command(
        intent_id,
        create.virtual_key_id,
        &snapshot.request_id,
        Arc::clone(&create.pricing_fingerprint),
        None,
        cost,
    )?;
    budget
        .registry
        .record_recovery_settlement(&snapshot.request_id, command.clone(), unresolved)
        .map_err(|error| LifecycleHookError::new("budget_registry_invalid", error.to_string()))?;
    Ok(Some(command))
}

fn build_pricing_snapshot(
    budget: &SupportedBudgetRuntime,
    ctx: &RequestCtx,
) -> Result<BudgetPricingSnapshot, LifecycleHookError> {
    let state = ctx.extensions.get::<AiRequestState>().ok_or_else(|| {
        LifecycleHookError::new("budget_pricing_unavailable", "ai-proxy 未形成模型选择结果")
    })?;
    let provider_type = state.provider_config.provider_type.trim();
    let model = state.model.model_name.trim();
    if provider_type.is_empty() || model.is_empty() {
        return Err(LifecycleHookError::new(
            "budget_pricing_unavailable",
            "模型或 provider 为空",
        ));
    }
    let features = ctx
        .extensions
        .get::<AiUsageContext>()
        .map(|usage| usage.pricing_features.clone())
        .unwrap_or_default();
    let overrides =
        model_price_overrides(&state.model, provider_type, model, ctx.lifecycle.started_at);
    let prompt_tokens = i64::try_from(state.estimated_prompt_tokens).map_err(|_| {
        LifecycleHookError::new(
            "budget_pricing_unavailable",
            "prompt token 估值超出预算计价范围",
        )
    })?;
    let pricing = budget.catalog.resolve_snapshot(
        provider_type,
        model,
        ctx.lifecycle.started_at,
        Some(prompt_tokens),
        &overrides,
        &features,
    );
    if pricing.status != PricingStatus::Matched
        || pricing.input.is_none()
        || pricing.output.is_none()
    {
        return Err(LifecycleHookError::new(
            "budget_pricing_unavailable",
            if pricing.unsupported_reasons.is_empty() {
                "未找到完整价格".to_string()
            } else {
                pricing.unsupported_reasons.join(",")
            },
        ));
    }
    let full_override = overrides.input.is_some() && overrides.output.is_some();
    Ok(BudgetPricingSnapshot {
        schema_version: BUDGET_SCHEMA_VERSION,
        provider_type: provider_type.to_string(),
        model: model.to_string(),
        input: pricing.input.as_ref().map(BudgetUnitPriceSnapshot::from),
        output: pricing.output.as_ref().map(BudgetUnitPriceSnapshot::from),
        max_prompt_tokens: (!full_override)
            .then(|| {
                budget
                    .catalog
                    .max_prompt_tokens(provider_type, model, ctx.lifecycle.started_at)
            })
            .flatten(),
    })
}

async fn ensure_intent_for_safe_zero(
    budget: &SupportedBudgetRuntime,
    ctx: &mut RequestCtx,
) -> Result<(), LifecycleHookError> {
    let (needs_replay, command) = {
        let request = ctx
            .extensions
            .get::<AiBudgetRequestContext>()
            .expect("预算请求上下文必须存在");
        (
            request.intent_id.is_none() && request.create_command.is_some(),
            request.create_command.clone(),
        )
    };
    if needs_replay {
        let command = command.expect("需要重放时必须保存 create command");
        let intent = budget.store.create_intent(command).await.map_err(|error| {
            record_budget_error(budget, BudgetMetricOperation::CreateIntent, &error);
            LifecycleHookError::new(budget_error_code(error.kind()), error.to_string())
        })?;
        record_intent_result(budget, BudgetMetricOperation::CreateIntent, &intent);
        if intent.disposition == BudgetIntentDisposition::Paused {
            let request = ctx
                .extensions
                .get_mut::<AiBudgetRequestContext>()
                .expect("预算请求上下文必须存在");
            request.state = BudgetRequestState::Paused;
            request.acknowledge();
            return Ok(());
        }
        let record = intent.record.ok_or_else(|| {
            LifecycleHookError::new("budget_accounting_invalid", "重放 intent 缺少记录")
        })?;
        let request = ctx
            .extensions
            .get_mut::<AiBudgetRequestContext>()
            .expect("预算请求上下文必须存在");
        request
            .guard
            .as_ref()
            .ok_or_else(|| LifecycleHookError::new("budget_registry_missing", "预算 guard 丢失"))?
            .mark_prepared(record.id)
            .map_err(|error| {
                LifecycleHookError::new("budget_registry_invalid", error.to_string())
            })?;
        request.intent_id = Some(record.id);
        request.state = BudgetRequestState::Prepared;
    }
    Ok(())
}

fn prepare_safe_zero_settlement(
    ctx: &mut RequestCtx,
) -> Result<Option<SettleBudgetIntent>, LifecycleHookError> {
    let request_id = ctx.lifecycle.request_id.clone();
    let request = ctx
        .extensions
        .get_mut::<AiBudgetRequestContext>()
        .expect("预算请求上下文必须存在");
    if matches!(
        request.state,
        BudgetRequestState::InspectedEligible
            | BudgetRequestState::Paused
            | BudgetRequestState::Settled
    ) {
        return Ok(None);
    }
    if request.intent_id.is_none() && request.create_command.is_none() && request.guard.is_none() {
        return Ok(None);
    }
    if let Some(command) = request.settlement_command.clone() {
        return Ok(Some(command));
    }
    let intent_id = request
        .intent_id
        .ok_or_else(|| LifecycleHookError::new("budget_intent_missing", "预算 intent 尚未确认"))?;
    let pricing_fingerprint = request.pricing_fingerprint.clone().ok_or_else(|| {
        LifecycleHookError::new("budget_pricing_fingerprint_missing", "预算价格指纹丢失")
    })?;
    let cost = BudgetCostOutcome {
        status: CostStatus::NotIncurred,
        amount_usd: Some(rust_decimal::Decimal::ZERO),
        reasons: vec!["upstream_dispatch_aborted".to_string()],
    };
    let command = settlement_command(
        intent_id,
        request.virtual_key_id,
        &request_id,
        pricing_fingerprint,
        None,
        cost,
    )?;
    request
        .guard
        .as_ref()
        .ok_or_else(|| LifecycleHookError::new("budget_registry_missing", "预算 guard 丢失"))?
        .mark_needs_safe_zero()
        .and_then(|_| {
            request
                .guard
                .as_ref()
                .expect("预算 guard 必须存在")
                .record_safe_zero_settlement(command.clone())
        })
        .map_err(|error| LifecycleHookError::new("budget_registry_invalid", error.to_string()))?;
    request.settlement_command = Some(command.clone());
    Ok(Some(command))
}

fn prepare_final_settlement(
    ctx: &mut RequestCtx,
) -> Result<Option<SettleBudgetIntent>, LifecycleHookError> {
    let request_id = ctx.lifecycle.request_id.clone();
    let upstream_attempted = ctx.lifecycle.upstream_attempted;
    let fact = ctx.extensions.get::<Arc<AiUsageFact>>().cloned();
    let request = ctx
        .extensions
        .get_mut::<AiBudgetRequestContext>()
        .expect("预算请求上下文必须存在");
    if matches!(
        request.state,
        BudgetRequestState::InspectedEligible
            | BudgetRequestState::Paused
            | BudgetRequestState::Settled
    ) {
        return Ok(None);
    }
    if request.intent_id.is_none() && request.guard.is_none() {
        return Ok(None);
    }
    if let Some(command) = request.settlement_command.clone() {
        return Ok(Some(command));
    }
    let intent_id = request
        .intent_id
        .ok_or_else(|| LifecycleHookError::new("budget_intent_missing", "预算 intent 尚未确认"))?;
    let pricing_fingerprint = request.pricing_fingerprint.clone().ok_or_else(|| {
        LifecycleHookError::new("budget_pricing_fingerprint_missing", "预算价格指纹丢失")
    })?;
    let pricing_snapshot = request
        .create_command
        .as_ref()
        .map(|command| command.pricing_snapshot.clone())
        .ok_or_else(|| {
            LifecycleHookError::new("budget_pricing_snapshot_missing", "预算价格快照丢失")
        })?;
    let (usage_fact_id, cost) = cost_from_fact(
        fact.as_deref(),
        &request_id,
        request.virtual_key_id,
        upstream_attempted,
        &pricing_fingerprint,
        &pricing_snapshot,
    );
    let command = settlement_command(
        intent_id,
        request.virtual_key_id,
        &request_id,
        pricing_fingerprint,
        usage_fact_id,
        cost,
    )?;
    request
        .guard
        .as_ref()
        .ok_or_else(|| LifecycleHookError::new("budget_registry_missing", "预算 guard 丢失"))?
        .record_settlement(command.clone())
        .map_err(|error| LifecycleHookError::new("budget_registry_invalid", error.to_string()))?;
    request.settlement_command = Some(command.clone());
    Ok(Some(command))
}

fn cost_from_fact(
    fact: Option<&AiUsageFact>,
    request_id: &str,
    virtual_key_id: Uuid,
    upstream_attempted: bool,
    pricing_fingerprint: &str,
    pricing_snapshot: &BudgetPricingSnapshot,
) -> (Option<Uuid>, BudgetCostOutcome) {
    let Some(fact) = fact else {
        return if upstream_attempted {
            (
                None,
                BudgetCostOutcome {
                    status: CostStatus::Unavailable,
                    amount_usd: None,
                    reasons: vec!["missing_usage_fact".to_string()],
                },
            )
        } else {
            (
                None,
                BudgetCostOutcome {
                    status: CostStatus::NotIncurred,
                    amount_usd: Some(rust_decimal::Decimal::ZERO),
                    reasons: vec!["upstream_not_attempted".to_string()],
                },
            )
        };
    };
    if fact.request_id != request_id || fact.virtual_key_id != Some(virtual_key_id) {
        return (
            Some(fact.id),
            BudgetCostOutcome {
                status: CostStatus::Unavailable,
                amount_usd: None,
                reasons: vec!["usage_fact_identity_mismatch".to_string()],
            },
        );
    }
    if fact.pricing_fingerprint.as_deref() != Some(pricing_fingerprint) {
        return (
            Some(fact.id),
            BudgetCostOutcome {
                status: CostStatus::Unavailable,
                amount_usd: None,
                reasons: vec!["usage_fact_pricing_fingerprint_mismatch".to_string()],
            },
        );
    }
    if pricing_snapshot
        .max_prompt_tokens
        .zip(fact.prompt_tokens.map(|field| field.value))
        .is_some_and(|(maximum, actual)| actual > maximum)
    {
        return (
            Some(fact.id),
            BudgetCostOutcome {
                status: CostStatus::Unavailable,
                amount_usd: None,
                reasons: vec!["actual_prompt_exceeds_frozen_pricing_limit".to_string()],
            },
        );
    }
    let pricing = ResolvedPricing {
        input: pricing_snapshot.input.as_ref().map(Into::into),
        output: pricing_snapshot.output.as_ref().map(Into::into),
        status: PricingStatus::Matched,
        unsupported_reasons: Vec::new(),
    };
    if fact.input_price != pricing.input
        || fact.output_price != pricing.output
        || fact.pricing_status != pricing.status
    {
        return (
            Some(fact.id),
            BudgetCostOutcome {
                status: CostStatus::Unavailable,
                amount_usd: None,
                reasons: vec!["usage_fact_pricing_snapshot_mismatch".to_string()],
            },
        );
    }
    let computed = calculate_cost(
        &pricing,
        fact.prompt_tokens,
        fact.completion_tokens,
        fact.usage_source,
        upstream_attempted,
        fact.usage_unavailable_reasons
            .iter()
            .any(|reason| reason == "invalid_token_value"),
    );
    if fact.cost_status != computed.status
        || fact.cost_usd != computed.cost_usd
        || fact.cost_unavailable_reasons != computed.unavailable_reasons
    {
        return (
            Some(fact.id),
            BudgetCostOutcome {
                status: CostStatus::Unavailable,
                amount_usd: None,
                reasons: vec!["usage_fact_cost_mismatch".to_string()],
            },
        );
    }
    (
        Some(fact.id),
        BudgetCostOutcome {
            status: computed.status,
            amount_usd: computed.cost_usd,
            reasons: computed.unavailable_reasons,
        },
    )
}

fn settlement_command(
    intent_id: Uuid,
    virtual_key_id: Uuid,
    request_id: &str,
    pricing_fingerprint: Arc<str>,
    usage_fact_id: Option<Uuid>,
    mut cost: BudgetCostOutcome,
) -> Result<SettleBudgetIntent, LifecycleHookError> {
    if matches!(cost.status, CostStatus::Calculated | CostStatus::Estimated) {
        cost.amount_usd = cost
            .amount_usd
            .map(normalize_budget_amount)
            .transpose()
            .map_err(|error| LifecycleHookError::new("budget_numeric_invalid", error))?;
        if cost.amount_usd.is_none() {
            cost.status = CostStatus::Unavailable;
            cost.reasons.push("missing_cost_amount".to_string());
        }
    }
    if cost.status == CostStatus::NotIncurred {
        cost.amount_usd = Some(rust_decimal::Decimal::ZERO);
    }
    let amount = cost
        .amount_usd
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let command_fingerprint: Arc<str> = Arc::from(sha256_hex(
        format!(
            "budget-settle:v1\n{request_id}\n{virtual_key_id}\n{intent_id}\n{}\n{amount}\n{}\n{}",
            cost.status.as_str(),
            cost.reasons.join("\u{1f}"),
            usage_fact_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "null".to_string())
        )
        .as_bytes(),
    ));
    Ok(SettleBudgetIntent {
        intent_id,
        virtual_key_id,
        request_id: Arc::from(request_id),
        operation_id: Arc::from(format!("budget-settle:v1:{request_id}")),
        command_fingerprint,
        pricing_fingerprint,
        usage_fact_id,
        cost,
    })
}

fn acknowledge_budget_guard(ctx: &mut RequestCtx) {
    if let Some(request) = ctx.extensions.get_mut::<AiBudgetRequestContext>() {
        request.acknowledge();
    }
}

fn mark_budget_retry(ctx: &mut RequestCtx) {
    if let Some(request) = ctx.extensions.get_mut::<AiBudgetRequestContext>() {
        request.state = BudgetRequestState::RetryRequired;
    }
}

fn record_intent_result(
    budget: &SupportedBudgetRuntime,
    operation: BudgetMetricOperation,
    intent: &BudgetIntent,
) {
    budget.telemetry.record_operation(
        operation,
        match intent.disposition {
            BudgetIntentDisposition::CreateReplayed | BudgetIntentDisposition::DispatchReplayed => {
                BudgetMetricResult::Replayed
            }
            BudgetIntentDisposition::Paused => BudgetMetricResult::Rejected,
            BudgetIntentDisposition::Created | BudgetIntentDisposition::DispatchApplied => {
                BudgetMetricResult::Success
            }
        },
        None,
    );
}

fn record_settlement_result(budget: &SupportedBudgetRuntime, settlement: &BudgetSettlement) {
    budget.telemetry.record_operation(
        BudgetMetricOperation::Settle,
        match settlement.disposition {
            BudgetSettlementDisposition::Applied => BudgetMetricResult::Success,
            BudgetSettlementDisposition::Replayed
            | BudgetSettlementDisposition::AlreadyReconciled => BudgetMetricResult::Replayed,
            BudgetSettlementDisposition::MarkedUnresolved
            | BudgetSettlementDisposition::AlreadyUnresolved => BudgetMetricResult::Unresolved,
        },
        None,
    );
}

fn record_budget_error(
    budget: &SupportedBudgetRuntime,
    operation: BudgetMetricOperation,
    error: &BudgetStoreError,
) {
    budget.telemetry.record_operation(
        operation,
        match error.kind() {
            BudgetErrorKind::Exhausted => BudgetMetricResult::Rejected,
            BudgetErrorKind::AccountingUnresolved
            | BudgetErrorKind::ReconciliationRequired
            | BudgetErrorKind::IntentActive
            | BudgetErrorKind::AlreadyReconciled => BudgetMetricResult::Unresolved,
            _ => BudgetMetricResult::Failed,
        },
        Some(error.kind()),
    );
}

fn reject_store_error(ctx: &mut RequestCtx, protocol: AiClientProtocol, error: &BudgetStoreError) {
    let (status, code, message) = match error.kind() {
        BudgetErrorKind::Exhausted => (
            403,
            "budget_exhausted",
            "The virtual key budget has been exhausted.",
        ),
        BudgetErrorKind::AccountingUnresolved
        | BudgetErrorKind::Corrupt
        | BudgetErrorKind::NumericOverflow
        | BudgetErrorKind::Conflict
        | BudgetErrorKind::ReconciliationRequired
        | BudgetErrorKind::IntentActive
        | BudgetErrorKind::AlreadyReconciled
        | BudgetErrorKind::AccountBusy => (
            503,
            "budget_accounting_unresolved",
            "Budget accounting requires reconciliation.",
        ),
        BudgetErrorKind::Unsupported => (
            503,
            "budget_accounting_unsupported",
            "Budget accounting is not supported in this deployment mode.",
        ),
        BudgetErrorKind::PricingUnavailable => (
            503,
            "budget_pricing_unavailable",
            "Budget pricing is unavailable for this request.",
        ),
        BudgetErrorKind::AccountingUnavailable
        | BudgetErrorKind::OutcomeUnknown
        | BudgetErrorKind::NotFound => (
            503,
            "budget_accounting_unavailable",
            "Budget accounting is temporarily unavailable.",
        ),
    };
    reject_with_protocol_error(ctx, protocol, status, code, message);
}

fn budget_runtime_error(reason: BudgetRuntimeUnavailable) -> (&'static str, &'static str) {
    match reason {
        BudgetRuntimeUnavailable::DbLessMode | BudgetRuntimeUnavailable::HybridMode => (
            "budget_accounting_unsupported",
            "Budget accounting is not supported in this deployment mode.",
        ),
        BudgetRuntimeUnavailable::AccountingUnavailable | BudgetRuntimeUnavailable::OwnerFenced => {
            (
                "budget_accounting_unavailable",
                "Budget accounting is temporarily unavailable.",
            )
        }
    }
}

fn budget_error_code(kind: BudgetErrorKind) -> &'static str {
    match kind {
        BudgetErrorKind::Exhausted => "budget_exhausted",
        BudgetErrorKind::AccountingUnresolved
        | BudgetErrorKind::Corrupt
        | BudgetErrorKind::NumericOverflow
        | BudgetErrorKind::Conflict
        | BudgetErrorKind::ReconciliationRequired
        | BudgetErrorKind::IntentActive
        | BudgetErrorKind::AlreadyReconciled
        | BudgetErrorKind::AccountBusy => "budget_accounting_unresolved",
        BudgetErrorKind::Unsupported => "budget_accounting_unsupported",
        BudgetErrorKind::PricingUnavailable => "budget_pricing_unavailable",
        BudgetErrorKind::AccountingUnavailable
        | BudgetErrorKind::OutcomeUnknown
        | BudgetErrorKind::NotFound => "budget_accounting_unavailable",
    }
}

fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use chrono::Utc;
    use rust_decimal::Decimal;

    use super::*;
    use crate::budget::{
        BudgetBackendDescriptor, BudgetBackendKind, BudgetCheckpoint, BudgetInspection,
        BudgetIntentRecord, BudgetOwnerLease, BudgetRecoveryBatch, BudgetStore,
        CheckpointBudgetAccount, HeartbeatBudgetOwner, LookupBudgetIntent,
        RecoverStaleBudgetIntents, RegisterBudgetOwner, StopBudgetOwner,
    };
    use crate::enforcement::ActiveBudgetIntentRegistry;
    use crate::usage::model::{
        AiUsageOutcome, CacheStatus, PriceSnapshot, TokenField, TokenFieldSource, UsageSource,
    };
    use crate::usage::PriceCatalog;

    struct MissingIntentRecoveryStore {
        lookups: AtomicUsize,
    }

    #[async_trait]
    impl BudgetStore for MissingIntentRecoveryStore {
        fn descriptor(&self) -> BudgetBackendDescriptor {
            BudgetBackendDescriptor {
                kind: BudgetBackendKind::Postgres,
                authoritative: true,
                deployment_namespace: Arc::from("recovery-test"),
            }
        }

        async fn inspect(
            &self,
            _command: BudgetInspectCommand,
        ) -> Result<BudgetInspection, BudgetStoreError> {
            panic!("测试不应调用 inspect")
        }

        async fn create_intent(
            &self,
            _command: CreateBudgetIntent,
        ) -> Result<BudgetIntent, BudgetStoreError> {
            panic!("恢复不得用已失效 owner 重放 create")
        }

        async fn lookup_intent(
            &self,
            _command: LookupBudgetIntent,
        ) -> Result<Option<BudgetIntentRecord>, BudgetStoreError> {
            self.lookups.fetch_add(1, Ordering::Relaxed);
            Ok(None)
        }

        async fn mark_dispatching(
            &self,
            _command: MarkBudgetDispatching,
        ) -> Result<BudgetIntent, BudgetStoreError> {
            panic!("测试不应调用 mark_dispatching")
        }

        async fn settle(
            &self,
            _command: SettleBudgetIntent,
        ) -> Result<BudgetSettlement, BudgetStoreError> {
            panic!("不存在的 intent 不应执行 settle")
        }

        async fn register_owner(
            &self,
            _command: RegisterBudgetOwner,
        ) -> Result<BudgetOwnerLease, BudgetStoreError> {
            panic!("测试不应调用 register_owner")
        }

        async fn heartbeat_owner(
            &self,
            _command: HeartbeatBudgetOwner,
        ) -> Result<BudgetOwnerLease, BudgetStoreError> {
            panic!("测试不应调用 heartbeat_owner")
        }

        async fn stop_owner(&self, _command: StopBudgetOwner) -> Result<(), BudgetStoreError> {
            panic!("测试不应调用 stop_owner")
        }

        async fn recover_stale(
            &self,
            _command: RecoverStaleBudgetIntents,
        ) -> Result<BudgetRecoveryBatch, BudgetStoreError> {
            panic!("测试不应调用 recover_stale")
        }

        async fn checkpoint_account(
            &self,
            _command: CheckpointBudgetAccount,
        ) -> Result<BudgetCheckpoint, BudgetStoreError> {
            panic!("测试不应调用 checkpoint_account")
        }
    }

    fn test_pricing_snapshot(max_prompt_tokens: Option<i64>) -> BudgetPricingSnapshot {
        let effective_from = Utc::now();
        let price = |rate| BudgetUnitPriceSnapshot {
            usd_per_million: rate,
            source: "test".to_string(),
            version: "frozen-v1".to_string(),
            snapshot_date: effective_from.date_naive(),
            effective_from,
            effective_to: None,
        };
        BudgetPricingSnapshot {
            schema_version: BUDGET_SCHEMA_VERSION,
            provider_type: "openai".to_string(),
            model: "gpt-frozen".to_string(),
            input: Some(price(Decimal::ONE)),
            output: Some(price(Decimal::from(2))),
            max_prompt_tokens,
        }
    }

    #[tokio::test]
    async fn missing_unknown_create_is_acknowledged_after_owner_is_fenced() {
        let store = Arc::new(MissingIntentRecoveryStore {
            lookups: AtomicUsize::new(0),
        });
        let registry = ActiveBudgetIntentRegistry::new(1).unwrap();
        let request_id: Arc<str> = Arc::from("unknown-create");
        let command = CreateBudgetIntent {
            intent_id: Uuid::new_v4(),
            virtual_key_id: Uuid::new_v4(),
            request_id: Arc::clone(&request_id),
            operation_id: Arc::from(format!("intent:v1:{request_id}")),
            command_fingerprint: Arc::from("a".repeat(64)),
            pricing_fingerprint: Arc::from("b".repeat(64)),
            pricing_snapshot: test_pricing_snapshot(Some(1_000)),
            node_id: Uuid::new_v4(),
            owner_session_id: Uuid::new_v4(),
            stale_after: std::time::Duration::from_secs(30),
        };
        let guard = registry.try_reserve(Arc::clone(&request_id)).unwrap();
        guard.record_create_command(command).unwrap();
        drop(guard);

        let runtime = SupportedBudgetRuntime::new(
            store.clone(),
            store.clone(),
            Arc::new(PriceCatalog::builtin().unwrap()),
            Arc::clone(&registry),
            Uuid::new_v4(),
            Uuid::new_v4(),
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(10),
            1,
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        runtime.set_owner_available(false);

        let stats = recover_budget_registry_once(&runtime, 1).await;

        assert_eq!(
            stats,
            BudgetRegistryRecoveryStats {
                scanned: 1,
                settled: 1,
                failed: 0,
            }
        );
        assert!(registry.is_empty());
        assert_eq!(store.lookups.load(Ordering::Relaxed), 1);
    }

    fn test_fact(
        request_id: &str,
        virtual_key_id: Uuid,
        fingerprint: Arc<str>,
        pricing: &BudgetPricingSnapshot,
        prompt_tokens: i64,
    ) -> AiUsageFact {
        let started_at = Utc::now();
        let completion_tokens = 20;
        let cost_usd = Decimal::from(prompt_tokens)
            .checked_mul(Decimal::ONE)
            .and_then(|input| {
                Decimal::from(completion_tokens)
                    .checked_mul(Decimal::from(2))
                    .and_then(|output| input.checked_add(output))
            })
            .and_then(|total| total.checked_div(Decimal::from(1_000_000u64)))
            .unwrap();
        AiUsageFact {
            id: Uuid::now_v7(),
            ingest_seq: None,
            request_id: request_id.to_string(),
            node_id: Uuid::new_v4(),
            started_at,
            finished_at: started_at,
            recorded_at: None,
            workspace_id: Some(Uuid::new_v4()),
            route_id: None,
            route_name: None,
            service_id: None,
            service_name: None,
            provider_id: None,
            provider_name: Some("provider".to_string()),
            provider_type: Some("openai".to_string()),
            model_id: None,
            requested_model: Some("requested".to_string()),
            model_group: None,
            actual_model: Some("provider-returned-model".to_string()),
            attempt_count: 1,
            virtual_key_id: Some(virtual_key_id),
            virtual_key_name: Some("key".to_string()),
            virtual_key_prefix: Some("prefix".to_string()),
            consumer_id: None,
            prompt_tokens: Some(TokenField {
                value: prompt_tokens,
                source: TokenFieldSource::Provider,
                derived: false,
            }),
            completion_tokens: Some(TokenField {
                value: completion_tokens,
                source: TokenFieldSource::Provider,
                derived: false,
            }),
            total_tokens: Some(TokenField {
                value: prompt_tokens + completion_tokens,
                source: TokenFieldSource::Provider,
                derived: false,
            }),
            reasoning_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            usage_source: UsageSource::Provider,
            usage_unavailable_reasons: Vec::new(),
            input_price: pricing.input.as_ref().map(PriceSnapshot::from),
            output_price: pricing.output.as_ref().map(PriceSnapshot::from),
            pricing_fingerprint: Some(fingerprint),
            pricing_status: PricingStatus::Matched,
            pricing_unsupported_reasons: Vec::new(),
            cost_usd: Some(cost_usd),
            cost_status: CostStatus::Calculated,
            cost_unavailable_reasons: Vec::new(),
            status_code: Some(200),
            upstream_status_code: Some(200),
            outcome: AiUsageOutcome::Success,
            e2e_ms: 1,
            ttft_ms: None,
            upstream_attempted: true,
            stream: Some(false),
            cache_status: CacheStatus::NotConfigured,
        }
    }

    #[test]
    fn missing_fact_is_zero_only_when_upstream_was_not_attempted() {
        let key_id = Uuid::new_v4();
        let fingerprint = "a".repeat(64);
        let pricing = test_pricing_snapshot(Some(100));
        let (_, before_upstream) =
            cost_from_fact(None, "request", key_id, false, &fingerprint, &pricing);
        let (_, after_upstream) =
            cost_from_fact(None, "request", key_id, true, &fingerprint, &pricing);

        assert_eq!(before_upstream.status, CostStatus::NotIncurred);
        assert_eq!(before_upstream.amount_usd, Some(Decimal::ZERO));
        assert_eq!(after_upstream.status, CostStatus::Unavailable);
        assert!(after_upstream.amount_usd.is_none());
        assert_eq!(after_upstream.reasons, ["missing_usage_fact"]);
    }

    #[test]
    fn fingerprint_mismatch_marks_budget_cost_unavailable() {
        let key_id = Uuid::new_v4();
        let pricing = test_pricing_snapshot(Some(100));
        let fact = test_fact("request", key_id, Arc::from("b".repeat(64)), &pricing, 100);

        let (_, cost) = cost_from_fact(
            Some(&fact),
            "request",
            key_id,
            true,
            &"a".repeat(64),
            &pricing,
        );

        assert_eq!(cost.status, CostStatus::Unavailable);
        assert!(cost.amount_usd.is_none());
        assert_eq!(cost.reasons, ["usage_fact_pricing_fingerprint_mismatch"]);
    }

    #[test]
    fn actual_prompt_beyond_frozen_limit_marks_budget_cost_unavailable() {
        let key_id = Uuid::new_v4();
        let fingerprint: Arc<str> = Arc::from("a".repeat(64));
        let pricing = test_pricing_snapshot(Some(100));
        let fact = test_fact("request", key_id, Arc::clone(&fingerprint), &pricing, 101);

        let (_, cost) =
            cost_from_fact(Some(&fact), "request", key_id, true, &fingerprint, &pricing);

        assert_eq!(cost.status, CostStatus::Unavailable);
        assert!(cost.amount_usd.is_none());
        assert_eq!(cost.reasons, ["actual_prompt_exceeds_frozen_pricing_limit"]);
    }

    #[test]
    fn settlement_recomputes_cost_instead_of_trusting_fact_amount() {
        let key_id = Uuid::new_v4();
        let fingerprint: Arc<str> = Arc::from("a".repeat(64));
        let pricing = test_pricing_snapshot(Some(100));
        let mut fact = test_fact("request", key_id, Arc::clone(&fingerprint), &pricing, 100);
        fact.cost_usd = Some(Decimal::from(999));

        let (_, cost) =
            cost_from_fact(Some(&fact), "request", key_id, true, &fingerprint, &pricing);

        assert_eq!(cost.status, CostStatus::Unavailable);
        assert!(cost.amount_usd.is_none());
        assert_eq!(cost.reasons, ["usage_fact_cost_mismatch"]);
    }

    #[test]
    fn settlement_operation_and_fingerprint_are_stable() {
        let intent_id = Uuid::new_v4();
        let key_id = Uuid::new_v4();
        let fingerprint: Arc<str> = Arc::from("a".repeat(64));
        let cost = BudgetCostOutcome {
            status: CostStatus::Calculated,
            amount_usd: Some(Decimal::new(125, 3)),
            reasons: Vec::new(),
        };

        let first = settlement_command(
            intent_id,
            key_id,
            "0123456789abcdef0123456789abcdef",
            Arc::clone(&fingerprint),
            None,
            cost.clone(),
        )
        .unwrap();
        let second = settlement_command(
            intent_id,
            key_id,
            "0123456789abcdef0123456789abcdef",
            fingerprint,
            None,
            cost,
        )
        .unwrap();

        assert_eq!(
            first.operation_id.as_ref(),
            "budget-settle:v1:0123456789abcdef0123456789abcdef"
        );
        assert_eq!(first.command_fingerprint, second.command_fingerprint);
        assert_eq!(first.command_fingerprint.len(), 64);
    }
}

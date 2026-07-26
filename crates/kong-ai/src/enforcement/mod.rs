//! AI 配额与预算执行运行时。
//!
//! 本模块只暴露稳定领域边界；具体 Memory、PostgreSQL 以及未来 Redis adapter
//! 通过这些边界装配，插件本身不创建私有 Store。

pub mod budget;
pub mod budget_registry;
pub mod policy;
pub mod quota;
pub mod response;
pub mod runtime;

pub use budget::{
    inspect_budget_before_quota, recover_budget_registry_once, AiBudgetDispatchAbortCompensator,
    AiBudgetDispatchHook, AiBudgetFinalizer, AiBudgetRequestContext, BudgetInspectionOutcome,
    BudgetRegistryRecoveryStats, BudgetRequestState,
};
pub use budget_registry::{
    estimate_active_budget_intent_capacity, ActiveBudgetIntentCapacityEstimate,
    ActiveBudgetIntentRegistry, ActiveBudgetIntentSnapshot, ActiveBudgetIntentState,
    BudgetIntentGuard, BudgetIntentRegistryError, BudgetIntentRegistryField,
    ACTIVE_BUDGET_INTENT_ESTIMATED_FIXED_BYTES, ACTIVE_BUDGET_INTENT_ESTIMATED_WORST_CASE_BYTES,
    ACTIVE_BUDGET_INTENT_MAX_COST_REASONS, ACTIVE_BUDGET_INTENT_MAX_COST_REASON_BYTES,
    ACTIVE_BUDGET_INTENT_MAX_FINGERPRINT_BYTES, ACTIVE_BUDGET_INTENT_MAX_OPERATION_ID_BYTES,
    ACTIVE_BUDGET_INTENT_MAX_PRICING_SNAPSHOT_BYTES, ACTIVE_BUDGET_INTENT_MAX_REQUEST_ID_BYTES,
};
pub use policy::{
    AiClientProtocol, AiPolicyChainObserver, AiPolicyChainSnapshot, AiPolicyConfigError,
    AiPolicyConfigErrorCode, AiRateLimitMode,
};
pub use quota::{
    quota_error_contract, AiQuotaDispatchAbortCompensator, AiRateLimitFinalizer,
    AiRateLimitRequestContext, QuotaSettlementState,
};
pub use response::{
    apply_quota_headers, clear_quota_headers, reject_with_protocol_error, QuotaHeaderMode,
};
pub use runtime::{
    AiEnforcementCapability, AiEnforcementRuntime, BudgetCapability, BudgetRuntime,
    BudgetRuntimeUnavailable, QuotaCapability, QuotaRuntime, QuotaRuntimeUnavailable,
    SupportedBudgetRuntime,
};

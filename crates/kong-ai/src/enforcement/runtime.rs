//! 由 server 构造并注入所有 AI 策略组件的共享运行时。

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::budget::{
    BudgetBackendDescriptor, BudgetErrorKind, BudgetMetricOperation, BudgetMetricResult,
    BudgetStore, BudgetTelemetry, BudgetTelemetrySnapshot,
};
use crate::ratelimit::{RateLimitBackendDescriptor, RateLimitStore};
use crate::usage::PriceCatalog;

use super::ActiveBudgetIntentRegistry;

/// 实时配额能力状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaCapability {
    LocalMemory,
    LocalMemoryEphemeral,
    UnsupportedHybrid,
}

/// 生命周期预算能力状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetCapability {
    PostgresAuthoritative,
    AccountingUnavailable,
    UnsupportedDbLess,
    UnsupportedHybrid,
}

/// Admin 与 Manager 可直接消费的运行时能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiEnforcementCapability {
    pub quota: QuotaCapability,
    pub budget: BudgetCapability,
}

/// 可用的 quota runtime。
pub struct SupportedQuotaRuntime {
    pub store: Arc<dyn RateLimitStore>,
    pub deployment_namespace: Arc<str>,
    pub descriptor: RateLimitBackendDescriptor,
}

impl fmt::Debug for SupportedQuotaRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SupportedQuotaRuntime")
            .field("deployment_namespace", &self.deployment_namespace)
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

/// quota runtime 的显式可用性，不通过一次调用是否碰巧成功来推断。
#[derive(Debug)]
pub enum QuotaRuntime {
    Supported(SupportedQuotaRuntime),
    Unsupported(QuotaRuntimeUnavailable),
}

/// quota 不可用的稳定原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaRuntimeUnavailable {
    HybridMode,
}

/// 已完成 owner 注册、可执行权威账务的预算运行时。
pub struct SupportedBudgetRuntime {
    pub store: Arc<dyn BudgetStore>,
    /// owner lease 使用独立小池，避免被 admission/finalize 排队饿死。
    pub owner_store: Arc<dyn BudgetStore>,
    pub descriptor: BudgetBackendDescriptor,
    pub catalog: Arc<PriceCatalog>,
    pub registry: Arc<ActiveBudgetIntentRegistry>,
    pub node_id: Uuid,
    pub owner_session_id: Uuid,
    pub stale_after: Duration,
    pub owner_lease: Duration,
    pub telemetry: Arc<BudgetTelemetry>,
    admission_limiter: Arc<Semaphore>,
    operation_timeout: Duration,
    owner_available: AtomicBool,
}

impl SupportedBudgetRuntime {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<dyn BudgetStore>,
        owner_store: Arc<dyn BudgetStore>,
        catalog: Arc<PriceCatalog>,
        registry: Arc<ActiveBudgetIntentRegistry>,
        node_id: Uuid,
        owner_session_id: Uuid,
        stale_after: Duration,
        owner_lease: Duration,
        max_concurrent_admissions: usize,
        operation_timeout: Duration,
    ) -> Result<Self, &'static str> {
        let descriptor = store.descriptor();
        if !descriptor.authoritative {
            return Err("budget Store 必须声明 authoritative");
        }
        let owner_descriptor = owner_store.descriptor();
        if !owner_descriptor.authoritative
            || owner_descriptor.kind != descriptor.kind
            || owner_descriptor.deployment_namespace != descriptor.deployment_namespace
        {
            return Err("budget owner Store 必须与热路径使用同一权威部署域");
        }
        if stale_after.is_zero()
            || owner_lease.is_zero()
            || operation_timeout.is_zero()
            || max_concurrent_admissions == 0
        {
            return Err("budget stale、owner lease、并发与 operation timeout 必须为正数");
        }
        let telemetry = Arc::new(BudgetTelemetry::new(descriptor.kind));
        telemetry.record_owner_heartbeat();
        Ok(Self {
            store,
            owner_store,
            descriptor,
            catalog,
            registry,
            node_id,
            owner_session_id,
            stale_after,
            owner_lease,
            telemetry,
            admission_limiter: Arc::new(Semaphore::new(max_concurrent_admissions)),
            operation_timeout,
            owner_available: AtomicBool::new(true),
        })
    }

    pub async fn acquire_admission_permit(
        &self,
    ) -> Result<OwnedSemaphorePermit, BudgetRuntimeUnavailable> {
        let result = match tokio::time::timeout(
            self.operation_timeout,
            Arc::clone(&self.admission_limiter).acquire_owned(),
        )
        .await
        {
            Ok(result) => result.map_err(|_| BudgetRuntimeUnavailable::AccountingUnavailable),
            Err(_) => Err(BudgetRuntimeUnavailable::AccountingUnavailable),
        };
        self.telemetry.record_operation(
            BudgetMetricOperation::AdmissionPermit,
            if result.is_ok() {
                BudgetMetricResult::Success
            } else {
                BudgetMetricResult::Rejected
            },
            result
                .as_ref()
                .err()
                .map(|_| BudgetErrorKind::AccountingUnavailable),
        );
        result
    }

    pub fn owner_available(&self) -> bool {
        self.owner_available.load(Ordering::Acquire)
    }

    pub fn set_owner_available(&self, available: bool) {
        self.owner_available.store(available, Ordering::Release);
        if available {
            self.telemetry.record_owner_heartbeat();
        }
        self.telemetry.record_operation(
            BudgetMetricOperation::HeartbeatOwner,
            if available {
                BudgetMetricResult::Success
            } else {
                BudgetMetricResult::Failed
            },
            (!available).then_some(BudgetErrorKind::AccountingUnavailable),
        );
    }

    pub fn metrics_snapshot(&self) -> BudgetTelemetrySnapshot {
        self.telemetry
            .snapshot(self.registry.metrics_snapshot(), self.owner_available())
    }
}

impl fmt::Debug for SupportedBudgetRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SupportedBudgetRuntime")
            .field("descriptor", &self.descriptor)
            .field("node_id", &self.node_id)
            .field("owner_session_id", &self.owner_session_id)
            .field("stale_after", &self.stale_after)
            .field("owner_lease", &self.owner_lease)
            .field("owner_available", &self.owner_available())
            .finish_non_exhaustive()
    }
}

/// budget runtime 的显式可用性。
#[derive(Debug)]
pub enum BudgetRuntime {
    Supported(Arc<SupportedBudgetRuntime>),
    Unsupported(BudgetRuntimeUnavailable),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetRuntimeUnavailable {
    DbLessMode,
    HybridMode,
    AccountingUnavailable,
    OwnerFenced,
}

/// 所有 AI enforcement 组件共享的运行时根对象。
#[derive(Debug)]
pub struct AiEnforcementRuntime {
    pub capability: AiEnforcementCapability,
    pub quota: QuotaRuntime,
    budget: RwLock<BudgetRuntime>,
}

impl AiEnforcementRuntime {
    pub fn with_local_quota(
        store: Arc<dyn RateLimitStore>,
        deployment_namespace: impl Into<Arc<str>>,
        ephemeral: bool,
        budget: BudgetCapability,
    ) -> Result<Self, &'static str> {
        let deployment_namespace = deployment_namespace.into();
        if deployment_namespace.is_empty() {
            return Err("AI enforcement deployment namespace 不能为空");
        }
        let descriptor = store.descriptor();
        let budget_runtime = match budget {
            BudgetCapability::UnsupportedDbLess => {
                BudgetRuntime::Unsupported(BudgetRuntimeUnavailable::DbLessMode)
            }
            BudgetCapability::UnsupportedHybrid => {
                BudgetRuntime::Unsupported(BudgetRuntimeUnavailable::HybridMode)
            }
            BudgetCapability::AccountingUnavailable | BudgetCapability::PostgresAuthoritative => {
                BudgetRuntime::Unsupported(BudgetRuntimeUnavailable::AccountingUnavailable)
            }
        };
        Ok(Self {
            capability: AiEnforcementCapability {
                quota: if ephemeral {
                    QuotaCapability::LocalMemoryEphemeral
                } else {
                    QuotaCapability::LocalMemory
                },
                budget,
            },
            quota: QuotaRuntime::Supported(SupportedQuotaRuntime {
                store,
                deployment_namespace,
                descriptor,
            }),
            budget: RwLock::new(budget_runtime),
        })
    }

    pub fn with_supported_budget(mut self, runtime: SupportedBudgetRuntime) -> Self {
        self.capability.budget = BudgetCapability::PostgresAuthoritative;
        *self
            .budget
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            BudgetRuntime::Supported(Arc::new(runtime));
        self
    }

    /// 后台 owner 重注册成功后原子发布新的预算 session。
    pub fn install_supported_budget(
        &self,
        runtime: SupportedBudgetRuntime,
    ) -> Arc<SupportedBudgetRuntime> {
        let runtime = Arc::new(runtime);
        *self
            .budget
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            BudgetRuntime::Supported(Arc::clone(&runtime));
        runtime
    }

    pub fn unsupported_hybrid() -> Self {
        Self {
            capability: AiEnforcementCapability {
                quota: QuotaCapability::UnsupportedHybrid,
                budget: BudgetCapability::UnsupportedHybrid,
            },
            quota: QuotaRuntime::Unsupported(QuotaRuntimeUnavailable::HybridMode),
            budget: RwLock::new(BudgetRuntime::Unsupported(
                BudgetRuntimeUnavailable::HybridMode,
            )),
        }
    }

    pub fn quota_runtime(&self) -> Result<&SupportedQuotaRuntime, QuotaRuntimeUnavailable> {
        match &self.quota {
            QuotaRuntime::Supported(runtime) => Ok(runtime),
            QuotaRuntime::Unsupported(reason) => Err(*reason),
        }
    }

    pub fn budget_runtime(&self) -> Result<Arc<SupportedBudgetRuntime>, BudgetRuntimeUnavailable> {
        match &*self
            .budget
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            BudgetRuntime::Supported(runtime) if runtime.owner_available() => {
                Ok(Arc::clone(runtime))
            }
            BudgetRuntime::Supported(_) => Err(BudgetRuntimeUnavailable::OwnerFenced),
            BudgetRuntime::Unsupported(reason) => Err(*reason),
        }
    }

    /// 后台恢复需要读取已 fenced runtime 的 registry，不能把它误当作可准入状态。
    pub fn budget_runtime_any(&self) -> Option<Arc<SupportedBudgetRuntime>> {
        match &*self
            .budget
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            BudgetRuntime::Supported(runtime) => Some(Arc::clone(runtime)),
            BudgetRuntime::Unsupported(_) => None,
        }
    }

    /// 部署能力与当前 owner 可用性的统一投影。
    pub fn effective_budget_capability(&self) -> BudgetCapability {
        match self.capability.budget {
            BudgetCapability::UnsupportedDbLess => BudgetCapability::UnsupportedDbLess,
            BudgetCapability::UnsupportedHybrid => BudgetCapability::UnsupportedHybrid,
            BudgetCapability::PostgresAuthoritative | BudgetCapability::AccountingUnavailable => {
                if self.budget_runtime().is_ok() {
                    BudgetCapability::PostgresAuthoritative
                } else {
                    BudgetCapability::AccountingUnavailable
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::ratelimit::{MemoryRateLimitStore, SystemRateLimitClock};

    use super::*;

    #[test]
    fn local_runtime_exposes_descriptor_and_namespace() {
        let store = Arc::new(MemoryRateLimitStore::with_defaults(Arc::new(
            SystemRateLimitClock::new(),
        )));
        let runtime = AiEnforcementRuntime::with_local_quota(
            store,
            "test-deployment",
            true,
            BudgetCapability::UnsupportedDbLess,
        )
        .unwrap();

        assert_eq!(
            runtime.capability.quota,
            QuotaCapability::LocalMemoryEphemeral
        );
        assert_eq!(
            runtime
                .quota_runtime()
                .unwrap()
                .deployment_namespace
                .as_ref(),
            "test-deployment"
        );
    }

    #[test]
    fn hybrid_capability_is_explicitly_unsupported() {
        let runtime = AiEnforcementRuntime::unsupported_hybrid();

        assert_eq!(
            runtime.quota_runtime().unwrap_err(),
            QuotaRuntimeUnavailable::HybridMode
        );
    }
}

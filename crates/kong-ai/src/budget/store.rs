//! 异步预算 Store 契约。

use async_trait::async_trait;

use super::model::{
    BudgetBackendDescriptor, BudgetCheckpoint, BudgetInspection, BudgetIntent, BudgetOwnerLease,
    BudgetRecoveryBatch, BudgetSettlement, BudgetStoreError, CheckpointBudgetAccount,
    CreateBudgetIntent, HeartbeatBudgetOwner, LookupBudgetIntent, MarkBudgetDispatching,
    RecoverStaleBudgetIntents, RegisterBudgetOwner, SettleBudgetIntent, StopBudgetOwner,
};

/// 可替换的强一致预算后端。
#[async_trait]
pub trait BudgetStore: Send + Sync {
    fn descriptor(&self) -> BudgetBackendDescriptor;

    async fn inspect(
        &self,
        command: super::model::BudgetInspectCommand,
    ) -> Result<BudgetInspection, BudgetStoreError>;

    async fn create_intent(
        &self,
        command: CreateBudgetIntent,
    ) -> Result<BudgetIntent, BudgetStoreError>;

    /// create 结果未知时按 request ID 只读查找，不要求原 owner 仍然存活。
    async fn lookup_intent(
        &self,
        command: LookupBudgetIntent,
    ) -> Result<Option<super::model::BudgetIntentRecord>, BudgetStoreError>;

    async fn mark_dispatching(
        &self,
        command: MarkBudgetDispatching,
    ) -> Result<BudgetIntent, BudgetStoreError>;

    async fn settle(
        &self,
        command: SettleBudgetIntent,
    ) -> Result<BudgetSettlement, BudgetStoreError>;

    async fn register_owner(
        &self,
        command: RegisterBudgetOwner,
    ) -> Result<BudgetOwnerLease, BudgetStoreError>;

    async fn heartbeat_owner(
        &self,
        command: HeartbeatBudgetOwner,
    ) -> Result<BudgetOwnerLease, BudgetStoreError>;

    async fn stop_owner(&self, command: StopBudgetOwner) -> Result<(), BudgetStoreError>;

    /// 不支持 recovery 的 adapter 默认明确返回 unsupported，不静默空跑。
    async fn recover_stale(
        &self,
        _command: RecoverStaleBudgetIntents,
    ) -> Result<BudgetRecoveryBatch, BudgetStoreError> {
        Err(BudgetStoreError::unsupported(
            "当前预算后端尚未启用 stale recovery",
        ))
    }

    /// 不支持 checkpoint 的 adapter 默认明确返回 unsupported，不静默成功。
    async fn checkpoint_account(
        &self,
        _command: CheckpointBudgetAccount,
    ) -> Result<BudgetCheckpoint, BudgetStoreError> {
        Err(BudgetStoreError::unsupported(
            "当前预算后端尚未启用 checkpoint",
        ))
    }
}

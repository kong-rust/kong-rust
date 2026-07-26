//! AI 预算后端无关的领域类型。

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use uuid::Uuid;

use crate::usage::model::{CostStatus, PriceSnapshot};

/// 当前预算命令与 pricing snapshot 的 schema 版本。
pub const BUDGET_SCHEMA_VERSION: u16 = 1;

/// 预算后端类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetBackendKind {
    Postgres,
}

impl BudgetBackendKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
        }
    }
}

/// 对外公开的预算后端能力描述，不包含连接信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetBackendDescriptor {
    pub kind: BudgetBackendKind,
    pub authoritative: bool,
    pub deployment_namespace: Arc<str>,
}

/// Virtual Key 的数据库生成账务状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetAccountingState {
    Clean,
    Pending,
    Unresolved,
}

impl BudgetAccountingState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Pending => "pending",
            Self::Unresolved => "unresolved",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "clean" => Some(Self::Clean),
            "pending" => Some(Self::Pending),
            "unresolved" => Some(Self::Unresolved),
            _ => None,
        }
    }
}

/// 预算账户在同一权威 revision 上的快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetAccountSnapshot {
    pub virtual_key_id: Uuid,
    pub virtual_key_name: String,
    pub virtual_key_prefix: String,
    pub workspace_id: Option<Uuid>,
    pub limit_usd: Option<Decimal>,
    pub used_usd: Decimal,
    pub pending_count: i64,
    pub unresolved_count: i64,
    pub accounting_revision: i64,
    pub checkpoint_tail_events: i64,
    pub state: BudgetAccountingState,
    pub state_updated_at: DateTime<Utc>,
}

/// inspect 的业务结论。异常/损坏状态通过 `BudgetStoreError` 返回。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetEligibility {
    Eligible,
    Paused,
    Exhausted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetInspection {
    pub eligibility: BudgetEligibility,
    pub account: BudgetAccountSnapshot,
}

/// pricing snapshot 中单个方向的固定价格。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetUnitPriceSnapshot {
    pub usd_per_million: Decimal,
    pub source: String,
    pub version: String,
    pub snapshot_date: NaiveDate,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
}

impl From<&PriceSnapshot> for BudgetUnitPriceSnapshot {
    fn from(value: &PriceSnapshot) -> Self {
        Self {
            usd_per_million: value.usd_per_million,
            source: value.source.clone(),
            version: value.version.clone(),
            snapshot_date: value.snapshot_date,
            effective_from: value.effective_from,
            effective_to: value.effective_to,
        }
    }
}

impl From<&BudgetUnitPriceSnapshot> for PriceSnapshot {
    fn from(value: &BudgetUnitPriceSnapshot) -> Self {
        Self {
            usd_per_million: value.usd_per_million,
            source: value.source.clone(),
            version: value.version.clone(),
            snapshot_date: value.snapshot_date,
            effective_from: value.effective_from,
            effective_to: value.effective_to,
        }
    }
}

impl Serialize for BudgetUnitPriceSnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("BudgetUnitPriceSnapshot", 6)?;
        state.serialize_field(
            "usd_per_million_decimal",
            &crate::models::normalize_budget_amount(self.usd_per_million)
                .map_err(serde::ser::Error::custom)?
                .to_string(),
        )?;
        state.serialize_field("source", &self.source)?;
        state.serialize_field("version", &self.version)?;
        state.serialize_field("snapshot_date", &self.snapshot_date)?;
        state.serialize_field("effective_from", &self.effective_from)?;
        state.serialize_field("effective_to", &self.effective_to)?;
        state.end()
    }
}

/// 预算 intent 固化的白名单 pricing 数据。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BudgetPricingSnapshot {
    pub schema_version: u16,
    pub provider_type: String,
    pub model: String,
    pub input: Option<BudgetUnitPriceSnapshot>,
    pub output: Option<BudgetUnitPriceSnapshot>,
    pub max_prompt_tokens: Option<i64>,
}

/// inspect 命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetInspectCommand {
    pub virtual_key_id: Uuid,
}

/// 按 request ID 只读确认 create intent 是否已经提交。
///
/// 该命令不依赖 owner 是否仍然存活，专门用于 create 结果未知时的恢复。其余
/// 幂等身份字段必须与原 create 命令一致，避免把碰撞的 request ID 当成己方
/// intent 收口。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupBudgetIntent {
    pub intent_id: Uuid,
    pub virtual_key_id: Uuid,
    pub request_id: Arc<str>,
    pub operation_id: Arc<str>,
    pub command_fingerprint: Arc<str>,
    pub pricing_fingerprint: Arc<str>,
    pub node_id: Uuid,
    pub owner_session_id: Uuid,
}

/// 创建 prepared intent。所有时间边界由 Store 使用数据库时钟计算。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateBudgetIntent {
    pub intent_id: Uuid,
    pub virtual_key_id: Uuid,
    pub request_id: Arc<str>,
    pub operation_id: Arc<str>,
    pub command_fingerprint: Arc<str>,
    pub pricing_fingerprint: Arc<str>,
    pub pricing_snapshot: BudgetPricingSnapshot,
    pub node_id: Uuid,
    pub owner_session_id: Uuid,
    pub stale_after: Duration,
}

impl From<&CreateBudgetIntent> for LookupBudgetIntent {
    fn from(command: &CreateBudgetIntent) -> Self {
        Self {
            intent_id: command.intent_id,
            virtual_key_id: command.virtual_key_id,
            request_id: Arc::clone(&command.request_id),
            operation_id: Arc::clone(&command.operation_id),
            command_fingerprint: Arc::clone(&command.command_fingerprint),
            pricing_fingerprint: Arc::clone(&command.pricing_fingerprint),
            node_id: command.node_id,
            owner_session_id: command.owner_session_id,
        }
    }
}

/// dispatch transition 命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkBudgetDispatching {
    pub intent_id: Uuid,
    pub virtual_key_id: Uuid,
    pub request_id: Arc<str>,
    pub operation_id: Arc<str>,
    pub node_id: Uuid,
    pub owner_session_id: Uuid,
}

/// intent 的持久状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetIntentState {
    Prepared,
    Dispatching,
    Unresolved,
    Settled,
    Resolved,
}

/// 账本中的 request intent 投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetIntentRecord {
    pub id: Uuid,
    pub virtual_key_id: Uuid,
    pub request_id: Arc<str>,
    pub state: BudgetIntentState,
    pub operation_id: Arc<str>,
    pub command_fingerprint: Arc<str>,
    pub dispatch_operation_id: Option<Arc<str>>,
    pub terminal_operation_id: Option<Arc<str>>,
    pub terminal_command_fingerprint: Option<Arc<str>>,
    pub pricing_fingerprint: Arc<str>,
    pub owner_session_id: Uuid,
    pub node_id: Uuid,
    pub last_account_revision: i64,
    pub observed_cost_usd: Option<Decimal>,
    pub accounted_cost_usd: Option<Decimal>,
    pub cost_status: Option<CostStatus>,
    pub cost_reasons: Vec<String>,
    pub usage_fact_id: Option<Uuid>,
    pub resolution_entry_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// create/dispatch 的结果类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetIntentDisposition {
    Created,
    CreateReplayed,
    Paused,
    DispatchApplied,
    DispatchReplayed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetIntent {
    pub disposition: BudgetIntentDisposition,
    pub record: Option<BudgetIntentRecord>,
    pub account: Option<BudgetAccountSnapshot>,
}

/// 一次冻结的成本事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetCostOutcome {
    pub status: CostStatus,
    pub amount_usd: Option<Decimal>,
    pub reasons: Vec<String>,
}

/// request intent 终态命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettleBudgetIntent {
    pub intent_id: Uuid,
    pub virtual_key_id: Uuid,
    pub request_id: Arc<str>,
    pub operation_id: Arc<str>,
    pub command_fingerprint: Arc<str>,
    pub pricing_fingerprint: Arc<str>,
    pub usage_fact_id: Option<Uuid>,
    pub cost: BudgetCostOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetSettlementDisposition {
    Applied,
    Replayed,
    MarkedUnresolved,
    AlreadyUnresolved,
    AlreadyReconciled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetSettlement {
    pub disposition: BudgetSettlementDisposition,
    pub intent: BudgetIntentRecord,
    /// key 已删除后的 terminal replay 不再有账户投影。
    pub account: Option<BudgetAccountSnapshot>,
}

/// owner 注册命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterBudgetOwner {
    pub session_id: Uuid,
    pub node_id: Uuid,
    pub lease_duration: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeartbeatBudgetOwner {
    pub session_id: Uuid,
    pub node_id: Uuid,
    pub lease_duration: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopBudgetOwner {
    pub session_id: Uuid,
    pub node_id: Uuid,
}

/// 数据库时钟签发的 owner lease。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetOwnerLease {
    pub session_id: Uuid,
    pub node_id: Uuid,
    pub deployment_namespace: Arc<str>,
    pub started_at: DateTime<Utc>,
    pub last_heartbeat_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub replayed: bool,
}

/// recovery runner 的单次有界扫描命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoverStaleBudgetIntents {
    pub max_intents: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BudgetRecoveryBatch {
    pub scanned: u32,
    pub settled_not_incurred: u32,
    pub marked_unresolved: u32,
}

/// checkpoint runner 的单账户幂等命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointBudgetAccount {
    pub virtual_key_id: Uuid,
    pub operation_id: Arc<str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetCheckpoint {
    pub virtual_key_id: Uuid,
    pub revision: i64,
    pub accounted_cost_usd: Decimal,
    pub created_at: DateTime<Utc>,
}

/// Budget Store 错误类别。HTTP/Kong 映射必须留在调用方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetErrorKind {
    Exhausted,
    AccountingUnavailable,
    AccountingUnresolved,
    Unsupported,
    PricingUnavailable,
    OutcomeUnknown,
    Corrupt,
    NumericOverflow,
    NotFound,
    Conflict,
    ReconciliationRequired,
    IntentActive,
    AlreadyReconciled,
    AccountBusy,
}

/// 后端无关错误，消息不得包含 SQL 或凭据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetStoreError {
    kind: BudgetErrorKind,
    message: Arc<str>,
}

impl BudgetStoreError {
    pub fn new(kind: BudgetErrorKind, message: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> BudgetErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn corrupt(message: impl Into<Arc<str>>) -> Self {
        Self::new(BudgetErrorKind::Corrupt, message)
    }

    pub(crate) fn unavailable(message: impl Into<Arc<str>>) -> Self {
        Self::new(BudgetErrorKind::AccountingUnavailable, message)
    }

    pub(crate) fn unsupported(message: impl Into<Arc<str>>) -> Self {
        Self::new(BudgetErrorKind::Unsupported, message)
    }
}

impl fmt::Display for BudgetStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for BudgetStoreError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pricing_snapshot_serializes_decimal_as_fixed_string() {
        let snapshot = BudgetPricingSnapshot {
            schema_version: BUDGET_SCHEMA_VERSION,
            provider_type: "openai".to_string(),
            model: "gpt-test".to_string(),
            input: Some(BudgetUnitPriceSnapshot {
                usd_per_million: Decimal::new(125, 2),
                source: "catalog".to_string(),
                version: "v1".to_string(),
                snapshot_date: NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
                effective_from: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
                effective_to: None,
            }),
            output: None,
            max_prompt_tokens: Some(1000),
        };

        let value = serde_json::to_value(snapshot).unwrap();
        assert_eq!(value["input"]["usd_per_million_decimal"], "1.250000000000");
    }

    #[test]
    fn budget_error_does_not_expose_backend_specific_type() {
        let error = BudgetStoreError::new(BudgetErrorKind::OutcomeUnknown, "提交结果未知");
        assert_eq!(error.kind(), BudgetErrorKind::OutcomeUnknown);
        assert_eq!(error.message(), "提交结果未知");
        assert_eq!(error.to_string(), "OutcomeUnknown: 提交结果未知");
    }
}

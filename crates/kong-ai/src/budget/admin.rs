//! 预算账本查询、人工 reconciliation 与校验/重建。

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::models::{normalize_budget_amount, parse_budget_amount};
use crate::usage::model::CostStatus;

use super::governance::{
    lock_account, not_found, validate_account, BudgetAdminScope, GovernanceAccountRow,
};
use super::model::{
    BudgetAccountSnapshot, BudgetErrorKind, BudgetStoreError, BUDGET_SCHEMA_VERSION,
};
use super::postgres::{commit, map_sqlx_error, PgBudgetStore};

const MAX_LEDGER_PAGE_SIZE: u16 = 200;
const LEDGER_COLUMNS: &str = "\
id, virtual_key_id, virtual_key_name, virtual_key_prefix, workspace_id, \
kind, status, request_id, operation_id, command_fingerprint, \
dispatch_operation_id, terminal_operation_id, terminal_command_fingerprint, \
last_account_revision, parent_intent_id, usage_fact_id, attempt_no, \
observed_cost_usd, accounted_cost_usd, cost_status, cost_reasons, \
pricing_fingerprint, pricing_snapshot, dispatch_state, node_id, owner_session_id, \
stale_not_before, resolution_reason, resolution_actor, resolution_entry_id, \
created_at, updated_at, settled_at, resolved_at";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetLedgerKind {
    Request,
    OpeningBalance,
    Reconciliation,
    ReconciliationAttempt,
    AccountIssue,
    RebuildAudit,
}

impl BudgetLedgerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::OpeningBalance => "opening_balance",
            Self::Reconciliation => "reconciliation",
            Self::ReconciliationAttempt => "reconciliation_attempt",
            Self::AccountIssue => "account_issue",
            Self::RebuildAudit => "rebuild_audit",
        }
    }

    fn parse(value: &str) -> Result<Self, BudgetStoreError> {
        match value {
            "request" => Ok(Self::Request),
            "opening_balance" => Ok(Self::OpeningBalance),
            "reconciliation" => Ok(Self::Reconciliation),
            "reconciliation_attempt" => Ok(Self::ReconciliationAttempt),
            "account_issue" => Ok(Self::AccountIssue),
            "rebuild_audit" => Ok(Self::RebuildAudit),
            _ => Err(BudgetStoreError::corrupt("预算账本 kind 非法")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetLedgerStatus {
    Pending,
    Unresolved,
    Settled,
    Resolved,
    Waived,
}

impl BudgetLedgerStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Unresolved => "unresolved",
            Self::Settled => "settled",
            Self::Resolved => "resolved",
            Self::Waived => "waived",
        }
    }

    fn parse(value: &str) -> Result<Self, BudgetStoreError> {
        match value {
            "pending" => Ok(Self::Pending),
            "unresolved" => Ok(Self::Unresolved),
            "settled" => Ok(Self::Settled),
            "resolved" => Ok(Self::Resolved),
            "waived" => Ok(Self::Waived),
            _ => Err(BudgetStoreError::corrupt("预算账本 status 非法")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetLedgerCursor {
    pub created_at: DateTime<Utc>,
    pub id: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetLedgerQuery {
    pub virtual_key_id: Uuid,
    pub scope: BudgetAdminScope,
    pub statuses: Vec<BudgetLedgerStatus>,
    pub created_at_from: Option<DateTime<Utc>>,
    pub created_at_to: Option<DateTime<Utc>>,
    pub after: Option<BudgetLedgerCursor>,
    pub page_size: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BudgetLedgerEntry {
    pub id: Uuid,
    pub virtual_key_id: Uuid,
    pub virtual_key_name: String,
    pub virtual_key_prefix: String,
    pub workspace_id: Option<Uuid>,
    pub kind: BudgetLedgerKind,
    pub status: BudgetLedgerStatus,
    pub request_id: Option<Arc<str>>,
    pub operation_id: Arc<str>,
    pub command_fingerprint: Option<Arc<str>>,
    pub dispatch_operation_id: Option<Arc<str>>,
    pub terminal_operation_id: Option<Arc<str>>,
    pub terminal_command_fingerprint: Option<Arc<str>>,
    pub last_account_revision: i64,
    pub parent_intent_id: Option<Uuid>,
    pub usage_fact_id: Option<Uuid>,
    pub attempt_no: i16,
    pub observed_cost_usd: Option<Decimal>,
    pub accounted_cost_usd: Option<Decimal>,
    pub cost_status: Option<CostStatus>,
    pub cost_reasons: Vec<String>,
    pub pricing_fingerprint: Option<Arc<str>>,
    pub pricing_snapshot: Option<serde_json::Value>,
    pub dispatch_state: Option<Arc<str>>,
    pub node_id: Option<Uuid>,
    pub owner_session_id: Option<Uuid>,
    pub stale_not_before: Option<DateTime<Utc>>,
    pub resolution_reason: Option<String>,
    pub resolution_actor: Option<String>,
    pub resolution_entry_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub settled_at: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BudgetLedgerPage {
    /// key 已删除时仍可按账本 workspace snapshot 查询历史，此时账户为 None。
    pub account: Option<BudgetAccountSnapshot>,
    pub entries: Vec<BudgetLedgerEntry>,
    pub next_cursor: Option<BudgetLedgerCursor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetReconciliationResolution {
    Settle { cost_usd: Decimal },
    Waive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconcileBudgetIntent {
    pub virtual_key_id: Uuid,
    pub intent_id: Uuid,
    pub operation_id: Uuid,
    pub scope: BudgetAdminScope,
    pub actor: Arc<str>,
    pub reason: String,
    pub resolution: BudgetReconciliationResolution,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetReconciliationDisposition {
    Applied,
    Replayed,
    NumericOverflow,
    NumericOverflowReplayed,
}

impl BudgetReconciliationDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Replayed => "replayed",
            Self::NumericOverflow => "numeric_overflow",
            Self::NumericOverflowReplayed => "numeric_overflow_replayed",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BudgetReconciliation {
    pub disposition: BudgetReconciliationDisposition,
    pub account: BudgetAccountSnapshot,
    pub intent: BudgetLedgerEntry,
    pub audit_entry: BudgetLedgerEntry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebuildBudgetAccount {
    pub virtual_key_id: Uuid,
    pub operation_id: Uuid,
    pub scope: BudgetAdminScope,
    pub actor: Arc<str>,
    pub reason: String,
    pub dry_run: bool,
    pub max_attempts: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BudgetRebuildComparison {
    pub snapshot_revision: i64,
    pub checkpoint_revision: i64,
    pub stored_used_usd: Decimal,
    pub recomputed_used_usd: Decimal,
    pub difference_usd: Decimal,
    pub stored_pending_count: i64,
    pub recomputed_pending_count: i64,
    pub stored_unresolved_count: i64,
    pub recomputed_unresolved_count: i64,
    pub unresolved_request_count: i64,
    pub open_account_issue_count: i64,
    pub is_current: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetRebuildDisposition {
    Verified,
    Rebuilt,
    Replayed,
}

impl BudgetRebuildDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Rebuilt => "rebuilt",
            Self::Replayed => "replayed",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BudgetRebuildResult {
    pub disposition: BudgetRebuildDisposition,
    pub account: BudgetAccountSnapshot,
    pub comparison: BudgetRebuildComparison,
    pub audit_entry: Option<BudgetLedgerEntry>,
}

#[async_trait]
pub trait BudgetAdminStore: Send + Sync {
    async fn list_ledger(
        &self,
        query: BudgetLedgerQuery,
    ) -> Result<BudgetLedgerPage, BudgetStoreError>;

    async fn reconcile(
        &self,
        command: ReconcileBudgetIntent,
    ) -> Result<BudgetReconciliation, BudgetStoreError>;

    async fn verify_or_rebuild(
        &self,
        command: RebuildBudgetAccount,
    ) -> Result<BudgetRebuildResult, BudgetStoreError>;
}

#[async_trait]
impl BudgetAdminStore for PgBudgetStore {
    async fn list_ledger(
        &self,
        query: BudgetLedgerQuery,
    ) -> Result<BudgetLedgerPage, BudgetStoreError> {
        validate_ledger_query(&query)?;
        let mut transaction = self.begin_repeatable_read().await?;
        let account = load_account(&mut transaction, query.virtual_key_id).await?;
        let account = match account {
            Some(row) if query.scope.contains(row.ws_id) => {
                let account = row.into_snapshot()?;
                validate_account(&account)?;
                Some(account)
            }
            Some(_) => return Err(not_found()),
            None => None,
        };

        let statuses = (!query.statuses.is_empty()).then(|| {
            query
                .statuses
                .iter()
                .map(|status| status.as_str().to_string())
                .collect::<Vec<_>>()
        });
        let cursor_time = query.after.as_ref().map(|cursor| cursor.created_at);
        let cursor_id = query.after.as_ref().map(|cursor| cursor.id);
        let sql = format!(
            "SELECT {LEDGER_COLUMNS} \
               FROM ai_budget_ledger \
              WHERE virtual_key_id = $1 \
                AND COALESCE(workspace_id, $3) = $2 \
                AND ($4::text[] IS NULL OR status = ANY($4)) \
                AND ($5::timestamptz IS NULL OR created_at >= $5) \
                AND ($6::timestamptz IS NULL OR created_at <= $6) \
                AND (\
                    $7::timestamptz IS NULL \
                    OR (created_at, id) < ($7, $8)\
                ) \
              ORDER BY created_at DESC, id DESC \
              LIMIT $9"
        );
        let mut rows: Vec<LedgerRow> = sqlx::query_as(&sql)
            .bind(query.virtual_key_id)
            .bind(query.scope.workspace_id)
            .bind(query.scope.default_workspace_id)
            .bind(statuses)
            .bind(query.created_at_from)
            .bind(query.created_at_to)
            .bind(cursor_time)
            .bind(cursor_id)
            .bind(i64::from(query.page_size) + 1)
            .fetch_all(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;

        if account.is_none()
            && !ledger_history_exists(&mut transaction, query.virtual_key_id, query.scope).await?
        {
            return Err(not_found());
        }
        let has_more = rows.len() > usize::from(query.page_size);
        if has_more {
            rows.pop();
        }
        let entries = rows
            .into_iter()
            .map(LedgerRow::into_entry)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor =
            has_more
                .then(|| entries.last())
                .flatten()
                .map(|entry| BudgetLedgerCursor {
                    created_at: entry.created_at,
                    id: entry.id,
                });
        commit(transaction).await?;
        Ok(BudgetLedgerPage {
            account,
            entries,
            next_cursor,
        })
    }

    async fn reconcile(
        &self,
        command: ReconcileBudgetIntent,
    ) -> Result<BudgetReconciliation, BudgetStoreError> {
        reconcile_pg(self, command).await
    }

    async fn verify_or_rebuild(
        &self,
        command: RebuildBudgetAccount,
    ) -> Result<BudgetRebuildResult, BudgetStoreError> {
        verify_or_rebuild_pg(self, command).await
    }
}

#[derive(Clone, FromRow)]
struct LedgerRow {
    id: Uuid,
    virtual_key_id: Uuid,
    virtual_key_name: String,
    virtual_key_prefix: String,
    workspace_id: Option<Uuid>,
    kind: String,
    status: String,
    request_id: Option<String>,
    operation_id: String,
    command_fingerprint: Option<String>,
    dispatch_operation_id: Option<String>,
    terminal_operation_id: Option<String>,
    terminal_command_fingerprint: Option<String>,
    last_account_revision: i64,
    parent_intent_id: Option<Uuid>,
    usage_fact_id: Option<Uuid>,
    attempt_no: i16,
    observed_cost_usd: Option<Decimal>,
    accounted_cost_usd: Option<Decimal>,
    cost_status: Option<String>,
    cost_reasons: Vec<String>,
    pricing_fingerprint: Option<String>,
    pricing_snapshot: Option<serde_json::Value>,
    dispatch_state: Option<String>,
    node_id: Option<Uuid>,
    owner_session_id: Option<Uuid>,
    stale_not_before: Option<DateTime<Utc>>,
    resolution_reason: Option<String>,
    resolution_actor: Option<String>,
    resolution_entry_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    settled_at: Option<DateTime<Utc>>,
    resolved_at: Option<DateTime<Utc>>,
}

impl LedgerRow {
    fn into_entry(self) -> Result<BudgetLedgerEntry, BudgetStoreError> {
        if self.last_account_revision < 0 || self.attempt_no != 0 {
            return Err(BudgetStoreError::corrupt("预算账本 revision/attempt 非法"));
        }
        let observed_cost_usd = self
            .observed_cost_usd
            .map(normalize_budget_amount)
            .transpose()
            .map_err(BudgetStoreError::corrupt)?;
        let accounted_cost_usd = self
            .accounted_cost_usd
            .map(normalize_budget_amount)
            .transpose()
            .map_err(BudgetStoreError::corrupt)?;
        let cost_status = self
            .cost_status
            .map(|value| CostStatus::from_str(&value))
            .transpose()
            .map_err(BudgetStoreError::corrupt)?;
        Ok(BudgetLedgerEntry {
            id: self.id,
            virtual_key_id: self.virtual_key_id,
            virtual_key_name: self.virtual_key_name,
            virtual_key_prefix: self.virtual_key_prefix,
            workspace_id: self.workspace_id,
            kind: BudgetLedgerKind::parse(&self.kind)?,
            status: BudgetLedgerStatus::parse(&self.status)?,
            request_id: self.request_id.map(Arc::from),
            operation_id: Arc::from(self.operation_id),
            command_fingerprint: trimmed_arc(self.command_fingerprint),
            dispatch_operation_id: self.dispatch_operation_id.map(Arc::from),
            terminal_operation_id: self.terminal_operation_id.map(Arc::from),
            terminal_command_fingerprint: trimmed_arc(self.terminal_command_fingerprint),
            last_account_revision: self.last_account_revision,
            parent_intent_id: self.parent_intent_id,
            usage_fact_id: self.usage_fact_id,
            attempt_no: self.attempt_no,
            observed_cost_usd,
            accounted_cost_usd,
            cost_status,
            cost_reasons: self.cost_reasons,
            pricing_fingerprint: trimmed_arc(self.pricing_fingerprint),
            pricing_snapshot: self.pricing_snapshot,
            dispatch_state: self.dispatch_state.map(Arc::from),
            node_id: self.node_id,
            owner_session_id: self.owner_session_id,
            stale_not_before: self.stale_not_before,
            resolution_reason: self.resolution_reason,
            resolution_actor: self.resolution_actor,
            resolution_entry_id: self.resolution_entry_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
            settled_at: self.settled_at,
            resolved_at: self.resolved_at,
        })
    }
}

async fn load_account(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    virtual_key_id: Uuid,
) -> Result<Option<GovernanceAccountRow>, BudgetStoreError> {
    sqlx::query_as(
        "SELECT id, name, key_prefix, ws_id, budget_limit, budget_used, \
                budget_pending_count, budget_unresolved_count, \
                budget_accounting_revision, budget_checkpoint_tail_events, \
                budget_accounting_state, budget_state_updated_at \
           FROM ai_virtual_keys WHERE id = $1",
    )
    .bind(virtual_key_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx_error)
}

async fn load_ledger_by_operation(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    operation_id: &str,
) -> Result<Option<LedgerRow>, BudgetStoreError> {
    let sql = format!("SELECT {LEDGER_COLUMNS} FROM ai_budget_ledger WHERE operation_id = $1");
    sqlx::query_as(&sql)
        .bind(operation_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx_error)
}

async fn ledger_history_exists(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    virtual_key_id: Uuid,
    scope: BudgetAdminScope,
) -> Result<bool, BudgetStoreError> {
    sqlx::query_scalar(
        "SELECT EXISTS (\
             SELECT 1 FROM ai_budget_ledger \
              WHERE virtual_key_id = $1 \
                AND COALESCE(workspace_id, $3) = $2\
         )",
    )
    .bind(virtual_key_id)
    .bind(scope.workspace_id)
    .bind(scope.default_workspace_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(map_sqlx_error)
}

async fn lock_ledger_by_id(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<LedgerRow>, BudgetStoreError> {
    let sql = format!("SELECT {LEDGER_COLUMNS} FROM ai_budget_ledger WHERE id = $1 FOR UPDATE");
    sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx_error)
}

fn validate_ledger_query(query: &BudgetLedgerQuery) -> Result<(), BudgetStoreError> {
    if query.virtual_key_id.is_nil()
        || query.page_size == 0
        || query.page_size > MAX_LEDGER_PAGE_SIZE
        || query
            .created_at_from
            .zip(query.created_at_to)
            .is_some_and(|(from, to)| from > to)
    {
        return Err(BudgetStoreError::corrupt("预算账本分页参数非法"));
    }
    Ok(())
}

fn trimmed_arc(value: Option<String>) -> Option<Arc<str>> {
    value.map(|value| Arc::from(value.trim().to_string()))
}

async fn lock_admin_operation(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    operation_id: &str,
) -> Result<(), BudgetStoreError> {
    // key-first 之后按稳定 operation ID 串行化。不同 key 并发复用同一 ID 时，
    // 后到事务会在前一事务提交后重读 replay/conflict，而不是撞 UNIQUE 变成 500。
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(operation_id)
        .execute(&mut **transaction)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

async fn reconcile_pg(
    store: &PgBudgetStore,
    command: ReconcileBudgetIntent,
) -> Result<BudgetReconciliation, BudgetStoreError> {
    let normalized = normalize_reconciliation(command)?;
    let mut transaction = store.begin_write().await?;

    // 所有 reconciliation 都先锁 key，再读取或锁账本行。
    let account_row = lock_account(&mut transaction, normalized.virtual_key_id)
        .await?
        .ok_or_else(not_found)?;
    if !normalized.scope.contains(account_row.ws_id) {
        return Err(not_found());
    }
    let account = account_row.into_snapshot()?;
    validate_account(&account)?;
    lock_admin_operation(&mut transaction, &normalized.operation_id).await?;

    let parent = lock_ledger_by_id(&mut transaction, normalized.intent_id)
        .await?
        .ok_or_else(not_found)?;
    validate_reconciliation_parent(&parent, &account, normalized.scope)?;

    if let Some(existing) =
        load_ledger_by_operation(&mut transaction, &normalized.operation_id).await?
    {
        let disposition = validate_reconciliation_replay(&existing, &normalized)?;
        let parent = parent.into_entry()?;
        let audit_entry = existing.into_entry()?;
        commit(transaction).await?;
        return Ok(BudgetReconciliation {
            disposition,
            account,
            intent: parent,
            audit_entry,
        });
    }

    if parent.status == BudgetLedgerStatus::Resolved.as_str() {
        return Err(BudgetStoreError::new(
            BudgetErrorKind::AlreadyReconciled,
            "预算 intent 已被 reconciliation",
        ));
    }
    if !matches!(parent.status.as_str(), "pending" | "unresolved") {
        return Err(BudgetStoreError::new(
            BudgetErrorKind::Conflict,
            "只有 pending/unresolved request intent 可以 reconciliation",
        ));
    }
    if parent.status == "pending" && owner_is_still_live(&mut transaction, &parent).await? {
        return Err(BudgetStoreError::new(
            BudgetErrorKind::IntentActive,
            "预算 intent 所属 owner 仍处于活动状态",
        ));
    }

    let amount = match normalized.resolution {
        NormalizedResolution::Settle(amount) => amount,
        NormalizedResolution::Waive => Decimal::ZERO,
    };
    let prospective_used = account
        .used_usd
        .checked_add(amount)
        .and_then(|value| normalize_budget_amount(value).ok());
    if prospective_used.is_none() {
        let result =
            apply_reconciliation_overflow(&mut transaction, &account, &parent, &normalized, amount)
                .await?;
        commit(transaction).await?;
        return Ok(result);
    }

    let new_revision = next_admin_revision(&account, 2)?;
    let account_update_sql = if parent.status == "pending" {
        if account.pending_count <= 0 {
            return Err(BudgetStoreError::corrupt(
                "pending intent 与预算账户 count 不一致",
            ));
        }
        "UPDATE ai_virtual_keys \
                SET budget_used = budget_used + $2, \
                    budget_pending_count = budget_pending_count - 1, \
                    budget_accounting_revision = $3, \
                    budget_checkpoint_tail_events = budget_checkpoint_tail_events + 2, \
                    budget_state_updated_at = clock_timestamp(), \
                    updated_at = clock_timestamp() \
              WHERE id = $1 \
                AND budget_pending_count > 0 \
                AND budget_accounting_revision = $4 \
                AND budget_used <= 9999999999999999.999999999999::numeric - $2 \
          RETURNING id, name, key_prefix, ws_id, budget_limit, budget_used, \
                    budget_pending_count, budget_unresolved_count, \
                    budget_accounting_revision, budget_checkpoint_tail_events, \
                    budget_accounting_state, budget_state_updated_at"
            .to_string()
    } else {
        if account.unresolved_count <= 0 {
            return Err(BudgetStoreError::corrupt(
                "unresolved intent 与预算账户 count 不一致",
            ));
        }
        "UPDATE ai_virtual_keys \
                SET budget_used = budget_used + $2, \
                    budget_unresolved_count = budget_unresolved_count - 1, \
                    budget_accounting_revision = $3, \
                    budget_checkpoint_tail_events = budget_checkpoint_tail_events + 2, \
                    budget_state_updated_at = clock_timestamp(), \
                    updated_at = clock_timestamp() \
              WHERE id = $1 \
                AND budget_unresolved_count > 0 \
                AND budget_accounting_revision = $4 \
                AND budget_used <= 9999999999999999.999999999999::numeric - $2 \
          RETURNING id, name, key_prefix, ws_id, budget_limit, budget_used, \
                    budget_pending_count, budget_unresolved_count, \
                    budget_accounting_revision, budget_checkpoint_tail_events, \
                    budget_accounting_state, budget_state_updated_at"
            .to_string()
    };
    let updated_account: GovernanceAccountRow = sqlx::query_as(&account_update_sql)
        .bind(normalized.virtual_key_id)
        .bind(amount)
        .bind(new_revision)
        .bind(account.accounting_revision)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?
        .ok_or_else(|| {
            BudgetStoreError::corrupt("reconciliation 更新 aggregate/count 时状态不一致")
        })?;

    let child_id = Uuid::new_v4();
    let (child_status, cost_status) = match normalized.resolution {
        NormalizedResolution::Settle(_) => ("settled", CostStatus::Calculated.as_str()),
        NormalizedResolution::Waive => ("waived", CostStatus::NotIncurred.as_str()),
    };
    let insert_sql = format!(
        "INSERT INTO ai_budget_ledger (\
             id, virtual_key_id, virtual_key_name, virtual_key_prefix, workspace_id, \
             kind, status, operation_id, command_fingerprint, last_account_revision, \
             parent_intent_id, observed_cost_usd, accounted_cost_usd, cost_status, \
             resolution_reason, resolution_actor, settled_at\
         ) VALUES (\
             $1, $2, $3, $4, $5, 'reconciliation', $6, $7, $8, $9, \
             $10, $11, $11, $12, $13, $14, clock_timestamp()\
         ) RETURNING {LEDGER_COLUMNS}"
    );
    let child: LedgerRow = sqlx::query_as(&insert_sql)
        .bind(child_id)
        .bind(normalized.virtual_key_id)
        .bind(&account.virtual_key_name)
        .bind(&account.virtual_key_prefix)
        .bind(account.workspace_id)
        .bind(child_status)
        .bind(&normalized.operation_id)
        .bind(&normalized.command_fingerprint)
        .bind(new_revision)
        .bind(normalized.intent_id)
        .bind(amount)
        .bind(cost_status)
        .bind(&normalized.reason)
        .bind(&normalized.actor)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;

    let parent_update_sql = format!(
        "UPDATE ai_budget_ledger \
            SET status = 'resolved', \
                terminal_operation_id = $2, \
                terminal_command_fingerprint = $3, \
                last_account_revision = $4, \
                resolution_reason = $5, \
                resolution_actor = $6, \
                resolution_entry_id = $7, \
                updated_at = clock_timestamp(), \
                resolved_at = clock_timestamp() \
          WHERE id = $1 AND status = $8 \
      RETURNING {LEDGER_COLUMNS}"
    );
    let parent: LedgerRow = sqlx::query_as(&parent_update_sql)
        .bind(normalized.intent_id)
        .bind(&normalized.operation_id)
        .bind(&normalized.command_fingerprint)
        .bind(new_revision)
        .bind(&normalized.reason)
        .bind(&normalized.actor)
        .bind(child_id)
        .bind(&parent.status)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?
        .ok_or_else(|| BudgetStoreError::corrupt("reconciliation 更新父 intent 失败"))?;

    let account = updated_account.into_snapshot()?;
    validate_account(&account)?;
    let intent = parent.into_entry()?;
    let audit_entry = child.into_entry()?;
    commit(transaction).await?;
    Ok(BudgetReconciliation {
        disposition: BudgetReconciliationDisposition::Applied,
        account,
        intent,
        audit_entry,
    })
}

#[derive(Clone, Copy)]
enum NormalizedResolution {
    Settle(Decimal),
    Waive,
}

struct NormalizedReconciliation {
    virtual_key_id: Uuid,
    intent_id: Uuid,
    scope: BudgetAdminScope,
    operation_id: String,
    command_fingerprint: String,
    actor: String,
    reason: String,
    resolution: NormalizedResolution,
}

fn normalize_reconciliation(
    command: ReconcileBudgetIntent,
) -> Result<NormalizedReconciliation, BudgetStoreError> {
    if command.virtual_key_id.is_nil()
        || command.intent_id.is_nil()
        || command.operation_id.is_nil()
    {
        return Err(BudgetStoreError::corrupt("reconciliation 命令 ID 不能为空"));
    }
    let reason = normalize_audit_text(&command.reason, "reason", true)?;
    let actor = normalize_audit_text(&command.actor, "actor", false)?;
    let resolution = match command.resolution {
        BudgetReconciliationResolution::Settle { cost_usd } => NormalizedResolution::Settle(
            normalize_budget_amount(cost_usd).map_err(BudgetStoreError::corrupt)?,
        ),
        BudgetReconciliationResolution::Waive => NormalizedResolution::Waive,
    };
    let operation_id = format!("budget-reconcile:v1:{}", command.operation_id);
    let resolution_name = match resolution {
        NormalizedResolution::Settle(_) => "settle",
        NormalizedResolution::Waive => "waive",
    };
    let amount = match resolution {
        NormalizedResolution::Settle(amount) => Some(amount.to_string()),
        NormalizedResolution::Waive => None,
    };
    let canonical = serde_json::to_vec(&(
        BUDGET_SCHEMA_VERSION,
        command.virtual_key_id,
        command.intent_id,
        resolution_name,
        amount,
        &reason,
        &actor,
    ))
    .map_err(|_| BudgetStoreError::corrupt("reconciliation 命令无法规范编码"))?;
    let command_fingerprint = format!("{:x}", Sha256::digest(canonical));
    Ok(NormalizedReconciliation {
        virtual_key_id: command.virtual_key_id,
        intent_id: command.intent_id,
        scope: command.scope,
        operation_id,
        command_fingerprint,
        actor,
        reason,
        resolution,
    })
}

fn normalize_audit_text(
    value: &str,
    field: &str,
    enforce_max: bool,
) -> Result<String, BudgetStoreError> {
    let normalized: String = value.trim().nfc().collect();
    if normalized.is_empty() || (enforce_max && normalized.len() > 1024) {
        return Err(BudgetStoreError::corrupt(format!(
            "{field} 必须为 1..=1024 bytes 的非空文本"
        )));
    }
    Ok(normalized)
}

fn validate_reconciliation_parent(
    parent: &LedgerRow,
    account: &BudgetAccountSnapshot,
    scope: BudgetAdminScope,
) -> Result<(), BudgetStoreError> {
    if parent.kind != "request"
        || parent.virtual_key_id != account.virtual_key_id
        || !scope.contains(parent.workspace_id)
        || parent.workspace_id.unwrap_or(scope.default_workspace_id)
            != account.workspace_id.unwrap_or(scope.default_workspace_id)
    {
        return Err(not_found());
    }
    Ok(())
}

fn validate_reconciliation_replay(
    existing: &LedgerRow,
    command: &NormalizedReconciliation,
) -> Result<BudgetReconciliationDisposition, BudgetStoreError> {
    if existing.virtual_key_id != command.virtual_key_id
        || existing.parent_intent_id != Some(command.intent_id)
        || existing.command_fingerprint.as_deref().map(str::trim)
            != Some(command.command_fingerprint.as_str())
    {
        return Err(BudgetStoreError::new(
            BudgetErrorKind::Conflict,
            "reconciliation operation ID 的幂等载荷冲突",
        ));
    }
    match existing.kind.as_str() {
        "reconciliation" => Ok(BudgetReconciliationDisposition::Replayed),
        "reconciliation_attempt" => Ok(BudgetReconciliationDisposition::NumericOverflowReplayed),
        _ => Err(BudgetStoreError::new(
            BudgetErrorKind::Conflict,
            "operation ID 已被其他预算操作占用",
        )),
    }
}

async fn owner_is_still_live(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    parent: &LedgerRow,
) -> Result<bool, BudgetStoreError> {
    let owner_session_id = parent
        .owner_session_id
        .ok_or_else(|| BudgetStoreError::corrupt("pending intent 缺少 owner session"))?;
    let live: Option<bool> = sqlx::query_scalar(
        "SELECT stopped_at IS NULL AND expires_at > clock_timestamp() \
           FROM ai_budget_owner_sessions \
          WHERE session_id = $1 \
          FOR UPDATE",
    )
    .bind(owner_session_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx_error)?;
    Ok(live.unwrap_or(false))
}

fn next_admin_revision(
    account: &BudgetAccountSnapshot,
    tail_events: i64,
) -> Result<i64, BudgetStoreError> {
    account
        .checkpoint_tail_events
        .checked_add(tail_events)
        .ok_or_else(|| BudgetStoreError::corrupt("预算 tail event count 溢出"))?;
    account
        .accounting_revision
        .checked_add(1)
        .ok_or_else(|| BudgetStoreError::corrupt("预算 revision 溢出"))
}

async fn apply_reconciliation_overflow(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account: &BudgetAccountSnapshot,
    parent: &LedgerRow,
    command: &NormalizedReconciliation,
    amount: Decimal,
) -> Result<BudgetReconciliation, BudgetStoreError> {
    let new_revision = next_admin_revision(account, 2)?;
    let update_account_sql = if parent.status == "pending" {
        if account.pending_count <= 0 {
            return Err(BudgetStoreError::corrupt(
                "pending intent 与预算账户 count 不一致",
            ));
        }
        "UPDATE ai_virtual_keys \
            SET budget_pending_count = budget_pending_count - 1, \
                budget_unresolved_count = budget_unresolved_count + 1, \
                budget_accounting_revision = $2, \
                budget_checkpoint_tail_events = budget_checkpoint_tail_events + 2, \
                budget_state_updated_at = clock_timestamp(), \
                updated_at = clock_timestamp() \
          WHERE id = $1 \
            AND budget_pending_count > 0 \
            AND budget_accounting_revision = $3 \
      RETURNING id, name, key_prefix, ws_id, budget_limit, budget_used, \
                budget_pending_count, budget_unresolved_count, \
                budget_accounting_revision, budget_checkpoint_tail_events, \
                budget_accounting_state, budget_state_updated_at"
    } else {
        if account.unresolved_count <= 0 {
            return Err(BudgetStoreError::corrupt(
                "unresolved intent 与预算账户 count 不一致",
            ));
        }
        "UPDATE ai_virtual_keys \
            SET budget_accounting_revision = $2, \
                budget_checkpoint_tail_events = budget_checkpoint_tail_events + 2, \
                budget_state_updated_at = clock_timestamp(), \
                updated_at = clock_timestamp() \
          WHERE id = $1 \
            AND budget_unresolved_count > 0 \
            AND budget_accounting_revision = $3 \
      RETURNING id, name, key_prefix, ws_id, budget_limit, budget_used, \
                budget_pending_count, budget_unresolved_count, \
                budget_accounting_revision, budget_checkpoint_tail_events, \
                budget_accounting_state, budget_state_updated_at"
    };
    let updated_account: GovernanceAccountRow = sqlx::query_as(update_account_sql)
        .bind(account.virtual_key_id)
        .bind(new_revision)
        .bind(account.accounting_revision)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx_error)?
        .ok_or_else(|| BudgetStoreError::corrupt("overflow reconciliation 更新账户状态失败"))?;

    let reasons = vec!["budget_numeric_overflow".to_string()];
    let parent_update_sql = format!(
        "UPDATE ai_budget_ledger \
            SET status = 'unresolved', \
                observed_cost_usd = $2, \
                accounted_cost_usd = NULL, \
                cost_status = 'unavailable', \
                cost_reasons = $3, \
                last_account_revision = $4, \
                updated_at = clock_timestamp() \
          WHERE id = $1 AND status = $5 \
      RETURNING {LEDGER_COLUMNS}"
    );
    let updated_parent: LedgerRow = sqlx::query_as(&parent_update_sql)
        .bind(command.intent_id)
        .bind(amount)
        .bind(&reasons)
        .bind(new_revision)
        .bind(&parent.status)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx_error)?
        .ok_or_else(|| BudgetStoreError::corrupt("overflow reconciliation 更新父 intent 失败"))?;

    let attempt_id = Uuid::new_v4();
    let insert_sql = format!(
        "INSERT INTO ai_budget_ledger (\
             id, virtual_key_id, virtual_key_name, virtual_key_prefix, workspace_id, \
             kind, status, operation_id, command_fingerprint, last_account_revision, \
             parent_intent_id, observed_cost_usd, cost_status, cost_reasons, \
             resolution_reason, resolution_actor, resolved_at\
         ) VALUES (\
             $1, $2, $3, $4, $5, 'reconciliation_attempt', 'resolved', \
             $6, $7, $8, $9, $10, 'unavailable', $11, $12, $13, clock_timestamp()\
         ) RETURNING {LEDGER_COLUMNS}"
    );
    let attempt: LedgerRow = sqlx::query_as(&insert_sql)
        .bind(attempt_id)
        .bind(account.virtual_key_id)
        .bind(&account.virtual_key_name)
        .bind(&account.virtual_key_prefix)
        .bind(account.workspace_id)
        .bind(&command.operation_id)
        .bind(&command.command_fingerprint)
        .bind(new_revision)
        .bind(command.intent_id)
        .bind(amount)
        .bind(&reasons)
        .bind(&command.reason)
        .bind(&command.actor)
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_sqlx_error)?;

    let account = updated_account.into_snapshot()?;
    validate_account(&account)?;
    Ok(BudgetReconciliation {
        disposition: BudgetReconciliationDisposition::NumericOverflow,
        account,
        intent: updated_parent.into_entry()?,
        audit_entry: attempt.into_entry()?,
    })
}

async fn verify_or_rebuild_pg(
    store: &PgBudgetStore,
    command: RebuildBudgetAccount,
) -> Result<BudgetRebuildResult, BudgetStoreError> {
    let normalized = normalize_rebuild_command(command)?;
    if normalized.dry_run {
        let scan =
            scan_rebuild_with_issue(store, normalized.virtual_key_id, normalized.scope).await?;
        let is_current = account_revision_is_current(
            store,
            normalized.virtual_key_id,
            normalized.scope,
            scan.account.accounting_revision,
        )
        .await?;
        return Ok(BudgetRebuildResult {
            disposition: BudgetRebuildDisposition::Verified,
            account: scan.account.clone(),
            comparison: scan.comparison(is_current),
            audit_entry: None,
        });
    }

    for _ in 0..normalized.max_attempts {
        let scan =
            scan_rebuild_with_issue(store, normalized.virtual_key_id, normalized.scope).await?;
        match apply_rebuild_snapshot(store, &normalized, scan).await? {
            RebuildApplyOutcome::Applied(result) | RebuildApplyOutcome::Replayed(result) => {
                return Ok(result);
            }
            RebuildApplyOutcome::RevisionChanged => continue,
        }
    }
    Err(BudgetStoreError::new(
        BudgetErrorKind::AccountBusy,
        "预算账户在 rebuild 期间持续变化",
    ))
}

async fn scan_rebuild_with_issue(
    store: &PgBudgetStore,
    virtual_key_id: Uuid,
    scope: BudgetAdminScope,
) -> Result<RebuildScan, BudgetStoreError> {
    match scan_rebuild_snapshot(store, virtual_key_id, scope).await {
        Ok(scan) => Ok(scan),
        Err(error)
            if matches!(
                error.kind(),
                BudgetErrorKind::Corrupt | BudgetErrorKind::NumericOverflow
            ) =>
        {
            let reason = if error.kind() == BudgetErrorKind::NumericOverflow {
                "budget_rebuild_numeric_overflow"
            } else {
                "budget_rebuild_invariant_invalid"
            };
            record_account_issue(store, virtual_key_id, scope, reason).await?;
            Err(error)
        }
        Err(error) => Err(error),
    }
}

async fn record_account_issue(
    store: &PgBudgetStore,
    virtual_key_id: Uuid,
    scope: BudgetAdminScope,
    reason: &str,
) -> Result<(), BudgetStoreError> {
    let mut transaction = store.begin_write().await?;
    let account_row = lock_account(&mut transaction, virtual_key_id)
        .await?
        .ok_or_else(not_found)?;
    if !scope.contains(account_row.ws_id) {
        return Err(not_found());
    }
    let account = account_row.into_snapshot()?;
    validate_account(&account)?;
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM ai_budget_ledger \
          WHERE virtual_key_id = $1 \
            AND kind = 'account_issue' \
            AND status = 'unresolved' \
            AND cost_reasons @> $2::text[] \
            AND COALESCE(workspace_id, $4) = $3 \
          ORDER BY created_at, id \
          LIMIT 1 \
          FOR UPDATE",
    )
    .bind(virtual_key_id)
    .bind(vec![reason.to_string()])
    .bind(scope.workspace_id)
    .bind(scope.default_workspace_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_sqlx_error)?;
    if existing.is_some() {
        commit(transaction).await?;
        return Ok(());
    }

    let new_revision = next_admin_revision(&account, 1)?;
    account
        .unresolved_count
        .checked_add(1)
        .ok_or_else(|| BudgetStoreError::corrupt("预算 unresolved count 溢出"))?;
    let operation_id = format!(
        "budget-account-issue:v1:{}:{}",
        virtual_key_id, account.accounting_revision
    );
    let canonical = serde_json::to_vec(&(
        BUDGET_SCHEMA_VERSION,
        virtual_key_id,
        account.accounting_revision,
        reason,
    ))
    .map_err(|_| BudgetStoreError::corrupt("account issue 无法规范编码"))?;
    let fingerprint = format!("{:x}", Sha256::digest(canonical));
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO ai_budget_ledger (\
             id, virtual_key_id, virtual_key_name, virtual_key_prefix, workspace_id, \
             kind, status, operation_id, command_fingerprint, last_account_revision, \
             cost_status, cost_reasons\
         ) VALUES (\
             $1, $2, $3, $4, $5, 'account_issue', 'unresolved', \
             $6, $7, $8, 'unavailable', $9\
         )",
    )
    .bind(issue_id)
    .bind(virtual_key_id)
    .bind(&account.virtual_key_name)
    .bind(&account.virtual_key_prefix)
    .bind(account.workspace_id)
    .bind(operation_id)
    .bind(fingerprint)
    .bind(new_revision)
    .bind(vec![reason.to_string()])
    .execute(&mut *transaction)
    .await
    .map_err(map_sqlx_error)?;
    let updated = sqlx::query(
        "UPDATE ai_virtual_keys \
            SET budget_unresolved_count = budget_unresolved_count + 1, \
                budget_accounting_revision = $2, \
                budget_checkpoint_tail_events = budget_checkpoint_tail_events + 1, \
                budget_state_updated_at = clock_timestamp(), \
                updated_at = clock_timestamp() \
          WHERE id = $1 AND budget_accounting_revision = $3",
    )
    .bind(virtual_key_id)
    .bind(new_revision)
    .bind(account.accounting_revision)
    .execute(&mut *transaction)
    .await
    .map_err(map_sqlx_error)?;
    if updated.rows_affected() != 1 {
        return Err(BudgetStoreError::corrupt(
            "account issue 更新账户 revision 失败",
        ));
    }
    commit(transaction).await
}

struct NormalizedRebuildCommand {
    virtual_key_id: Uuid,
    operation_id: String,
    checkpoint_operation_id: String,
    command_fingerprint: String,
    scope: BudgetAdminScope,
    actor: String,
    reason: String,
    dry_run: bool,
    max_attempts: u8,
}

fn normalize_rebuild_command(
    command: RebuildBudgetAccount,
) -> Result<NormalizedRebuildCommand, BudgetStoreError> {
    if command.virtual_key_id.is_nil()
        || command.operation_id.is_nil()
        || command.max_attempts == 0
        || command.max_attempts > 8
    {
        return Err(BudgetStoreError::corrupt("预算 rebuild 命令参数非法"));
    }
    let reason = normalize_audit_text(&command.reason, "reason", true)?;
    let actor = normalize_audit_text(&command.actor, "actor", false)?;
    let operation_id = format!("budget-rebuild:v1:{}", command.operation_id);
    let checkpoint_operation_id = format!("budget-checkpoint-rebuild:v1:{}", command.operation_id);
    let canonical = serde_json::to_vec(&(
        BUDGET_SCHEMA_VERSION,
        command.virtual_key_id,
        "rebuild",
        &reason,
        &actor,
    ))
    .map_err(|_| BudgetStoreError::corrupt("rebuild 命令无法规范编码"))?;
    let command_fingerprint = format!("{:x}", Sha256::digest(canonical));
    Ok(NormalizedRebuildCommand {
        virtual_key_id: command.virtual_key_id,
        operation_id,
        checkpoint_operation_id,
        command_fingerprint,
        scope: command.scope,
        actor,
        reason,
        dry_run: command.dry_run,
        max_attempts: command.max_attempts,
    })
}

#[derive(FromRow)]
struct CheckpointRow {
    checkpoint_revision: i64,
    accounted_cost_usd: Decimal,
}

#[derive(FromRow)]
struct TailAggregateRow {
    tail_used_usd: String,
    foreign_workspace_count: i64,
    future_revision_count: i64,
}

#[derive(FromRow)]
struct OpenAggregateRow {
    pending_count: i64,
    unresolved_request_count: i64,
    open_account_issue_count: i64,
    foreign_workspace_count: i64,
    future_revision_count: i64,
}

struct RebuildScan {
    account: BudgetAccountSnapshot,
    checkpoint_revision: i64,
    recomputed_used_usd: Decimal,
    pending_count: i64,
    unresolved_request_count: i64,
    open_account_issue_count: i64,
}

impl RebuildScan {
    fn recomputed_unresolved_count(&self) -> Result<i64, BudgetStoreError> {
        self.unresolved_request_count
            .checked_add(self.open_account_issue_count)
            .ok_or_else(|| BudgetStoreError::corrupt("预算 unresolved count 溢出"))
    }

    fn comparison(&self, is_current: bool) -> BudgetRebuildComparison {
        let recomputed_unresolved_count = self
            .unresolved_request_count
            .checked_add(self.open_account_issue_count)
            .unwrap_or(i64::MAX);
        BudgetRebuildComparison {
            snapshot_revision: self.account.accounting_revision,
            checkpoint_revision: self.checkpoint_revision,
            stored_used_usd: self.account.used_usd,
            recomputed_used_usd: self.recomputed_used_usd,
            difference_usd: self.recomputed_used_usd - self.account.used_usd,
            stored_pending_count: self.account.pending_count,
            recomputed_pending_count: self.pending_count,
            stored_unresolved_count: self.account.unresolved_count,
            recomputed_unresolved_count,
            unresolved_request_count: self.unresolved_request_count,
            open_account_issue_count: self.open_account_issue_count,
            is_current,
        }
    }
}

async fn scan_rebuild_snapshot(
    store: &PgBudgetStore,
    virtual_key_id: Uuid,
    scope: BudgetAdminScope,
) -> Result<RebuildScan, BudgetStoreError> {
    let mut transaction = store.begin_repeatable_read().await?;
    let account_row = load_account(&mut transaction, virtual_key_id)
        .await?
        .ok_or_else(not_found)?;
    if !scope.contains(account_row.ws_id) {
        return Err(not_found());
    }
    let account = account_row.into_snapshot()?;
    validate_account(&account)?;

    let checkpoint: Option<CheckpointRow> = sqlx::query_as(
        "SELECT checkpoint_revision, accounted_cost_usd \
           FROM ai_budget_checkpoints \
          WHERE virtual_key_id = $1 AND checkpoint_revision <= $2 \
          ORDER BY checkpoint_revision DESC \
          LIMIT 1",
    )
    .bind(virtual_key_id)
    .bind(account.accounting_revision)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_sqlx_error)?;
    let checkpoint =
        checkpoint.ok_or_else(|| BudgetStoreError::corrupt("预算账户缺少有效 checkpoint"))?;
    if checkpoint.checkpoint_revision < 0 {
        return Err(BudgetStoreError::corrupt("checkpoint revision 非法"));
    }
    let checkpoint_amount = normalize_budget_amount(checkpoint.accounted_cost_usd)
        .map_err(BudgetStoreError::corrupt)?;

    // settled tail 走 (virtual_key_id, last_account_revision) partial index，
    // 不扫描 checkpoint 已覆盖的永久历史。
    let tail: TailAggregateRow = sqlx::query_as(
        "SELECT \
             COALESCE(SUM(accounted_cost_usd) FILTER (\
                 WHERE last_account_revision <= $3 \
                   AND COALESCE(workspace_id, $5) = $4\
             ), 0::numeric)::text AS tail_used_usd, \
             COUNT(*) FILTER (\
                 WHERE last_account_revision <= $3 \
                   AND COALESCE(workspace_id, $5) <> $4\
             )::bigint AS foreign_workspace_count, \
             COUNT(*) FILTER (\
                 WHERE last_account_revision > $3\
             )::bigint AS future_revision_count \
         FROM ai_budget_ledger \
        WHERE virtual_key_id = $1 \
          AND status = 'settled' \
          AND last_account_revision > $2",
    )
    .bind(virtual_key_id)
    .bind(checkpoint.checkpoint_revision)
    .bind(account.accounting_revision)
    .bind(scope.workspace_id)
    .bind(scope.default_workspace_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(map_sqlx_error)?;

    // pending/unresolved 只走 open partial index，成本由活跃 intent 基数决定。
    let open: OpenAggregateRow = sqlx::query_as(
        "SELECT \
             COUNT(*) FILTER (\
                 WHERE status = 'pending' \
                   AND last_account_revision <= $2 \
                   AND COALESCE(workspace_id, $4) = $3\
             )::bigint AS pending_count, \
             COUNT(*) FILTER (\
                 WHERE kind = 'request' AND status = 'unresolved' \
                   AND last_account_revision <= $2 \
                   AND COALESCE(workspace_id, $4) = $3\
             )::bigint AS unresolved_request_count, \
             COUNT(*) FILTER (\
                 WHERE kind = 'account_issue' AND status = 'unresolved' \
                   AND last_account_revision <= $2 \
                   AND COALESCE(workspace_id, $4) = $3\
             )::bigint AS open_account_issue_count, \
             COUNT(*) FILTER (\
                 WHERE last_account_revision <= $2 \
                   AND COALESCE(workspace_id, $4) <> $3\
             )::bigint AS foreign_workspace_count, \
             COUNT(*) FILTER (\
                 WHERE last_account_revision > $2\
             )::bigint AS future_revision_count \
         FROM ai_budget_ledger \
        WHERE virtual_key_id = $1 \
          AND status IN ('pending', 'unresolved')",
    )
    .bind(virtual_key_id)
    .bind(account.accounting_revision)
    .bind(scope.workspace_id)
    .bind(scope.default_workspace_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(map_sqlx_error)?;
    if tail.foreign_workspace_count != 0
        || tail.future_revision_count != 0
        || open.foreign_workspace_count != 0
        || open.future_revision_count != 0
        || open.pending_count < 0
        || open.unresolved_request_count < 0
        || open.open_account_issue_count < 0
    {
        return Err(BudgetStoreError::corrupt(
            "预算账本 workspace/count invariant 损坏",
        ));
    }
    let tail_amount = database_numeric_to_budget_amount(&tail.tail_used_usd)?;
    let recomputed_used_usd = checkpoint_amount
        .checked_add(tail_amount)
        .and_then(|value| normalize_budget_amount(value).ok())
        .ok_or_else(|| {
            BudgetStoreError::new(
                BudgetErrorKind::NumericOverflow,
                "rebuild 合计超出 NUMERIC(28,12) 范围",
            )
        })?;
    commit(transaction).await?;
    Ok(RebuildScan {
        account,
        checkpoint_revision: checkpoint.checkpoint_revision,
        recomputed_used_usd,
        pending_count: open.pending_count,
        unresolved_request_count: open.unresolved_request_count,
        open_account_issue_count: open.open_account_issue_count,
    })
}

fn database_numeric_to_budget_amount(value: &str) -> Result<Decimal, BudgetStoreError> {
    parse_budget_amount(value).map_err(|_| {
        BudgetStoreError::new(
            BudgetErrorKind::NumericOverflow,
            "预算账本合计超出 NUMERIC(28,12) 范围",
        )
    })
}

async fn account_revision_is_current(
    store: &PgBudgetStore,
    virtual_key_id: Uuid,
    scope: BudgetAdminScope,
    revision: i64,
) -> Result<bool, BudgetStoreError> {
    let mut transaction = store.begin_repeatable_read().await?;
    let row = load_account(&mut transaction, virtual_key_id)
        .await?
        .ok_or_else(not_found)?;
    if !scope.contains(row.ws_id) {
        return Err(not_found());
    }
    let is_current = row.budget_accounting_revision == revision;
    commit(transaction).await?;
    Ok(is_current)
}

enum RebuildApplyOutcome {
    Applied(BudgetRebuildResult),
    Replayed(BudgetRebuildResult),
    RevisionChanged,
}

async fn apply_rebuild_snapshot(
    store: &PgBudgetStore,
    command: &NormalizedRebuildCommand,
    scan: RebuildScan,
) -> Result<RebuildApplyOutcome, BudgetStoreError> {
    let mut transaction = store.begin_write().await?;
    let current_row = lock_account(&mut transaction, command.virtual_key_id)
        .await?
        .ok_or_else(not_found)?;
    if !command.scope.contains(current_row.ws_id) {
        return Err(not_found());
    }
    let current = current_row.into_snapshot()?;
    validate_account(&current)?;
    lock_admin_operation(&mut transaction, &command.operation_id).await?;

    if let Some(existing) =
        load_ledger_by_operation(&mut transaction, &command.operation_id).await?
    {
        if existing.virtual_key_id != command.virtual_key_id
            || existing.kind != "rebuild_audit"
            || existing.command_fingerprint.as_deref().map(str::trim)
                != Some(command.command_fingerprint.as_str())
        {
            return Err(BudgetStoreError::new(
                BudgetErrorKind::Conflict,
                "rebuild operation ID 的幂等载荷冲突",
            ));
        }
        let comparison =
            scan.comparison(current.accounting_revision == scan.account.accounting_revision);
        let audit_entry = existing.into_entry()?;
        commit(transaction).await?;
        return Ok(RebuildApplyOutcome::Replayed(BudgetRebuildResult {
            disposition: BudgetRebuildDisposition::Replayed,
            account: current,
            comparison,
            audit_entry: Some(audit_entry),
        }));
    }

    if current.accounting_revision != scan.account.accounting_revision {
        return Ok(RebuildApplyOutcome::RevisionChanged);
    }
    if current.workspace_id != scan.account.workspace_id {
        return Err(BudgetStoreError::corrupt(
            "rebuild 扫描与提交的 workspace 不一致",
        ));
    }
    let recomputed_unresolved_count = scan.recomputed_unresolved_count()?;
    let new_revision = next_admin_revision(&current, 1)?;
    let audit_id = Uuid::new_v4();
    let insert_audit_sql = format!(
        "INSERT INTO ai_budget_ledger (\
             id, virtual_key_id, virtual_key_name, virtual_key_prefix, workspace_id, \
             kind, status, operation_id, command_fingerprint, last_account_revision, \
             observed_cost_usd, accounted_cost_usd, cost_status, \
             resolution_reason, resolution_actor, resolved_at\
         ) VALUES (\
             $1, $2, $3, $4, $5, 'rebuild_audit', 'resolved', $6, $7, $8, \
             $9, $10, 'calculated', $11, $12, clock_timestamp()\
         ) RETURNING {LEDGER_COLUMNS}"
    );
    let audit: LedgerRow = sqlx::query_as(&insert_audit_sql)
        .bind(audit_id)
        .bind(command.virtual_key_id)
        .bind(&current.virtual_key_name)
        .bind(&current.virtual_key_prefix)
        .bind(current.workspace_id)
        .bind(&command.operation_id)
        .bind(&command.command_fingerprint)
        .bind(new_revision)
        .bind(current.used_usd)
        .bind(scan.recomputed_used_usd)
        .bind(&command.reason)
        .bind(&command.actor)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;

    let resolved_issues = sqlx::query(
        "UPDATE ai_budget_ledger \
            SET status = 'resolved', \
                last_account_revision = $2, \
                resolution_reason = $3, \
                resolution_actor = $4, \
                resolution_entry_id = $5, \
                updated_at = clock_timestamp(), \
                resolved_at = clock_timestamp() \
          WHERE virtual_key_id = $1 \
            AND kind = 'account_issue' \
            AND status = 'unresolved' \
            AND last_account_revision <= $6 \
            AND COALESCE(workspace_id, $8) = $7",
    )
    .bind(command.virtual_key_id)
    .bind(new_revision)
    .bind(&command.reason)
    .bind(&command.actor)
    .bind(audit_id)
    .bind(scan.account.accounting_revision)
    .bind(command.scope.workspace_id)
    .bind(command.scope.default_workspace_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_sqlx_error)?;
    if resolved_issues.rows_affected()
        != u64::try_from(scan.open_account_issue_count)
            .map_err(|_| BudgetStoreError::corrupt("account issue count 非法"))?
    {
        return Err(BudgetStoreError::corrupt(
            "rebuild 关闭 account issue 的数量发生变化",
        ));
    }

    sqlx::query(
        "INSERT INTO ai_budget_checkpoints (\
             virtual_key_id, checkpoint_revision, accounted_cost_usd, operation_id\
         ) VALUES ($1, $2, $3, $4)",
    )
    .bind(command.virtual_key_id)
    .bind(new_revision)
    .bind(scan.recomputed_used_usd)
    .bind(&command.checkpoint_operation_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_sqlx_error)?;

    let update_account_sql = "\
        UPDATE ai_virtual_keys \
           SET budget_used = $2, \
               budget_pending_count = $3, \
               budget_unresolved_count = $4, \
               budget_accounting_revision = $5, \
               budget_checkpoint_tail_events = 0, \
               budget_state_updated_at = clock_timestamp(), \
               updated_at = clock_timestamp() \
         WHERE id = $1 AND budget_accounting_revision = $6 \
     RETURNING id, name, key_prefix, ws_id, budget_limit, budget_used, \
               budget_pending_count, budget_unresolved_count, \
               budget_accounting_revision, budget_checkpoint_tail_events, \
               budget_accounting_state, budget_state_updated_at";
    let updated: GovernanceAccountRow = sqlx::query_as(update_account_sql)
        .bind(command.virtual_key_id)
        .bind(scan.recomputed_used_usd)
        .bind(scan.pending_count)
        .bind(scan.unresolved_request_count)
        .bind(new_revision)
        .bind(scan.account.accounting_revision)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?
        .ok_or_else(|| BudgetStoreError::corrupt("rebuild CAS 提交失败"))?;
    let account = updated.into_snapshot()?;
    validate_account(&account)?;
    let comparison = scan.comparison(true);
    let audit_entry = audit.into_entry()?;
    commit(transaction).await?;
    Ok(RebuildApplyOutcome::Applied(BudgetRebuildResult {
        disposition: BudgetRebuildDisposition::Rebuilt,
        account,
        comparison: BudgetRebuildComparison {
            recomputed_unresolved_count,
            ..comparison
        },
        audit_entry: Some(audit_entry),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{
        BudgetAccountGovernance, BudgetLimitMutation, BudgetOptionalMutation, CreateBudgetAccount,
        DeleteBudgetAccount, PgBudgetStoreConfig, UpdateBudgetAccount, UpdateBudgetLimit,
    };
    use sqlx::postgres::PgPoolOptions;
    use std::time::Duration;

    #[test]
    fn ledger_query_rejects_unbounded_page() {
        let workspace_id = Uuid::new_v4();
        let query = BudgetLedgerQuery {
            virtual_key_id: Uuid::new_v4(),
            scope: BudgetAdminScope {
                workspace_id,
                default_workspace_id: workspace_id,
            },
            statuses: Vec::new(),
            created_at_from: None,
            created_at_to: None,
            after: None,
            page_size: MAX_LEDGER_PAGE_SIZE + 1,
        };
        assert_eq!(
            validate_ledger_query(&query).unwrap_err().kind(),
            BudgetErrorKind::Corrupt
        );
    }

    #[test]
    fn reconciliation_fingerprint_uses_exact_amount_and_nfc_reason() {
        let workspace_id = Uuid::new_v4();
        let key_id = Uuid::new_v4();
        let intent_id = Uuid::new_v4();
        let operation_id = Uuid::new_v4();
        let scope = BudgetAdminScope {
            workspace_id,
            default_workspace_id: workspace_id,
        };
        let first = normalize_reconciliation(ReconcileBudgetIntent {
            virtual_key_id: key_id,
            intent_id,
            operation_id,
            scope,
            actor: Arc::from("system/node:test"),
            reason: " Cafe\u{301} ".to_string(),
            resolution: BudgetReconciliationResolution::Settle {
                cost_usd: Decimal::new(123, 2),
            },
        })
        .unwrap();
        let second = normalize_reconciliation(ReconcileBudgetIntent {
            virtual_key_id: key_id,
            intent_id,
            operation_id,
            scope,
            actor: Arc::from("system/node:test"),
            reason: "Café".to_string(),
            resolution: BudgetReconciliationResolution::Settle {
                cost_usd: Decimal::new(123_000_000_000_000, 14),
            },
        })
        .unwrap();

        assert_eq!(first.reason, "Café");
        assert_eq!(first.command_fingerprint, second.command_fingerprint);
    }

    #[test]
    fn database_sum_parser_rejects_out_of_range_without_rounding() {
        assert_eq!(
            database_numeric_to_budget_amount("10000000000000000.000000000000")
                .unwrap_err()
                .kind(),
            BudgetErrorKind::NumericOverflow
        );
        assert_eq!(
            database_numeric_to_budget_amount("1.0000000000001")
                .unwrap_err()
                .kind(),
            BudgetErrorKind::NumericOverflow
        );
    }

    #[tokio::test]
    async fn postgres_admin_governance_reconcile_rebuild_and_deleted_history() {
        let Some(database_url) = crate::budget::postgres_test_url() else {
            return;
        };
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await
            .unwrap();
        let workspace_id: Uuid =
            sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'default' LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        let suffix = Uuid::new_v4().simple().to_string();
        let key_id = Uuid::new_v4();
        let intent_id = Uuid::new_v4();
        let request_id = Uuid::new_v4().simple().to_string();
        let scope = BudgetAdminScope {
            workspace_id,
            default_workspace_id: workspace_id,
        };
        let store = PgBudgetStore::new(
            pool.clone(),
            PgBudgetStoreConfig {
                deployment_namespace: Arc::from(format!("budget-admin-test-{suffix}")),
                checkpoint_hard_tail_events: 1_000,
                config_fingerprint: Arc::from("a".repeat(64)),
                statement_timeout: Duration::from_secs(3),
                lock_timeout: Duration::from_secs(1),
            },
        )
        .unwrap();

        let created = store
            .create_account(CreateBudgetAccount {
                virtual_key_id: key_id,
                name: format!("budget-admin-{suffix}"),
                key_hash: format!("budget-admin-hash-{suffix}"),
                key_prefix: "sk-admin".to_string(),
                consumer_id: None,
                allowed_models: Some(vec!["gpt-test".to_string()]),
                tpm_limit: Some(1_000),
                rpm_limit: Some(10),
                budget_limit_usd: Some(Decimal::new(100, 0)),
                enabled: true,
                expires_at: None,
                tags: Some(vec!["budget-admin-test".to_string()]),
                workspace_id,
            })
            .await
            .unwrap();
        assert_eq!(created.used_usd, Decimal::ZERO);
        assert_eq!(created.accounting_revision, 0);

        let renamed_key = format!("budget-admin-renamed-{suffix}");
        let mixed_update = store
            .update_account(UpdateBudgetAccount {
                virtual_key_id: key_id,
                scope,
                name: Some(renamed_key.clone()),
                consumer_id: BudgetOptionalMutation::Unchanged,
                allowed_models: BudgetOptionalMutation::Set(vec![
                    "gpt-test".to_string(),
                    "gpt-next".to_string(),
                ]),
                tpm_limit: BudgetOptionalMutation::Clear,
                rpm_limit: BudgetOptionalMutation::Set(20),
                budget_limit: BudgetLimitMutation::Set(Decimal::new(120, 0)),
                enabled: Some(false),
                expires_at: BudgetOptionalMutation::Unchanged,
                tags: BudgetOptionalMutation::Clear,
            })
            .await
            .unwrap();
        assert_eq!(mixed_update.accounting_revision, 1);
        assert_eq!(mixed_update.limit_usd, Some(Decimal::new(120, 0)));
        let persisted: (
            String,
            Option<i32>,
            Option<i32>,
            Option<Decimal>,
            bool,
            Option<Vec<String>>,
        ) = sqlx::query_as(
            "SELECT name, tpm_limit, rpm_limit, budget_limit, enabled, tags \
               FROM ai_virtual_keys WHERE id = $1",
        )
        .bind(key_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(persisted.0, renamed_key);
        assert_eq!(persisted.1, None);
        assert_eq!(persisted.2, Some(20));
        assert_eq!(persisted.3, Some(Decimal::new(120, 0)));
        assert!(!persisted.4);
        assert_eq!(persisted.5, None);

        sqlx::query(
            "UPDATE ai_virtual_keys \
                SET budget_unresolved_count = 1, \
                    budget_accounting_revision = budget_accounting_revision + 1, \
                    budget_checkpoint_tail_events = 1 \
              WHERE id = $1",
        )
        .bind(key_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO ai_budget_ledger (\
                 id, virtual_key_id, virtual_key_name, virtual_key_prefix, workspace_id, \
                 kind, status, request_id, operation_id, command_fingerprint, \
                 last_account_revision, cost_status, cost_reasons, pricing_fingerprint\
             ) VALUES (\
                 $1, $2, $3, 'sk-admin', $4, 'request', 'unresolved', $5, $6, $7, \
                 2, 'unavailable', ARRAY['missing_usage'], $8\
             )",
        )
        .bind(intent_id)
        .bind(key_id)
        .bind(format!("budget-admin-renamed-{suffix}"))
        .bind(workspace_id)
        .bind(&request_id)
        .bind(format!("intent:v1:{request_id}"))
        .bind("b".repeat(64))
        .bind("c".repeat(64))
        .execute(&pool)
        .await
        .unwrap();

        let unresolved_page = store
            .list_ledger(BudgetLedgerQuery {
                virtual_key_id: key_id,
                scope,
                statuses: vec![BudgetLedgerStatus::Unresolved],
                created_at_from: None,
                created_at_to: None,
                after: None,
                page_size: 20,
            })
            .await
            .unwrap();
        assert_eq!(unresolved_page.entries.len(), 1);
        assert_eq!(unresolved_page.account.unwrap().unresolved_count, 1);
        assert_eq!(
            store
                .update_limit(UpdateBudgetLimit {
                    virtual_key_id: key_id,
                    scope,
                    mutation: BudgetLimitMutation::Clear,
                })
                .await
                .unwrap_err()
                .kind(),
            BudgetErrorKind::ReconciliationRequired
        );
        assert_eq!(
            store
                .delete_account(DeleteBudgetAccount {
                    virtual_key_id: key_id,
                    scope,
                })
                .await
                .unwrap_err()
                .kind(),
            BudgetErrorKind::ReconciliationRequired
        );

        let reconciliation_operation = Uuid::new_v4();
        let reconciliation_command = ReconcileBudgetIntent {
            virtual_key_id: key_id,
            intent_id,
            operation_id: reconciliation_operation,
            scope,
            actor: Arc::from("system/node:test"),
            reason: "provider invoice reviewed".to_string(),
            resolution: BudgetReconciliationResolution::Settle {
                cost_usd: Decimal::new(125, 2),
            },
        };
        let reconciled = store
            .reconcile(reconciliation_command.clone())
            .await
            .unwrap();
        assert_eq!(
            reconciled.disposition,
            BudgetReconciliationDisposition::Applied
        );
        assert_eq!(reconciled.account.used_usd, Decimal::new(125, 2));
        assert_eq!(reconciled.account.unresolved_count, 0);
        assert_eq!(
            store
                .reconcile(reconciliation_command.clone())
                .await
                .unwrap()
                .disposition,
            BudgetReconciliationDisposition::Replayed
        );
        let mut conflicting = reconciliation_command;
        conflicting.resolution = BudgetReconciliationResolution::Waive;
        assert_eq!(
            store.reconcile(conflicting).await.unwrap_err().kind(),
            BudgetErrorKind::Conflict
        );

        let waive_intent_id = Uuid::new_v4();
        let waive_request_id = Uuid::new_v4().simple().to_string();
        sqlx::query(
            "UPDATE ai_virtual_keys \
                SET budget_unresolved_count = 1, \
                    budget_accounting_revision = budget_accounting_revision + 1, \
                    budget_checkpoint_tail_events = budget_checkpoint_tail_events + 1 \
              WHERE id = $1",
        )
        .bind(key_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO ai_budget_ledger (\
                 id, virtual_key_id, virtual_key_name, virtual_key_prefix, workspace_id, \
                 kind, status, request_id, operation_id, command_fingerprint, \
                 last_account_revision, cost_status, cost_reasons, pricing_fingerprint\
             ) SELECT \
                 $1, id, name, key_prefix, ws_id, 'request', 'unresolved', \
                 $2, $3, $4, budget_accounting_revision, \
                 'unavailable', ARRAY['invoice_unknown'], $5 \
               FROM ai_virtual_keys WHERE id = $6",
        )
        .bind(waive_intent_id)
        .bind(&waive_request_id)
        .bind(format!("intent:v1:{waive_request_id}"))
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .bind(key_id)
        .execute(&pool)
        .await
        .unwrap();
        let waived = store
            .reconcile(ReconcileBudgetIntent {
                virtual_key_id: key_id,
                intent_id: waive_intent_id,
                operation_id: Uuid::new_v4(),
                scope,
                actor: Arc::from("system/node:test"),
                reason: "operator approved waiver".to_string(),
                resolution: BudgetReconciliationResolution::Waive,
            })
            .await
            .unwrap();
        assert_eq!(waived.audit_entry.status, BudgetLedgerStatus::Waived);
        assert_eq!(waived.account.used_usd, Decimal::new(125, 2));

        let verified = store
            .verify_or_rebuild(RebuildBudgetAccount {
                virtual_key_id: key_id,
                operation_id: Uuid::new_v4(),
                scope,
                actor: Arc::from("system/node:test"),
                reason: "verify after reconciliation".to_string(),
                dry_run: true,
                max_attempts: 3,
            })
            .await
            .unwrap();
        assert_eq!(verified.disposition, BudgetRebuildDisposition::Verified);
        assert_eq!(
            verified.comparison.recomputed_used_usd,
            Decimal::new(125, 2)
        );

        sqlx::query(
            "UPDATE ai_virtual_keys \
                SET budget_used = 9.000000000000, \
                    budget_accounting_revision = budget_accounting_revision + 1 \
              WHERE id = $1",
        )
        .bind(key_id)
        .execute(&pool)
        .await
        .unwrap();
        let rebuild_operation = Uuid::new_v4();
        let rebuild_command = RebuildBudgetAccount {
            virtual_key_id: key_id,
            operation_id: rebuild_operation,
            scope,
            actor: Arc::from("system/node:test"),
            reason: "repair deliberate aggregate drift".to_string(),
            dry_run: false,
            max_attempts: 3,
        };
        let rebuilt = store
            .verify_or_rebuild(rebuild_command.clone())
            .await
            .unwrap();
        assert_eq!(rebuilt.disposition, BudgetRebuildDisposition::Rebuilt);
        assert_eq!(rebuilt.account.used_usd, Decimal::new(125, 2));
        assert_eq!(rebuilt.account.checkpoint_tail_events, 0);
        assert_eq!(
            store
                .verify_or_rebuild(rebuild_command)
                .await
                .unwrap()
                .disposition,
            BudgetRebuildDisposition::Replayed
        );

        let cleared = store
            .update_limit(UpdateBudgetLimit {
                virtual_key_id: key_id,
                scope,
                mutation: BudgetLimitMutation::Clear,
            })
            .await
            .unwrap();
        assert_eq!(cleared.limit_usd, None);
        store
            .delete_account(DeleteBudgetAccount {
                virtual_key_id: key_id,
                scope,
            })
            .await
            .unwrap();

        let first_history_page = store
            .list_ledger(BudgetLedgerQuery {
                virtual_key_id: key_id,
                scope,
                statuses: Vec::new(),
                created_at_from: None,
                created_at_to: None,
                after: None,
                page_size: 1,
            })
            .await
            .unwrap();
        assert!(first_history_page.account.is_none());
        assert_eq!(first_history_page.entries.len(), 1);
        let second_history_page = store
            .list_ledger(BudgetLedgerQuery {
                virtual_key_id: key_id,
                scope,
                statuses: Vec::new(),
                created_at_from: None,
                created_at_to: None,
                after: first_history_page.next_cursor.clone(),
                page_size: 20,
            })
            .await
            .unwrap();
        assert!(!second_history_page.entries.is_empty());
        assert_ne!(
            first_history_page.entries[0].id,
            second_history_page.entries[0].id
        );

        sqlx::query("DELETE FROM ai_budget_ledger WHERE virtual_key_id = $1")
            .bind(key_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ai_budget_checkpoints WHERE virtual_key_id = $1")
            .bind(key_id)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn postgres_reconciliation_overflow_is_audited_and_replayable() {
        let Some(database_url) = crate::budget::postgres_test_url() else {
            return;
        };
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .connect(&database_url)
            .await
            .unwrap();
        let workspace_id: Uuid =
            sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'default' LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        let scope = BudgetAdminScope {
            workspace_id,
            default_workspace_id: workspace_id,
        };
        let suffix = Uuid::new_v4().simple().to_string();
        let key_id = Uuid::new_v4();
        let intent_id = Uuid::new_v4();
        let request_id = Uuid::new_v4().simple().to_string();
        let store = PgBudgetStore::new(
            pool.clone(),
            PgBudgetStoreConfig {
                deployment_namespace: Arc::from(format!("budget-overflow-test-{suffix}")),
                checkpoint_hard_tail_events: 1_000,
                config_fingerprint: Arc::from("f".repeat(64)),
                statement_timeout: Duration::from_secs(3),
                lock_timeout: Duration::from_secs(1),
            },
        )
        .unwrap();
        store
            .create_account(CreateBudgetAccount {
                virtual_key_id: key_id,
                name: format!("budget-overflow-{suffix}"),
                key_hash: format!("budget-overflow-hash-{suffix}"),
                key_prefix: "sk-over".to_string(),
                consumer_id: None,
                allowed_models: None,
                tpm_limit: None,
                rpm_limit: None,
                budget_limit_usd: Some(Decimal::from_str("9999999999999999.999999999999").unwrap()),
                enabled: true,
                expires_at: None,
                tags: None,
                workspace_id,
            })
            .await
            .unwrap();
        let existing_used = Decimal::from_str("9999999999999999.500000000000").unwrap();
        sqlx::query(
            "UPDATE ai_virtual_keys \
                SET budget_used = $2, budget_unresolved_count = 1, \
                    budget_accounting_revision = 1, budget_checkpoint_tail_events = 1 \
              WHERE id = $1",
        )
        .bind(key_id)
        .bind(existing_used)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO ai_budget_ledger (\
                 id, virtual_key_id, virtual_key_name, virtual_key_prefix, workspace_id, \
                 kind, status, request_id, operation_id, command_fingerprint, \
                 last_account_revision, cost_status, cost_reasons, pricing_fingerprint\
             ) SELECT \
                 $1, id, name, key_prefix, ws_id, 'request', 'unresolved', \
                 $2, $3, $4, 1, 'unavailable', ARRAY['manual_review'], $5 \
               FROM ai_virtual_keys WHERE id = $6",
        )
        .bind(intent_id)
        .bind(&request_id)
        .bind(format!("intent:v1:{request_id}"))
        .bind("1".repeat(64))
        .bind("2".repeat(64))
        .bind(key_id)
        .execute(&pool)
        .await
        .unwrap();

        let operation_id = Uuid::new_v4();
        let overflow_command = ReconcileBudgetIntent {
            virtual_key_id: key_id,
            intent_id,
            operation_id,
            scope,
            actor: Arc::from("system/node:test"),
            reason: "invoice amount exceeds account range".to_string(),
            resolution: BudgetReconciliationResolution::Settle {
                cost_usd: Decimal::ONE,
            },
        };
        let overflow = store.reconcile(overflow_command.clone()).await.unwrap();
        assert_eq!(
            overflow.disposition,
            BudgetReconciliationDisposition::NumericOverflow
        );
        assert_eq!(overflow.account.used_usd, existing_used);
        assert_eq!(overflow.account.unresolved_count, 1);
        assert_eq!(
            overflow.audit_entry.kind,
            BudgetLedgerKind::ReconciliationAttempt
        );
        assert_eq!(
            store.reconcile(overflow_command).await.unwrap().disposition,
            BudgetReconciliationDisposition::NumericOverflowReplayed
        );

        let waived = store
            .reconcile(ReconcileBudgetIntent {
                virtual_key_id: key_id,
                intent_id,
                operation_id: Uuid::new_v4(),
                scope,
                actor: Arc::from("system/node:test"),
                reason: "waive overflowed invoice after review".to_string(),
                resolution: BudgetReconciliationResolution::Waive,
            })
            .await
            .unwrap();
        assert_eq!(waived.account.used_usd, existing_used);
        assert_eq!(waived.account.unresolved_count, 0);
        store
            .delete_account(DeleteBudgetAccount {
                virtual_key_id: key_id,
                scope,
            })
            .await
            .unwrap();
        sqlx::query("DELETE FROM ai_budget_ledger WHERE virtual_key_id = $1")
            .bind(key_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ai_budget_checkpoints WHERE virtual_key_id = $1")
            .bind(key_id)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn postgres_verify_records_one_account_issue_for_broken_checkpoint() {
        let Some(database_url) = crate::budget::postgres_test_url() else {
            return;
        };
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .connect(&database_url)
            .await
            .unwrap();
        let workspace_id: Uuid =
            sqlx::query_scalar("SELECT id FROM workspaces WHERE name = 'default' LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        let scope = BudgetAdminScope {
            workspace_id,
            default_workspace_id: workspace_id,
        };
        let suffix = Uuid::new_v4().simple().to_string();
        let key_id = Uuid::new_v4();
        let store = PgBudgetStore::new(
            pool.clone(),
            PgBudgetStoreConfig {
                deployment_namespace: Arc::from(format!("budget-issue-test-{suffix}")),
                checkpoint_hard_tail_events: 1_000,
                config_fingerprint: Arc::from("9".repeat(64)),
                statement_timeout: Duration::from_secs(3),
                lock_timeout: Duration::from_secs(1),
            },
        )
        .unwrap();
        store
            .create_account(CreateBudgetAccount {
                virtual_key_id: key_id,
                name: format!("budget-issue-{suffix}"),
                key_hash: format!("budget-issue-hash-{suffix}"),
                key_prefix: "sk-issue".to_string(),
                consumer_id: None,
                allowed_models: None,
                tpm_limit: None,
                rpm_limit: None,
                budget_limit_usd: Some(Decimal::TEN),
                enabled: true,
                expires_at: None,
                tags: None,
                workspace_id,
            })
            .await
            .unwrap();
        sqlx::query("DELETE FROM ai_budget_checkpoints WHERE virtual_key_id = $1")
            .bind(key_id)
            .execute(&pool)
            .await
            .unwrap();
        let verify = || RebuildBudgetAccount {
            virtual_key_id: key_id,
            operation_id: Uuid::new_v4(),
            scope,
            actor: Arc::from("system/node:test"),
            reason: "verify broken checkpoint".to_string(),
            dry_run: true,
            max_attempts: 2,
        };
        assert_eq!(
            store.verify_or_rebuild(verify()).await.unwrap_err().kind(),
            BudgetErrorKind::Corrupt
        );
        assert_eq!(
            store.verify_or_rebuild(verify()).await.unwrap_err().kind(),
            BudgetErrorKind::Corrupt
        );
        let (unresolved_count, issue_count): (i64, i64) = sqlx::query_as(
            "SELECT key.budget_unresolved_count, COUNT(ledger.id)::bigint \
               FROM ai_virtual_keys AS key \
               LEFT JOIN ai_budget_ledger AS ledger \
                 ON ledger.virtual_key_id = key.id \
                AND ledger.kind = 'account_issue' \
                AND ledger.status = 'unresolved' \
              WHERE key.id = $1 \
              GROUP BY key.budget_unresolved_count",
        )
        .bind(key_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(unresolved_count, 1);
        assert_eq!(issue_count, 1);

        sqlx::query("DELETE FROM ai_budget_ledger WHERE virtual_key_id = $1")
            .bind(key_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE ai_virtual_keys SET budget_unresolved_count = 0 WHERE id = $1")
            .bind(key_id)
            .execute(&pool)
            .await
            .unwrap();
        store
            .delete_account(DeleteBudgetAccount {
                virtual_key_id: key_id,
                scope,
            })
            .await
            .unwrap();
    }
}

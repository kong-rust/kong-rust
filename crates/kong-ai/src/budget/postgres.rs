//! PostgreSQL 强一致预算 Store。

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::models::normalize_budget_amount;
use crate::usage::model::CostStatus;

use super::model::{
    BudgetAccountSnapshot, BudgetAccountingState, BudgetBackendDescriptor, BudgetBackendKind,
    BudgetCheckpoint, BudgetCostOutcome, BudgetEligibility, BudgetErrorKind, BudgetInspectCommand,
    BudgetInspection, BudgetIntent, BudgetIntentDisposition, BudgetIntentRecord, BudgetIntentState,
    BudgetOwnerLease, BudgetRecoveryBatch, BudgetSettlement, BudgetSettlementDisposition,
    BudgetStoreError, CheckpointBudgetAccount, CreateBudgetIntent, HeartbeatBudgetOwner,
    LookupBudgetIntent, MarkBudgetDispatching, RecoverStaleBudgetIntents, RegisterBudgetOwner,
    SettleBudgetIntent, StopBudgetOwner, BUDGET_SCHEMA_VERSION,
};
use super::store::BudgetStore;

const ACCOUNT_COLUMNS: &str = "\
id, name, key_prefix, ws_id, budget_limit, budget_used, \
budget_pending_count, budget_unresolved_count, budget_accounting_revision, \
budget_checkpoint_tail_events, budget_accounting_state, budget_state_updated_at";

const INTENT_COLUMNS: &str = "\
id, virtual_key_id, request_id, status, operation_id, command_fingerprint, \
dispatch_operation_id, terminal_operation_id, terminal_command_fingerprint, \
last_account_revision, usage_fact_id, observed_cost_usd, accounted_cost_usd, \
cost_status, cost_reasons, pricing_fingerprint, dispatch_state, node_id, \
owner_session_id, stale_not_before, resolution_entry_id, created_at, updated_at";

fn max_budget_amount() -> Decimal {
    Decimal::from_i128_with_scale(9_999_999_999_999_999_999_999_999_999i128, 12)
}

/// PostgreSQL budget adapter 配置。连接池由 server 作为独立、限额 pool 注入。
#[derive(Clone, Debug)]
pub struct PgBudgetStoreConfig {
    pub deployment_namespace: Arc<str>,
    pub checkpoint_hard_tail_events: i64,
    pub config_fingerprint: Arc<str>,
    pub statement_timeout: Duration,
    pub lock_timeout: Duration,
}

impl PgBudgetStoreConfig {
    pub fn validate(&self) -> Result<(), BudgetStoreError> {
        if self.deployment_namespace.trim().is_empty() {
            return Err(BudgetStoreError::corrupt("deployment namespace 不能为空"));
        }
        if self.checkpoint_hard_tail_events <= 0 {
            return Err(BudgetStoreError::corrupt("checkpoint hard tail 必须为正数"));
        }
        validate_fingerprint(&self.config_fingerprint, "config fingerprint")?;
        validate_positive_duration(self.statement_timeout, "statement timeout")?;
        validate_positive_duration(self.lock_timeout, "lock timeout")?;
        if self.lock_timeout > self.statement_timeout {
            return Err(BudgetStoreError::corrupt(
                "lock timeout 不能大于 statement timeout",
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct PgBudgetStore {
    pool: PgPool,
    config: PgBudgetStoreConfig,
}

/// 单次 PG checkpoint 调度统计；runner 可据此上报背压与失败指标。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PgBudgetCheckpointBatch {
    pub scanned: u32,
    pub checkpointed: u32,
    pub skipped_below_soft_tail: u32,
    pub failed: u32,
}

impl PgBudgetStore {
    pub fn new(pool: PgPool, config: PgBudgetStoreConfig) -> Result<Self, BudgetStoreError> {
        config.validate()?;
        Ok(Self { pool, config })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 有界选择超过 soft tail 的账户并推进 checkpoint。
    ///
    /// 该 API 仅属于 PG adapter，不把 SQL/游标泄漏到通用 `BudgetStore` 契约。
    pub async fn checkpoint_due_accounts(
        &self,
        soft_tail_events: i64,
        max_accounts: u32,
    ) -> Result<PgBudgetCheckpointBatch, BudgetStoreError> {
        if soft_tail_events <= 0 || soft_tail_events >= self.config.checkpoint_hard_tail_events {
            return Err(BudgetStoreError::corrupt(
                "checkpoint soft tail 必须大于 0 且小于 hard tail",
            ));
        }
        if max_accounts == 0 || max_accounts > 10_000 {
            return Err(BudgetStoreError::corrupt(
                "checkpoint batch 必须在 1..=10000",
            ));
        }

        let mut transaction = self.begin_write().await?;
        self.load_authoritative_hard_tail(&mut transaction).await?;
        let candidates: Vec<DueCheckpointCandidate> = sqlx::query_as(
            "SELECT id AS virtual_key_id, \
                    budget_accounting_revision AS accounting_revision \
               FROM ai_virtual_keys \
              WHERE budget_checkpoint_tail_events >= $1 \
              ORDER BY budget_checkpoint_tail_events DESC, id \
              LIMIT $2",
        )
        .bind(soft_tail_events)
        .bind(i64::from(max_accounts))
        .fetch_all(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
        commit(transaction).await?;

        let mut batch = PgBudgetCheckpointBatch {
            scanned: u32::try_from(candidates.len())
                .map_err(|_| BudgetStoreError::corrupt("checkpoint 候选数溢出"))?,
            ..PgBudgetCheckpointBatch::default()
        };
        for candidate in candidates {
            match self
                .checkpoint_due_candidate(candidate, soft_tail_events)
                .await
            {
                Ok(true) => batch.checkpointed = batch.checkpointed.saturating_add(1),
                Ok(false) => {
                    batch.skipped_below_soft_tail = batch.skipped_below_soft_tail.saturating_add(1);
                }
                Err(error) => {
                    batch.failed = batch.failed.saturating_add(1);
                    tracing::warn!(
                        virtual_key_id = %candidate.virtual_key_id,
                        error_kind = ?error.kind(),
                        "预算 checkpoint 推进失败"
                    );
                }
            }
        }
        Ok(batch)
    }

    async fn checkpoint_due_candidate(
        &self,
        candidate: DueCheckpointCandidate,
        soft_tail_events: i64,
    ) -> Result<bool, BudgetStoreError> {
        let mut revision = candidate.accounting_revision;
        // 高并发 key 在候选扫描后可能推进 revision；重读并重试一次，后续交给下一轮。
        for attempt in 0..=1 {
            let command = CheckpointBudgetAccount {
                virtual_key_id: candidate.virtual_key_id,
                operation_id: Arc::from(format!(
                    "budget-checkpoint:v1:{}:{revision}",
                    candidate.virtual_key_id
                )),
            };
            match self.checkpoint_account(command).await {
                Ok(_) => return Ok(true),
                Err(error) if attempt == 0 && error.kind() == BudgetErrorKind::Corrupt => {
                    let mut transaction = self.begin_write().await?;
                    self.load_authoritative_hard_tail(&mut transaction).await?;
                    let state: Option<(i64, i64)> = sqlx::query_as(
                        "SELECT budget_accounting_revision, budget_checkpoint_tail_events \
                           FROM ai_virtual_keys WHERE id = $1",
                    )
                    .bind(candidate.virtual_key_id)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(map_sqlx_error)?;
                    commit(transaction).await?;
                    let Some((current_revision, current_tail)) = state else {
                        return Err(error);
                    };
                    if current_tail < soft_tail_events {
                        return Ok(false);
                    }
                    if current_revision == revision {
                        return Err(error);
                    }
                    revision = current_revision;
                }
                Err(error) => return Err(error),
            }
        }
        Err(BudgetStoreError::unavailable(
            "checkpoint revision 持续变化，留待下一轮重试",
        ))
    }

    pub(crate) async fn begin_write(&self) -> Result<Transaction<'_, Postgres>, BudgetStoreError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx_error)?;
        let statement_timeout = duration_as_pg_millis(self.config.statement_timeout)?;
        let lock_timeout = duration_as_pg_millis(self.config.lock_timeout)?;
        sqlx::query("SELECT set_config('statement_timeout', $1, true)")
            .bind(statement_timeout)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        sqlx::query("SELECT set_config('lock_timeout', $1, true)")
            .bind(lock_timeout)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        Ok(transaction)
    }

    pub(crate) async fn begin_repeatable_read(
        &self,
    ) -> Result<Transaction<'_, Postgres>, BudgetStoreError> {
        let mut transaction = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        let statement_timeout = duration_as_pg_millis(self.config.statement_timeout)?;
        let lock_timeout = duration_as_pg_millis(self.config.lock_timeout)?;
        sqlx::query("SELECT set_config('statement_timeout', $1, true)")
            .bind(statement_timeout)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        sqlx::query("SELECT set_config('lock_timeout', $1, true)")
            .bind(lock_timeout)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        Ok(transaction)
    }

    async fn load_authoritative_hard_tail(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
    ) -> Result<i64, BudgetStoreError> {
        let row: Option<RuntimeSettingsRow> = sqlx::query_as(
            "SELECT checkpoint_hard_tail_events, config_fingerprint \
               FROM ai_budget_runtime_settings \
              WHERE deployment_namespace = $1",
        )
        .bind(self.config.deployment_namespace.as_ref())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx_error)?;
        let row = row.ok_or_else(|| {
            BudgetStoreError::unavailable("预算 owner 尚未注册权威 runtime settings")
        })?;
        if row.checkpoint_hard_tail_events != self.config.checkpoint_hard_tail_events
            || row.config_fingerprint.trim() != self.config.config_fingerprint.as_ref()
        {
            return Err(BudgetStoreError::unavailable(
                "本节点预算配置与 deployment 权威配置不一致",
            ));
        }
        Ok(row.checkpoint_hard_tail_events)
    }

    async fn owner_is_live(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        session_id: Uuid,
        node_id: Uuid,
    ) -> Result<bool, BudgetStoreError> {
        let live: Option<Uuid> = sqlx::query_scalar(
            "SELECT session_id FROM ai_budget_owner_sessions \
              WHERE session_id = $1 \
                AND deployment_namespace = $2 \
                AND node_id = $3 \
                AND stopped_at IS NULL \
                AND expires_at > clock_timestamp() \
              FOR UPDATE",
        )
        .bind(session_id)
        .bind(self.config.deployment_namespace.as_ref())
        .bind(node_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx_error)?;
        Ok(live.is_some())
    }

    fn inspect_account(
        &self,
        row: AccountRow,
        hard_tail: i64,
    ) -> Result<BudgetInspection, BudgetStoreError> {
        let account = row.into_snapshot()?;
        validate_account_invariants(&account)?;
        if account.unresolved_count > 0 {
            return Err(BudgetStoreError::new(
                BudgetErrorKind::AccountingUnresolved,
                "预算账户存在未解决账务",
            ));
        }
        if account.checkpoint_tail_events >= hard_tail {
            return Err(BudgetStoreError::unavailable(
                "预算账户 checkpoint tail 已达到权威上限",
            ));
        }
        let eligibility = match account.limit_usd {
            None => {
                if account.pending_count > 0 {
                    return Err(BudgetStoreError::new(
                        BudgetErrorKind::AccountingUnresolved,
                        "暂停预算的账户仍存在 pending intent",
                    ));
                }
                BudgetEligibility::Paused
            }
            Some(limit) if account.used_usd >= limit => BudgetEligibility::Exhausted,
            Some(_) => BudgetEligibility::Eligible,
        };
        Ok(BudgetInspection {
            eligibility,
            account,
        })
    }

    async fn recover_stale_candidate(
        &self,
        candidate: StaleIntentCandidate,
    ) -> Result<RecoveredStaleIntent, BudgetStoreError> {
        let mut transaction = self.begin_write().await?;
        self.load_authoritative_hard_tail(&mut transaction).await?;

        // recovery 也固定遵循 key -> intent -> owner 的锁序。
        let account_sql =
            format!("SELECT {ACCOUNT_COLUMNS} FROM ai_virtual_keys WHERE id = $1 FOR UPDATE");
        let account_row: Option<AccountRow> = sqlx::query_as(&account_sql)
            .bind(candidate.virtual_key_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        let account = account_row
            .ok_or_else(|| BudgetStoreError::corrupt("stale intent 的预算账户不存在"))?
            .into_snapshot()?;
        validate_account_invariants(&account)?;

        let Some(existing) = lock_intent_by_id(&mut transaction, candidate.intent_id).await? else {
            commit(transaction).await?;
            return Ok(RecoveredStaleIntent::Skipped);
        };
        if existing.virtual_key_id != candidate.virtual_key_id || existing.status != "pending" {
            commit(transaction).await?;
            return Ok(RecoveredStaleIntent::Skipped);
        }
        if existing.stale_not_before.is_none() {
            return Err(BudgetStoreError::corrupt(
                "stale pending intent 缺少 stale_not_before",
            ));
        }
        let stale_now: bool = sqlx::query_scalar(
            "SELECT stale_not_before IS NOT NULL \
                    AND stale_not_before <= clock_timestamp() \
               FROM ai_budget_ledger WHERE id = $1",
        )
        .bind(existing.id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
        if !stale_now {
            commit(transaction).await?;
            return Ok(RecoveredStaleIntent::Skipped);
        }

        let owner_session_id = existing
            .owner_session_id
            .ok_or_else(|| BudgetStoreError::corrupt("stale intent 缺少 owner session"))?;
        let owner = lock_owner(&mut transaction, owner_session_id)
            .await?
            .ok_or_else(|| BudgetStoreError::corrupt("stale intent 的 owner session 不存在"))?;
        if owner.deployment_namespace != self.config.deployment_namespace.as_ref() || owner.live {
            commit(transaction).await?;
            return Ok(RecoveredStaleIntent::Skipped);
        }

        match existing.dispatch_state.as_deref() {
            Some("prepared") => {
                if account.pending_count <= 0 {
                    return Err(BudgetStoreError::corrupt(
                        "prepared stale intent 与账户 pending count 不一致",
                    ));
                }
                let new_revision = checked_next_revision(&account)?;
                let request_id = existing.request_id.as_deref().ok_or_else(|| {
                    BudgetStoreError::corrupt("prepared stale intent 缺少 request ID")
                })?;
                let reasons = vec!["stale_owner_before_dispatch".to_string()];
                let operation_id = format!("budget-settle:v1:{request_id}");
                let fingerprint = settlement_fingerprint(
                    request_id,
                    existing.virtual_key_id,
                    existing.id,
                    CostStatus::NotIncurred,
                    Some(Decimal::ZERO),
                    &reasons,
                    None,
                );

                let update_account_sql = format!(
                    "UPDATE ai_virtual_keys \
                        SET budget_pending_count = budget_pending_count - 1, \
                            budget_accounting_revision = $2, \
                            budget_checkpoint_tail_events = budget_checkpoint_tail_events + 1, \
                            budget_state_updated_at = clock_timestamp() \
                      WHERE id = $1 \
                        AND budget_pending_count > 0 \
                        AND budget_accounting_revision = $3 \
                  RETURNING {ACCOUNT_COLUMNS}"
                );
                let updated_account: Option<AccountRow> = sqlx::query_as(&update_account_sql)
                    .bind(account.virtual_key_id)
                    .bind(new_revision)
                    .bind(account.accounting_revision)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(map_sqlx_error)?;
                updated_account.ok_or_else(|| {
                    BudgetStoreError::corrupt("prepared stale recovery 的账户 CAS 失败")
                })?;

                let update_intent_sql = format!(
                    "UPDATE ai_budget_ledger \
                        SET status = 'settled', \
                            observed_cost_usd = 0.000000000000, \
                            accounted_cost_usd = 0.000000000000, \
                            cost_status = 'not_incurred', \
                            cost_reasons = $2, \
                            terminal_operation_id = $3, \
                            terminal_command_fingerprint = $4, \
                            last_account_revision = $5, \
                            updated_at = clock_timestamp(), \
                            settled_at = clock_timestamp() \
                      WHERE id = $1 \
                        AND status = 'pending' \
                        AND dispatch_state = 'prepared' \
                  RETURNING {INTENT_COLUMNS}"
                );
                let updated: Option<IntentRow> = sqlx::query_as(&update_intent_sql)
                    .bind(existing.id)
                    .bind(&reasons)
                    .bind(&operation_id)
                    .bind(&fingerprint)
                    .bind(new_revision)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(map_sqlx_error)?;
                updated.ok_or_else(|| {
                    BudgetStoreError::corrupt("prepared stale recovery 的 intent CAS 失败")
                })?;
                commit(transaction).await?;
                Ok(RecoveredStaleIntent::SettledNotIncurred)
            }
            Some("dispatching") => {
                let reasons = vec!["stale_owner_after_dispatch".to_string()];
                mark_unresolved(
                    &mut transaction,
                    &account,
                    &existing,
                    None,
                    &reasons,
                    None,
                    None,
                )
                .await?;
                commit(transaction).await?;
                Ok(RecoveredStaleIntent::MarkedUnresolved)
            }
            _ => Err(BudgetStoreError::corrupt(
                "stale pending intent 的 dispatch state 非法",
            )),
        }
    }
}

#[async_trait]
impl BudgetStore for PgBudgetStore {
    fn descriptor(&self) -> BudgetBackendDescriptor {
        BudgetBackendDescriptor {
            kind: BudgetBackendKind::Postgres,
            authoritative: true,
            deployment_namespace: Arc::clone(&self.config.deployment_namespace),
        }
    }

    async fn inspect(
        &self,
        command: BudgetInspectCommand,
    ) -> Result<BudgetInspection, BudgetStoreError> {
        let mut transaction = self.begin_write().await?;
        let hard_tail = self.load_authoritative_hard_tail(&mut transaction).await?;
        let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM ai_virtual_keys WHERE id = $1");
        let row: Option<AccountRow> = sqlx::query_as(&sql)
            .bind(command.virtual_key_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        let row = row.ok_or_else(|| BudgetStoreError::corrupt("预算账户不存在"))?;
        let inspection = self.inspect_account(row, hard_tail)?;
        commit(transaction).await?;
        Ok(inspection)
    }

    async fn create_intent(
        &self,
        command: CreateBudgetIntent,
    ) -> Result<BudgetIntent, BudgetStoreError> {
        validate_create_command(&command)?;
        let pricing_snapshot = serde_json::to_value(&command.pricing_snapshot).map_err(|_| {
            BudgetStoreError::new(
                BudgetErrorKind::PricingUnavailable,
                "pricing snapshot 无法序列化",
            )
        })?;
        if serde_json::to_vec(&pricing_snapshot)
            .map_err(|_| BudgetStoreError::corrupt("pricing snapshot 无法编码"))?
            .len()
            > 4096
        {
            return Err(BudgetStoreError::new(
                BudgetErrorKind::PricingUnavailable,
                "pricing snapshot 超过 4 KiB",
            ));
        }

        let mut transaction = self.begin_write().await?;

        // 所有会同时触碰 key 与 ledger 的写事务固定先锁 key。
        let account_sql =
            format!("SELECT {ACCOUNT_COLUMNS} FROM ai_virtual_keys WHERE id = $1 FOR UPDATE");
        let account_row: Option<AccountRow> = sqlx::query_as(&account_sql)
            .bind(command.virtual_key_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        let account_row = account_row.ok_or_else(|| BudgetStoreError::corrupt("预算账户不存在"))?;
        let account = account_row.clone().into_snapshot()?;
        validate_account_invariants(&account)?;

        // ACK 丢失后的重放优先返回原 intent，不受账户后来 exhausted/unresolved 影响。
        if let Some(existing) =
            lock_intent_by_request(&mut transaction, &command.request_id).await?
        {
            verify_create_replay(&existing, &command)?;
            let record = existing.into_record()?;
            commit(transaction).await?;
            return Ok(BudgetIntent {
                disposition: BudgetIntentDisposition::CreateReplayed,
                record: Some(record),
                account: Some(account),
            });
        }

        let hard_tail = self.load_authoritative_hard_tail(&mut transaction).await?;
        let inspection = self.inspect_account(account_row, hard_tail)?;
        match inspection.eligibility {
            BudgetEligibility::Paused => {
                commit(transaction).await?;
                return Ok(BudgetIntent {
                    disposition: BudgetIntentDisposition::Paused,
                    record: None,
                    account: Some(inspection.account),
                });
            }
            BudgetEligibility::Exhausted => {
                return Err(BudgetStoreError::new(
                    BudgetErrorKind::Exhausted,
                    "预算已耗尽",
                ));
            }
            BudgetEligibility::Eligible => {}
        }

        if !self
            .owner_is_live(&mut transaction, command.owner_session_id, command.node_id)
            .await?
        {
            return Err(BudgetStoreError::unavailable(
                "预算 owner session 已被 fence",
            ));
        }

        let new_revision = inspection
            .account
            .accounting_revision
            .checked_add(1)
            .ok_or_else(|| BudgetStoreError::corrupt("预算 revision 溢出"))?;
        inspection
            .account
            .pending_count
            .checked_add(1)
            .ok_or_else(|| BudgetStoreError::corrupt("预算 pending count 溢出"))?;
        inspection
            .account
            .checkpoint_tail_events
            .checked_add(1)
            .ok_or_else(|| BudgetStoreError::corrupt("预算 tail count 溢出"))?;
        let stale_seconds = i64::try_from(command.stale_after.as_secs())
            .map_err(|_| BudgetStoreError::corrupt("intent stale duration 过大"))?;

        let insert_sql = format!(
            "INSERT INTO ai_budget_ledger (\
                 id, virtual_key_id, virtual_key_name, virtual_key_prefix, workspace_id, \
                 kind, status, request_id, operation_id, command_fingerprint, \
                 last_account_revision, pricing_fingerprint, pricing_snapshot, dispatch_state, \
                 node_id, owner_session_id, stale_not_before\
             ) VALUES (\
                 $1, $2, $3, $4, $5, 'request', 'pending', $6, $7, $8, \
                 $9, $10, $11, 'prepared', $12, $13, \
                 clock_timestamp() + $14::bigint * interval '1 second'\
             ) \
             ON CONFLICT (request_id) DO NOTHING \
             RETURNING {INTENT_COLUMNS}"
        );
        let inserted: Option<IntentRow> = sqlx::query_as(&insert_sql)
            .bind(command.intent_id)
            .bind(command.virtual_key_id)
            .bind(&inspection.account.virtual_key_name)
            .bind(&inspection.account.virtual_key_prefix)
            .bind(inspection.account.workspace_id)
            .bind(command.request_id.as_ref())
            .bind(command.operation_id.as_ref())
            .bind(command.command_fingerprint.as_ref())
            .bind(new_revision)
            .bind(command.pricing_fingerprint.as_ref())
            .bind(pricing_snapshot)
            .bind(command.node_id)
            .bind(command.owner_session_id)
            .bind(stale_seconds)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;

        let Some(inserted) = inserted else {
            let existing = lock_intent_by_request(&mut transaction, &command.request_id)
                .await?
                .ok_or_else(|| {
                    BudgetStoreError::new(
                        BudgetErrorKind::OutcomeUnknown,
                        "intent conflict 后无法读取权威结果",
                    )
                })?;
            verify_create_replay(&existing, &command)?;
            let record = existing.into_record()?;
            commit(transaction).await?;
            return Ok(BudgetIntent {
                disposition: BudgetIntentDisposition::CreateReplayed,
                record: Some(record),
                account: Some(inspection.account),
            });
        };

        let update_sql = format!(
            "UPDATE ai_virtual_keys \
                SET budget_pending_count = budget_pending_count + 1, \
                    budget_accounting_revision = $2, \
                    budget_checkpoint_tail_events = budget_checkpoint_tail_events + 1, \
                    budget_state_updated_at = clock_timestamp() \
              WHERE id = $1 \
                AND budget_accounting_revision = $3 \
                AND budget_checkpoint_tail_events < $4 \
          RETURNING {ACCOUNT_COLUMNS}"
        );
        let updated_account: Option<AccountRow> = sqlx::query_as(&update_sql)
            .bind(command.virtual_key_id)
            .bind(new_revision)
            .bind(inspection.account.accounting_revision)
            .bind(hard_tail)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        let updated_account = updated_account
            .ok_or_else(|| BudgetStoreError::corrupt("intent 创建时账户 revision/tail 不一致"))?;
        let record = inserted.into_record()?;
        let account = updated_account.into_snapshot()?;
        commit(transaction).await?;
        Ok(BudgetIntent {
            disposition: BudgetIntentDisposition::Created,
            record: Some(record),
            account: Some(account),
        })
    }

    async fn lookup_intent(
        &self,
        command: LookupBudgetIntent,
    ) -> Result<Option<BudgetIntentRecord>, BudgetStoreError> {
        validate_lookup_command(&command)?;
        let mut transaction = self.begin_repeatable_read().await?;
        let sql = format!(
            "SELECT {INTENT_COLUMNS} FROM ai_budget_ledger \
              WHERE request_id = $1"
        );
        let row: Option<IntentRow> = sqlx::query_as(&sql)
            .bind(command.request_id.as_ref())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        let record = match row {
            Some(row) => {
                verify_lookup_identity(&row, &command)?;
                Some(row.into_record()?)
            }
            None => None,
        };
        commit(transaction).await?;
        Ok(record)
    }

    async fn mark_dispatching(
        &self,
        command: MarkBudgetDispatching,
    ) -> Result<BudgetIntent, BudgetStoreError> {
        validate_dispatch_command(&command)?;
        let mut transaction = self.begin_write().await?;

        // dispatch-only 是 migration/design 明确允许的 ledger-only 短事务；
        // 一旦锁住 ledger，本事务绝不反向获取 Virtual Key 锁。
        let existing = lock_intent_by_id(&mut transaction, command.intent_id)
            .await?
            .ok_or_else(|| BudgetStoreError::corrupt("预算 intent 不存在"))?;
        verify_intent_identity(
            &existing,
            command.virtual_key_id,
            &command.request_id,
            Some(command.owner_session_id),
            Some(command.node_id),
        )?;

        if existing.status != "pending" {
            return Err(BudgetStoreError::corrupt(
                "只有 pending intent 可以进入 dispatching",
            ));
        }
        if existing.dispatch_state.as_deref() == Some("dispatching") {
            if existing.dispatch_operation_id.as_deref() != Some(command.operation_id.as_ref()) {
                return Err(BudgetStoreError::corrupt(
                    "dispatch operation 与已提交结果冲突",
                ));
            }
            let record = existing.into_record()?;
            commit(transaction).await?;
            return Ok(BudgetIntent {
                disposition: BudgetIntentDisposition::DispatchReplayed,
                record: Some(record),
                account: None,
            });
        }
        if existing.dispatch_state.as_deref() != Some("prepared")
            || existing.dispatch_operation_id.is_some()
        {
            return Err(BudgetStoreError::corrupt(
                "prepared intent 的 dispatch 状态损坏",
            ));
        }
        if !self
            .owner_is_live(&mut transaction, command.owner_session_id, command.node_id)
            .await?
        {
            return Err(BudgetStoreError::unavailable(
                "预算 owner session 已被 fence",
            ));
        }

        let update_sql = format!(
            "UPDATE ai_budget_ledger \
                SET dispatch_state = 'dispatching', \
                    dispatch_operation_id = $2, \
                    updated_at = clock_timestamp() \
              WHERE id = $1 \
                AND status = 'pending' \
                AND dispatch_state = 'prepared' \
                AND dispatch_operation_id IS NULL \
          RETURNING {INTENT_COLUMNS}"
        );
        let updated: Option<IntentRow> = sqlx::query_as(&update_sql)
            .bind(command.intent_id)
            .bind(command.operation_id.as_ref())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        let record = updated
            .ok_or_else(|| BudgetStoreError::corrupt("dispatch transition 条件失效"))?
            .into_record()?;
        commit(transaction).await?;
        Ok(BudgetIntent {
            disposition: BudgetIntentDisposition::DispatchApplied,
            record: Some(record),
            account: None,
        })
    }

    async fn settle(
        &self,
        command: SettleBudgetIntent,
    ) -> Result<BudgetSettlement, BudgetStoreError> {
        validate_settle_command(&command)?;
        let mut transaction = self.begin_write().await?;

        // 同时修改 aggregate 与 ledger 时，始终先锁 key。
        let account_sql =
            format!("SELECT {ACCOUNT_COLUMNS} FROM ai_virtual_keys WHERE id = $1 FOR UPDATE");
        let account_row: Option<AccountRow> = sqlx::query_as(&account_sql)
            .bind(command.virtual_key_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;

        // key 删除后的 terminal replay 是只读 fast-path，不产生任何账务写。
        let Some(account_row) = account_row else {
            let existing = find_intent_by_id(&mut transaction, command.intent_id)
                .await?
                .ok_or_else(|| BudgetStoreError::corrupt("预算账户和 intent 均不存在"))?;
            verify_settle_identity(&existing, &command)?;
            let disposition = terminal_disposition(&existing, &command)?;
            let intent = existing.into_record()?;
            commit(transaction).await?;
            return Ok(BudgetSettlement {
                disposition,
                intent,
                account: None,
            });
        };
        let account = account_row.into_snapshot()?;
        validate_account_invariants(&account)?;

        let existing = lock_intent_by_id(&mut transaction, command.intent_id)
            .await?
            .ok_or_else(|| BudgetStoreError::corrupt("预算 intent 不存在"))?;
        verify_settle_identity(&existing, &command)?;
        if existing.status == "unresolved"
            && (existing.terminal_operation_id.is_some()
                || existing.terminal_command_fingerprint.is_some())
        {
            if existing.terminal_operation_id.as_deref() != Some(command.operation_id.as_ref())
                || existing
                    .terminal_command_fingerprint
                    .as_deref()
                    .map(str::trim)
                    != Some(command.command_fingerprint.as_ref())
            {
                return Err(BudgetStoreError::corrupt(
                    "unresolved settlement operation 的幂等载荷冲突",
                ));
            }
            let intent = existing.into_record()?;
            commit(transaction).await?;
            return Ok(BudgetSettlement {
                disposition: BudgetSettlementDisposition::AlreadyUnresolved,
                intent,
                account: Some(account),
            });
        }
        if existing.status == "settled" || existing.status == "resolved" {
            let disposition = terminal_disposition(&existing, &command)?;
            let intent = existing.into_record()?;
            commit(transaction).await?;
            return Ok(BudgetSettlement {
                disposition,
                intent,
                account: Some(account),
            });
        }
        if !matches!(existing.status.as_str(), "pending" | "unresolved") {
            return Err(BudgetStoreError::corrupt("预算 intent 状态不支持结算"));
        }
        if existing.dispatch_state.as_deref() == Some("prepared")
            && command.cost.status != CostStatus::NotIncurred
        {
            return Err(BudgetStoreError::corrupt(
                "prepared intent 只能按 not_incurred 结算",
            ));
        }

        let normalized_cost = normalize_cost(&command.cost)?;
        if command.cost.status == CostStatus::Unavailable {
            if existing.status == "unresolved" {
                let intent = existing.into_record()?;
                commit(transaction).await?;
                return Ok(BudgetSettlement {
                    disposition: BudgetSettlementDisposition::AlreadyUnresolved,
                    intent,
                    account: Some(account),
                });
            }
            let reasons = nonempty_unavailable_reasons(&command.cost.reasons);
            let (account, intent) = mark_unresolved(
                &mut transaction,
                &account,
                &existing,
                normalized_cost,
                &reasons,
                command.usage_fact_id,
                None,
            )
            .await?;
            commit(transaction).await?;
            return Ok(BudgetSettlement {
                disposition: BudgetSettlementDisposition::MarkedUnresolved,
                intent,
                account: Some(account),
            });
        }

        let amount = normalized_cost.unwrap_or(Decimal::ZERO);
        let new_used = account
            .used_usd
            .checked_add(amount)
            .and_then(|value| normalize_budget_amount(value).ok());
        let Some(_new_used) = new_used else {
            let mut reasons = command.cost.reasons.clone();
            if !reasons
                .iter()
                .any(|reason| reason == "budget_numeric_overflow")
            {
                reasons.push("budget_numeric_overflow".to_string());
            }
            let (account, intent) = mark_unresolved(
                &mut transaction,
                &account,
                &existing,
                Some(amount),
                &reasons,
                command.usage_fact_id,
                Some((
                    command.operation_id.as_ref(),
                    command.command_fingerprint.as_ref(),
                )),
            )
            .await?;
            commit(transaction).await?;
            return Ok(BudgetSettlement {
                disposition: BudgetSettlementDisposition::MarkedUnresolved,
                intent,
                account: Some(account),
            });
        };

        let new_revision = account
            .accounting_revision
            .checked_add(1)
            .ok_or_else(|| BudgetStoreError::corrupt("预算 revision 溢出"))?;
        account
            .checkpoint_tail_events
            .checked_add(1)
            .ok_or_else(|| BudgetStoreError::corrupt("预算 tail count 溢出"))?;

        let update_account_sql = if existing.status == "pending" {
            if account.pending_count <= 0 {
                return Err(BudgetStoreError::corrupt(
                    "pending intent 与账户 pending count 不一致",
                ));
            }
            format!(
                "UPDATE ai_virtual_keys \
                    SET budget_used = budget_used + $2, \
                        budget_pending_count = budget_pending_count - 1, \
                        budget_accounting_revision = $3, \
                        budget_checkpoint_tail_events = budget_checkpoint_tail_events + 1, \
                        budget_state_updated_at = clock_timestamp() \
                  WHERE id = $1 \
                    AND budget_pending_count > 0 \
                    AND budget_accounting_revision = $4 \
                    AND budget_used <= $5 - $2 \
              RETURNING {ACCOUNT_COLUMNS}"
            )
        } else {
            if account.unresolved_count <= 0 {
                return Err(BudgetStoreError::corrupt(
                    "unresolved intent 与账户 unresolved count 不一致",
                ));
            }
            format!(
                "UPDATE ai_virtual_keys \
                    SET budget_used = budget_used + $2, \
                        budget_unresolved_count = budget_unresolved_count - 1, \
                        budget_accounting_revision = $3, \
                        budget_checkpoint_tail_events = budget_checkpoint_tail_events + 1, \
                        budget_state_updated_at = clock_timestamp() \
                  WHERE id = $1 \
                    AND budget_unresolved_count > 0 \
                    AND budget_accounting_revision = $4 \
                    AND budget_used <= $5 - $2 \
              RETURNING {ACCOUNT_COLUMNS}"
            )
        };
        let updated_account: Option<AccountRow> = sqlx::query_as(&update_account_sql)
            .bind(command.virtual_key_id)
            .bind(amount)
            .bind(new_revision)
            .bind(account.accounting_revision)
            .bind(max_budget_amount())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        let updated_account = updated_account.ok_or_else(|| {
            BudgetStoreError::corrupt("结算时账户 aggregate/count/revision 不一致")
        })?;

        let update_intent_sql = format!(
            "UPDATE ai_budget_ledger \
                SET status = 'settled', \
                    observed_cost_usd = $2, \
                    accounted_cost_usd = $2, \
                    cost_status = $3, \
                    cost_reasons = $4, \
                    usage_fact_id = $5, \
                    terminal_operation_id = $6, \
                    terminal_command_fingerprint = $7, \
                    last_account_revision = $8, \
                    updated_at = clock_timestamp(), \
                    settled_at = clock_timestamp() \
              WHERE id = $1 AND status = $9 \
          RETURNING {INTENT_COLUMNS}"
        );
        let updated_intent: Option<IntentRow> = sqlx::query_as(&update_intent_sql)
            .bind(command.intent_id)
            .bind(amount)
            .bind(command.cost.status.as_str())
            .bind(&command.cost.reasons)
            .bind(command.usage_fact_id)
            .bind(command.operation_id.as_ref())
            .bind(command.command_fingerprint.as_ref())
            .bind(new_revision)
            .bind(&existing.status)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        let intent = updated_intent
            .ok_or_else(|| BudgetStoreError::corrupt("结算时 intent 状态发生冲突"))?
            .into_record()?;
        let account = updated_account.into_snapshot()?;
        commit(transaction).await?;
        Ok(BudgetSettlement {
            disposition: BudgetSettlementDisposition::Applied,
            intent,
            account: Some(account),
        })
    }

    async fn recover_stale(
        &self,
        command: RecoverStaleBudgetIntents,
    ) -> Result<BudgetRecoveryBatch, BudgetStoreError> {
        if command.max_intents == 0 || command.max_intents > 10_000 {
            return Err(BudgetStoreError::corrupt(
                "stale recovery batch 必须在 1..=10000",
            ));
        }
        let max_intents = i64::from(command.max_intents);
        let mut transaction = self.begin_write().await?;
        self.load_authoritative_hard_tail(&mut transaction).await?;
        // 候选扫描不持有 ledger 锁；真正写入时逐条重新按 key-first 锁序校验。
        let candidates: Vec<StaleIntentCandidate> = sqlx::query_as(
            "SELECT ledger.id AS intent_id, ledger.virtual_key_id \
               FROM ai_budget_ledger AS ledger \
               JOIN ai_budget_owner_sessions AS owner \
                 ON owner.session_id = ledger.owner_session_id \
              WHERE ledger.kind = 'request' \
                AND ledger.status = 'pending' \
                AND ledger.stale_not_before <= clock_timestamp() \
                AND owner.deployment_namespace = $1 \
                AND (owner.stopped_at IS NOT NULL \
                     OR owner.expires_at <= clock_timestamp()) \
              ORDER BY ledger.stale_not_before, ledger.id \
              LIMIT $2",
        )
        .bind(self.config.deployment_namespace.as_ref())
        .bind(max_intents)
        .fetch_all(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
        commit(transaction).await?;

        let mut batch = BudgetRecoveryBatch {
            scanned: u32::try_from(candidates.len())
                .map_err(|_| BudgetStoreError::corrupt("stale recovery 候选数溢出"))?,
            ..BudgetRecoveryBatch::default()
        };
        for candidate in candidates {
            match self.recover_stale_candidate(candidate).await? {
                RecoveredStaleIntent::SettledNotIncurred => {
                    batch.settled_not_incurred = batch.settled_not_incurred.saturating_add(1);
                }
                RecoveredStaleIntent::MarkedUnresolved => {
                    batch.marked_unresolved = batch.marked_unresolved.saturating_add(1);
                }
                RecoveredStaleIntent::Skipped => {}
            }
        }
        Ok(batch)
    }

    async fn checkpoint_account(
        &self,
        command: CheckpointBudgetAccount,
    ) -> Result<BudgetCheckpoint, BudgetStoreError> {
        validate_checkpoint_command(&command)?;
        let mut transaction = self.begin_write().await?;
        self.load_authoritative_hard_tail(&mut transaction).await?;

        // checkpoint 必须与 settlement/recovery 使用同一 key-first 锁序。
        let account_sql =
            format!("SELECT {ACCOUNT_COLUMNS} FROM ai_virtual_keys WHERE id = $1 FOR UPDATE");
        let account_row: Option<AccountRow> = sqlx::query_as(&account_sql)
            .bind(command.virtual_key_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        let account = account_row
            .ok_or_else(|| BudgetStoreError::corrupt("checkpoint 的预算账户不存在"))?
            .into_snapshot()?;
        validate_account_invariants(&account)?;

        if let Some(existing) =
            find_checkpoint_by_operation(&mut transaction, command.operation_id.as_ref()).await?
        {
            if existing.virtual_key_id != command.virtual_key_id {
                return Err(BudgetStoreError::corrupt(
                    "checkpoint operation ID 与其他预算账户冲突",
                ));
            }
            let checkpoint = existing.into_checkpoint()?;
            commit(transaction).await?;
            return Ok(checkpoint);
        }
        let expected_operation = format!(
            "budget-checkpoint:v1:{}:{}",
            command.virtual_key_id, account.accounting_revision
        );
        if command.operation_id.as_ref() != expected_operation {
            return Err(BudgetStoreError::corrupt(
                "checkpoint operation ID 与当前账户 revision 不匹配",
            ));
        }

        let latest = lock_latest_checkpoint(&mut transaction, command.virtual_key_id)
            .await?
            .ok_or_else(|| BudgetStoreError::corrupt("预算账户缺少 genesis checkpoint"))?;
        let latest_checkpoint = latest.clone().into_checkpoint()?;
        if latest.checkpoint_revision > account.accounting_revision {
            return Err(BudgetStoreError::corrupt(
                "checkpoint revision 超前于预算账户 revision",
            ));
        }
        if latest.checkpoint_revision == account.accounting_revision {
            if account.checkpoint_tail_events != 0 {
                return Err(BudgetStoreError::corrupt(
                    "checkpoint 已覆盖当前 revision 但 tail 未归零",
                ));
            }
            commit(transaction).await?;
            return Ok(latest_checkpoint);
        }
        // soft threshold 负责决定何时调用；Store 同时允许 hard-tail 账户自救推进，
        // 但只按 ledger tail event 计容量。Admin rename/limit 等 key-only 更新也会
        // 推进 account revision，却不会制造账本事件，因此 revision delta 不能与
        // tail event 数量等同。

        let tail: CheckpointTailRow = sqlx::query_as(
            "SELECT COALESCE(SUM(accounted_cost_usd), 0::numeric) AS settled_cost, \
                    COUNT(*)::bigint AS settled_events \
               FROM ai_budget_ledger \
              WHERE virtual_key_id = $1 \
                AND status = 'settled' \
                AND last_account_revision > $2 \
                AND last_account_revision <= $3",
        )
        .bind(command.virtual_key_id)
        .bind(latest.checkpoint_revision)
        .bind(account.accounting_revision)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
        if tail.settled_events > account.checkpoint_tail_events {
            return Err(BudgetStoreError::corrupt(
                "checkpoint settled 事件数超过 tail counter",
            ));
        }
        let settled_cost =
            normalize_budget_amount(tail.settled_cost).map_err(BudgetStoreError::corrupt)?;
        let derived_used = latest_checkpoint
            .accounted_cost_usd
            .checked_add(settled_cost)
            .and_then(|value| normalize_budget_amount(value).ok())
            .ok_or_else(|| {
                BudgetStoreError::new(BudgetErrorKind::NumericOverflow, "checkpoint 累计金额溢出")
            })?;
        if derived_used != account.used_usd {
            record_checkpoint_issue(
                &mut transaction,
                &account,
                latest.checkpoint_revision,
                derived_used,
            )
            .await?;
            commit(transaction).await?;
            return Err(BudgetStoreError::new(
                BudgetErrorKind::AccountingUnresolved,
                "checkpoint 校验发现预算 aggregate 漂移",
            ));
        }

        let inserted: CheckpointRow = sqlx::query_as(
            "INSERT INTO ai_budget_checkpoints (\
                 virtual_key_id, checkpoint_revision, accounted_cost_usd, operation_id\
             ) VALUES ($1, $2, $3, $4) \
             RETURNING virtual_key_id, checkpoint_revision, accounted_cost_usd, \
                       operation_id, created_at",
        )
        .bind(command.virtual_key_id)
        .bind(account.accounting_revision)
        .bind(derived_used)
        .bind(command.operation_id.as_ref())
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;

        let updated: Option<Uuid> = sqlx::query_scalar(
            "UPDATE ai_virtual_keys \
                SET budget_checkpoint_tail_events = 0, \
                    budget_state_updated_at = clock_timestamp() \
              WHERE id = $1 \
                AND budget_accounting_revision = $2 \
                AND budget_checkpoint_tail_events = $3 \
          RETURNING id",
        )
        .bind(command.virtual_key_id)
        .bind(account.accounting_revision)
        .bind(account.checkpoint_tail_events)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
        if updated.is_none() {
            return Err(BudgetStoreError::corrupt(
                "checkpoint 归零 tail 的 CAS 失败",
            ));
        }

        let checkpoint = inserted.into_checkpoint()?;
        commit(transaction).await?;
        Ok(checkpoint)
    }

    async fn register_owner(
        &self,
        command: RegisterBudgetOwner,
    ) -> Result<BudgetOwnerLease, BudgetStoreError> {
        validate_owner_duration(command.lease_duration)?;
        let lease_seconds = i64::try_from(command.lease_duration.as_secs())
            .map_err(|_| BudgetStoreError::corrupt("owner lease 过大"))?;
        let mut transaction = self.begin_write().await?;

        sqlx::query(
            "INSERT INTO ai_budget_runtime_settings (\
                 deployment_namespace, checkpoint_hard_tail_events, config_fingerprint, updated_at\
             ) VALUES ($1, $2, $3, clock_timestamp()) \
             ON CONFLICT (deployment_namespace) DO NOTHING",
        )
        .bind(self.config.deployment_namespace.as_ref())
        .bind(self.config.checkpoint_hard_tail_events)
        .bind(self.config.config_fingerprint.as_ref())
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;

        let settings: RuntimeSettingsRow = sqlx::query_as(
            "SELECT checkpoint_hard_tail_events, config_fingerprint \
               FROM ai_budget_runtime_settings \
              WHERE deployment_namespace = $1 \
              FOR UPDATE",
        )
        .bind(self.config.deployment_namespace.as_ref())
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
        if settings.checkpoint_hard_tail_events != self.config.checkpoint_hard_tail_events
            || settings.config_fingerprint.trim() != self.config.config_fingerprint.as_ref()
        {
            // settings 由 deployment 共享；合法配置升级只能在没有 live owner 时
            // 原子接管。锁 settings 后再锁所有 live owner，使并发 heartbeat 与
            // 新旧配置注册只能有一方提交。
            let live_owner_ids: Vec<Uuid> = sqlx::query_scalar(
                "SELECT session_id \
                   FROM ai_budget_owner_sessions \
                  WHERE deployment_namespace = $1 \
                    AND stopped_at IS NULL \
                    AND expires_at > clock_timestamp() \
                  FOR UPDATE",
            )
            .bind(self.config.deployment_namespace.as_ref())
            .fetch_all(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
            if !live_owner_ids.is_empty() {
                return Err(BudgetStoreError::unavailable(
                    "本节点预算配置与仍有 live owner 的 deployment 权威配置不一致",
                ));
            }
            sqlx::query(
                "UPDATE ai_budget_runtime_settings \
                    SET checkpoint_hard_tail_events = $2, \
                        config_fingerprint = $3, \
                        updated_at = clock_timestamp() \
                  WHERE deployment_namespace = $1",
            )
            .bind(self.config.deployment_namespace.as_ref())
            .bind(self.config.checkpoint_hard_tail_events)
            .bind(self.config.config_fingerprint.as_ref())
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        }

        if let Some(existing) = lock_owner(&mut transaction, command.session_id).await? {
            if existing.deployment_namespace != self.config.deployment_namespace.as_ref()
                || existing.node_id != command.node_id
            {
                return Err(BudgetStoreError::corrupt(
                    "owner session ID 与已有 owner 冲突",
                ));
            }
            if existing.stopped_at.is_some() || !existing.live {
                return Err(BudgetStoreError::unavailable(
                    "owner session 已停止或过期，禁止复活",
                ));
            }
            let lease = existing.into_lease(Arc::clone(&self.config.deployment_namespace), true);
            commit(transaction).await?;
            return Ok(lease);
        }

        let row: OwnerRow = sqlx::query_as(
            "INSERT INTO ai_budget_owner_sessions (\
                 session_id, deployment_namespace, node_id, started_at, \
                 last_heartbeat_at, expires_at\
             ) VALUES (\
                 $1, $2, $3, clock_timestamp(), clock_timestamp(), \
                 clock_timestamp() + $4::bigint * interval '1 second'\
             ) \
             RETURNING session_id, deployment_namespace, node_id, started_at, \
                       last_heartbeat_at, expires_at, stopped_at, \
                       expires_at > clock_timestamp() AS live",
        )
        .bind(command.session_id)
        .bind(self.config.deployment_namespace.as_ref())
        .bind(command.node_id)
        .bind(lease_seconds)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
        let lease = row.into_lease(Arc::clone(&self.config.deployment_namespace), false);
        commit(transaction).await?;
        Ok(lease)
    }

    async fn heartbeat_owner(
        &self,
        command: HeartbeatBudgetOwner,
    ) -> Result<BudgetOwnerLease, BudgetStoreError> {
        validate_owner_duration(command.lease_duration)?;
        let lease_seconds = i64::try_from(command.lease_duration.as_secs())
            .map_err(|_| BudgetStoreError::corrupt("owner lease 过大"))?;
        let mut transaction = self.begin_write().await?;
        self.load_authoritative_hard_tail(&mut transaction).await?;
        let row: Option<OwnerRow> = sqlx::query_as(
            "UPDATE ai_budget_owner_sessions \
                SET last_heartbeat_at = clock_timestamp(), \
                    expires_at = clock_timestamp() + $4::bigint * interval '1 second' \
              WHERE session_id = $1 \
                AND deployment_namespace = $2 \
                AND node_id = $3 \
                AND stopped_at IS NULL \
                AND expires_at > clock_timestamp() \
          RETURNING session_id, deployment_namespace, node_id, started_at, \
                    last_heartbeat_at, expires_at, stopped_at, TRUE AS live",
        )
        .bind(command.session_id)
        .bind(self.config.deployment_namespace.as_ref())
        .bind(command.node_id)
        .bind(lease_seconds)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
        let row = row.ok_or_else(|| {
            BudgetStoreError::unavailable("owner session 已被 fence，heartbeat 被拒绝")
        })?;
        let lease = row.into_lease(Arc::clone(&self.config.deployment_namespace), false);
        commit(transaction).await?;
        Ok(lease)
    }

    async fn stop_owner(&self, command: StopBudgetOwner) -> Result<(), BudgetStoreError> {
        let mut transaction = self.begin_write().await?;
        let stopped: Option<Uuid> = sqlx::query_scalar(
            "UPDATE ai_budget_owner_sessions \
                SET stopped_at = clock_timestamp() \
              WHERE session_id = $1 \
                AND deployment_namespace = $2 \
                AND node_id = $3 \
                AND stopped_at IS NULL \
          RETURNING session_id",
        )
        .bind(command.session_id)
        .bind(self.config.deployment_namespace.as_ref())
        .bind(command.node_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
        if stopped.is_none() {
            let existing = lock_owner(&mut transaction, command.session_id)
                .await?
                .ok_or_else(|| BudgetStoreError::corrupt("owner session 不存在"))?;
            if existing.deployment_namespace != self.config.deployment_namespace.as_ref()
                || existing.node_id != command.node_id
            {
                return Err(BudgetStoreError::corrupt(
                    "owner session ID 与 node/namespace 不匹配",
                ));
            }
            if existing.stopped_at.is_none() {
                return Err(BudgetStoreError::unavailable("owner session 无法停止"));
            }
        }
        commit(transaction).await?;
        Ok(())
    }
}

#[derive(Clone, FromRow)]
struct AccountRow {
    id: Uuid,
    name: String,
    key_prefix: String,
    ws_id: Option<Uuid>,
    budget_limit: Option<Decimal>,
    budget_used: Decimal,
    budget_pending_count: i64,
    budget_unresolved_count: i64,
    budget_accounting_revision: i64,
    budget_checkpoint_tail_events: i64,
    budget_accounting_state: String,
    budget_state_updated_at: DateTime<Utc>,
}

impl AccountRow {
    fn into_snapshot(self) -> Result<BudgetAccountSnapshot, BudgetStoreError> {
        Ok(BudgetAccountSnapshot {
            virtual_key_id: self.id,
            virtual_key_name: self.name,
            virtual_key_prefix: self.key_prefix,
            workspace_id: self.ws_id,
            limit_usd: self
                .budget_limit
                .map(normalize_budget_amount)
                .transpose()
                .map_err(BudgetStoreError::corrupt)?,
            used_usd: normalize_budget_amount(self.budget_used)
                .map_err(BudgetStoreError::corrupt)?,
            pending_count: self.budget_pending_count,
            unresolved_count: self.budget_unresolved_count,
            accounting_revision: self.budget_accounting_revision,
            checkpoint_tail_events: self.budget_checkpoint_tail_events,
            state: BudgetAccountingState::parse(&self.budget_accounting_state)
                .ok_or_else(|| BudgetStoreError::corrupt("预算账户 state 非法"))?,
            state_updated_at: self.budget_state_updated_at,
        })
    }
}

#[derive(Clone, FromRow)]
struct IntentRow {
    id: Uuid,
    virtual_key_id: Uuid,
    request_id: Option<String>,
    status: String,
    operation_id: String,
    command_fingerprint: Option<String>,
    dispatch_operation_id: Option<String>,
    terminal_operation_id: Option<String>,
    terminal_command_fingerprint: Option<String>,
    last_account_revision: i64,
    usage_fact_id: Option<Uuid>,
    observed_cost_usd: Option<Decimal>,
    accounted_cost_usd: Option<Decimal>,
    cost_status: Option<String>,
    cost_reasons: Vec<String>,
    pricing_fingerprint: Option<String>,
    dispatch_state: Option<String>,
    node_id: Option<Uuid>,
    owner_session_id: Option<Uuid>,
    stale_not_before: Option<DateTime<Utc>>,
    resolution_entry_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl IntentRow {
    fn into_record(self) -> Result<BudgetIntentRecord, BudgetStoreError> {
        if self.last_account_revision < 0 {
            return Err(BudgetStoreError::corrupt(
                "intent last account revision 非法",
            ));
        }
        let state = match (self.status.as_str(), self.dispatch_state.as_deref()) {
            ("pending", Some("prepared")) => BudgetIntentState::Prepared,
            ("pending", Some("dispatching")) => BudgetIntentState::Dispatching,
            ("unresolved", _) => BudgetIntentState::Unresolved,
            ("settled", _) => BudgetIntentState::Settled,
            ("resolved", _) => BudgetIntentState::Resolved,
            _ => return Err(BudgetStoreError::corrupt("request intent 状态非法")),
        };
        let request_id = self
            .request_id
            .ok_or_else(|| BudgetStoreError::corrupt("request intent 缺少 request ID"))?;
        let command_fingerprint = self
            .command_fingerprint
            .ok_or_else(|| BudgetStoreError::corrupt("request intent 缺少 command fingerprint"))?;
        let pricing_fingerprint = self
            .pricing_fingerprint
            .ok_or_else(|| BudgetStoreError::corrupt("request intent 缺少 pricing fingerprint"))?;
        let owner_session_id = self
            .owner_session_id
            .ok_or_else(|| BudgetStoreError::corrupt("request intent 缺少 owner session"))?;
        let node_id = self
            .node_id
            .ok_or_else(|| BudgetStoreError::corrupt("request intent 缺少 node ID"))?;
        let cost_status = self
            .cost_status
            .map(|value| CostStatus::from_str(&value))
            .transpose()
            .map_err(BudgetStoreError::corrupt)?;
        Ok(BudgetIntentRecord {
            id: self.id,
            virtual_key_id: self.virtual_key_id,
            request_id: Arc::from(request_id),
            state,
            operation_id: Arc::from(self.operation_id),
            command_fingerprint: Arc::from(command_fingerprint.trim().to_string()),
            dispatch_operation_id: self.dispatch_operation_id.map(Arc::from),
            terminal_operation_id: self.terminal_operation_id.map(Arc::from),
            terminal_command_fingerprint: self
                .terminal_command_fingerprint
                .map(|value| Arc::from(value.trim().to_string())),
            pricing_fingerprint: Arc::from(pricing_fingerprint.trim().to_string()),
            owner_session_id,
            node_id,
            last_account_revision: self.last_account_revision,
            observed_cost_usd: self.observed_cost_usd,
            accounted_cost_usd: self.accounted_cost_usd,
            cost_status,
            cost_reasons: self.cost_reasons,
            usage_fact_id: self.usage_fact_id,
            resolution_entry_id: self.resolution_entry_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(FromRow)]
struct RuntimeSettingsRow {
    checkpoint_hard_tail_events: i64,
    config_fingerprint: String,
}

#[derive(FromRow)]
struct OwnerRow {
    session_id: Uuid,
    deployment_namespace: String,
    node_id: Uuid,
    started_at: DateTime<Utc>,
    last_heartbeat_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    stopped_at: Option<DateTime<Utc>>,
    live: bool,
}

impl OwnerRow {
    fn into_lease(self, namespace: Arc<str>, replayed: bool) -> BudgetOwnerLease {
        BudgetOwnerLease {
            session_id: self.session_id,
            node_id: self.node_id,
            deployment_namespace: namespace,
            started_at: self.started_at,
            last_heartbeat_at: self.last_heartbeat_at,
            expires_at: self.expires_at,
            replayed,
        }
    }
}

#[derive(FromRow)]
struct StaleIntentCandidate {
    intent_id: Uuid,
    virtual_key_id: Uuid,
}

#[derive(Clone, Copy, FromRow)]
struct DueCheckpointCandidate {
    virtual_key_id: Uuid,
    accounting_revision: i64,
}

#[derive(Clone, Copy)]
enum RecoveredStaleIntent {
    SettledNotIncurred,
    MarkedUnresolved,
    Skipped,
}

#[derive(Clone, FromRow)]
struct CheckpointRow {
    virtual_key_id: Uuid,
    checkpoint_revision: i64,
    accounted_cost_usd: Decimal,
    #[allow(dead_code)]
    operation_id: String,
    created_at: DateTime<Utc>,
}

impl CheckpointRow {
    fn into_checkpoint(self) -> Result<BudgetCheckpoint, BudgetStoreError> {
        if self.checkpoint_revision < 0 {
            return Err(BudgetStoreError::corrupt("checkpoint revision 不能为负数"));
        }
        Ok(BudgetCheckpoint {
            virtual_key_id: self.virtual_key_id,
            revision: self.checkpoint_revision,
            accounted_cost_usd: normalize_budget_amount(self.accounted_cost_usd)
                .map_err(BudgetStoreError::corrupt)?,
            created_at: self.created_at,
        })
    }
}

#[derive(FromRow)]
struct CheckpointTailRow {
    settled_cost: Decimal,
    settled_events: i64,
}

async fn find_checkpoint_by_operation(
    transaction: &mut Transaction<'_, Postgres>,
    operation_id: &str,
) -> Result<Option<CheckpointRow>, BudgetStoreError> {
    sqlx::query_as(
        "SELECT virtual_key_id, checkpoint_revision, accounted_cost_usd, \
                operation_id, created_at \
           FROM ai_budget_checkpoints \
          WHERE operation_id = $1",
    )
    .bind(operation_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx_error)
}

async fn lock_latest_checkpoint(
    transaction: &mut Transaction<'_, Postgres>,
    virtual_key_id: Uuid,
) -> Result<Option<CheckpointRow>, BudgetStoreError> {
    sqlx::query_as(
        "SELECT virtual_key_id, checkpoint_revision, accounted_cost_usd, \
                operation_id, created_at \
           FROM ai_budget_checkpoints \
          WHERE virtual_key_id = $1 \
          ORDER BY checkpoint_revision DESC \
          LIMIT 1 \
          FOR UPDATE",
    )
    .bind(virtual_key_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx_error)
}

async fn record_checkpoint_issue(
    transaction: &mut Transaction<'_, Postgres>,
    account: &BudgetAccountSnapshot,
    checkpoint_revision: i64,
    derived_used: Decimal,
) -> Result<(), BudgetStoreError> {
    let existing_issue: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM ai_budget_ledger \
          WHERE virtual_key_id = $1 \
            AND kind = 'account_issue' \
            AND status = 'unresolved' \
          ORDER BY created_at \
          LIMIT 1 \
          FOR UPDATE",
    )
    .bind(account.virtual_key_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx_error)?;
    if existing_issue.is_some() {
        return Ok(());
    }

    let new_revision = checked_next_revision(account)?;
    let operation_id = format!(
        "budget-account-issue:v1:{}:{}",
        account.virtual_key_id, account.accounting_revision
    );
    let fingerprint = sha256_hex(
        format!(
            "budget-account-issue:v1\n{}\n{}\n{}\n{}\n{}",
            account.virtual_key_id,
            checkpoint_revision,
            account.accounting_revision,
            account.used_usd,
            derived_used
        )
        .as_bytes(),
    );
    let reasons = vec!["checkpoint_amount_mismatch".to_string()];

    sqlx::query(
        "INSERT INTO ai_budget_ledger (\
             id, virtual_key_id, virtual_key_name, virtual_key_prefix, workspace_id, \
             kind, status, operation_id, command_fingerprint, last_account_revision, \
             observed_cost_usd, accounted_cost_usd, cost_status, cost_reasons\
         ) VALUES (\
             $1, $2, $3, $4, $5, 'account_issue', 'unresolved', $6, $7, $8, \
             $9, NULL, 'unavailable', $10\
         )",
    )
    .bind(Uuid::now_v7())
    .bind(account.virtual_key_id)
    .bind(&account.virtual_key_name)
    .bind(&account.virtual_key_prefix)
    .bind(account.workspace_id)
    .bind(&operation_id)
    .bind(&fingerprint)
    .bind(new_revision)
    .bind(derived_used)
    .bind(&reasons)
    .execute(&mut **transaction)
    .await
    .map_err(map_sqlx_error)?;

    let updated: Option<Uuid> = sqlx::query_scalar(
        "UPDATE ai_virtual_keys \
            SET budget_unresolved_count = budget_unresolved_count + 1, \
                budget_accounting_revision = $2, \
                budget_checkpoint_tail_events = budget_checkpoint_tail_events + 1, \
                budget_state_updated_at = clock_timestamp() \
          WHERE id = $1 \
            AND budget_accounting_revision = $3 \
            AND budget_checkpoint_tail_events = $4 \
      RETURNING id",
    )
    .bind(account.virtual_key_id)
    .bind(new_revision)
    .bind(account.accounting_revision)
    .bind(account.checkpoint_tail_events)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx_error)?;
    if updated.is_none() {
        return Err(BudgetStoreError::corrupt(
            "记录 checkpoint account issue 的账户 CAS 失败",
        ));
    }
    Ok(())
}

async fn lock_intent_by_request(
    transaction: &mut Transaction<'_, Postgres>,
    request_id: &str,
) -> Result<Option<IntentRow>, BudgetStoreError> {
    let sql = format!(
        "SELECT {INTENT_COLUMNS} FROM ai_budget_ledger \
          WHERE request_id = $1 FOR UPDATE"
    );
    sqlx::query_as(&sql)
        .bind(request_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx_error)
}

async fn lock_intent_by_id(
    transaction: &mut Transaction<'_, Postgres>,
    intent_id: Uuid,
) -> Result<Option<IntentRow>, BudgetStoreError> {
    let sql = format!("SELECT {INTENT_COLUMNS} FROM ai_budget_ledger WHERE id = $1 FOR UPDATE");
    sqlx::query_as(&sql)
        .bind(intent_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx_error)
}

async fn find_intent_by_id(
    transaction: &mut Transaction<'_, Postgres>,
    intent_id: Uuid,
) -> Result<Option<IntentRow>, BudgetStoreError> {
    let sql = format!("SELECT {INTENT_COLUMNS} FROM ai_budget_ledger WHERE id = $1");
    sqlx::query_as(&sql)
        .bind(intent_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx_error)
}

async fn lock_owner(
    transaction: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
) -> Result<Option<OwnerRow>, BudgetStoreError> {
    sqlx::query_as(
        "SELECT session_id, deployment_namespace, node_id, started_at, \
                last_heartbeat_at, expires_at, stopped_at, \
                expires_at > clock_timestamp() AND stopped_at IS NULL AS live \
           FROM ai_budget_owner_sessions \
          WHERE session_id = $1 \
          FOR UPDATE",
    )
    .bind(session_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_sqlx_error)
}

fn verify_create_replay(
    existing: &IntentRow,
    command: &CreateBudgetIntent,
) -> Result<(), BudgetStoreError> {
    if existing.id != command.intent_id {
        return Err(BudgetStoreError::corrupt(
            "intent request ID 与 intent ID 不匹配",
        ));
    }
    verify_intent_identity(
        existing,
        command.virtual_key_id,
        &command.request_id,
        Some(command.owner_session_id),
        Some(command.node_id),
    )?;
    if existing.operation_id != command.operation_id.as_ref()
        || existing.command_fingerprint.as_deref().map(str::trim)
            != Some(command.command_fingerprint.as_ref())
        || existing.pricing_fingerprint.as_deref().map(str::trim)
            != Some(command.pricing_fingerprint.as_ref())
    {
        return Err(BudgetStoreError::corrupt(
            "intent request ID 的幂等载荷冲突",
        ));
    }
    Ok(())
}

fn verify_lookup_identity(
    existing: &IntentRow,
    command: &LookupBudgetIntent,
) -> Result<(), BudgetStoreError> {
    if existing.id != command.intent_id {
        return Err(BudgetStoreError::corrupt(
            "intent lookup 的 request ID 与 intent ID 不匹配",
        ));
    }
    verify_intent_identity(
        existing,
        command.virtual_key_id,
        &command.request_id,
        Some(command.owner_session_id),
        Some(command.node_id),
    )?;
    if existing.operation_id != command.operation_id.as_ref()
        || existing.command_fingerprint.as_deref().map(str::trim)
            != Some(command.command_fingerprint.as_ref())
        || existing.pricing_fingerprint.as_deref().map(str::trim)
            != Some(command.pricing_fingerprint.as_ref())
    {
        return Err(BudgetStoreError::corrupt("intent lookup 的幂等载荷冲突"));
    }
    Ok(())
}

fn verify_intent_identity(
    existing: &IntentRow,
    virtual_key_id: Uuid,
    request_id: &str,
    owner_session_id: Option<Uuid>,
    node_id: Option<Uuid>,
) -> Result<(), BudgetStoreError> {
    if existing.virtual_key_id != virtual_key_id
        || existing.request_id.as_deref() != Some(request_id)
        || owner_session_id.is_some_and(|value| existing.owner_session_id != Some(value))
        || node_id.is_some_and(|value| existing.node_id != Some(value))
    {
        return Err(BudgetStoreError::corrupt(
            "intent 与 key/request/owner 身份不匹配",
        ));
    }
    Ok(())
}

fn verify_settle_identity(
    existing: &IntentRow,
    command: &SettleBudgetIntent,
) -> Result<(), BudgetStoreError> {
    verify_intent_identity(
        existing,
        command.virtual_key_id,
        &command.request_id,
        None,
        None,
    )?;
    if existing.pricing_fingerprint.as_deref().map(str::trim)
        != Some(command.pricing_fingerprint.as_ref())
    {
        return Err(BudgetStoreError::corrupt(
            "settlement pricing fingerprint 与 intent 不一致",
        ));
    }
    Ok(())
}

fn terminal_disposition(
    existing: &IntentRow,
    command: &SettleBudgetIntent,
) -> Result<BudgetSettlementDisposition, BudgetStoreError> {
    match existing.status.as_str() {
        "settled" => {
            if existing.terminal_operation_id.as_deref() != Some(command.operation_id.as_ref())
                || existing
                    .terminal_command_fingerprint
                    .as_deref()
                    .map(str::trim)
                    != Some(command.command_fingerprint.as_ref())
            {
                return Err(BudgetStoreError::corrupt(
                    "settlement operation 的幂等载荷冲突",
                ));
            }
            Ok(BudgetSettlementDisposition::Replayed)
        }
        "resolved" => Ok(BudgetSettlementDisposition::AlreadyReconciled),
        _ => Err(BudgetStoreError::corrupt(
            "key 删除后仅允许 terminal intent 重放",
        )),
    }
}

async fn mark_unresolved(
    transaction: &mut Transaction<'_, Postgres>,
    account: &BudgetAccountSnapshot,
    existing: &IntentRow,
    observed_cost: Option<Decimal>,
    reasons: &[String],
    usage_fact_id: Option<Uuid>,
    terminal_command: Option<(&str, &str)>,
) -> Result<(BudgetAccountSnapshot, BudgetIntentRecord), BudgetStoreError> {
    let new_revision = account
        .accounting_revision
        .checked_add(1)
        .ok_or_else(|| BudgetStoreError::corrupt("预算 revision 溢出"))?;
    account
        .checkpoint_tail_events
        .checked_add(1)
        .ok_or_else(|| BudgetStoreError::corrupt("预算 tail count 溢出"))?;

    let update_account_sql = if existing.status == "pending" {
        if account.pending_count <= 0 {
            return Err(BudgetStoreError::corrupt(
                "pending intent 与账户 pending count 不一致",
            ));
        }
        format!(
            "UPDATE ai_virtual_keys \
                SET budget_pending_count = budget_pending_count - 1, \
                    budget_unresolved_count = budget_unresolved_count + 1, \
                    budget_accounting_revision = $2, \
                    budget_checkpoint_tail_events = budget_checkpoint_tail_events + 1, \
                    budget_state_updated_at = clock_timestamp() \
              WHERE id = $1 \
                AND budget_pending_count > 0 \
                AND budget_accounting_revision = $3 \
          RETURNING {ACCOUNT_COLUMNS}"
        )
    } else {
        if account.unresolved_count <= 0 {
            return Err(BudgetStoreError::corrupt(
                "unresolved intent 与账户 unresolved count 不一致",
            ));
        }
        format!(
            "UPDATE ai_virtual_keys \
                SET budget_accounting_revision = $2, \
                    budget_checkpoint_tail_events = budget_checkpoint_tail_events + 1, \
                    budget_state_updated_at = clock_timestamp() \
              WHERE id = $1 \
                AND budget_unresolved_count > 0 \
                AND budget_accounting_revision = $3 \
          RETURNING {ACCOUNT_COLUMNS}"
        )
    };
    let updated_account: Option<AccountRow> = sqlx::query_as(&update_account_sql)
        .bind(account.virtual_key_id)
        .bind(new_revision)
        .bind(account.accounting_revision)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx_error)?;
    let updated_account = updated_account
        .ok_or_else(|| BudgetStoreError::corrupt("标记 unresolved 时账户状态不一致"))?;

    let update_intent_sql = format!(
        "UPDATE ai_budget_ledger \
            SET status = 'unresolved', \
                observed_cost_usd = $2, \
                accounted_cost_usd = NULL, \
                cost_status = 'unavailable', \
                cost_reasons = $3, \
                usage_fact_id = $4, \
                last_account_revision = $5, \
                terminal_operation_id = $7, \
                terminal_command_fingerprint = $8, \
                updated_at = clock_timestamp() \
          WHERE id = $1 AND status = $6 \
      RETURNING {INTENT_COLUMNS}"
    );
    let updated_intent: Option<IntentRow> = sqlx::query_as(&update_intent_sql)
        .bind(existing.id)
        .bind(observed_cost)
        .bind(reasons)
        .bind(usage_fact_id)
        .bind(new_revision)
        .bind(&existing.status)
        .bind(terminal_command.map(|(operation_id, _)| operation_id))
        .bind(terminal_command.map(|(_, fingerprint)| fingerprint))
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx_error)?;
    let updated_intent = updated_intent
        .ok_or_else(|| BudgetStoreError::corrupt("标记 unresolved 时 intent 状态不一致"))?;
    Ok((
        updated_account.into_snapshot()?,
        updated_intent.into_record()?,
    ))
}

fn validate_account_invariants(account: &BudgetAccountSnapshot) -> Result<(), BudgetStoreError> {
    if account.pending_count < 0
        || account.unresolved_count < 0
        || account.accounting_revision < 0
        || account.checkpoint_tail_events < 0
    {
        return Err(BudgetStoreError::corrupt("预算账户计数/revision 为负数"));
    }
    let expected = if account.unresolved_count > 0 {
        BudgetAccountingState::Unresolved
    } else if account.pending_count > 0 {
        BudgetAccountingState::Pending
    } else {
        BudgetAccountingState::Clean
    };
    if account.state != expected {
        return Err(BudgetStoreError::corrupt("预算账户生成 state 与计数不一致"));
    }
    Ok(())
}

fn validate_create_command(command: &CreateBudgetIntent) -> Result<(), BudgetStoreError> {
    validate_request_id(&command.request_id)?;
    validate_operation_id(
        &command.operation_id,
        &format!("intent:v1:{}", command.request_id),
        "intent operation",
    )?;
    validate_fingerprint(&command.command_fingerprint, "intent command fingerprint")?;
    validate_fingerprint(&command.pricing_fingerprint, "pricing fingerprint")?;
    validate_owner_duration(command.stale_after)?;
    validate_pricing_snapshot(&command.pricing_snapshot)?;
    if command.pricing_snapshot.schema_version != BUDGET_SCHEMA_VERSION {
        return Err(BudgetStoreError::new(
            BudgetErrorKind::PricingUnavailable,
            "pricing snapshot schema version 不受支持",
        ));
    }
    Ok(())
}

fn validate_pricing_snapshot(
    snapshot: &super::model::BudgetPricingSnapshot,
) -> Result<(), BudgetStoreError> {
    if snapshot.provider_type.trim().is_empty()
        || snapshot.provider_type.len() > 64
        || snapshot.model.trim().is_empty()
        || snapshot.model.len() > 256
        || snapshot.max_prompt_tokens.is_some_and(|value| value <= 0)
    {
        return Err(BudgetStoreError::new(
            BudgetErrorKind::PricingUnavailable,
            "pricing snapshot 的 provider/model/条件非法",
        ));
    }
    for price in [snapshot.input.as_ref(), snapshot.output.as_ref()]
        .into_iter()
        .flatten()
    {
        normalize_budget_amount(price.usd_per_million).map_err(|_| {
            BudgetStoreError::new(
                BudgetErrorKind::PricingUnavailable,
                "pricing snapshot 金额非法",
            )
        })?;
        if price.source.trim().is_empty()
            || price.source.len() > 256
            || price.version.trim().is_empty()
            || price.version.len() > 256
            || price
                .effective_to
                .is_some_and(|value| value <= price.effective_from)
        {
            return Err(BudgetStoreError::new(
                BudgetErrorKind::PricingUnavailable,
                "pricing snapshot 的来源、版本或生效时间非法",
            ));
        }
    }
    Ok(())
}

fn validate_dispatch_command(command: &MarkBudgetDispatching) -> Result<(), BudgetStoreError> {
    validate_request_id(&command.request_id)?;
    validate_operation_id(
        &command.operation_id,
        &format!("budget-dispatch:v1:{}", command.request_id),
        "dispatch operation",
    )
}

fn validate_lookup_command(command: &LookupBudgetIntent) -> Result<(), BudgetStoreError> {
    validate_request_id(&command.request_id)?;
    validate_operation_id(
        &command.operation_id,
        &format!("intent:v1:{}", command.request_id),
        "intent lookup operation",
    )?;
    validate_fingerprint(
        &command.command_fingerprint,
        "intent lookup command fingerprint",
    )?;
    validate_fingerprint(
        &command.pricing_fingerprint,
        "intent lookup pricing fingerprint",
    )
}

fn validate_settle_command(command: &SettleBudgetIntent) -> Result<(), BudgetStoreError> {
    validate_request_id(&command.request_id)?;
    validate_operation_id(
        &command.operation_id,
        &format!("budget-settle:v1:{}", command.request_id),
        "settlement operation",
    )?;
    validate_fingerprint(
        &command.command_fingerprint,
        "settlement command fingerprint",
    )?;
    validate_fingerprint(&command.pricing_fingerprint, "pricing fingerprint")?;
    validate_reasons(&command.cost.reasons)
}

fn validate_checkpoint_command(command: &CheckpointBudgetAccount) -> Result<(), BudgetStoreError> {
    if command.operation_id.trim().is_empty() || command.operation_id.len() > 128 {
        return Err(BudgetStoreError::corrupt(
            "checkpoint operation ID 长度必须在 1..=128",
        ));
    }
    Ok(())
}

fn checked_next_revision(account: &BudgetAccountSnapshot) -> Result<i64, BudgetStoreError> {
    account
        .checkpoint_tail_events
        .checked_add(1)
        .ok_or_else(|| BudgetStoreError::corrupt("预算 tail count 溢出"))?;
    account
        .accounting_revision
        .checked_add(1)
        .ok_or_else(|| BudgetStoreError::corrupt("预算 revision 溢出"))
}

fn settlement_fingerprint(
    request_id: &str,
    virtual_key_id: Uuid,
    intent_id: Uuid,
    status: CostStatus,
    amount_usd: Option<Decimal>,
    reasons: &[String],
    usage_fact_id: Option<Uuid>,
) -> String {
    let amount = amount_usd
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    sha256_hex(
        format!(
            "budget-settle:v1\n{request_id}\n{virtual_key_id}\n{intent_id}\n{}\n{amount}\n{}\n{}",
            status.as_str(),
            reasons.join("\u{1f}"),
            usage_fact_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "null".to_string())
        )
        .as_bytes(),
    )
}

fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn validate_request_id(value: &str) -> Result<(), BudgetStoreError> {
    if value.len() != 32
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(BudgetStoreError::corrupt(
            "request ID 必须是 32 位小写十六进制",
        ));
    }
    Ok(())
}

fn validate_operation_id(value: &str, expected: &str, field: &str) -> Result<(), BudgetStoreError> {
    if value.len() > 128 || value != expected {
        return Err(BudgetStoreError::corrupt(format!(
            "{field} 不符合固定幂等 ID 规则"
        )));
    }
    Ok(())
}

fn validate_fingerprint(value: &str, field: &str) -> Result<(), BudgetStoreError> {
    if value.len() != 64
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(BudgetStoreError::corrupt(format!(
            "{field} 必须是 64 位小写十六进制"
        )));
    }
    Ok(())
}

fn validate_reasons(reasons: &[String]) -> Result<(), BudgetStoreError> {
    if reasons.len() > 32
        || reasons
            .iter()
            .any(|reason| reason.trim().is_empty() || reason.len() > 128)
    {
        return Err(BudgetStoreError::corrupt("cost reasons 数量或长度超出限制"));
    }
    Ok(())
}

fn normalize_cost(cost: &BudgetCostOutcome) -> Result<Option<Decimal>, BudgetStoreError> {
    match cost.status {
        CostStatus::Calculated | CostStatus::Estimated => cost
            .amount_usd
            .ok_or_else(|| BudgetStoreError::corrupt("可计费 cost 缺少金额"))
            .and_then(|amount| {
                normalize_budget_amount(amount)
                    .map(Some)
                    .map_err(BudgetStoreError::corrupt)
            }),
        CostStatus::NotIncurred => {
            if cost
                .amount_usd
                .is_some_and(|amount| amount != Decimal::ZERO)
            {
                return Err(BudgetStoreError::corrupt("not_incurred cost 必须为空或 0"));
            }
            Ok(Some(Decimal::ZERO))
        }
        CostStatus::Unavailable => cost
            .amount_usd
            .map(normalize_budget_amount)
            .transpose()
            .map_err(BudgetStoreError::corrupt),
    }
}

fn nonempty_unavailable_reasons(reasons: &[String]) -> Vec<String> {
    if reasons.is_empty() {
        vec!["cost_unavailable".to_string()]
    } else {
        reasons.to_vec()
    }
}

fn validate_owner_duration(duration: Duration) -> Result<(), BudgetStoreError> {
    validate_positive_duration(duration, "owner/stale duration")?;
    if duration.as_secs() == 0 {
        return Err(BudgetStoreError::corrupt(
            "owner/stale duration 不能小于 1 秒",
        ));
    }
    if duration > Duration::from_secs(7 * 24 * 60 * 60) {
        return Err(BudgetStoreError::corrupt(
            "owner/stale duration 不能超过 7 天",
        ));
    }
    Ok(())
}

fn validate_positive_duration(duration: Duration, field: &str) -> Result<(), BudgetStoreError> {
    if duration.as_millis() == 0 {
        return Err(BudgetStoreError::corrupt(format!("{field} 不能小于 1ms")));
    }
    Ok(())
}

fn duration_as_pg_millis(duration: Duration) -> Result<String, BudgetStoreError> {
    let millis = u64::try_from(duration.as_millis())
        .map_err(|_| BudgetStoreError::corrupt("数据库 timeout 过大"))?;
    if millis == 0 {
        return Err(BudgetStoreError::corrupt("数据库 timeout 不能小于 1ms"));
    }
    Ok(format!("{millis}ms"))
}

pub(crate) async fn commit(transaction: Transaction<'_, Postgres>) -> Result<(), BudgetStoreError> {
    transaction.commit().await.map_err(map_commit_error)
}

fn map_commit_error(error: sqlx::Error) -> BudgetStoreError {
    if sqlx_outcome_unknown(&error) {
        BudgetStoreError::new(
            BudgetErrorKind::OutcomeUnknown,
            "预算事务提交结果未知，请使用相同 operation ID 查询或重放",
        )
    } else {
        map_sqlx_error(error)
    }
}

pub(crate) fn map_sqlx_error(error: sqlx::Error) -> BudgetStoreError {
    let sqlstate = error
        .as_database_error()
        .and_then(|database_error| database_error.code())
        .map(|code| code.into_owned());
    let kind = match sqlstate.as_deref() {
        Some("57014" | "55P03" | "40P01" | "40001") => BudgetErrorKind::AccountingUnavailable,
        Some("22003") => BudgetErrorKind::NumericOverflow,
        Some(code)
            if code.starts_with("08") || code.starts_with("53") || code.starts_with("57P") =>
        {
            BudgetErrorKind::AccountingUnavailable
        }
        Some("23505" | "23514" | "23503" | "23502") => BudgetErrorKind::Corrupt,
        Some(_) => BudgetErrorKind::AccountingUnavailable,
        None if matches!(
            error,
            sqlx::Error::PoolTimedOut
                | sqlx::Error::PoolClosed
                | sqlx::Error::WorkerCrashed
                | sqlx::Error::Io(_)
                | sqlx::Error::Tls(_)
                | sqlx::Error::Protocol(_)
                | sqlx::Error::BeginFailed
        ) =>
        {
            BudgetErrorKind::AccountingUnavailable
        }
        None => BudgetErrorKind::AccountingUnavailable,
    };
    tracing::warn!(error = %error, ?kind, "预算 PostgreSQL 操作失败");
    BudgetStoreError::new(kind, "预算 PostgreSQL 操作失败")
}

fn sqlx_outcome_unknown(error: &sqlx::Error) -> bool {
    if matches!(
        error,
        sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::Protocol(_)
            | sqlx::Error::PoolClosed
            | sqlx::Error::WorkerCrashed
            | sqlx::Error::BeginFailed
    ) {
        return true;
    }
    error
        .as_database_error()
        .and_then(|database_error| database_error.code())
        .is_none_or(|code| {
            code.starts_with("08") || matches!(code.as_ref(), "57P01" | "57P02" | "57P03")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::model::{
        BudgetPricingSnapshot, BudgetSettlementDisposition, BUDGET_SCHEMA_VERSION,
    };
    use sqlx::postgres::PgPoolOptions;

    fn test_create_command(
        intent_id: Uuid,
        virtual_key_id: Uuid,
        request_id: Arc<str>,
        node_id: Uuid,
        owner_session_id: Uuid,
        fingerprint_byte: char,
    ) -> CreateBudgetIntent {
        CreateBudgetIntent {
            intent_id,
            virtual_key_id,
            operation_id: Arc::from(format!("intent:v1:{request_id}")),
            request_id,
            command_fingerprint: Arc::from(fingerprint_byte.to_string().repeat(64)),
            pricing_fingerprint: Arc::from("c".repeat(64)),
            pricing_snapshot: BudgetPricingSnapshot {
                schema_version: BUDGET_SCHEMA_VERSION,
                provider_type: "openai".to_string(),
                model: "gpt-test".to_string(),
                input: None,
                output: None,
                max_prompt_tokens: None,
            },
            node_id,
            owner_session_id,
            stale_after: Duration::from_secs(120),
        }
    }

    #[test]
    fn config_rejects_invalid_authoritative_values() {
        let config = PgBudgetStoreConfig {
            deployment_namespace: Arc::from(""),
            checkpoint_hard_tail_events: 0,
            config_fingerprint: Arc::from("x"),
            statement_timeout: Duration::ZERO,
            lock_timeout: Duration::from_secs(1),
        };
        assert_eq!(
            config.validate().unwrap_err().kind(),
            BudgetErrorKind::Corrupt
        );
    }

    #[test]
    fn fixed_operation_ids_and_fingerprints_are_validated() {
        let request_id: Arc<str> = Arc::from("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let command = MarkBudgetDispatching {
            intent_id: Uuid::nil(),
            virtual_key_id: Uuid::nil(),
            request_id: Arc::clone(&request_id),
            operation_id: Arc::from(format!("budget-dispatch:v1:{request_id}")),
            node_id: Uuid::nil(),
            owner_session_id: Uuid::nil(),
        };
        assert!(validate_dispatch_command(&command).is_ok());

        let invalid = MarkBudgetDispatching {
            operation_id: Arc::from("new-random-id"),
            ..command
        };
        assert_eq!(
            validate_dispatch_command(&invalid).unwrap_err().kind(),
            BudgetErrorKind::Corrupt
        );
    }

    #[test]
    fn cost_status_requires_consistent_amount() {
        assert_eq!(
            normalize_cost(&BudgetCostOutcome {
                status: CostStatus::Calculated,
                amount_usd: None,
                reasons: Vec::new(),
            })
            .unwrap_err()
            .kind(),
            BudgetErrorKind::Corrupt
        );
        assert_eq!(
            normalize_cost(&BudgetCostOutcome {
                status: CostStatus::NotIncurred,
                amount_usd: Some(Decimal::ONE),
                reasons: Vec::new(),
            })
            .unwrap_err()
            .kind(),
            BudgetErrorKind::Corrupt
        );
    }

    #[tokio::test]
    async fn postgres_hot_path_is_atomic_idempotent_and_owner_fenced() {
        let Some(database_url) = crate::budget::postgres_test_url() else {
            return;
        };
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await
            .unwrap();
        let suffix = Uuid::new_v4().simple().to_string();
        let namespace: Arc<str> = Arc::from(format!("budget-test-{suffix}"));
        let store = PgBudgetStore::new(
            pool.clone(),
            PgBudgetStoreConfig {
                deployment_namespace: Arc::clone(&namespace),
                checkpoint_hard_tail_events: 1000,
                config_fingerprint: Arc::from("a".repeat(64)),
                statement_timeout: Duration::from_secs(2),
                lock_timeout: Duration::from_secs(1),
            },
        )
        .unwrap();
        let key_id = Uuid::new_v4();
        let node_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let intent_id = Uuid::new_v4();
        let request_id: Arc<str> = Arc::from(Uuid::new_v4().simple().to_string());

        sqlx::query(
            "INSERT INTO ai_virtual_keys (\
                 id, name, key_hash, key_prefix, budget_limit, ws_id\
             ) SELECT $1, $2, $3, $4, 100.000000000000, id \
                 FROM workspaces WHERE name = 'default' LIMIT 1",
        )
        .bind(key_id)
        .bind(format!("budget-key-{suffix}"))
        .bind(format!("budget-hash-{suffix}"))
        .bind("sk-test")
        .execute(&pool)
        .await
        .unwrap();

        let lease = store
            .register_owner(RegisterBudgetOwner {
                session_id,
                node_id,
                lease_duration: Duration::from_secs(60),
            })
            .await
            .unwrap();
        assert!(!lease.replayed);
        let replayed_lease = store
            .register_owner(RegisterBudgetOwner {
                session_id,
                node_id,
                lease_duration: Duration::from_secs(60),
            })
            .await
            .unwrap();
        assert!(replayed_lease.replayed);
        let mismatched_store = PgBudgetStore::new(
            pool.clone(),
            PgBudgetStoreConfig {
                deployment_namespace: Arc::clone(&namespace),
                checkpoint_hard_tail_events: 1001,
                config_fingerprint: Arc::from("9".repeat(64)),
                statement_timeout: Duration::from_secs(2),
                lock_timeout: Duration::from_secs(1),
            },
        )
        .unwrap();
        assert_eq!(
            mismatched_store
                .register_owner(RegisterBudgetOwner {
                    session_id: Uuid::new_v4(),
                    node_id: Uuid::new_v4(),
                    lease_duration: Duration::from_secs(60),
                })
                .await
                .unwrap_err()
                .kind(),
            BudgetErrorKind::AccountingUnavailable
        );

        let inspection = store
            .inspect(BudgetInspectCommand {
                virtual_key_id: key_id,
            })
            .await
            .unwrap();
        assert_eq!(inspection.eligibility, BudgetEligibility::Eligible);

        let create = CreateBudgetIntent {
            intent_id,
            virtual_key_id: key_id,
            request_id: Arc::clone(&request_id),
            operation_id: Arc::from(format!("intent:v1:{request_id}")),
            command_fingerprint: Arc::from("b".repeat(64)),
            pricing_fingerprint: Arc::from("c".repeat(64)),
            pricing_snapshot: BudgetPricingSnapshot {
                schema_version: BUDGET_SCHEMA_VERSION,
                provider_type: "openai".to_string(),
                model: "gpt-test".to_string(),
                input: None,
                output: None,
                max_prompt_tokens: None,
            },
            node_id,
            owner_session_id: session_id,
            stale_after: Duration::from_secs(120),
        };
        let created = store.create_intent(create.clone()).await.unwrap();
        assert_eq!(created.disposition, BudgetIntentDisposition::Created);
        assert_eq!(created.account.unwrap().pending_count, 1);
        let replayed = store.create_intent(create.clone()).await.unwrap();
        assert_eq!(
            replayed.disposition,
            BudgetIntentDisposition::CreateReplayed
        );
        assert_eq!(replayed.account.unwrap().pending_count, 1);

        let dispatch = MarkBudgetDispatching {
            intent_id,
            virtual_key_id: key_id,
            request_id: Arc::clone(&request_id),
            operation_id: Arc::from(format!("budget-dispatch:v1:{request_id}")),
            node_id,
            owner_session_id: session_id,
        };
        assert_eq!(
            store
                .mark_dispatching(dispatch.clone())
                .await
                .unwrap()
                .disposition,
            BudgetIntentDisposition::DispatchApplied
        );
        assert_eq!(
            store.mark_dispatching(dispatch).await.unwrap().disposition,
            BudgetIntentDisposition::DispatchReplayed
        );

        let settle = SettleBudgetIntent {
            intent_id,
            virtual_key_id: key_id,
            request_id: Arc::clone(&request_id),
            operation_id: Arc::from(format!("budget-settle:v1:{request_id}")),
            command_fingerprint: Arc::from("d".repeat(64)),
            pricing_fingerprint: Arc::from("c".repeat(64)),
            usage_fact_id: Some(Uuid::new_v4()),
            cost: BudgetCostOutcome {
                status: CostStatus::Calculated,
                amount_usd: Some(Decimal::new(125, 2)),
                reasons: Vec::new(),
            },
        };
        let settled = store.settle(settle.clone()).await.unwrap();
        assert_eq!(settled.disposition, BudgetSettlementDisposition::Applied);
        let settled_account = settled.account.unwrap();
        assert_eq!(settled_account.used_usd, Decimal::new(125, 2));
        assert_eq!(settled_account.pending_count, 0);
        assert_eq!(
            store.settle(settle).await.unwrap().disposition,
            BudgetSettlementDisposition::Replayed
        );

        let late_request: Arc<str> = Arc::from(Uuid::new_v4().simple().to_string());
        let late_intent_id = Uuid::new_v4();
        let late_create = CreateBudgetIntent {
            intent_id: late_intent_id,
            request_id: Arc::clone(&late_request),
            operation_id: Arc::from(format!("intent:v1:{late_request}")),
            command_fingerprint: Arc::from("e".repeat(64)),
            ..create.clone()
        };
        store.create_intent(late_create).await.unwrap();
        store
            .mark_dispatching(MarkBudgetDispatching {
                intent_id: late_intent_id,
                virtual_key_id: key_id,
                request_id: Arc::clone(&late_request),
                operation_id: Arc::from(format!("budget-dispatch:v1:{late_request}")),
                node_id,
                owner_session_id: session_id,
            })
            .await
            .unwrap();
        let unavailable = SettleBudgetIntent {
            intent_id: late_intent_id,
            virtual_key_id: key_id,
            request_id: Arc::clone(&late_request),
            operation_id: Arc::from(format!("budget-settle:v1:{late_request}")),
            command_fingerprint: Arc::from("f".repeat(64)),
            pricing_fingerprint: Arc::from("c".repeat(64)),
            usage_fact_id: Some(Uuid::new_v4()),
            cost: BudgetCostOutcome {
                status: CostStatus::Unavailable,
                amount_usd: None,
                reasons: vec!["missing_usage".to_string()],
            },
        };
        assert_eq!(
            store.settle(unavailable.clone()).await.unwrap().disposition,
            BudgetSettlementDisposition::MarkedUnresolved
        );
        assert_eq!(
            store
                .inspect(BudgetInspectCommand {
                    virtual_key_id: key_id,
                })
                .await
                .unwrap_err()
                .kind(),
            BudgetErrorKind::AccountingUnresolved
        );
        assert_eq!(
            store.settle(unavailable).await.unwrap().disposition,
            BudgetSettlementDisposition::AlreadyUnresolved
        );
        let strict_constraint_error = sqlx::query(
            "UPDATE ai_budget_ledger \
                SET terminal_operation_id = $2, terminal_command_fingerprint = $3 \
              WHERE id = $1",
        )
        .bind(late_intent_id)
        .bind(format!("budget-settle:v1:{late_request}"))
        .bind("8".repeat(64))
        .execute(&pool)
        .await
        .unwrap_err();
        assert_eq!(
            strict_constraint_error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("23514")
        );
        let late_settlement = SettleBudgetIntent {
            intent_id: late_intent_id,
            virtual_key_id: key_id,
            request_id: Arc::clone(&late_request),
            operation_id: Arc::from(format!("budget-settle:v1:{late_request}")),
            command_fingerprint: Arc::from("1".repeat(64)),
            pricing_fingerprint: Arc::from("c".repeat(64)),
            usage_fact_id: Some(Uuid::new_v4()),
            cost: BudgetCostOutcome {
                status: CostStatus::Calculated,
                amount_usd: Some(Decimal::new(75, 2)),
                reasons: Vec::new(),
            },
        };
        let late_settled = store.settle(late_settlement).await.unwrap();
        assert_eq!(
            late_settled.disposition,
            BudgetSettlementDisposition::Applied
        );
        assert_eq!(late_settled.account.unwrap().used_usd, Decimal::new(200, 2));

        let local_request: Arc<str> = Arc::from(Uuid::new_v4().simple().to_string());
        let local_intent_id = Uuid::new_v4();
        let concurrent_create = CreateBudgetIntent {
            intent_id: local_intent_id,
            request_id: Arc::clone(&local_request),
            operation_id: Arc::from(format!("intent:v1:{local_request}")),
            command_fingerprint: Arc::from("2".repeat(64)),
            ..create.clone()
        };
        let (first_create, second_create) = tokio::join!(
            store.create_intent(concurrent_create.clone()),
            store.create_intent(concurrent_create)
        );
        let mut dispositions = [
            first_create.unwrap().disposition,
            second_create.unwrap().disposition,
        ];
        dispositions.sort_by_key(|value| match value {
            BudgetIntentDisposition::Created => 0,
            BudgetIntentDisposition::CreateReplayed => 1,
            _ => 2,
        });
        assert_eq!(
            dispositions,
            [
                BudgetIntentDisposition::Created,
                BudgetIntentDisposition::CreateReplayed
            ]
        );
        let local_settled = store
            .settle(SettleBudgetIntent {
                intent_id: local_intent_id,
                virtual_key_id: key_id,
                request_id: Arc::clone(&local_request),
                operation_id: Arc::from(format!("budget-settle:v1:{local_request}")),
                command_fingerprint: Arc::from("3".repeat(64)),
                pricing_fingerprint: Arc::from("c".repeat(64)),
                usage_fact_id: None,
                cost: BudgetCostOutcome {
                    status: CostStatus::NotIncurred,
                    amount_usd: None,
                    reasons: Vec::new(),
                },
            })
            .await
            .unwrap();
        assert_eq!(
            local_settled.disposition,
            BudgetSettlementDisposition::Applied
        );
        assert_eq!(local_settled.account.unwrap().pending_count, 0);

        let overflow_key_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO ai_virtual_keys (\
                 id, name, key_hash, key_prefix, budget_limit, budget_used, ws_id\
             ) SELECT $1, $2, $3, $4, \
                      9999999999999999.999999999999, \
                      9999999999999999.500000000000, id \
                 FROM workspaces WHERE name = 'default' LIMIT 1",
        )
        .bind(overflow_key_id)
        .bind(format!("budget-overflow-{suffix}"))
        .bind(format!("budget-overflow-hash-{suffix}"))
        .bind("sk-over")
        .execute(&pool)
        .await
        .unwrap();
        let overflow_request: Arc<str> = Arc::from(Uuid::new_v4().simple().to_string());
        let overflow_intent_id = Uuid::new_v4();
        store
            .create_intent(CreateBudgetIntent {
                intent_id: overflow_intent_id,
                virtual_key_id: overflow_key_id,
                request_id: Arc::clone(&overflow_request),
                operation_id: Arc::from(format!("intent:v1:{overflow_request}")),
                command_fingerprint: Arc::from("5".repeat(64)),
                ..create.clone()
            })
            .await
            .unwrap();
        store
            .mark_dispatching(MarkBudgetDispatching {
                intent_id: overflow_intent_id,
                virtual_key_id: overflow_key_id,
                request_id: Arc::clone(&overflow_request),
                operation_id: Arc::from(format!("budget-dispatch:v1:{overflow_request}")),
                node_id,
                owner_session_id: session_id,
            })
            .await
            .unwrap();
        let overflow_settlement = SettleBudgetIntent {
            intent_id: overflow_intent_id,
            virtual_key_id: overflow_key_id,
            request_id: Arc::clone(&overflow_request),
            operation_id: Arc::from(format!("budget-settle:v1:{overflow_request}")),
            command_fingerprint: Arc::from("6".repeat(64)),
            pricing_fingerprint: Arc::from("c".repeat(64)),
            usage_fact_id: Some(Uuid::new_v4()),
            cost: BudgetCostOutcome {
                status: CostStatus::Calculated,
                amount_usd: Some(Decimal::ONE),
                reasons: Vec::new(),
            },
        };
        let overflow = store.settle(overflow_settlement.clone()).await.unwrap();
        assert_eq!(
            overflow.disposition,
            BudgetSettlementDisposition::MarkedUnresolved
        );
        let overflow_account = overflow.account.unwrap();
        let replay = store.settle(overflow_settlement.clone()).await.unwrap();
        assert_eq!(
            replay.disposition,
            BudgetSettlementDisposition::AlreadyUnresolved
        );
        let replay_account = replay.account.unwrap();
        assert_eq!(
            replay_account.accounting_revision,
            overflow_account.accounting_revision
        );
        assert_eq!(
            replay_account.checkpoint_tail_events,
            overflow_account.checkpoint_tail_events
        );
        assert_eq!(
            replay.intent.last_account_revision,
            overflow_account.accounting_revision
        );
        let conflicting_overflow = SettleBudgetIntent {
            command_fingerprint: Arc::from("7".repeat(64)),
            ..overflow_settlement
        };
        assert_eq!(
            store.settle(conflicting_overflow).await.unwrap_err().kind(),
            BudgetErrorKind::Corrupt
        );
        assert_eq!(
            store
                .inspect(BudgetInspectCommand {
                    virtual_key_id: overflow_key_id,
                })
                .await
                .unwrap_err()
                .kind(),
            BudgetErrorKind::AccountingUnresolved
        );

        // key-only Admin revision 不产生 ledger tail；后续真实 settlement 仍必须能
        // checkpoint，不能把 revision delta 错当成 tail event 数。
        let revision_gap_key_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO ai_virtual_keys (\
                 id, name, key_hash, key_prefix, budget_limit, ws_id\
             ) SELECT $1, $2, $3, $4, 100.000000000000, id \
                 FROM workspaces WHERE name = 'default' LIMIT 1",
        )
        .bind(revision_gap_key_id)
        .bind(format!("budget-revision-gap-{suffix}"))
        .bind(format!("budget-revision-gap-hash-{suffix}"))
        .bind("sk-gap")
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO ai_budget_checkpoints (\
                 virtual_key_id, checkpoint_revision, accounted_cost_usd, operation_id\
             ) VALUES ($1, 0, 0.000000000000, $2)",
        )
        .bind(revision_gap_key_id)
        .bind(format!(
            "budget-checkpoint-genesis:v1:{revision_gap_key_id}"
        ))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE ai_virtual_keys \
                SET name = name || '-updated', budget_accounting_revision = 1 \
              WHERE id = $1",
        )
        .bind(revision_gap_key_id)
        .execute(&pool)
        .await
        .unwrap();
        let gap_request: Arc<str> = Arc::from(Uuid::new_v4().simple().to_string());
        let gap_intent_id = Uuid::new_v4();
        store
            .create_intent(test_create_command(
                gap_intent_id,
                revision_gap_key_id,
                Arc::clone(&gap_request),
                node_id,
                session_id,
                '8',
            ))
            .await
            .unwrap();
        store
            .mark_dispatching(MarkBudgetDispatching {
                intent_id: gap_intent_id,
                virtual_key_id: revision_gap_key_id,
                request_id: Arc::clone(&gap_request),
                operation_id: Arc::from(format!("budget-dispatch:v1:{gap_request}")),
                node_id,
                owner_session_id: session_id,
            })
            .await
            .unwrap();
        let gap_settled = store
            .settle(SettleBudgetIntent {
                intent_id: gap_intent_id,
                virtual_key_id: revision_gap_key_id,
                request_id: Arc::clone(&gap_request),
                operation_id: Arc::from(format!("budget-settle:v1:{gap_request}")),
                command_fingerprint: Arc::from("9".repeat(64)),
                pricing_fingerprint: Arc::from("c".repeat(64)),
                usage_fact_id: None,
                cost: BudgetCostOutcome {
                    status: CostStatus::Calculated,
                    amount_usd: Some(Decimal::ONE),
                    reasons: Vec::new(),
                },
            })
            .await
            .unwrap();
        let gap_account = gap_settled.account.unwrap();
        let checkpoint = store
            .checkpoint_account(CheckpointBudgetAccount {
                virtual_key_id: revision_gap_key_id,
                operation_id: Arc::from(format!(
                    "budget-checkpoint:v1:{revision_gap_key_id}:{}",
                    gap_account.accounting_revision
                )),
            })
            .await
            .unwrap();
        assert_eq!(checkpoint.revision, gap_account.accounting_revision);

        let lookup_request: Arc<str> = Arc::from(Uuid::new_v4().simple().to_string());
        let lookup_intent_id = Uuid::new_v4();
        let lookup_create = test_create_command(
            lookup_intent_id,
            key_id,
            Arc::clone(&lookup_request),
            node_id,
            session_id,
            'a',
        );
        store.create_intent(lookup_create.clone()).await.unwrap();

        store
            .heartbeat_owner(HeartbeatBudgetOwner {
                session_id,
                node_id,
                lease_duration: Duration::from_secs(60),
            })
            .await
            .unwrap();
        store
            .stop_owner(StopBudgetOwner {
                session_id,
                node_id,
            })
            .await
            .unwrap();
        store
            .stop_owner(StopBudgetOwner {
                session_id,
                node_id,
            })
            .await
            .unwrap();
        let looked_up = store
            .lookup_intent((&lookup_create).into())
            .await
            .unwrap()
            .expect("owner 被 fence 后仍应能只读确认已提交 intent");
        assert_eq!(looked_up.id, lookup_intent_id);
        assert_eq!(looked_up.state, BudgetIntentState::Prepared);
        let missing_request: Arc<str> = Arc::from(Uuid::new_v4().simple().to_string());
        assert!(store
            .lookup_intent(LookupBudgetIntent {
                intent_id: Uuid::new_v4(),
                virtual_key_id: key_id,
                request_id: Arc::clone(&missing_request),
                operation_id: Arc::from(format!("intent:v1:{missing_request}")),
                command_fingerprint: Arc::from("0".repeat(64)),
                pricing_fingerprint: Arc::from("c".repeat(64)),
                node_id,
                owner_session_id: session_id,
            })
            .await
            .unwrap()
            .is_none());
        let recovered = store
            .settle(SettleBudgetIntent {
                intent_id: lookup_intent_id,
                virtual_key_id: key_id,
                request_id: Arc::clone(&lookup_request),
                operation_id: Arc::from(format!("budget-settle:v1:{lookup_request}")),
                command_fingerprint: Arc::from("a".repeat(64)),
                pricing_fingerprint: Arc::from("c".repeat(64)),
                usage_fact_id: None,
                cost: BudgetCostOutcome {
                    status: CostStatus::NotIncurred,
                    amount_usd: Some(Decimal::ZERO),
                    reasons: vec!["request_lifecycle_cancelled_before_dispatch".to_string()],
                },
            })
            .await
            .unwrap();
        assert_eq!(recovered.disposition, BudgetSettlementDisposition::Applied);
        assert_eq!(
            store
                .heartbeat_owner(HeartbeatBudgetOwner {
                    session_id,
                    node_id,
                    lease_duration: Duration::from_secs(60),
                })
                .await
                .unwrap_err()
                .kind(),
            BudgetErrorKind::AccountingUnavailable
        );

        let fenced_request: Arc<str> = Arc::from(Uuid::new_v4().simple().to_string());
        let fenced = CreateBudgetIntent {
            intent_id: Uuid::new_v4(),
            request_id: Arc::clone(&fenced_request),
            operation_id: Arc::from(format!("intent:v1:{fenced_request}")),
            command_fingerprint: Arc::from("4".repeat(64)),
            ..create
        };
        assert_eq!(
            store.create_intent(fenced).await.unwrap_err().kind(),
            BudgetErrorKind::AccountingUnavailable
        );

        sqlx::query("UPDATE ai_virtual_keys SET budget_limit = budget_used WHERE id = $1")
            .bind(key_id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            store
                .inspect(BudgetInspectCommand {
                    virtual_key_id: key_id,
                })
                .await
                .unwrap()
                .eligibility,
            BudgetEligibility::Exhausted
        );
        sqlx::query("UPDATE ai_virtual_keys SET budget_limit = NULL WHERE id = $1")
            .bind(key_id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            store
                .inspect(BudgetInspectCommand {
                    virtual_key_id: key_id,
                })
                .await
                .unwrap()
                .eligibility,
            BudgetEligibility::Paused
        );

        // 所有旧 owner 停止后，新的合法配置可原子接管 deployment settings。
        let upgraded_session_id = Uuid::new_v4();
        let upgraded_node_id = Uuid::new_v4();
        mismatched_store
            .register_owner(RegisterBudgetOwner {
                session_id: upgraded_session_id,
                node_id: upgraded_node_id,
                lease_duration: Duration::from_secs(60),
            })
            .await
            .unwrap();
        mismatched_store
            .stop_owner(StopBudgetOwner {
                session_id: upgraded_session_id,
                node_id: upgraded_node_id,
            })
            .await
            .unwrap();

        sqlx::query("DELETE FROM ai_budget_ledger WHERE virtual_key_id = ANY($1)")
            .bind(vec![key_id, overflow_key_id, revision_gap_key_id])
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ai_budget_checkpoints WHERE virtual_key_id = $1")
            .bind(revision_gap_key_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ai_virtual_keys WHERE id = ANY($1)")
            .bind(vec![key_id, overflow_key_id, revision_gap_key_id])
            .execute(&pool)
            .await
            .unwrap();
        // owner/settings 使用独立清理，避免测试共享数据库时留下 deployment 状态。
        sqlx::query(
            "DELETE FROM ai_budget_owner_sessions \
              WHERE deployment_namespace = $1",
        )
        .bind(namespace.as_ref())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("DELETE FROM ai_budget_runtime_settings WHERE deployment_namespace = $1")
            .bind(namespace.as_ref())
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn postgres_recovery_is_fenced_and_checkpoint_compacts_bounded_tail() {
        let Some(database_url) = crate::budget::postgres_test_url() else {
            return;
        };
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await
            .unwrap();
        let suffix = Uuid::new_v4().simple().to_string();
        let namespace: Arc<str> = Arc::from(format!("budget-recovery-{suffix}"));
        let foreign_namespace: Arc<str> = Arc::from(format!("budget-recovery-foreign-{suffix}"));
        let store = PgBudgetStore::new(
            pool.clone(),
            PgBudgetStoreConfig {
                deployment_namespace: Arc::clone(&namespace),
                checkpoint_hard_tail_events: 4,
                config_fingerprint: Arc::from("a".repeat(64)),
                statement_timeout: Duration::from_secs(3),
                lock_timeout: Duration::from_secs(1),
            },
        )
        .unwrap();
        let foreign_store = PgBudgetStore::new(
            pool.clone(),
            PgBudgetStoreConfig {
                deployment_namespace: Arc::clone(&foreign_namespace),
                checkpoint_hard_tail_events: 4,
                config_fingerprint: Arc::from("b".repeat(64)),
                statement_timeout: Duration::from_secs(3),
                lock_timeout: Duration::from_secs(1),
            },
        )
        .unwrap();
        let node_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let foreign_node_id = Uuid::new_v4();
        let foreign_session_id = Uuid::new_v4();
        store
            .register_owner(RegisterBudgetOwner {
                session_id,
                node_id,
                lease_duration: Duration::from_secs(60),
            })
            .await
            .unwrap();
        foreign_store
            .register_owner(RegisterBudgetOwner {
                session_id: foreign_session_id,
                node_id: foreign_node_id,
                lease_duration: Duration::from_secs(60),
            })
            .await
            .unwrap();

        let prepared_key_id = Uuid::new_v4();
        let dispatching_key_id = Uuid::new_v4();
        let checkpoint_key_id = Uuid::new_v4();
        let foreign_key_id = Uuid::new_v4();
        let key_ids = [
            prepared_key_id,
            dispatching_key_id,
            checkpoint_key_id,
            foreign_key_id,
        ];
        for (index, key_id) in key_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO ai_virtual_keys (\
                     id, name, key_hash, key_prefix, budget_limit, ws_id\
                 ) SELECT $1, $2, $3, $4, 100.000000000000, id \
                     FROM workspaces WHERE name = 'default' LIMIT 1",
            )
            .bind(*key_id)
            .bind(format!("budget-recovery-{suffix}-{index}"))
            .bind(format!("budget-recovery-hash-{suffix}-{index}"))
            .bind(format!("sk-r{index}"))
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO ai_budget_checkpoints (\
                     virtual_key_id, checkpoint_revision, accounted_cost_usd, operation_id\
                 ) VALUES ($1, 0, 0.000000000000, $2)",
            )
            .bind(*key_id)
            .bind(format!("budget-checkpoint-genesis:v1:{key_id}"))
            .execute(&pool)
            .await
            .unwrap();
        }

        let prepared_intent_id = Uuid::new_v4();
        let prepared_request: Arc<str> = Arc::from(Uuid::new_v4().simple().to_string());
        store
            .create_intent(test_create_command(
                prepared_intent_id,
                prepared_key_id,
                Arc::clone(&prepared_request),
                node_id,
                session_id,
                '1',
            ))
            .await
            .unwrap();

        let dispatching_intent_id = Uuid::new_v4();
        let dispatching_request: Arc<str> = Arc::from(Uuid::new_v4().simple().to_string());
        store
            .create_intent(test_create_command(
                dispatching_intent_id,
                dispatching_key_id,
                Arc::clone(&dispatching_request),
                node_id,
                session_id,
                '2',
            ))
            .await
            .unwrap();
        store
            .mark_dispatching(MarkBudgetDispatching {
                intent_id: dispatching_intent_id,
                virtual_key_id: dispatching_key_id,
                request_id: Arc::clone(&dispatching_request),
                operation_id: Arc::from(format!("budget-dispatch:v1:{dispatching_request}")),
                node_id,
                owner_session_id: session_id,
            })
            .await
            .unwrap();

        let checkpoint_intent_id = Uuid::new_v4();
        let checkpoint_request: Arc<str> = Arc::from(Uuid::new_v4().simple().to_string());
        store
            .create_intent(test_create_command(
                checkpoint_intent_id,
                checkpoint_key_id,
                Arc::clone(&checkpoint_request),
                node_id,
                session_id,
                '3',
            ))
            .await
            .unwrap();
        store
            .mark_dispatching(MarkBudgetDispatching {
                intent_id: checkpoint_intent_id,
                virtual_key_id: checkpoint_key_id,
                request_id: Arc::clone(&checkpoint_request),
                operation_id: Arc::from(format!("budget-dispatch:v1:{checkpoint_request}")),
                node_id,
                owner_session_id: session_id,
            })
            .await
            .unwrap();
        store
            .settle(SettleBudgetIntent {
                intent_id: checkpoint_intent_id,
                virtual_key_id: checkpoint_key_id,
                request_id: Arc::clone(&checkpoint_request),
                operation_id: Arc::from(format!("budget-settle:v1:{checkpoint_request}")),
                command_fingerprint: Arc::from("4".repeat(64)),
                pricing_fingerprint: Arc::from("c".repeat(64)),
                usage_fact_id: None,
                cost: BudgetCostOutcome {
                    status: CostStatus::Calculated,
                    amount_usd: Some(Decimal::new(250, 2)),
                    reasons: Vec::new(),
                },
            })
            .await
            .unwrap();

        let foreign_intent_id = Uuid::new_v4();
        let foreign_request: Arc<str> = Arc::from(Uuid::new_v4().simple().to_string());
        foreign_store
            .create_intent(test_create_command(
                foreign_intent_id,
                foreign_key_id,
                Arc::clone(&foreign_request),
                foreign_node_id,
                foreign_session_id,
                '5',
            ))
            .await
            .unwrap();

        store
            .stop_owner(StopBudgetOwner {
                session_id,
                node_id,
            })
            .await
            .unwrap();
        foreign_store
            .stop_owner(StopBudgetOwner {
                session_id: foreign_session_id,
                node_id: foreign_node_id,
            })
            .await
            .unwrap();
        sqlx::query(
            "UPDATE ai_budget_ledger \
                SET stale_not_before = clock_timestamp() - interval '1 second' \
              WHERE id = ANY($1)",
        )
        .bind(vec![
            prepared_intent_id,
            dispatching_intent_id,
            foreign_intent_id,
        ])
        .execute(&pool)
        .await
        .unwrap();

        // 两个 scanner 可同时看到候选，但 key-first CAS 只允许各 transition 一次。
        let (first, second) = tokio::join!(
            store.recover_stale(RecoverStaleBudgetIntents { max_intents: 10 }),
            store.recover_stale(RecoverStaleBudgetIntents { max_intents: 10 })
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first.settled_not_incurred + second.settled_not_incurred, 1);
        assert_eq!(first.marked_unresolved + second.marked_unresolved, 1);
        assert!(first.scanned + second.scanned >= 2);
        assert_eq!(
            store
                .recover_stale(RecoverStaleBudgetIntents { max_intents: 10 })
                .await
                .unwrap(),
            BudgetRecoveryBatch::default()
        );

        let prepared_status: (String, Option<Decimal>, Option<String>) = sqlx::query_as(
            "SELECT status, accounted_cost_usd, cost_status \
               FROM ai_budget_ledger WHERE id = $1",
        )
        .bind(prepared_intent_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(prepared_status.0, "settled");
        assert_eq!(prepared_status.1, Some(Decimal::ZERO));
        assert_eq!(prepared_status.2.as_deref(), Some("not_incurred"));
        let dispatching_status: String =
            sqlx::query_scalar("SELECT status FROM ai_budget_ledger WHERE id = $1")
                .bind(dispatching_intent_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(dispatching_status, "unresolved");
        let foreign_status: String =
            sqlx::query_scalar("SELECT status FROM ai_budget_ledger WHERE id = $1")
                .bind(foreign_intent_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(foreign_status, "pending");
        assert_eq!(
            store
                .register_owner(RegisterBudgetOwner {
                    session_id,
                    node_id,
                    lease_duration: Duration::from_secs(60),
                })
                .await
                .unwrap_err()
                .kind(),
            BudgetErrorKind::AccountingUnavailable
        );

        let checkpoint_command = CheckpointBudgetAccount {
            virtual_key_id: checkpoint_key_id,
            operation_id: Arc::from(format!("budget-checkpoint:v1:{checkpoint_key_id}:2")),
        };
        let due = store.checkpoint_due_accounts(2, 10).await.unwrap();
        assert!(due.scanned >= 1);
        assert!(due.checkpointed >= 1);
        let checkpoint = store
            .checkpoint_account(checkpoint_command.clone())
            .await
            .unwrap();
        assert_eq!(checkpoint.revision, 2);
        assert_eq!(checkpoint.accounted_cost_usd, Decimal::new(250, 2));
        let checkpoint_replay = store.checkpoint_account(checkpoint_command).await.unwrap();
        assert_eq!(checkpoint_replay, checkpoint);
        let checkpoint_tail: i64 = sqlx::query_scalar(
            "SELECT budget_checkpoint_tail_events FROM ai_virtual_keys WHERE id = $1",
        )
        .bind(checkpoint_key_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(checkpoint_tail, 0);

        sqlx::query("DELETE FROM ai_budget_ledger WHERE virtual_key_id = ANY($1)")
            .bind(key_ids.to_vec())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ai_budget_checkpoints WHERE virtual_key_id = ANY($1)")
            .bind(key_ids.to_vec())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ai_virtual_keys WHERE id = ANY($1)")
            .bind(key_ids.to_vec())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ai_budget_owner_sessions WHERE session_id = ANY($1)")
            .bind(vec![session_id, foreign_session_id])
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ai_budget_runtime_settings WHERE deployment_namespace = ANY($1)")
            .bind(vec![namespace.as_ref(), foreign_namespace.as_ref()])
            .execute(&pool)
            .await
            .unwrap();
    }
}

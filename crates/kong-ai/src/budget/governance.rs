//! Virtual Key 预算账户的原子治理契约。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::FromRow;
use uuid::Uuid;

use crate::models::normalize_budget_amount;

use super::model::{
    BudgetAccountSnapshot, BudgetAccountingState, BudgetErrorKind, BudgetStoreError,
};
use super::postgres::{commit, map_sqlx_error, PgBudgetStore};

const ACCOUNT_COLUMNS: &str = "\
id, name, key_prefix, ws_id, budget_limit, budget_used, \
budget_pending_count, budget_unresolved_count, budget_accounting_revision, \
budget_checkpoint_tail_events, budget_accounting_state, budget_state_updated_at";

/// Admin workspace 边界。历史 null workspace 只归一到显式 default workspace。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetAdminScope {
    pub workspace_id: Uuid,
    pub default_workspace_id: Uuid,
}

impl BudgetAdminScope {
    pub(crate) fn contains(self, workspace_id: Option<Uuid>) -> bool {
        workspace_id.unwrap_or(self.default_workspace_id) == self.workspace_id
    }
}

/// 创建 Virtual Key 与其零值预算账户所需的服务端命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateBudgetAccount {
    pub virtual_key_id: Uuid,
    pub name: String,
    pub key_hash: String,
    pub key_prefix: String,
    pub consumer_id: Option<Uuid>,
    pub allowed_models: Option<Vec<String>>,
    pub tpm_limit: Option<i32>,
    pub rpm_limit: Option<i32>,
    pub budget_limit_usd: Option<Decimal>,
    pub enabled: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub tags: Option<Vec<String>>,
    pub workspace_id: Uuid,
}

/// limit 的 set 与 clear 必须在类型层显式区分。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetLimitMutation {
    Unchanged,
    Set(Decimal),
    Clear,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BudgetOptionalMutation<T> {
    Unchanged,
    Set(T),
    Clear,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateBudgetAccount {
    pub virtual_key_id: Uuid,
    pub scope: BudgetAdminScope,
    pub name: Option<String>,
    pub consumer_id: BudgetOptionalMutation<Uuid>,
    pub allowed_models: BudgetOptionalMutation<Vec<String>>,
    pub tpm_limit: BudgetOptionalMutation<i32>,
    pub rpm_limit: BudgetOptionalMutation<i32>,
    pub budget_limit: BudgetLimitMutation,
    pub enabled: Option<bool>,
    pub expires_at: BudgetOptionalMutation<DateTime<Utc>>,
    pub tags: BudgetOptionalMutation<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateBudgetLimit {
    pub virtual_key_id: Uuid,
    pub scope: BudgetAdminScope,
    pub mutation: BudgetLimitMutation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteBudgetAccount {
    pub virtual_key_id: Uuid,
    pub scope: BudgetAdminScope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletedBudgetAccount {
    pub account: BudgetAccountSnapshot,
    pub deleted_at: DateTime<Utc>,
}

/// 与 intent 共用同一个强一致 account/revision 域的治理接口。
#[async_trait]
pub trait BudgetAccountGovernance: Send + Sync {
    async fn create_account(
        &self,
        command: CreateBudgetAccount,
    ) -> Result<BudgetAccountSnapshot, BudgetStoreError>;

    async fn update_limit(
        &self,
        command: UpdateBudgetLimit,
    ) -> Result<BudgetAccountSnapshot, BudgetStoreError>;

    async fn update_account(
        &self,
        command: UpdateBudgetAccount,
    ) -> Result<BudgetAccountSnapshot, BudgetStoreError>;

    async fn delete_account(
        &self,
        command: DeleteBudgetAccount,
    ) -> Result<DeletedBudgetAccount, BudgetStoreError>;
}

#[async_trait]
impl BudgetAccountGovernance for PgBudgetStore {
    async fn create_account(
        &self,
        command: CreateBudgetAccount,
    ) -> Result<BudgetAccountSnapshot, BudgetStoreError> {
        validate_create_account(&command)?;
        let budget_limit = command
            .budget_limit_usd
            .map(normalize_budget_amount)
            .transpose()
            .map_err(BudgetStoreError::corrupt)?;
        let mut transaction = self.begin_write().await?;
        let sql = format!(
            "INSERT INTO ai_virtual_keys (\
                 id, name, key_hash, key_prefix, consumer_id, allowed_models, \
                 tpm_limit, rpm_limit, budget_limit, budget_used, \
                 budget_pending_count, budget_unresolved_count, \
                 budget_accounting_revision, budget_checkpoint_tail_events, \
                 enabled, expires_at, tags, ws_id\
             ) VALUES (\
                 $1, $2, $3, $4, $5, COALESCE($6, '{{}}'::text[]), \
                 $7, $8, $9, 0.000000000000, 0, 0, 0, 0, \
                 $10, $11, $12, $13\
             ) \
             RETURNING {ACCOUNT_COLUMNS}"
        );
        let row: GovernanceAccountRow = sqlx::query_as(&sql)
            .bind(command.virtual_key_id)
            .bind(command.name.trim())
            .bind(&command.key_hash)
            .bind(&command.key_prefix)
            .bind(command.consumer_id)
            .bind(command.allowed_models)
            .bind(command.tpm_limit)
            .bind(command.rpm_limit)
            .bind(budget_limit)
            .bind(command.enabled)
            .bind(command.expires_at)
            .bind(command.tags)
            .bind(command.workspace_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;

        sqlx::query(
            "INSERT INTO ai_budget_checkpoints (\
                 virtual_key_id, checkpoint_revision, accounted_cost_usd, operation_id\
             ) VALUES ($1, 0, 0.000000000000, $2)",
        )
        .bind(command.virtual_key_id)
        .bind(format!(
            "budget-checkpoint-genesis:v1:{}",
            command.virtual_key_id
        ))
        .execute(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;

        let account = row.into_snapshot()?;
        validate_account(&account)?;
        commit(transaction).await?;
        Ok(account)
    }

    async fn update_limit(
        &self,
        command: UpdateBudgetLimit,
    ) -> Result<BudgetAccountSnapshot, BudgetStoreError> {
        self.update_account(UpdateBudgetAccount {
            virtual_key_id: command.virtual_key_id,
            scope: command.scope,
            name: None,
            consumer_id: BudgetOptionalMutation::Unchanged,
            allowed_models: BudgetOptionalMutation::Unchanged,
            tpm_limit: BudgetOptionalMutation::Unchanged,
            rpm_limit: BudgetOptionalMutation::Unchanged,
            budget_limit: command.mutation,
            enabled: None,
            expires_at: BudgetOptionalMutation::Unchanged,
            tags: BudgetOptionalMutation::Unchanged,
        })
        .await
    }

    async fn update_account(
        &self,
        command: UpdateBudgetAccount,
    ) -> Result<BudgetAccountSnapshot, BudgetStoreError> {
        let normalized = normalize_update_account(command)?;
        let mut transaction = self.begin_write().await?;
        let current = lock_account(&mut transaction, normalized.virtual_key_id)
            .await?
            .ok_or_else(not_found)?;
        if !normalized.scope.contains(current.ws_id) {
            return Err(not_found());
        }
        let current = current.into_snapshot()?;
        validate_account(&current)?;
        if !normalized.has_changes {
            commit(transaction).await?;
            return Ok(current);
        }
        if normalized.budget_clear && (current.pending_count != 0 || current.unresolved_count != 0)
        {
            return Err(BudgetStoreError::new(
                BudgetErrorKind::ReconciliationRequired,
                "存在 pending/unresolved 账务，不能清空预算上限",
            ));
        }
        let new_revision = current
            .accounting_revision
            .checked_add(1)
            .ok_or_else(|| BudgetStoreError::corrupt("预算 revision 溢出"))?;
        let sql = format!(
            "UPDATE ai_virtual_keys \
                SET name = CASE WHEN $2 THEN $3::text ELSE name END, \
                    consumer_id = CASE WHEN $4 THEN $5::uuid ELSE consumer_id END, \
                    allowed_models = CASE WHEN $6 THEN $7::text[] ELSE allowed_models END, \
                    tpm_limit = CASE WHEN $8 THEN $9::integer ELSE tpm_limit END, \
                    rpm_limit = CASE WHEN $10 THEN $11::integer ELSE rpm_limit END, \
                    budget_limit = CASE WHEN $12 THEN $13::numeric ELSE budget_limit END, \
                    enabled = CASE WHEN $14 THEN $15::boolean ELSE enabled END, \
                    expires_at = CASE WHEN $16 THEN $17::timestamptz ELSE expires_at END, \
                    tags = CASE WHEN $18 THEN $19::text[] ELSE tags END, \
                    budget_accounting_revision = $20, \
                    budget_state_updated_at = clock_timestamp(), \
                    updated_at = clock_timestamp() \
              WHERE id = $1 AND budget_accounting_revision = $21 \
          RETURNING {ACCOUNT_COLUMNS}"
        );
        let updated: Option<GovernanceAccountRow> = sqlx::query_as(&sql)
            .bind(normalized.virtual_key_id)
            .bind(normalized.name_provided)
            .bind(normalized.name)
            .bind(normalized.consumer_id_provided)
            .bind(normalized.consumer_id)
            .bind(normalized.allowed_models_provided)
            .bind(normalized.allowed_models)
            .bind(normalized.tpm_limit_provided)
            .bind(normalized.tpm_limit)
            .bind(normalized.rpm_limit_provided)
            .bind(normalized.rpm_limit)
            .bind(normalized.budget_limit_provided)
            .bind(normalized.budget_limit)
            .bind(normalized.enabled_provided)
            .bind(normalized.enabled)
            .bind(normalized.expires_at_provided)
            .bind(normalized.expires_at)
            .bind(normalized.tags_provided)
            .bind(normalized.tags)
            .bind(new_revision)
            .bind(current.accounting_revision)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        let account = updated
            .ok_or_else(|| BudgetStoreError::corrupt("预算 limit 更新时 revision 不一致"))?
            .into_snapshot()?;
        validate_account(&account)?;
        commit(transaction).await?;
        Ok(account)
    }

    async fn delete_account(
        &self,
        command: DeleteBudgetAccount,
    ) -> Result<DeletedBudgetAccount, BudgetStoreError> {
        let mut transaction = self.begin_write().await?;
        let current = lock_account(&mut transaction, command.virtual_key_id)
            .await?
            .ok_or_else(not_found)?;
        if !command.scope.contains(current.ws_id) {
            return Err(not_found());
        }
        let account = current.into_snapshot()?;
        validate_account(&account)?;
        if account.pending_count != 0 || account.unresolved_count != 0 {
            return Err(BudgetStoreError::new(
                BudgetErrorKind::ReconciliationRequired,
                "存在 pending/unresolved 账务，不能删除 Virtual Key",
            ));
        }
        let deleted = sqlx::query("DELETE FROM ai_virtual_keys WHERE id = $1")
            .bind(command.virtual_key_id)
            .execute(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        if deleted.rows_affected() != 1 {
            return Err(BudgetStoreError::corrupt("Virtual Key 删除时锁定行已消失"));
        }
        let deleted_at: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *transaction)
            .await
            .map_err(map_sqlx_error)?;
        commit(transaction).await?;
        Ok(DeletedBudgetAccount {
            account,
            deleted_at,
        })
    }
}

#[derive(Clone, FromRow)]
pub(crate) struct GovernanceAccountRow {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) key_prefix: String,
    pub(crate) ws_id: Option<Uuid>,
    pub(crate) budget_limit: Option<Decimal>,
    pub(crate) budget_used: Decimal,
    pub(crate) budget_pending_count: i64,
    pub(crate) budget_unresolved_count: i64,
    pub(crate) budget_accounting_revision: i64,
    pub(crate) budget_checkpoint_tail_events: i64,
    pub(crate) budget_accounting_state: String,
    pub(crate) budget_state_updated_at: DateTime<Utc>,
}

impl GovernanceAccountRow {
    pub(crate) fn into_snapshot(self) -> Result<BudgetAccountSnapshot, BudgetStoreError> {
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

pub(crate) async fn lock_account(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    virtual_key_id: Uuid,
) -> Result<Option<GovernanceAccountRow>, BudgetStoreError> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM ai_virtual_keys WHERE id = $1 FOR UPDATE");
    sqlx::query_as(&sql)
        .bind(virtual_key_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_sqlx_error)
}

pub(crate) fn validate_account(account: &BudgetAccountSnapshot) -> Result<(), BudgetStoreError> {
    if account.pending_count < 0
        || account.unresolved_count < 0
        || account.accounting_revision < 0
        || account.checkpoint_tail_events < 0
    {
        return Err(BudgetStoreError::corrupt("预算账户计数或 revision 非法"));
    }
    let expected = if account.unresolved_count > 0 {
        BudgetAccountingState::Unresolved
    } else if account.pending_count > 0 {
        BudgetAccountingState::Pending
    } else {
        BudgetAccountingState::Clean
    };
    if account.state != expected {
        return Err(BudgetStoreError::corrupt("预算账户 state 与计数不一致"));
    }
    Ok(())
}

struct NormalizedUpdateBudgetAccount {
    virtual_key_id: Uuid,
    scope: BudgetAdminScope,
    name_provided: bool,
    name: Option<String>,
    consumer_id_provided: bool,
    consumer_id: Option<Uuid>,
    allowed_models_provided: bool,
    allowed_models: Option<Vec<String>>,
    tpm_limit_provided: bool,
    tpm_limit: Option<i32>,
    rpm_limit_provided: bool,
    rpm_limit: Option<i32>,
    budget_limit_provided: bool,
    budget_limit: Option<Decimal>,
    budget_clear: bool,
    enabled_provided: bool,
    enabled: Option<bool>,
    expires_at_provided: bool,
    expires_at: Option<DateTime<Utc>>,
    tags_provided: bool,
    tags: Option<Vec<String>>,
    has_changes: bool,
}

fn normalize_update_account(
    command: UpdateBudgetAccount,
) -> Result<NormalizedUpdateBudgetAccount, BudgetStoreError> {
    if command.virtual_key_id.is_nil() {
        return Err(BudgetStoreError::corrupt(
            "Virtual Key update 命令缺少 key ID",
        ));
    }
    let name_provided = command.name.is_some();
    let name = command
        .name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    if name_provided && name.is_none() {
        return Err(BudgetStoreError::corrupt("Virtual Key name 不能为空"));
    }
    let (consumer_id_provided, consumer_id) = optional_mutation(command.consumer_id);
    let (allowed_models_provided, allowed_models) = optional_mutation(command.allowed_models);
    let (tpm_limit_provided, tpm_limit) = quota_mutation(command.tpm_limit, "tpm_limit")?;
    let (rpm_limit_provided, rpm_limit) = quota_mutation(command.rpm_limit, "rpm_limit")?;
    let (budget_limit_provided, budget_limit, budget_clear) = match command.budget_limit {
        BudgetLimitMutation::Unchanged => (false, None, false),
        BudgetLimitMutation::Set(limit) => (
            true,
            Some(normalize_budget_amount(limit).map_err(BudgetStoreError::corrupt)?),
            false,
        ),
        BudgetLimitMutation::Clear => (true, None, true),
    };
    let enabled_provided = command.enabled.is_some();
    let enabled = command.enabled;
    let (expires_at_provided, expires_at) = optional_mutation(command.expires_at);
    let (tags_provided, tags) = optional_mutation(command.tags);
    let has_changes = name_provided
        || consumer_id_provided
        || allowed_models_provided
        || tpm_limit_provided
        || rpm_limit_provided
        || budget_limit_provided
        || enabled_provided
        || expires_at_provided
        || tags_provided;
    Ok(NormalizedUpdateBudgetAccount {
        virtual_key_id: command.virtual_key_id,
        scope: command.scope,
        name_provided,
        name,
        consumer_id_provided,
        consumer_id,
        allowed_models_provided,
        allowed_models,
        tpm_limit_provided,
        tpm_limit,
        rpm_limit_provided,
        rpm_limit,
        budget_limit_provided,
        budget_limit,
        budget_clear,
        enabled_provided,
        enabled,
        expires_at_provided,
        expires_at,
        tags_provided,
        tags,
        has_changes,
    })
}

fn optional_mutation<T>(mutation: BudgetOptionalMutation<T>) -> (bool, Option<T>) {
    match mutation {
        BudgetOptionalMutation::Unchanged => (false, None),
        BudgetOptionalMutation::Set(value) => (true, Some(value)),
        BudgetOptionalMutation::Clear => (true, None),
    }
}

fn quota_mutation(
    mutation: BudgetOptionalMutation<i32>,
    field: &str,
) -> Result<(bool, Option<i32>), BudgetStoreError> {
    match mutation {
        BudgetOptionalMutation::Unchanged => Ok((false, None)),
        BudgetOptionalMutation::Set(value) if value > 0 => Ok((true, Some(value))),
        BudgetOptionalMutation::Set(_) => {
            Err(BudgetStoreError::corrupt(format!("{field} 必须为正整数")))
        }
        BudgetOptionalMutation::Clear => Ok((true, None)),
    }
}

fn validate_create_account(command: &CreateBudgetAccount) -> Result<(), BudgetStoreError> {
    if command.virtual_key_id.is_nil()
        || command.name.trim().is_empty()
        || command.key_hash.trim().is_empty()
        || command.key_prefix.trim().is_empty()
    {
        return Err(BudgetStoreError::corrupt(
            "Virtual Key create 命令缺少必要身份字段",
        ));
    }
    for (field, value) in [
        ("tpm_limit", command.tpm_limit),
        ("rpm_limit", command.rpm_limit),
    ] {
        if value.is_some_and(|limit| limit <= 0) {
            return Err(BudgetStoreError::corrupt(format!("{field} 必须为正整数")));
        }
    }
    if let Some(limit) = command.budget_limit_usd {
        normalize_budget_amount(limit).map_err(BudgetStoreError::corrupt)?;
    }
    Ok(())
}

pub(crate) fn not_found() -> BudgetStoreError {
    BudgetStoreError::new(BudgetErrorKind::NotFound, "预算账户不存在")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_scope_normalizes_only_legacy_default_rows() {
        let default_workspace_id = Uuid::new_v4();
        let other_workspace_id = Uuid::new_v4();
        let default_scope = BudgetAdminScope {
            workspace_id: default_workspace_id,
            default_workspace_id,
        };
        let other_scope = BudgetAdminScope {
            workspace_id: other_workspace_id,
            default_workspace_id,
        };

        assert!(default_scope.contains(None));
        assert!(!other_scope.contains(None));
        assert!(other_scope.contains(Some(other_workspace_id)));
    }

    #[test]
    fn create_rejects_non_positive_quota() {
        let command = CreateBudgetAccount {
            virtual_key_id: Uuid::new_v4(),
            name: "key".to_string(),
            key_hash: "hash".to_string(),
            key_prefix: "prefix".to_string(),
            consumer_id: None,
            allowed_models: None,
            tpm_limit: Some(0),
            rpm_limit: None,
            budget_limit_usd: None,
            enabled: true,
            expires_at: None,
            tags: None,
            workspace_id: Uuid::new_v4(),
        };

        assert_eq!(
            validate_create_account(&command).unwrap_err().kind(),
            BudgetErrorKind::Corrupt
        );
    }
}

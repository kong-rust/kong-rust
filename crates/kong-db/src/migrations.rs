//! Database migration engine — Database migration 引擎
//!
//! Manages SQL migration execution and version tracking, compatible with Kong's schema_meta table. — 管理 SQL migration 的执行和版本追踪，与 Kong 的 schema_meta 表兼容。
//! Provides full migration command set: schema_state / bootstrap / up / finish / reset. — 提供完整的 migration 命令集：schema_state / bootstrap / up / finish / reset。

use kong_core::error::{KongError, Result};
use sqlx::PgPool;

/// Core migration definition — 核心 migration 定义
struct Migration {
    /// Migration name (e.g. "000_base") — migration 名称（如 "000_base"）
    name: &'static str,
    /// SQL content — SQL 内容
    sql: &'static str,
}

/// Core migration list (SQL embedded at compile time) — 核心 migration 列表（编译期嵌入 SQL）
const CORE_MIGRATIONS: &[Migration] = &[
    Migration {
        name: "000_base",
        sql: include_str!("../migrations/core/000_base.sql"),
    },
    Migration {
        name: "001_add_workspaces",
        sql: include_str!("../migrations/core/001_add_workspaces.sql"),
    },
    Migration {
        name: "002_ai_gateway",
        sql: include_str!("../migrations/core/002_ai_gateway.sql"),
    },
    Migration {
        name: "003_keys",
        sql: include_str!("../migrations/core/003_keys.sql"),
    },
    Migration {
        name: "004_ai_model_max_input_tokens",
        sql: include_str!("../migrations/core/004_ai_model_max_input_tokens.sql"),
    },
    Migration {
        name: "005_ai_model_weight_limit",
        sql: include_str!("../migrations/core/005_ai_model_weight_limit.sql"),
    },
    Migration {
        name: "006_ai_usage_logs",
        sql: include_str!("../migrations/core/006_ai_usage_logs.sql"),
    },
];

/// schema_meta subsystem identifier — schema_meta 的 subsystem 标识
const SUBSYSTEM: &str = "core";
/// schema_meta key — schema_meta 的 key
const META_KEY: &str = "1";

/// All known entity tables (used for DROP during reset) — 所有已知的实体表（reset 时 DROP 用）
const KNOWN_TABLES: &[&str] = &[
    "plugins",
    "targets",
    "snis",
    "routes",
    "upstreams",
    "services",
    "consumers",
    "certificates",
    "ca_certificates",
    "sm_vaults",
    "ai_usage_logs",
    "ai_virtual_keys",
    "ai_models",
    "ai_providers",
    "keys",
    "key_sets",
    "schema_meta",
    "workspaces",
];

/// Migration state (corresponds to Kong's schema_state) — migration 状态（对应 Kong 的 schema_state）
#[derive(Debug)]
pub struct MigrationState {
    /// schema_meta table does not exist, database not initialized — schema_meta 表不存在，数据库未初始化
    pub needs_bootstrap: bool,
    /// Names of executed migrations — 已执行的 migration 名称
    pub executed: Vec<String>,
    /// Migrations awaiting finish (reserved, currently empty) — 等待 finish 的 migration（预留，当前为空）
    pub pending: Vec<String>,
    /// New migrations pending execution — 新的待运行 migration
    pub new_migrations: Vec<String>,
}

/// Query the database's migration state — 查询数据库的 migration 状态
pub async fn schema_state(pool: &PgPool) -> Result<MigrationState> {
    // Check if schema_meta table exists — 检查 schema_meta 表是否存在
    let table_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT FROM information_schema.tables
            WHERE table_schema = current_schema()
            AND table_name = 'schema_meta'
        )",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| KongError::DatabaseError(format!("检查 schema_meta 表失败: {}", e)))?;

    if !table_exists {
        return Ok(MigrationState {
            needs_bootstrap: true,
            executed: vec![],
            pending: vec![],
            new_migrations: CORE_MIGRATIONS.iter().map(|m| m.name.to_string()).collect(),
        });
    }

    let executed = get_executed_migrations(pool).await?;
    let pending = get_pending_migrations(pool).await?;

    // Find migrations not yet executed — 找出尚未执行的 migration
    let new_migrations = CORE_MIGRATIONS
        .iter()
        .filter(|m| !executed.contains(&m.name.to_string()))
        .map(|m| m.name.to_string())
        .collect();

    Ok(MigrationState {
        needs_bootstrap: false,
        executed,
        pending,
        new_migrations,
    })
}

/// Bootstrap: create schema_meta + execute all migrations — bootstrap：创建 schema_meta + 执行所有 migration
///
/// Only used when database is not initialized. Returns error if already initialized. — 仅在数据库未初始化时使用。如果已初始化则返回错误。
pub async fn bootstrap(pool: &PgPool) -> Result<()> {
    let state = schema_state(pool).await?;
    if !state.needs_bootstrap {
        return Err(KongError::DatabaseError(
            "数据库已初始化，使用 'db up' 执行新 migration".to_string(),
        ));
    }

    ensure_schema_meta(pool).await?;

    for migration in CORE_MIGRATIONS {
        tracing::info!("执行 migration: {}", migration.name);
        execute_migration(pool, migration).await?;
    }

    tracing::info!(
        "Bootstrap 完成，共执行 {} 个 migration",
        CORE_MIGRATIONS.len()
    );
    Ok(())
}

/// Up: execute only new migrations — up：仅执行新的 migration
///
/// Skips already-executed migrations, only runs new ones. — 跳过已执行的 migration，只运行新增的。
pub async fn up(pool: &PgPool) -> Result<()> {
    let state = schema_state(pool).await?;
    if state.needs_bootstrap {
        return Err(KongError::DatabaseError(
            "数据库未初始化，请先运行 'db bootstrap'".to_string(),
        ));
    }

    if state.new_migrations.is_empty() {
        tracing::info!("数据库 schema 已是最新，无需 migration");
        return Ok(());
    }

    let executed = &state.executed;
    let mut new_count = 0;
    for migration in CORE_MIGRATIONS {
        if executed.contains(&migration.name.to_string()) {
            tracing::debug!("跳过已执行的 migration: {}", migration.name);
            continue;
        }
        tracing::info!("执行 migration: {}", migration.name);
        execute_migration(pool, migration).await?;
        new_count += 1;
    }

    tracing::info!("成功执行 {} 个新 migration", new_count);
    Ok(())
}

/// Finish: execute pending teardown (reserved) — finish：执行 pending teardown（预留）
///
/// Currently kong-rust has no teardown-phase migrations; this function only checks state. — 当前 kong-rust 无 teardown 阶段的 migration，此函数仅检查状态。
pub async fn finish(pool: &PgPool) -> Result<()> {
    let state = schema_state(pool).await?;
    if state.needs_bootstrap {
        return Err(KongError::DatabaseError(
            "数据库未初始化，请先运行 'db bootstrap'".to_string(),
        ));
    }

    if state.pending.is_empty() {
        tracing::info!("没有等待 finish 的 migration");
        return Ok(());
    }

    // Reserved: execute teardown here if there are pending migrations — 预留：如果有 pending migration，在此执行 teardown
    tracing::info!(
        "Finish 完成，处理了 {} 个 pending migration",
        state.pending.len()
    );
    Ok(())
}

/// Reset: DROP all known tables, return to uninitialized state — reset：DROP 所有已知表，回到未初始化状态
///
/// Dangerous operation: deletes all data! — 危险操作：会删除所有数据！
pub async fn reset(pool: &PgPool) -> Result<()> {
    let state = schema_state(pool).await?;
    if state.needs_bootstrap {
        return Err(KongError::DatabaseError(
            "数据库尚未初始化，无需 reset".to_string(),
        ));
    }

    tracing::warn!("正在重置数据库，删除所有表...");

    // DROP in foreign key dependency order (child tables first) — 按外键依赖顺序 DROP（子表先删）
    for table in KNOWN_TABLES {
        let sql = format!("DROP TABLE IF EXISTS {} CASCADE", table);
        sqlx::query(&sql)
            .execute(pool)
            .await
            .map_err(|e| KongError::DatabaseError(format!("删除表 {} 失败: {}", table, e)))?;
        tracing::info!("已删除表: {}", table);
    }

    tracing::info!("数据库已重置");
    Ok(())
}

/// Ensure schema_meta table exists — 确保 schema_meta 表存在
async fn ensure_schema_meta(pool: &PgPool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_meta (
            key            TEXT NOT NULL,
            subsystem      TEXT NOT NULL,
            last_executed  TEXT,
            executed       TEXT[],
            pending        TEXT[],
            PRIMARY KEY (key, subsystem)
        )",
    )
    .execute(pool)
    .await
    .map_err(|e| KongError::DatabaseError(format!("创建 schema_meta 表失败: {}", e)))?;

    Ok(())
}

/// Query executed migration list — 查询已执行的 migration 列表
async fn get_executed_migrations(pool: &PgPool) -> Result<Vec<String>> {
    let row: Option<(Vec<String>,)> = sqlx::query_as(
        "SELECT COALESCE(executed, '{}') FROM schema_meta WHERE key = $1 AND subsystem = $2",
    )
    .bind(META_KEY)
    .bind(SUBSYSTEM)
    .fetch_optional(pool)
    .await
    .map_err(|e| KongError::DatabaseError(format!("查询 schema_meta 失败: {}", e)))?;

    Ok(row.map(|r| r.0).unwrap_or_default())
}

/// Query pending migration list — 查询 pending 的 migration 列表
async fn get_pending_migrations(pool: &PgPool) -> Result<Vec<String>> {
    let row: Option<(Vec<String>,)> = sqlx::query_as(
        "SELECT COALESCE(pending, '{}') FROM schema_meta WHERE key = $1 AND subsystem = $2",
    )
    .bind(META_KEY)
    .bind(SUBSYSTEM)
    .fetch_optional(pool)
    .await
    .map_err(|e| KongError::DatabaseError(format!("查询 schema_meta pending 失败: {}", e)))?;

    Ok(row.map(|r| r.0).unwrap_or_default())
}

/// Execute a single migration and update schema_meta — 执行单个 migration 并更新 schema_meta
async fn execute_migration(pool: &PgPool, migration: &Migration) -> Result<()> {
    // Execute migration SQL and update schema_meta in a transaction — 在事务中执行 migration SQL 并更新 schema_meta
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| KongError::DatabaseError(format!("开启事务失败: {}", e)))?;

    // 使用 raw_sql 保留 PostgreSQL dollar-quoted procedural block，同时整份脚本仍在外层事务中原子执行。
    sqlx::raw_sql(migration.sql)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            KongError::DatabaseError(format!("执行 migration {} 失败: {}", migration.name, e))
        })?;

    // Update schema_meta (upsert) — 更新 schema_meta（upsert）
    sqlx::query(
        "INSERT INTO schema_meta (key, subsystem, last_executed, executed, pending)
         VALUES ($1, $2, $3, ARRAY[$3], '{}')
         ON CONFLICT (key, subsystem)
         DO UPDATE SET
             last_executed = $3,
             executed = array_append(schema_meta.executed, $3)",
    )
    .bind(META_KEY)
    .bind(SUBSYSTEM)
    .bind(migration.name)
    .execute(&mut *tx)
    .await
    .map_err(|e| KongError::DatabaseError(format!("更新 schema_meta 失败: {}", e)))?;

    tx.commit()
        .await
        .map_err(|e| KongError::DatabaseError(format!("提交事务失败: {}", e)))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CORE_MIGRATIONS, KNOWN_TABLES};

    #[test]
    fn max_input_tokens_has_a_forward_migration() {
        let migration = CORE_MIGRATIONS
            .iter()
            .find(|migration| migration.name == "004_ai_model_max_input_tokens")
            .expect("forward migration must be registered");

        assert!(migration
            .sql
            .contains("ADD COLUMN max_input_tokens INTEGER"));
        assert!(!migration.sql.contains("IF NOT EXISTS"));
    }

    #[test]
    fn ai_model_weight_limit_has_a_forward_migration() {
        let migration = CORE_MIGRATIONS
            .iter()
            .find(|migration| migration.name == "005_ai_model_weight_limit")
            .expect("weight limit migration must be registered");

        assert!(migration.sql.contains("CHECK (weight BETWEEN 0 AND 10000)"));
        assert!(!migration.sql.contains("IF NOT EXISTS"));
    }

    #[test]
    fn ai_usage_logs_migration_is_registered_and_reset_first() {
        assert_eq!(
            CORE_MIGRATIONS.last().map(|migration| migration.name),
            Some("006_ai_usage_logs")
        );

        let usage_index = KNOWN_TABLES
            .iter()
            .position(|table| *table == "ai_usage_logs")
            .expect("usage table must be reset");
        let model_index = KNOWN_TABLES
            .iter()
            .position(|table| *table == "ai_models")
            .expect("model table must be reset");
        let workspace_index = KNOWN_TABLES
            .iter()
            .position(|table| *table == "workspaces")
            .expect("workspace table must be reset");

        assert!(usage_index < model_index);
        assert!(usage_index < workspace_index);
    }

    #[test]
    fn ai_usage_logs_migration_has_exact_decimal_and_fact_schema() {
        let migration = CORE_MIGRATIONS
            .iter()
            .find(|migration| migration.name == "006_ai_usage_logs")
            .expect("usage migration must be registered");

        for fragment in [
            "ALTER COLUMN input_cost TYPE NUMERIC(28,12)",
            "ALTER COLUMN output_cost TYPE NUMERIC(28,12)",
            "ai_models_input_cost_valid",
            "ai_models_output_cost_valid",
            "CREATE TABLE ai_usage_logs",
            "ingest_seq                        BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE",
            "request_id                        VARCHAR(32) NOT NULL UNIQUE",
            "started_at                        TIMESTAMPTZ(3) NOT NULL",
            "workspace_id                      UUID",
            "input_price_per_million           NUMERIC(28,12)",
            "output_price_per_million          NUMERIC(28,12)",
            "cost_usd                          NUMERIC(28,12)",
        ] {
            assert!(
                migration.sql.contains(fragment),
                "missing migration fragment: {fragment}"
            );
        }

        assert!(migration.sql.contains("RAISE EXCEPTION"));
        assert!(migration.sql.contains("invalid_model_id"));
        assert!(!migration.sql.contains("IF NOT EXISTS"));
        assert!(!migration.sql.contains("REFERENCES"));
    }

    #[test]
    fn ai_usage_logs_migration_has_named_invariant_checks() {
        let migration = CORE_MIGRATIONS
            .iter()
            .find(|migration| migration.name == "006_ai_usage_logs")
            .expect("usage migration must be registered");

        for constraint in [
            "ai_usage_logs_request_id_format",
            "ai_usage_logs_prompt_tokens_nonnegative",
            "ai_usage_logs_completion_tokens_nonnegative",
            "ai_usage_logs_total_tokens_nonnegative",
            "ai_usage_logs_reasoning_tokens_nonnegative",
            "ai_usage_logs_cache_read_input_tokens_nonnegative",
            "ai_usage_logs_cache_write_input_tokens_nonnegative",
            "ai_usage_logs_e2e_ms_nonnegative",
            "ai_usage_logs_ttft_ms_nonnegative",
            "ai_usage_logs_input_price_nonnegative",
            "ai_usage_logs_output_price_nonnegative",
            "ai_usage_logs_cost_nonnegative",
            "ai_usage_logs_time_order",
            "ai_usage_logs_status_code_range",
            "ai_usage_logs_upstream_status_code_range",
            "ai_usage_logs_attempt_count_supported",
            "ai_usage_logs_attempt_upstream_consistency",
            "ai_usage_logs_prompt_source_value",
            "ai_usage_logs_completion_source_value",
            "ai_usage_logs_total_source_value",
            "ai_usage_logs_usage_source_value",
            "ai_usage_logs_pricing_status_value",
            "ai_usage_logs_cost_status_value",
            "ai_usage_logs_outcome_value",
            "ai_usage_logs_cache_status_value",
            "ai_usage_logs_prompt_token_source_presence",
            "ai_usage_logs_completion_token_source_presence",
            "ai_usage_logs_total_token_source_presence",
            "ai_usage_logs_usage_availability_consistency",
            "ai_usage_logs_provider_source_consistency",
            "ai_usage_logs_estimated_source_consistency",
            "ai_usage_logs_mixed_source_consistency",
            "ai_usage_logs_input_price_bundle_consistency",
            "ai_usage_logs_output_price_bundle_consistency",
            "ai_usage_logs_pricing_bundle_consistency",
            "ai_usage_logs_pricing_reason_consistency",
            "ai_usage_logs_not_applicable_cost_consistency",
            "ai_usage_logs_not_incurred_consistency",
            "ai_usage_logs_not_attempted_consistency",
            "ai_usage_logs_calculated_cost_consistency",
            "ai_usage_logs_estimated_cost_consistency",
            "ai_usage_logs_unmatched_unsupported_cost_consistency",
            "ai_usage_logs_cost_availability_consistency",
            "ai_usage_logs_currency_value",
            "ai_usage_logs_usage_reason_values",
            "ai_usage_logs_pricing_reason_values",
            "ai_usage_logs_cost_reason_values",
        ] {
            assert!(
                migration.sql.contains(&format!("CONSTRAINT {constraint}")),
                "missing named constraint: {constraint}"
            );
        }

        for reason in [
            "not_attempted",
            "invalid_token_value",
            "provider_cache_pricing",
            "additional_pricing",
            "missing_prompt_usage",
            "arithmetic_overflow",
        ] {
            assert!(
                migration.sql.contains(&format!("'{reason}'")),
                "missing reason value: {reason}"
            );
        }
    }

    #[test]
    fn ai_usage_logs_migration_has_required_indexes() {
        let migration = CORE_MIGRATIONS
            .iter()
            .find(|migration| migration.name == "006_ai_usage_logs")
            .expect("usage migration must be registered");

        for index in [
            "idx_ai_usage_logs_workspace_started",
            "idx_ai_usage_logs_workspace_actual_model_started",
            "idx_ai_usage_logs_workspace_model_group_started",
            "idx_ai_usage_logs_workspace_virtual_key_started",
            "idx_ai_usage_logs_workspace_route_started",
            "idx_ai_usage_logs_workspace_service_started",
            "idx_ai_usage_logs_workspace_provider_started",
            "idx_ai_usage_logs_workspace_consumer_started",
        ] {
            assert!(
                migration.sql.contains(&format!("CREATE INDEX {index}")),
                "missing index: {index}"
            );
        }
    }
}

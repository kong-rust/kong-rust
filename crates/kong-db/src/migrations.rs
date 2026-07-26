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
    Migration {
        name: "007_ai_virtual_key_budget_accounting",
        sql: include_str!("../migrations/core/007_ai_virtual_key_budget_accounting.sql"),
    },
    Migration {
        name: "008_ai_budget_overflow_idempotency",
        sql: include_str!("../migrations/core/008_ai_budget_overflow_idempotency.sql"),
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
    "ai_budget_ledger",
    "ai_budget_checkpoints",
    "ai_budget_owner_sessions",
    "ai_budget_runtime_settings",
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
    use super::{
        ensure_schema_meta, execute_migration, schema_state, up, CORE_MIGRATIONS, KNOWN_TABLES,
    };
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
    use sqlx::PgPool;
    use std::str::FromStr;

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
    fn ai_virtual_key_budget_migration_is_registered_and_reset_in_dependency_order() {
        assert_eq!(
            CORE_MIGRATIONS.last().map(|migration| migration.name),
            Some("008_ai_budget_overflow_idempotency")
        );

        let positions = [
            "ai_budget_ledger",
            "ai_budget_checkpoints",
            "ai_budget_owner_sessions",
            "ai_budget_runtime_settings",
            "ai_virtual_keys",
        ]
        .map(|table| {
            KNOWN_TABLES
                .iter()
                .position(|known| *known == table)
                .unwrap_or_else(|| panic!("{table} must be reset"))
        });

        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn ai_budget_overflow_idempotency_migration_is_strictly_scoped() {
        let migration = CORE_MIGRATIONS
            .iter()
            .find(|migration| migration.name == "008_ai_budget_overflow_idempotency")
            .expect("overflow idempotency migration must be registered");

        for fragment in [
            "DROP CONSTRAINT ai_budget_ledger_request_terminal_consistency",
            "status = 'unresolved'",
            "terminal_operation_id IS NOT NULL",
            "terminal_command_fingerprint IS NOT NULL",
            "cost_status = 'unavailable'",
            "'budget_numeric_overflow' = ANY(cost_reasons)",
        ] {
            assert!(
                migration.sql.contains(fragment),
                "missing migration fragment: {fragment}"
            );
        }
        assert!(!migration.sql.contains("IF NOT EXISTS"));
    }

    #[test]
    fn ai_virtual_key_budget_migration_has_exact_decimal_and_accounting_schema() {
        let migration = CORE_MIGRATIONS
            .iter()
            .find(|migration| migration.name == "007_ai_virtual_key_budget_accounting")
            .expect("budget accounting migration must be registered");

        for fragment in [
            "LOCK TABLE ai_virtual_keys IN ACCESS EXCLUSIVE MODE",
            "ALTER COLUMN budget_limit TYPE NUMERIC(28,12)",
            "ALTER COLUMN budget_used TYPE NUMERIC(28,12)",
            "ADD COLUMN budget_pending_count BIGINT NOT NULL DEFAULT 0",
            "ADD COLUMN budget_unresolved_count BIGINT NOT NULL DEFAULT 0",
            "ADD COLUMN budget_accounting_revision BIGINT NOT NULL DEFAULT 0",
            "ADD COLUMN budget_checkpoint_tail_events BIGINT NOT NULL DEFAULT 0",
            "GENERATED ALWAYS AS",
            "CREATE TABLE ai_budget_runtime_settings",
            "CREATE TABLE ai_budget_owner_sessions",
            "CREATE TABLE ai_budget_ledger",
            "CREATE TABLE ai_budget_checkpoints",
            "pricing_snapshot IS NULL",
            "octet_length(pricing_snapshot::TEXT) <= 4096",
            "accounted_cost_usd < 10000000000000000::NUMERIC",
            "'opening_balance'",
            "'budget-checkpoint-genesis:v1:'",
        ] {
            assert!(
                migration.sql.contains(fragment),
                "missing migration fragment: {fragment}"
            );
        }

        assert!(migration.sql.contains("ERRCODE = '22003'"));
        assert!(migration.sql.contains("'NaN'::NUMERIC"));
        assert!(!migration.sql.contains("IF NOT EXISTS"));
    }

    #[test]
    fn ai_virtual_key_budget_migration_has_required_indexes_and_no_key_foreign_key() {
        let migration = CORE_MIGRATIONS
            .iter()
            .find(|migration| migration.name == "007_ai_virtual_key_budget_accounting")
            .expect("budget accounting migration must be registered");

        for index in [
            "ai_budget_owner_sessions_expiry_idx",
            "ai_budget_ledger_key_time_idx",
            "ai_budget_ledger_open_idx",
            "ai_budget_ledger_owner_open_idx",
            "ai_budget_ledger_parent_idx",
            "ai_budget_ledger_revision_idx",
        ] {
            assert!(
                migration.sql.contains(&format!("CREATE INDEX {index}")),
                "missing index: {index}"
            );
        }

        assert!(!migration
            .sql
            .contains("virtual_key_id               UUID NOT NULL REFERENCES"));
        assert!(!migration
            .sql
            .contains("owner_session_id             UUID REFERENCES"));
        assert!(!migration
            .sql
            .contains("usage_fact_id                UUID REFERENCES"));
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

    fn migration_test_connect_options() -> std::result::Result<Option<PgConnectOptions>, String> {
        if let Ok(database_url) = std::env::var("KONG_DB_MIGRATION_PG_TEST_URL") {
            return PgConnectOptions::from_str(&database_url)
                .map(Some)
                .map_err(|error| {
                    format!("KONG_DB_MIGRATION_PG_TEST_URL 不是有效的 PostgreSQL URL: {error}")
                });
        }

        let Ok(host) = std::env::var("KONG_PG_HOST") else {
            return Ok(None);
        };
        let port = std::env::var("KONG_PG_PORT")
            .unwrap_or_else(|_| "5432".to_string())
            .parse::<u16>()
            .map_err(|error| format!("KONG_PG_PORT 不是有效端口: {error}"))?;
        let user = std::env::var("KONG_PG_USER").unwrap_or_else(|_| "kong".to_string());
        let database = std::env::var("KONG_PG_DATABASE").unwrap_or_else(|_| "kong".to_string());
        let ssl_mode = match std::env::var("KONG_PG_SSL")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "1" | "on" | "true" => PgSslMode::Require,
            _ => PgSslMode::Prefer,
        };

        let mut options = PgConnectOptions::new()
            .host(&host)
            .port(port)
            .username(&user)
            .database(&database)
            .ssl_mode(ssl_mode);
        if let Ok(password) = std::env::var("KONG_PG_PASSWORD") {
            options = options.password(&password);
        }
        Ok(Some(options))
    }

    async fn create_temp_schema_pool(
        admin_pool: &PgPool,
        connect_options: &PgConnectOptions,
        scenario: &str,
    ) -> std::result::Result<(String, PgPool), String> {
        let schema = format!(
            "kong_req_ai003_mig_{}_{}",
            scenario,
            uuid::Uuid::new_v4().simple()
        );
        if !schema.starts_with("kong_req_ai003_mig_")
            || !schema
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(format!("拒绝创建非测试 schema: {schema}"));
        }

        sqlx::query(&format!("CREATE SCHEMA \"{schema}\""))
            .execute(admin_pool)
            .await
            .map_err(|error| format!("创建临时 schema {schema} 失败: {error}"))?;

        let search_path_schema = schema.clone();
        let pool_result = PgPoolOptions::new()
            .max_connections(1)
            .after_connect(move |connection, _metadata| {
                let set_search_path =
                    format!("SET search_path TO \"{search_path_schema}\", pg_catalog");
                Box::pin(async move {
                    sqlx::query(&set_search_path).execute(connection).await?;
                    Ok(())
                })
            })
            .connect_with(connect_options.clone())
            .await;

        match pool_result {
            Ok(pool) => Ok((schema, pool)),
            Err(error) => {
                let cleanup_result = drop_temp_schema(admin_pool, &schema).await;
                Err(format!(
                    "连接临时 schema {schema} 失败: {error}; 清理结果: {cleanup_result:?}"
                ))
            }
        }
    }

    async fn drop_temp_schema(
        admin_pool: &PgPool,
        schema: &str,
    ) -> std::result::Result<(), String> {
        if !schema.starts_with("kong_req_ai003_mig_")
            || !schema
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(format!("拒绝删除非测试 schema: {schema}"));
        }

        sqlx::query(&format!("DROP SCHEMA \"{schema}\" CASCADE"))
            .execute(admin_pool)
            .await
            .map_err(|error| format!("清理临时 schema {schema} 失败: {error}"))?;
        Ok(())
    }

    async fn bootstrap_through_006(pool: &PgPool) -> std::result::Result<(), String> {
        ensure_schema_meta(pool)
            .await
            .map_err(|error| format!("创建 schema_meta 失败: {error}"))?;

        for migration in CORE_MIGRATIONS {
            execute_migration(pool, migration)
                .await
                .map_err(|error| format!("执行旧 migration {} 失败: {error}", migration.name))?;
            if migration.name == "006_ai_usage_logs" {
                return Ok(());
            }
        }

        Err("未找到 006_ai_usage_logs migration".to_string())
    }

    async fn verify_successful_budget_upgrade(pool: &PgPool) -> std::result::Result<(), String> {
        bootstrap_through_006(pool).await?;

        let key_id = "00000000-0000-0000-0000-000000000301";
        sqlx::query(
            "INSERT INTO ai_virtual_keys (
                 id, name, key_hash, key_prefix, budget_limit, budget_used
             )
             VALUES (
                 $1::UUID, 'migration-opening-balance', 'migration-opening-hash',
                 'migration-opening', 100.5, 42.125
             )",
        )
        .bind(key_id)
        .execute(pool)
        .await
        .map_err(|error| format!("写入 006 旧预算数据失败: {error}"))?;

        up(pool)
            .await
            .map_err(|error| format!("执行 006→007/008 升级失败: {error}"))?;

        let state = schema_state(pool)
            .await
            .map_err(|error| format!("读取升级后的 schema_meta 失败: {error}"))?;
        if state.executed.last().map(String::as_str) != Some("008_ai_budget_overflow_idempotency")
            || !state.new_migrations.is_empty()
        {
            return Err(format!("升级后 migration 状态不正确: {state:?}"));
        }

        let column_shape: (String, Option<i32>, Option<i32>) = sqlx::query_as(
            "SELECT data_type, numeric_precision, numeric_scale
               FROM information_schema.columns
              WHERE table_schema = current_schema()
                AND table_name = 'ai_virtual_keys'
                AND column_name = 'budget_used'",
        )
        .fetch_one(pool)
        .await
        .map_err(|error| format!("读取 budget_used 列定义失败: {error}"))?;
        if column_shape != ("numeric".to_string(), Some(28), Some(12)) {
            return Err(format!("budget_used 列定义不正确: {column_shape:?}"));
        }

        let opening_balance: (String, String, String, String, i64) = sqlx::query_as(
            "SELECT kind, status, operation_id, accounted_cost_usd::TEXT,
                    last_account_revision
               FROM ai_budget_ledger
              WHERE virtual_key_id = $1::UUID",
        )
        .bind(key_id)
        .fetch_one(pool)
        .await
        .map_err(|error| format!("读取 opening balance 失败: {error}"))?;
        let expected_opening_operation = format!("opening-balance:v1:{key_id}");
        if opening_balance
            != (
                "opening_balance".to_string(),
                "settled".to_string(),
                expected_opening_operation,
                "42.125000000000".to_string(),
                0,
            )
        {
            return Err(format!("opening balance 内容不正确: {opening_balance:?}"));
        }

        let genesis: (i64, String, String) = sqlx::query_as(
            "SELECT checkpoint_revision, accounted_cost_usd::TEXT, operation_id
               FROM ai_budget_checkpoints
              WHERE virtual_key_id = $1::UUID",
        )
        .bind(key_id)
        .fetch_one(pool)
        .await
        .map_err(|error| format!("读取 genesis checkpoint 失败: {error}"))?;
        let expected_checkpoint_operation = format!("budget-checkpoint-genesis:v1:{key_id}");
        if genesis
            != (
                0,
                "42.125000000000".to_string(),
                expected_checkpoint_operation,
            )
        {
            return Err(format!("genesis checkpoint 内容不正确: {genesis:?}"));
        }

        Ok(())
    }

    async fn verify_invalid_old_budget_rejected(pool: &PgPool) -> std::result::Result<(), String> {
        bootstrap_through_006(pool).await?;
        let baseline = schema_state(pool)
            .await
            .map_err(|error| format!("读取 006 schema_meta 失败: {error}"))?
            .executed;

        let invalid_values = [
            ("nan", "budget_used", "NaN"),
            ("infinity", "budget_limit", "Infinity"),
            ("negative", "budget_used", "-1"),
            ("precision", "budget_limit", "0.1234567890123"),
            ("range", "budget_used", "10000000000000000"),
        ];

        for (index, (label, column, value)) in invalid_values.iter().enumerate() {
            let key_id = format!("00000000-0000-0000-0000-{:012}", index + 400);
            let insert_sql = format!(
                "INSERT INTO ai_virtual_keys (
                     id, name, key_hash, key_prefix, {column}
                 )
                 VALUES (
                     $1::UUID, $2, $3, $4, $5::DOUBLE PRECISION
                 )"
            );
            sqlx::query(&insert_sql)
                .bind(&key_id)
                .bind(format!("migration-invalid-{label}"))
                .bind(format!("migration-invalid-hash-{label}"))
                .bind(format!("invalid-{label}"))
                .bind(value)
                .execute(pool)
                .await
                .map_err(|error| format!("写入 {label} 旧预算数据失败: {error}"))?;

            if up(pool).await.is_ok() {
                return Err(format!("非法旧预算 {label} 未阻止 migration 007"));
            }

            let state = schema_state(pool)
                .await
                .map_err(|error| format!("{label} 失败后读取 schema_meta 失败: {error}"))?;
            if state.executed != baseline
                || state.executed.last().map(String::as_str) != Some("006_ai_usage_logs")
                || state
                    .executed
                    .iter()
                    .any(|migration| migration.starts_with("007_") || migration.starts_with("008_"))
            {
                return Err(format!(
                    "{label} 失败后 schema_meta 被错误推进: {:?}",
                    state.executed
                ));
            }

            let ledger_table: Option<String> =
                sqlx::query_scalar("SELECT to_regclass('ai_budget_ledger')::TEXT")
                    .fetch_one(pool)
                    .await
                    .map_err(|error| format!("{label} 失败后检查账本表失败: {error}"))?;
            let budget_type: String = sqlx::query_scalar(
                "SELECT data_type
                   FROM information_schema.columns
                  WHERE table_schema = current_schema()
                    AND table_name = 'ai_virtual_keys'
                    AND column_name = 'budget_used'",
            )
            .fetch_one(pool)
            .await
            .map_err(|error| format!("{label} 失败后检查旧列类型失败: {error}"))?;
            if ledger_table.is_some() || budget_type != "double precision" {
                return Err(format!(
                    "{label} 失败后 migration 007 未完整回滚: \
                     ledger_table={ledger_table:?}, budget_type={budget_type}"
                ));
            }

            sqlx::query("DELETE FROM ai_virtual_keys WHERE id = $1::UUID")
                .bind(&key_id)
                .execute(pool)
                .await
                .map_err(|error| format!("清理 {label} 旧预算数据失败: {error}"))?;
        }

        Ok(())
    }

    async fn run_real_postgres_upgrade_test(
        connect_options: PgConnectOptions,
    ) -> std::result::Result<(), String> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(connect_options.clone())
            .await
            .map_err(|error| format!("连接 PostgreSQL 迁移测试库失败: {error}"))?;

        let (success_schema, success_pool) =
            create_temp_schema_pool(&admin_pool, &connect_options, "success").await?;
        let success_result = verify_successful_budget_upgrade(&success_pool).await;
        success_pool.close().await;
        let success_cleanup = drop_temp_schema(&admin_pool, &success_schema).await;
        if let Err(error) = success_cleanup {
            admin_pool.close().await;
            return Err(error);
        }
        success_result?;

        let (invalid_schema, invalid_pool) =
            create_temp_schema_pool(&admin_pool, &connect_options, "invalid").await?;
        let invalid_result = verify_invalid_old_budget_rejected(&invalid_pool).await;
        invalid_pool.close().await;
        let invalid_cleanup = drop_temp_schema(&admin_pool, &invalid_schema).await;
        admin_pool.close().await;
        invalid_cleanup?;
        invalid_result
    }

    #[tokio::test]
    async fn ai_virtual_key_budget_migrations_upgrade_real_postgres() {
        let connect_options = migration_test_connect_options()
            .unwrap_or_else(|error| panic!("PostgreSQL 迁移测试配置无效: {error}"));
        let Some(connect_options) = connect_options else {
            eprintln!(
                "跳过真实 PostgreSQL 迁移测试：未设置 KONG_DB_MIGRATION_PG_TEST_URL \
                 或 KONG_PG_HOST"
            );
            return;
        };

        if let Err(error) = run_real_postgres_upgrade_test(connect_options).await {
            panic!("真实 PostgreSQL 006→007/008 迁移验证失败: {error}");
        }
    }
}

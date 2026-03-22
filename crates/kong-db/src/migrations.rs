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
    "ai_virtual_keys",
    "ai_models",
    "ai_providers",
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

    // Execute SQL statements one by one (sqlx prepared statements don't support multiple statements) — 逐条执行 SQL 语句（sqlx prepared statement 不支持多条语句）
    for statement in split_sql_statements(migration.sql) {
        sqlx::query(&statement)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                KongError::DatabaseError(format!("执行 migration {} 失败: {}", migration.name, e))
            })?;
    }

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

/// Split SQL file into individual statements by semicolons, stripping comment lines and empty statements — 将 SQL 文件按分号拆分为独立语句，剥离注释行和空语句
fn split_sql_statements(sql: &str) -> Vec<String> {
    // Strip all SQL single-line comments (lines starting with --), then split by semicolons — 先剥离所有 SQL 单行注释（-- 开头的行），再按分号拆分
    let stripped: String = sql
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");

    stripped
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

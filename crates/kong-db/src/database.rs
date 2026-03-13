use kong_config::KongConfig;
use kong_core::error::{KongError, Result};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::str::FromStr;

/// Database connection manager — 数据库连接管理器
#[derive(Clone)]
pub struct Database {
    /// Read-write connection pool — 读写连接池
    pool: PgPool,
    /// Read-only connection pool (optional) — 只读连接池（可选）
    ro_pool: Option<PgPool>,
}

impl Database {
    /// Create database connection from KongConfig — 从 KongConfig 创建数据库连接
    pub async fn connect(config: &KongConfig) -> Result<Self> {
        if config.is_dbless() {
            return Err(KongError::DatabaseError(
                "database=off 模式下不应创建数据库连接".to_string(),
            ));
        }

        let pool = create_pool(
            &config.pg_host,
            config.pg_port,
            &config.pg_database,
            &config.pg_user,
            config.pg_password.as_deref(),
            config.pg_schema.as_deref(),
            config.pg_ssl,
            config.pg_pool_size.unwrap_or(10),
            config.pg_timeout,
        )
        .await?;

        // Create read-only pool if configured — 创建只读连接池（如果配置了）
        let ro_pool = if let Some(ro_host) = config.effective_pg_ro_host() {
            Some(
                create_pool(
                    ro_host,
                    config.effective_pg_ro_port(),
                    config.effective_pg_ro_database(),
                    config.effective_pg_ro_user(),
                    config.effective_pg_ro_password(),
                    config
                        .pg_ro_schema
                        .as_deref()
                        .or(config.pg_schema.as_deref()),
                    config.effective_pg_ro_ssl(),
                    config
                        .pg_ro_pool_size
                        .unwrap_or(config.pg_pool_size.unwrap_or(10)),
                    config.pg_ro_timeout.unwrap_or(config.pg_timeout),
                )
                .await?,
            )
        } else {
            None
        };

        tracing::info!(
            "数据库连接已建立: {}:{}/{}",
            config.pg_host,
            config.pg_port,
            config.pg_database
        );

        Ok(Self { pool, ro_pool })
    }

    /// Get the read-write connection pool — 获取读写连接池
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Get the read-only pool, falling back to the primary pool — 获取只读连接池（如果有的话，否则回退到主池）
    pub fn read_pool(&self) -> &PgPool {
        self.ro_pool.as_ref().unwrap_or(&self.pool)
    }

    /// Close all connections — 关闭所有连接
    pub async fn close(&self) {
        self.pool.close().await;
        if let Some(ro_pool) = &self.ro_pool {
            ro_pool.close().await;
        }
    }

    /// Create from an existing pool (for testing) — 从已有连接池创建（用于测试）
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool,
            ro_pool: None,
        }
    }
}

/// Create a PostgreSQL connection pool — 创建 PostgreSQL 连接池
async fn create_pool(
    host: &str,
    port: u16,
    database: &str,
    user: &str,
    password: Option<&str>,
    schema: Option<&str>,
    ssl: bool,
    pool_size: u32,
    timeout_ms: u64,
) -> Result<PgPool> {
    let mut opts = PgConnectOptions::new()
        .host(host)
        .port(port)
        .database(database)
        .username(user);

    if let Some(password) = password {
        opts = opts.password(password);
    }

    // SSL configuration — SSL 配置
    if ssl {
        opts = opts.ssl_mode(sqlx::postgres::PgSslMode::Require);
    } else {
        opts = opts.ssl_mode(sqlx::postgres::PgSslMode::Prefer);
    }

    // Schema configuration — Schema 配置
    if let Some(schema) = schema {
        // Set search_path via options parameter — 通过 options 参数设置 search_path
        opts = PgConnectOptions::from_str(&format!(
            "postgres://{}:{}@{}:{}/{}?options=-c search_path={}",
            user,
            password.unwrap_or(""),
            host,
            port,
            database,
            schema
        ))
        .map_err(|e| KongError::DatabaseError(format!("无效的连接字符串: {}", e)))?;
    }

    let pool = PgPoolOptions::new()
        .max_connections(pool_size)
        .acquire_timeout(std::time::Duration::from_millis(timeout_ms))
        .test_before_acquire(true)
        .connect_with(opts)
        .await
        .map_err(|e| KongError::DatabaseError(format!("数据库连接失败: {}", e)))?;

    Ok(pool)
}

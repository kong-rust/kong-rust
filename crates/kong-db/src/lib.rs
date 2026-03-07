//! Kong 数据库层 — PostgreSQL DAO、缓存、db-less 模式
//!
//! 完全兼容 Kong 的数据库模式：
//! - 直接操作 Kong 的 PostgreSQL 表（无 ORM）
//! - 支持分页（游标分页，与 Kong 格式兼容）
//! - 支持按外键查询（如 Service 下的 Routes）
//! - 支持标签过滤
//! - 内存缓存（moka）
//! - db-less 声明式配置模式

pub mod cache;
pub mod dao;
pub mod database;
pub mod dbless;
pub mod migrations;

pub use cache::{CacheConfig, KongCache};
pub use dao::postgres::{
    self, ca_certificate_schema, certificate_schema, consumer_schema, plugin_schema, route_schema,
    service_schema, sni_schema, target_schema, upstream_schema, vault_schema, ColumnType,
    EntitySchema, PgDao,
};
pub use database::Database;
pub use dbless::{DblessDao, DblessStore};
pub use migrations::MigrationState;

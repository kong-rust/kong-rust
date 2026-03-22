//! Kong database layer — PostgreSQL DAO, cache, db-less mode — Kong 数据库层 — PostgreSQL DAO、缓存、db-less 模式
//!
//! Fully compatible with Kong's database schema: — 完全兼容 Kong 的数据库模式：
//! - Direct access to Kong's PostgreSQL tables (no ORM) — 直接操作 Kong 的 PostgreSQL 表（无 ORM）
//! - Cursor-based pagination compatible with Kong's format — 支持分页（游标分页，与 Kong 格式兼容）
//! - Foreign key queries (e.g. Routes under a Service) — 支持按外键查询（如 Service 下的 Routes）
//! - Tag filtering — 支持标签过滤
//! - In-memory cache (moka) — 内存缓存（moka）
//! - DB-less declarative config mode — db-less 声明式配置模式

pub mod cache;
pub mod dao;
pub mod database;
pub mod dbless;
pub mod migrations;

pub use cache::{CacheConfig, KongCache};
pub use dao::postgres::{
    self, ai_model_schema, ai_provider_schema, ai_virtual_key_schema, ca_certificate_schema,
    certificate_schema, consumer_schema, plugin_schema, route_schema, service_schema, sni_schema,
    target_schema, upstream_schema, vault_schema, ColumnType, EntitySchema, PgDao,
};
pub use database::Database;
pub use dbless::{DblessDao, DblessStore};
pub use migrations::MigrationState;

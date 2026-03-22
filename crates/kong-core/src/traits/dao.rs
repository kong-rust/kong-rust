use async_trait::async_trait;
use uuid::Uuid;

use crate::error::Result;
use crate::traits::Entity;

/// Paginated result — 分页结果
#[derive(Debug, Clone, serde::Serialize)]
pub struct Page<T> {
    /// Data list — 数据列表
    pub data: Vec<T>,
    /// Next page offset (None means no next page) — 下一页的偏移量（None 表示没有下一页）
    pub offset: Option<String>,
    /// Next page URL path (Kong-compatible) — 下一页的 URL 路径（与 Kong 兼容）
    pub next: Option<String>,
}

/// Tag filter mode — AND (all must match) vs OR (any can match) — 标签过滤模式 — AND（全部匹配）vs OR（任一匹配）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagFilterMode {
    /// All tags must match (PostgreSQL @> operator) — 所有标签必须匹配
    And,
    /// Any tag can match (PostgreSQL && operator) — 任一标签匹配即可
    Or,
}

impl Default for TagFilterMode {
    fn default() -> Self {
        TagFilterMode::And
    }
}

/// Pagination parameters — 分页参数
#[derive(Debug, Clone)]
pub struct PageParams {
    /// Page size (default 100, max 1000) — 每页数量（默认 100，最大 1000）
    pub size: usize,
    /// Offset — 偏移量
    pub offset: Option<String>,
    /// Tag filter — 标签过滤
    pub tags: Option<Vec<String>>,
    /// Tag filter mode (AND or OR) — 标签过滤模式（AND 或 OR）
    pub tags_mode: TagFilterMode,
    /// Field equality filters (e.g. custom_id, username) — 字段等值过滤（如 custom_id, username）
    pub filters: Vec<(String, String)>,
}

impl Default for PageParams {
    fn default() -> Self {
        Self {
            size: 100,
            offset: None,
            tags: None,
            tags_mode: TagFilterMode::And,
            filters: Vec::new(),
        }
    }
}

/// Primary key type — supports lookup by UUID or string (name) — 主键类型 — 支持 UUID 或字符串（name）查找
#[derive(Debug, Clone)]
pub enum PrimaryKey {
    /// UUID primary key — UUID 主键
    Id(Uuid),
    /// Endpoint key (e.g. name) — 端点键（如 name）
    EndpointKey(String),
}

impl PrimaryKey {
    /// Try to parse string as UUID, fall back to endpoint key on failure — 尝试将字符串解析为 UUID，失败则作为端点键
    pub fn from_str_or_uuid(s: &str) -> Self {
        match Uuid::parse_str(s) {
            Ok(uuid) => PrimaryKey::Id(uuid),
            Err(_) => PrimaryKey::EndpointKey(s.to_string()),
        }
    }
}

/// Generic data access interface — all entity DAOs must implement this — 通用数据访问接口 — 所有实体 DAO 必须实现
#[async_trait]
pub trait Dao<T: Entity>: Send + Sync {
    /// Insert a new entity — 插入新实体
    async fn insert(&self, entity: &T) -> Result<T>;

    /// Select a single entity by primary key — 按主键查询单个实体
    async fn select(&self, pk: &PrimaryKey) -> Result<Option<T>>;

    /// Paginated query — 分页查询
    async fn page(&self, params: &PageParams) -> Result<Page<T>>;

    /// Update entity (PATCH semantics, only update provided fields) — 更新实体（PATCH 语义，只更新提供的字段）
    async fn update(&self, pk: &PrimaryKey, entity: &serde_json::Value) -> Result<T>;

    /// Insert or update (PUT semantics) — 插入或更新（PUT 语义）
    async fn upsert(&self, pk: &PrimaryKey, entity: &T) -> Result<T>;

    /// Delete entity — 删除实体
    async fn delete(&self, pk: &PrimaryKey) -> Result<()>;

    /// Query by foreign key (e.g. all Routes under a Service) — 按外键查询（如查询 Service 下的所有 Routes）
    async fn select_by_foreign_key(
        &self,
        foreign_key_field: &str,
        foreign_key_value: &Uuid,
        params: &PageParams,
    ) -> Result<Page<T>>;
}

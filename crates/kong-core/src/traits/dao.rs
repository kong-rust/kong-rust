use async_trait::async_trait;
use uuid::Uuid;

use crate::error::Result;
use crate::traits::Entity;

/// 分页结果
#[derive(Debug, Clone, serde::Serialize)]
pub struct Page<T> {
    /// 数据列表
    pub data: Vec<T>,
    /// 下一页的偏移量（None 表示没有下一页）
    pub offset: Option<String>,
    /// 下一页的 URL 路径（与 Kong 兼容）
    pub next: Option<String>,
}

/// 分页参数
#[derive(Debug, Clone)]
pub struct PageParams {
    /// 每页数量（默认 100，最大 1000）
    pub size: usize,
    /// 偏移量
    pub offset: Option<String>,
    /// 标签过滤
    pub tags: Option<Vec<String>>,
}

impl Default for PageParams {
    fn default() -> Self {
        Self {
            size: 100,
            offset: None,
            tags: None,
        }
    }
}

/// 主键类型 — 支持 UUID 或字符串（name）查找
#[derive(Debug, Clone)]
pub enum PrimaryKey {
    /// UUID 主键
    Id(Uuid),
    /// 端点键（如 name）
    EndpointKey(String),
}

impl PrimaryKey {
    /// 尝试将字符串解析为 UUID，失败则作为端点键
    pub fn from_str_or_uuid(s: &str) -> Self {
        match Uuid::parse_str(s) {
            Ok(uuid) => PrimaryKey::Id(uuid),
            Err(_) => PrimaryKey::EndpointKey(s.to_string()),
        }
    }
}

/// 通用数据访问接口 — 所有实体 DAO 必须实现
#[async_trait]
pub trait Dao<T: Entity>: Send + Sync {
    /// 插入新实体
    async fn insert(&self, entity: &T) -> Result<T>;

    /// 按主键查询单个实体
    async fn select(&self, pk: &PrimaryKey) -> Result<Option<T>>;

    /// 分页查询
    async fn page(&self, params: &PageParams) -> Result<Page<T>>;

    /// 更新实体（PATCH 语义，只更新提供的字段）
    async fn update(&self, pk: &PrimaryKey, entity: &serde_json::Value) -> Result<T>;

    /// 插入或更新（PUT 语义）
    async fn upsert(&self, pk: &PrimaryKey, entity: &T) -> Result<T>;

    /// 删除实体
    async fn delete(&self, pk: &PrimaryKey) -> Result<()>;

    /// 按外键查询（如查询 Service 下的所有 Routes）
    async fn select_by_foreign_key(
        &self,
        foreign_key_field: &str,
        foreign_key_value: &Uuid,
        params: &PageParams,
    ) -> Result<Page<T>>;
}

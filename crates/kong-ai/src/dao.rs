//! AI 实体扩展查询 — PgDao<T> 通用 CRUD 之外的特殊操作
//! AI entity extension queries — special operations beyond PgDao<T> generic CRUD

use crate::models::{AiModel, AiVirtualKey};
use kong_core::error::Result;
use uuid::Uuid;

/// AiVirtualKey 扩展查询 — AiVirtualKey extension queries
#[async_trait::async_trait]
pub trait AiVirtualKeyExt: Send + Sync {
    /// 按 key_hash 查找（认证时使用）— look up by key_hash (used during authentication)
    async fn get_by_hash(&self, key_hash: &str) -> Result<Option<AiVirtualKey>>;
    /// 累加已使用预算 — accumulate budget usage by cost delta
    async fn update_budget(&self, id: &Uuid, cost_delta: f64) -> Result<()>;
}

/// AiModel 扩展查询 — AiModel extension queries
#[async_trait::async_trait]
pub trait AiModelExt: Send + Sync {
    /// 按 model name 查找同名 model group 的所有成员 — list all members of a model group by name
    async fn list_by_model_name(&self, name: &str) -> Result<Vec<AiModel>>;
    /// 列出所有 distinct model name（用于 GET /ai-model-groups）— list all distinct model names
    async fn list_distinct_names(&self) -> Result<Vec<String>>;
}

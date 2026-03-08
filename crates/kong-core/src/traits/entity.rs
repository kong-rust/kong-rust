use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

/// Entity marker trait — all Kong data models must implement this — 实体标记 trait — 所有 Kong 数据模型必须实现
pub trait Entity: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    /// Database table name — 数据库表名
    fn table_name() -> &'static str;

    /// Primary key field name (usually "id") — 主键字段名（通常为 "id"）
    fn primary_key_field() -> &'static str {
        "id"
    }

    /// Get the entity's primary key value — 获取实体的主键值
    fn id(&self) -> Uuid;

    /// Endpoint key name (used for Admin API URL path parameter matching, e.g. name or id) — 端点键名（用于 Admin API 的 URL 路径参数匹配，如 name 或 id）
    /// If the entity has a name field, return Some("name") — 如果实体有 name 字段，返回 Some("name")
    fn endpoint_key() -> Option<&'static str> {
        None
    }

    /// Get the endpoint key value (e.g. name) — 获取端点键的值（如 name）
    fn endpoint_key_value(&self) -> Option<String> {
        None
    }

    /// Get the entity's tags — 获取实体的标签
    fn tags(&self) -> Option<&Vec<String>> {
        None
    }

    /// Cache key prefix — 缓存键前缀
    fn cache_key_prefix() -> &'static str {
        Self::table_name()
    }

    /// Generate cache key — 生成缓存键
    fn cache_key(&self) -> String {
        format!("{}:{}", Self::cache_key_prefix(), self.id())
    }
}

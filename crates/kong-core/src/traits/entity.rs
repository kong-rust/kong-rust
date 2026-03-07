use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

/// 实体标记 trait — 所有 Kong 数据模型必须实现
pub trait Entity: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    /// 数据库表名
    fn table_name() -> &'static str;

    /// 主键字段名（通常为 "id"）
    fn primary_key_field() -> &'static str {
        "id"
    }

    /// 获取实体的主键值
    fn id(&self) -> Uuid;

    /// 端点键名（用于 Admin API 的 URL 路径参数匹配，如 name 或 id）
    /// 如果实体有 name 字段，返回 Some("name")
    fn endpoint_key() -> Option<&'static str> {
        None
    }

    /// 获取端点键的值（如 name）
    fn endpoint_key_value(&self) -> Option<String> {
        None
    }

    /// 获取实体的标签
    fn tags(&self) -> Option<&Vec<String>> {
        None
    }

    /// 缓存键前缀
    fn cache_key_prefix() -> &'static str {
        Self::table_name()
    }

    /// 生成缓存键
    fn cache_key(&self) -> String {
        format!("{}:{}", Self::cache_key_prefix(), self.id())
    }
}

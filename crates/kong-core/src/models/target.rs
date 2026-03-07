use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::ForeignKey;
use crate::traits::Entity;

/// Target 实体 — 与 Kong targets 表完全一致
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Target {
    pub id: Uuid,
    /// 注意：Target 使用毫秒时间戳（与其他实体的秒级不同）
    pub created_at: f64,
    pub updated_at: f64,
    /// 所属上游（外键引用 upstreams，级联删除）
    pub upstream: ForeignKey,
    /// 目标地址（host:port 格式），必填
    pub target: String,
    /// 权重，默认 100，范围 0-65535
    pub weight: i32,
    /// 缓存键（由 Kong 自动生成）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl Default for Target {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: 0.0,
            updated_at: 0.0,
            upstream: ForeignKey::new(Uuid::nil()),
            target: String::new(),
            weight: 100,
            cache_key: None,
            tags: None,
        }
    }
}

impl Entity for Target {
    fn table_name() -> &'static str {
        "targets"
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn endpoint_key() -> Option<&'static str> {
        Some("target")
    }

    fn endpoint_key_value(&self) -> Option<String> {
        Some(self.target.clone())
    }

    fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
}

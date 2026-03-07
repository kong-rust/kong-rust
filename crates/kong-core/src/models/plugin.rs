use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{ForeignKey, PluginOrdering, Protocol};
use crate::traits::Entity;

/// Plugin 实体 — 与 Kong plugins 表完全一致
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Plugin {
    pub id: Uuid,
    /// 插件名称，必填
    pub name: String,
    /// 插件实例名称
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_name: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// 关联的 Route（可选，级联删除）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<ForeignKey>,
    /// 关联的 Service（可选，级联删除）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<ForeignKey>,
    /// 关联的 Consumer（可选，级联删除）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumer: Option<ForeignKey>,
    /// 插件配置（动态 JSON 结构，由各插件 schema 定义）
    pub config: serde_json::Value,
    /// 插件适用的协议
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocols: Option<Vec<Protocol>>,
    /// 是否启用，默认 true
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// 插件排序配置
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ordering: Option<PluginOrdering>,
}

impl Default for Plugin {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            instance_name: None,
            created_at: 0,
            updated_at: 0,
            route: None,
            service: None,
            consumer: None,
            config: serde_json::Value::Object(serde_json::Map::new()),
            protocols: None,
            enabled: true,
            tags: None,
            ordering: None,
        }
    }
}

impl Entity for Plugin {
    fn table_name() -> &'static str {
        "plugins"
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn endpoint_key() -> Option<&'static str> {
        Some("instance_name")
    }

    fn endpoint_key_value(&self) -> Option<String> {
        self.instance_name.clone()
    }

    fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
}

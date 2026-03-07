use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use super::common::{CidrPort, ForeignKey, PathHandling, Protocol};
use crate::traits::Entity;

/// Route 实体 — 与 Kong routes 表完全一致
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Route {
    pub id: Uuid,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 支持的协议集合，默认 ["http", "https"]
    pub protocols: Vec<Protocol>,
    /// HTTPS 重定向状态码，默认 426，可选 301/302/307/308
    pub https_redirect_status_code: u16,
    /// 是否去除匹配的路径前缀，默认 true
    pub strip_path: bool,
    /// 是否保留原始 Host 头，默认 false
    pub preserve_host: bool,
    /// 是否启用请求缓冲，默认 true
    pub request_buffering: bool,
    /// 是否启用响应缓冲，默认 true
    pub response_buffering: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// 关联的 Service
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<ForeignKey>,
    /// SNI 列表（TLS/HTTPS 路由匹配）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snis: Option<Vec<String>>,
    /// 源 IP/端口（流模式路由）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<CidrPort>>,
    /// 目标 IP/端口（流模式路由）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destinations: Option<Vec<CidrPort>>,
    /// 匹配的 HTTP 方法列表
    #[serde(skip_serializing_if = "Option::is_none")]
    pub methods: Option<Vec<String>>,
    /// 匹配的主机名列表（支持通配符 *.example.com）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosts: Option<Vec<String>>,
    /// 匹配的路径列表（支持前缀匹配和正则）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    /// 匹配的请求头
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, Vec<String>>>,
    /// 正则优先级，默认 0
    pub regex_priority: i32,
    /// 路径处理方式，默认 v0
    pub path_handling: PathHandling,
    /// 表达式路由（expressions 路由风格）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
    /// 表达式路由优先级
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
}

impl Default for Route {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: 0,
            updated_at: 0,
            name: None,
            protocols: vec![Protocol::Http, Protocol::Https],
            https_redirect_status_code: 426,
            strip_path: true,
            preserve_host: false,
            request_buffering: true,
            response_buffering: true,
            tags: None,
            service: None,
            snis: None,
            sources: None,
            destinations: None,
            methods: None,
            hosts: None,
            paths: None,
            headers: None,
            regex_priority: 0,
            path_handling: PathHandling::V0,
            expression: None,
            priority: None,
        }
    }
}

impl Entity for Route {
    fn table_name() -> &'static str {
        "routes"
    }

    fn id(&self) -> Uuid {
        self.id
    }

    fn endpoint_key() -> Option<&'static str> {
        Some("name")
    }

    fn endpoint_key_value(&self) -> Option<String> {
        self.name.clone()
    }

    fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
}

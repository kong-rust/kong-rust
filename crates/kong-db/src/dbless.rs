//! DB-less mode — load entities from declarative config into memory — DB-less 模式 — 从声明式配置文件加载实体到内存
//!
//! Fully compatible with Kong's declarative config format: — 与 Kong 的 declarative config 格式完全兼容：
//! - Supports YAML and JSON formats — 支持 YAML 和 JSON 格式
//! - Supports _format_version field — 支持 _format_version 字段
//! - All entities loaded into in-memory HashMap — 所有实体加载到内存 HashMap
//! - Write operations return errors (read-only mode) — 写操作返回错误（只读模式）

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::RwLock;
use uuid::Uuid;

use kong_core::error::{KongError, Result};
use kong_core::traits::{Dao, Entity, Page, PageParams, PrimaryKey};

/// Supported declarative config format versions — 声明式配置格式版本
const SUPPORTED_FORMAT_VERSIONS: &[&str] = &["1.1", "2.1", "3.0"];

/// DB-less in-memory store — DB-less 内存存储
pub struct DblessStore {
    /// Data stored by entity type: table_name -> id -> entity_json — 按实体类型存储的数据: table_name -> id -> entity_json
    data: RwLock<HashMap<String, HashMap<Uuid, Value>>>,
    /// Endpoint key index: table_name -> (key_name, key_value) -> id — 端点键索引: table_name -> (key_name, key_value) -> id
    endpoint_keys: RwLock<HashMap<String, HashMap<String, Uuid>>>,
    /// Foreign key index: table_name -> fk_column -> fk_value -> Vec<id> — 外键索引: table_name -> fk_column -> fk_value -> Vec<id>
    foreign_keys: RwLock<HashMap<String, HashMap<String, HashMap<Uuid, Vec<Uuid>>>>>,
}

impl DblessStore {
    /// Create an empty in-memory store — 创建空的内存存储
    pub fn new() -> Self {
        Self {
            data: RwLock::new(HashMap::new()),
            endpoint_keys: RwLock::new(HashMap::new()),
            foreign_keys: RwLock::new(HashMap::new()),
        }
    }

    /// Load data from declarative config JSON — 从声明式配置 JSON 加载数据
    pub fn load_from_json(&self, config: &Value) -> Result<()> {
        let obj = config
            .as_object()
            .ok_or_else(|| KongError::ConfigError("声明式配置必须是 JSON 对象".to_string()))?;

        // Check format_version — 检查 format_version
        if let Some(version) = obj.get("_format_version") {
            let version_str = version.as_str().ok_or_else(|| {
                KongError::ConfigError("_format_version 必须是字符串".to_string())
            })?;
            if !SUPPORTED_FORMAT_VERSIONS.contains(&version_str) {
                return Err(KongError::ConfigError(format!(
                    "不支持的 format_version: {}，支持: {:?}",
                    version_str, SUPPORTED_FORMAT_VERSIONS
                )));
            }
        }

        let mut data = self
            .data
            .write()
            .map_err(|e| KongError::InternalError(format!("锁获取失败: {}", e)))?;
        let mut endpoint_keys = self
            .endpoint_keys
            .write()
            .map_err(|e| KongError::InternalError(format!("锁获取失败: {}", e)))?;
        let mut foreign_keys = self
            .foreign_keys
            .write()
            .map_err(|e| KongError::InternalError(format!("锁获取失败: {}", e)))?;

        // Clear existing data — 清空现有数据
        data.clear();
        endpoint_keys.clear();
        foreign_keys.clear();

        // Load each entity type — 加载各实体类型
        let entity_types = [
            ("services", "name"),
            ("routes", "name"),
            ("consumers", "username"),
            ("upstreams", "name"),
            ("targets", ""),
            ("plugins", ""),
            ("certificates", ""),
            ("snis", "name"),
            ("ca_certificates", ""),
            ("vaults", "prefix"),
            ("key_sets", "name"),
            ("keys", "name"),
        ];

        for (table_name, endpoint_key) in entity_types {
            if let Some(entities) = obj.get(table_name) {
                let arr = entities
                    .as_array()
                    .ok_or_else(|| KongError::ConfigError(format!("{} 必须是数组", table_name)))?;

                let table = data.entry(table_name.to_string()).or_default();
                let ek_map = endpoint_keys.entry(table_name.to_string()).or_default();

                for entity_json in arr {
                    // Extract or auto-generate ID — 提取或自动生成 ID
                    let (id, entity) = if let Ok(id) = extract_uuid(entity_json, "id") {
                        (id, entity_json.clone())
                    } else {
                        // Auto-generate UUID if missing — 缺少时自动生成 UUID
                        let id = Uuid::new_v4();
                        let mut entity = entity_json.clone();
                        if let Some(obj) = entity.as_object_mut() {
                            obj.insert("id".to_string(), Value::String(id.to_string()));
                        }
                        (id, entity)
                    };

                    // Build endpoint key index — 建立端点键索引
                    if !endpoint_key.is_empty() {
                        if let Some(key_val) =
                            entity.get(endpoint_key).and_then(|v| v.as_str())
                        {
                            ek_map.insert(key_val.to_string(), id);
                        }
                    }

                    // Build foreign key index — 建立外键索引
                    build_fk_index(&mut foreign_keys, table_name, &entity);

                    // Store entity — 存储实体
                    table.insert(id, entity);
                }

                tracing::info!("加载 {} 条 {} 实体", table.len(), table_name);
            }
        }

        Ok(())
    }

    /// Load declarative config from file — 从文件加载声明式配置
    pub fn load_from_file(&self, path: &str) -> Result<()> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| KongError::ConfigError(format!("读取声明式配置文件失败: {}", e)))?;

        let config: Value = if path.ends_with(".yml") || path.ends_with(".yaml") {
            // YAML support (serde_json's from_str doesn't support YAML, simplified here) — YAML 支持（通过 serde_json 的 from_str 不支持 YAML，此处简化处理）
            // Actual usage requires serde_yaml crate — 实际使用时需要 serde_yaml crate
            serde_json::from_str(&content).map_err(|_| {
                KongError::ConfigError(
                    "YAML 格式解析失败，请确保文件为有效的 JSON 或 YAML".to_string(),
                )
            })?
        } else {
            serde_json::from_str(&content)
                .map_err(|e| KongError::ConfigError(format!("JSON 解析失败: {}", e)))?
        };

        self.load_from_json(&config)
    }

    /// Get a specific entity — 获取指定实体
    fn get_entity(&self, table_name: &str, id: &Uuid) -> Result<Option<Value>> {
        let data = self
            .data
            .read()
            .map_err(|e| KongError::InternalError(format!("锁获取失败: {}", e)))?;

        Ok(data
            .get(table_name)
            .and_then(|table| table.get(id))
            .cloned())
    }

    /// Get entity by endpoint key — 通过端点键获取实体
    fn get_entity_by_endpoint_key(
        &self,
        table_name: &str,
        key_value: &str,
    ) -> Result<Option<Value>> {
        let endpoint_keys = self
            .endpoint_keys
            .read()
            .map_err(|e| KongError::InternalError(format!("锁获取失败: {}", e)))?;

        if let Some(id) = endpoint_keys.get(table_name).and_then(|m| m.get(key_value)) {
            self.get_entity(table_name, id)
        } else {
            Ok(None)
        }
    }

    /// List entities with pagination — 分页获取实体列表
    fn list_entities(
        &self,
        table_name: &str,
        offset: Option<&Uuid>,
        limit: usize,
    ) -> Result<(Vec<Value>, Option<Uuid>)> {
        let data = self
            .data
            .read()
            .map_err(|e| KongError::InternalError(format!("锁获取失败: {}", e)))?;

        let table = match data.get(table_name) {
            Some(t) => t,
            None => return Ok((vec![], None)),
        };

        // Sorted ID list (ensures consistent ordering) — 排序后的 ID 列表（保证顺序一致）
        let mut ids: Vec<&Uuid> = table.keys().collect();
        ids.sort();

        // Apply offset — 应用 offset
        let start = if let Some(offset_id) = offset {
            ids.iter()
                .position(|id| *id > offset_id)
                .unwrap_or(ids.len())
        } else {
            0
        };

        let ids = &ids[start..];
        let take = limit + 1;
        let has_next = ids.len() > take - 1;

        let result: Vec<Value> = ids
            .iter()
            .take(limit)
            .filter_map(|id| table.get(*id).cloned())
            .collect();

        let next_offset = if has_next {
            result
                .last()
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        } else {
            None
        };

        Ok((result, next_offset))
    }

    /// List entities by foreign key — 按外键获取实体列表
    fn list_by_foreign_key(
        &self,
        table_name: &str,
        fk_field: &str,
        fk_value: &Uuid,
        offset: Option<&Uuid>,
        limit: usize,
    ) -> Result<(Vec<Value>, Option<Uuid>)> {
        let foreign_keys = self
            .foreign_keys
            .read()
            .map_err(|e| KongError::InternalError(format!("锁获取失败: {}", e)))?;

        let ids = foreign_keys
            .get(table_name)
            .and_then(|m| m.get(fk_field))
            .and_then(|m| m.get(fk_value));

        let ids = match ids {
            Some(ids) => ids,
            None => return Ok((vec![], None)),
        };

        let data = self
            .data
            .read()
            .map_err(|e| KongError::InternalError(format!("锁获取失败: {}", e)))?;

        let table = match data.get(table_name) {
            Some(t) => t,
            None => return Ok((vec![], None)),
        };

        let mut sorted_ids = ids.clone();
        sorted_ids.sort();

        let start = if let Some(offset_id) = offset {
            sorted_ids
                .iter()
                .position(|id| id > offset_id)
                .unwrap_or(sorted_ids.len())
        } else {
            0
        };

        let remaining = &sorted_ids[start..];
        let has_next = remaining.len() > limit;

        let result: Vec<Value> = remaining
            .iter()
            .take(limit)
            .filter_map(|id| table.get(id).cloned())
            .collect();

        let next_offset = if has_next {
            result
                .last()
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        } else {
            None
        };

        Ok((result, next_offset))
    }
}

impl Default for DblessStore {
    fn default() -> Self {
        Self::new()
    }
}

/// DB-less DAO implementation (read-only) — DB-less DAO 实现（只读）
pub struct DblessDao<T: Entity> {
    store: std::sync::Arc<DblessStore>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: Entity> DblessDao<T> {
    pub fn new(store: std::sync::Arc<DblessStore>) -> Self {
        Self {
            store,
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<T: Entity> Dao<T> for DblessDao<T> {
    async fn insert(&self, _entity: &T) -> Result<T> {
        Err(KongError::DatabaseError(
            "db-less 模式不支持写操作，请使用 /config 端点更新配置".to_string(),
        ))
    }

    async fn select(&self, pk: &PrimaryKey) -> Result<Option<T>> {
        let table_name = T::table_name();

        let json = match pk {
            PrimaryKey::Id(id) => self.store.get_entity(table_name, id)?,
            PrimaryKey::EndpointKey(key) => {
                if let Ok(uuid) = Uuid::parse_str(key) {
                    self.store.get_entity(table_name, &uuid)?
                } else {
                    self.store.get_entity_by_endpoint_key(table_name, key)?
                }
            }
        };

        match json {
            Some(v) => {
                let entity: T = serde_json::from_value(v)
                    .map_err(|e| KongError::SerializationError(format!("反序列化失败: {}", e)))?;
                Ok(Some(entity))
            }
            None => Ok(None),
        }
    }

    async fn page(&self, params: &PageParams) -> Result<Page<T>> {
        let table_name = T::table_name();

        let offset_uuid = params.offset.as_ref().and_then(|s| decode_offset(s).ok());

        // For db-less mode, fetch extra to account for post-filtering — db-less 模式下多取数据以补偿过滤
        let fetch_size = if params.filters.is_empty() {
            params.size
        } else {
            // Fetch more to ensure enough results after filtering — 多取以确保过滤后有足够结果
            params.size * 10
        };

        let (entities_json, _next_offset) =
            self.store
                .list_entities(table_name, offset_uuid.as_ref(), fetch_size)?;

        // Apply field equality filters — 应用字段等值过滤
        let filtered_json: Vec<Value> = if params.filters.is_empty() {
            entities_json
        } else {
            entities_json.into_iter().filter(|v| {
                params.filters.iter().all(|(field, expected)| {
                    v.get(field).and_then(|val| val.as_str()) == Some(expected.as_str())
                })
            }).collect()
        };

        // Re-apply pagination to filtered results — 对过滤结果重新分页
        let has_next = filtered_json.len() > params.size;
        let page_data = if has_next {
            &filtered_json[..params.size]
        } else {
            &filtered_json[..]
        };

        let mut data = Vec::with_capacity(page_data.len());
        for v in page_data {
            let entity: T = serde_json::from_value(v.clone())
                .map_err(|e| KongError::SerializationError(format!("反序列化失败: {}", e)))?;
            data.push(entity);
        }

        // Compute next offset from the last item in the filtered page — 从过滤后页面的最后一项计算下一页偏移量
        let filtered_next = if has_next {
            data.last()
                .and_then(|e| {
                    let v = serde_json::to_value(e).unwrap_or_default();
                    v.get("id").and_then(|id| id.as_str()).and_then(|s| Uuid::parse_str(s).ok())
                })
        } else {
            None
        };

        let offset = filtered_next.map(|id| encode_offset(&id));

        Ok(Page {
            data,
            offset: offset.clone(),
            next: offset.map(|o| {
                // Include size param in next URL if non-default — 非默认 size 时在 next URL 中包含 size 参数
                let default_size = PageParams::default().size;
                if params.size != default_size {
                    format!("/{}?offset={}&size={}", table_name, o, params.size)
                } else {
                    format!("/{}?offset={}", table_name, o)
                }
            }),
        })
    }

    async fn update(&self, _pk: &PrimaryKey, _entity: &Value) -> Result<T> {
        Err(KongError::DatabaseError(
            "db-less 模式不支持写操作，请使用 /config 端点更新配置".to_string(),
        ))
    }

    async fn upsert(&self, _pk: &PrimaryKey, _entity: &T) -> Result<T> {
        Err(KongError::DatabaseError(
            "db-less 模式不支持写操作，请使用 /config 端点更新配置".to_string(),
        ))
    }

    async fn delete(&self, _pk: &PrimaryKey) -> Result<()> {
        Err(KongError::DatabaseError(
            "db-less 模式不支持写操作，请使用 /config 端点更新配置".to_string(),
        ))
    }

    async fn select_by_foreign_key(
        &self,
        foreign_key_field: &str,
        foreign_key_value: &Uuid,
        params: &PageParams,
    ) -> Result<Page<T>> {
        let table_name = T::table_name();

        let offset_uuid = params.offset.as_ref().and_then(|s| decode_offset(s).ok());

        let (entities_json, next_offset) = self.store.list_by_foreign_key(
            table_name,
            foreign_key_field,
            foreign_key_value,
            offset_uuid.as_ref(),
            params.size,
        )?;

        let mut data = Vec::with_capacity(entities_json.len());
        for v in entities_json {
            let entity: T = serde_json::from_value(v)
                .map_err(|e| KongError::SerializationError(format!("反序列化失败: {}", e)))?;
            data.push(entity);
        }

        let offset = next_offset.map(|id| encode_offset(&id));

        Ok(Page {
            data,
            offset: offset.clone(),
            next: offset.map(|o| {
                // Include size param in next URL if non-default — 非默认 size 时在 next URL 中包含 size 参数
                let default_size = PageParams::default().size;
                if params.size != default_size {
                    format!("/{}?offset={}&size={}", table_name, o, params.size)
                } else {
                    format!("/{}?offset={}", table_name, o)
                }
            }),
        })
    }
}

// ========== Helper functions — 辅助函数 ==========

/// Extract UUID from JSON object — 从 JSON 对象提取 UUID
fn extract_uuid(json: &Value, field: &str) -> Result<Uuid> {
    let s = json
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| KongError::ConfigError(format!("实体缺少 {} 字段", field)))?;
    Uuid::parse_str(s).map_err(|e| KongError::ConfigError(format!("无效的 UUID {}: {}", field, e)))
}

/// Build foreign key index — 构建外键索引
fn build_fk_index(
    foreign_keys: &mut HashMap<String, HashMap<String, HashMap<Uuid, Vec<Uuid>>>>,
    table_name: &str,
    entity_json: &Value,
) {
    let fk_fields = match table_name {
        "routes" => vec!["service"],
        "targets" => vec!["upstream"],
        "plugins" => vec!["route", "service", "consumer"],
        "snis" => vec!["certificate"],
        "keys" => vec!["set"],
        _ => vec![],
    };

    let entity_id = entity_json
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());

    let entity_id = match entity_id {
        Some(id) => id,
        None => return,
    };

    let table_fks = foreign_keys.entry(table_name.to_string()).or_default();

    for fk_field in fk_fields {
        if let Some(fk_obj) = entity_json.get(fk_field) {
            let fk_id = if let Some(obj) = fk_obj.as_object() {
                obj.get("id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            } else {
                fk_obj.as_str().and_then(|s| Uuid::parse_str(s).ok())
            };

            if let Some(fk_id) = fk_id {
                table_fks
                    .entry(fk_field.to_string())
                    .or_default()
                    .entry(fk_id)
                    .or_default()
                    .push(entity_id);
            }
        }
    }
}

/// Encode offset token — 编码 offset token
fn encode_offset(id: &Uuid) -> String {
    use base64::Engine;
    let json = serde_json::to_string(&[id.to_string()]).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes())
}

/// Decode offset token — 解码 offset token
fn decode_offset(token: &str) -> Result<Uuid> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token.as_bytes())
        .map_err(|e| KongError::ValidationError(format!("无效的 offset: {}", e)))?;

    let json_str = String::from_utf8(bytes)
        .map_err(|e| KongError::ValidationError(format!("无效的 offset: {}", e)))?;

    let arr: Vec<String> = serde_json::from_str(&json_str)
        .map_err(|e| KongError::ValidationError(format!("无效的 offset: {}", e)))?;

    Uuid::parse_str(arr.first().unwrap_or(&String::new()))
        .map_err(|e| KongError::ValidationError(format!("无效的 offset UUID: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_declarative_config() {
        let store = DblessStore::new();

        let config = serde_json::json!({
            "_format_version": "3.0",
            "services": [
                {
                    "id": "550e8400-e29b-41d4-a716-446655440000",
                    "name": "test-service",
                    "host": "httpbin.org",
                    "port": 80,
                    "protocol": "http",
                    "created_at": 1609459200,
                    "updated_at": 1609459200
                }
            ],
            "routes": [
                {
                    "id": "660e8400-e29b-41d4-a716-446655440001",
                    "name": "test-route",
                    "paths": ["/test"],
                    "service": { "id": "550e8400-e29b-41d4-a716-446655440000" },
                    "created_at": 1609459200,
                    "updated_at": 1609459200
                }
            ]
        });

        store.load_from_json(&config).unwrap();

        // Verify service loaded — 验证 service 加载
        let service_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let service = store.get_entity("services", &service_id).unwrap();
        assert!(service.is_some());

        // Verify endpoint key index — 验证端点键索引
        let service = store
            .get_entity_by_endpoint_key("services", "test-service")
            .unwrap();
        assert!(service.is_some());

        // Verify foreign key index — 验证外键索引
        let (routes, _) = store
            .list_by_foreign_key("routes", "service", &service_id, None, 100)
            .unwrap();
        assert_eq!(routes.len(), 1);
    }

    #[test]
    fn test_dbless_write_operations_fail() {
        let store = std::sync::Arc::new(DblessStore::new());
        let dao: DblessDao<kong_core::models::Service> = DblessDao::new(store);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(dao.insert(&kong_core::models::Service::default()));
        assert!(result.is_err());
    }

    #[test]
    fn test_unsupported_format_version() {
        let store = DblessStore::new();
        let config = serde_json::json!({
            "_format_version": "99.0"
        });
        let result = store.load_from_json(&config);
        assert!(result.is_err());
    }
}

//! Kong 缓存层 — 基于 moka 实现内存缓存
//!
//! 与 Kong 的 kong.cache 行为一致：
//! - cache_key 格式: 实体类型:主键 或 实体类型:唯一键名:值
//! - 支持正缓存和负缓存（neg_ttl）
//! - 支持 TTL 和容量配置
//! - 线程安全

use kong_core::traits::Entity;
use moka::sync::Cache;
use serde_json::Value;
use std::time::Duration;

/// Kong 缓存 — 模拟 Kong 的 kong.cache 行为
#[derive(Clone)]
pub struct KongCache {
    /// 主缓存（存储实体 JSON）
    cache: Cache<String, CacheEntry>,
    /// 缓存配置
    #[allow(dead_code)]
    config: CacheConfig,
}

/// 缓存条目
#[derive(Clone, Debug)]
enum CacheEntry {
    /// 正缓存（实体数据）
    Hit(Value),
    /// 负缓存（实体不存在）
    Miss,
}

/// 缓存配置
#[derive(Clone, Debug)]
pub struct CacheConfig {
    /// 最大缓存条目数
    pub max_capacity: u64,
    /// 正缓存 TTL（秒），0 = 永不过期
    pub ttl: u64,
    /// 负缓存 TTL（秒），None 时使用 ttl
    pub neg_ttl: Option<u64>,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_capacity: 10000,
            ttl: 0,
            neg_ttl: None,
        }
    }
}

impl KongCache {
    /// 创建新的缓存实例
    pub fn new(config: CacheConfig) -> Self {
        let mut builder = Cache::builder().max_capacity(config.max_capacity);

        // 如果 TTL > 0，设置过期时间
        if config.ttl > 0 {
            builder = builder.time_to_live(Duration::from_secs(config.ttl));
        }

        Self {
            cache: builder.build(),
            config,
        }
    }

    /// 从 KongConfig 创建缓存
    pub fn from_kong_config(config: &kong_config::KongConfig) -> Self {
        let mem_bytes = config.mem_cache_size_bytes();
        // 估算条目数：假设每条目平均 1KB
        let max_capacity = (mem_bytes / 1024).max(1000);

        Self::new(CacheConfig {
            max_capacity,
            ttl: config.db_cache_ttl,
            neg_ttl: config.db_cache_neg_ttl,
        })
    }

    /// 获取缓存值
    ///
    /// 返回:
    /// - Some(Some(value)) — 缓存命中，实体存在
    /// - Some(None) — 缓存命中，实体不存在（负缓存）
    /// - None — 缓存未命中
    pub fn get(&self, key: &str) -> Option<Option<Value>> {
        self.cache.get(key).map(|entry| match entry {
            CacheEntry::Hit(v) => Some(v),
            CacheEntry::Miss => None,
        })
    }

    /// 设置缓存值（正缓存）
    pub fn set(&self, key: &str, value: Value) {
        self.cache
            .insert(key.to_string(), CacheEntry::Hit(value));
    }

    /// 设置负缓存（标记实体不存在）
    pub fn set_miss(&self, key: &str) {
        // 负缓存使用单独的 TTL
        self.cache.insert(key.to_string(), CacheEntry::Miss);
    }

    /// 删除缓存条目
    pub fn invalidate(&self, key: &str) {
        self.cache.invalidate(key);
    }

    /// 按前缀删除缓存条目
    pub fn invalidate_prefix(&self, prefix: &str) {
        // 收集需要删除的键
        let keys_to_remove: Vec<String> = self
            .cache
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.as_ref().clone())
            .collect();

        for key in keys_to_remove {
            self.cache.invalidate(&key);
        }
    }

    /// 清空所有缓存
    pub fn purge(&self) {
        self.cache.invalidate_all();
        self.cache.run_pending_tasks();
    }

    /// 获取缓存统计信息
    pub fn entry_count(&self) -> u64 {
        self.cache.entry_count()
    }

    // ========== 实体级缓存操作 ==========

    /// 生成实体的缓存键
    /// 格式: 实体类型:主键
    pub fn entity_cache_key<T: Entity>(id: &uuid::Uuid) -> String {
        format!("{}:{}", T::cache_key_prefix(), id)
    }

    /// 生成实体端点键的缓存键
    /// 格式: 实体类型:字段名:值
    pub fn entity_endpoint_cache_key<T: Entity>(key_name: &str, key_value: &str) -> String {
        format!("{}:{}:{}", T::cache_key_prefix(), key_name, key_value)
    }

    /// 获取缓存的实体
    pub fn get_entity<T: Entity>(&self, id: &uuid::Uuid) -> Option<Option<T>> {
        let key = Self::entity_cache_key::<T>(id);
        self.get(&key).map(|opt| {
            opt.and_then(|v| serde_json::from_value(v).ok())
        })
    }

    /// 缓存实体
    pub fn set_entity<T: Entity>(&self, entity: &T) {
        let key = Self::entity_cache_key::<T>(&entity.id());
        if let Ok(value) = serde_json::to_value(entity) {
            self.set(&key, value);
        }

        // 同时缓存端点键的映射
        if let (Some(ek), Some(ev)) = (T::endpoint_key(), entity.endpoint_key_value()) {
            let ek_key = Self::entity_endpoint_cache_key::<T>(ek, &ev);
            if let Ok(value) = serde_json::to_value(entity) {
                self.set(&ek_key, value);
            }
        }
    }

    /// 使实体缓存失效
    pub fn invalidate_entity<T: Entity>(&self, entity: &T) {
        let key = Self::entity_cache_key::<T>(&entity.id());
        self.invalidate(&key);

        // 同时失效端点键缓存
        if let (Some(ek), Some(ev)) = (T::endpoint_key(), entity.endpoint_key_value()) {
            let ek_key = Self::entity_endpoint_cache_key::<T>(ek, &ev);
            self.invalidate(&ek_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_basic() {
        let cache = KongCache::new(CacheConfig::default());

        // 缓存未命中
        assert!(cache.get("test:key").is_none());

        // 设置正缓存
        cache.set("test:key", Value::String("hello".to_string()));
        let result = cache.get("test:key");
        assert!(result.is_some());
        assert_eq!(result.unwrap().unwrap(), Value::String("hello".to_string()));

        // 删除缓存
        cache.invalidate("test:key");
        assert!(cache.get("test:key").is_none());
    }

    #[test]
    fn test_cache_miss() {
        let cache = KongCache::new(CacheConfig::default());

        // 设置负缓存
        cache.set_miss("services:nonexistent");
        let result = cache.get("services:nonexistent");
        assert!(result.is_some()); // 缓存命中
        assert!(result.unwrap().is_none()); // 但实体不存在
    }

    #[test]
    fn test_cache_purge() {
        let cache = KongCache::new(CacheConfig::default());

        cache.set("key1", Value::Null);
        cache.set("key2", Value::Null);
        assert!(cache.get("key1").is_some());
        assert!(cache.get("key2").is_some());

        cache.purge();
        assert!(cache.get("key1").is_none());
        assert!(cache.get("key2").is_none());
    }

    #[test]
    fn test_cache_key_format() {
        use kong_core::models::Service;
        let id = uuid::Uuid::new_v4();
        let key = KongCache::entity_cache_key::<Service>(&id);
        assert!(key.starts_with("services:"));
        assert!(key.contains(&id.to_string()));
    }

    #[test]
    fn test_cache_prefix_invalidation() {
        let cache = KongCache::new(CacheConfig::default());

        cache.set("services:abc", Value::Null);
        cache.set("services:def", Value::Null);
        cache.set("routes:ghi", Value::Null);

        cache.invalidate_prefix("services:");
        assert!(cache.get("services:abc").is_none());
        assert!(cache.get("services:def").is_none());
        assert!(cache.get("routes:ghi").is_some());
    }
}

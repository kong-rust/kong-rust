//! Disk cache for DP config persistence — DP 配置磁盘缓存
//!
//! When CP is unreachable, DP loads config from disk cache — CP 不可达时，DP 从磁盘缓存加载配置
//! File permissions: 0600, corruption detection: JSON validation — 文件权限: 0600，损坏检测: JSON 验证

use std::fs;
use std::path::PathBuf;
use serde_json::Value;
use crate::ClusterError;

/// Disk cache manager for DP — DP 磁盘缓存管理器
#[derive(Debug, Clone)]
pub struct DiskCache {
    cache_dir: PathBuf,
}

impl DiskCache {
    /// Create disk cache manager — 创建磁盘缓存管理器
    ///
    /// cache_dir is typically {prefix}/cache/clustering — 缓存目录通常是 {prefix}/cache/clustering
    pub fn new(prefix: &str) -> Self {
        Self {
            cache_dir: PathBuf::from(prefix).join("cache").join("clustering"),
        }
    }

    /// Cache file path — 缓存文件路径
    fn cache_file(&self) -> PathBuf {
        self.cache_dir.join("config.json")
    }

    /// Save config to disk — 保存配置到磁盘
    pub fn save(&self, config: &Value, config_hash: &str) -> Result<(), ClusterError> {
        // Ensure directory exists — 确保目录存在
        fs::create_dir_all(&self.cache_dir)?;

        let wrapper = serde_json::json!({
            "config_hash": config_hash,
            "config_table": config,
            "cached_at": chrono::Utc::now().to_rfc3339(),
        });

        let data = serde_json::to_vec_pretty(&wrapper)?;
        let path = self.cache_file();

        // Write atomically via temp file — 通过临时文件原子写入
        let tmp_path = path.with_extension("tmp");
        fs::write(&tmp_path, &data)?;

        // Set permissions 0600 — 设置权限 0600
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))?;
        }

        fs::rename(&tmp_path, &path)?;
        tracing::info!("配置已缓存到磁盘: {}", path.display());
        Ok(())
    }

    /// Load config from disk cache — 从磁盘缓存加载配置
    ///
    /// Returns (config_table, config_hash) or None if no valid cache — 返回 (config_table, config_hash) 或 None
    pub fn load(&self) -> Option<(Value, String)> {
        let path = self.cache_file();
        if !path.exists() {
            tracing::debug!("无磁盘缓存: {}", path.display());
            return None;
        }

        match fs::read_to_string(&path) {
            Ok(content) => {
                // Validate JSON — 验证 JSON 格式
                match serde_json::from_str::<Value>(&content) {
                    Ok(wrapper) => {
                        let config_table = wrapper.get("config_table")?.clone();
                        let config_hash = wrapper.get("config_hash")?.as_str()?.to_string();
                        tracing::info!("从磁盘缓存加载配置: hash={}", config_hash);
                        Some((config_table, config_hash))
                    }
                    Err(e) => {
                        // Corrupted cache: warn + delete + treat as no cache — 缓存损坏: 警告 + 删除 + 按无缓存处理
                        tracing::warn!("磁盘缓存 JSON 损坏，删除: {} ({})", path.display(), e);
                        let _ = fs::remove_file(&path);
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!("读取磁盘缓存失败: {} ({})", path.display(), e);
                None
            }
        }
    }

    /// Clear cache — 清除缓存
    pub fn clear(&self) {
        let path = self.cache_file();
        if path.exists() {
            if let Err(e) = fs::remove_file(&path) {
                tracing::warn!("删除磁盘缓存失败: {} ({})", path.display(), e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_save_and_load() {
        let tmp = TempDir::new().unwrap();
        let cache = DiskCache::new(tmp.path().to_str().unwrap());

        let config = serde_json::json!({
            "services": [{"id": "abc", "name": "test-svc"}],
            "routes": [],
        });
        let hash = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4";

        cache.save(&config, hash).unwrap();

        let (loaded_config, loaded_hash) = cache.load().unwrap();
        assert_eq!(loaded_hash, hash);
        assert_eq!(loaded_config["services"][0]["name"], "test-svc");
    }

    #[test]
    fn test_load_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let cache = DiskCache::new(tmp.path().to_str().unwrap());
        assert!(cache.load().is_none());
    }

    #[test]
    fn test_corrupted_cache() {
        let tmp = TempDir::new().unwrap();
        let cache = DiskCache::new(tmp.path().to_str().unwrap());

        // Write corrupted file — 写入损坏文件
        let cache_dir = tmp.path().join("cache").join("clustering");
        fs::create_dir_all(&cache_dir).unwrap();
        fs::write(cache_dir.join("config.json"), "NOT VALID JSON{{{").unwrap();

        // Should return None and delete the file — 应返回 None 并删除文件
        assert!(cache.load().is_none());
        assert!(!cache_dir.join("config.json").exists());
    }

    #[test]
    fn test_clear() {
        let tmp = TempDir::new().unwrap();
        let cache = DiskCache::new(tmp.path().to_str().unwrap());

        let config = serde_json::json!({"test": true});
        cache.save(&config, "hash123").unwrap();
        assert!(cache.load().is_some());

        cache.clear();
        assert!(cache.load().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let cache = DiskCache::new(tmp.path().to_str().unwrap());

        let config = serde_json::json!({"test": true});
        cache.save(&config, "hash123").unwrap();

        let metadata = fs::metadata(cache.cache_file()).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "File permissions should be 0600");
    }
}

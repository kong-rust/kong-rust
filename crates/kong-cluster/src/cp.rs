//! Control Plane — WebSocket server, config export, hash, broadcast
//! 控制面 — WebSocket 服务端、配置导出、哈希计算、广播

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;
use chrono::Utc;
use serde_json::Value;

use crate::{ConfigHashes, DataPlaneInfo, SyncStatus, ClusterError, EMPTY_CONFIG_HASH};
use crate::protocol;

/// CP state shared across connections — 跨连接共享的 CP 状态
pub struct ControlPlane {
    /// Connected DP nodes — 已连接的 DP 节点
    pub data_planes: Arc<RwLock<HashMap<Uuid, DataPlaneInfo>>>,
    /// Broadcast channel for config push — 配置推送广播通道
    config_tx: broadcast::Sender<Arc<Vec<u8>>>,
    /// Current config hash — 当前配置哈希
    current_hash: Arc<RwLock<String>>,
    /// Current hashes — 当前多级哈希
    current_hashes: Arc<RwLock<ConfigHashes>>,
    /// Current config payload (compressed V1 format) — 当前配置负载（V1 压缩格式）
    current_payload: Arc<RwLock<Option<Arc<Vec<u8>>>>>,
    /// Global config version counter for V2 protocol — V2 协议全局配置版本计数器
    config_version: AtomicU64,
}

impl ControlPlane {
    pub fn new() -> Self {
        let (config_tx, _) = broadcast::channel(16);
        Self {
            data_planes: Arc::new(RwLock::new(HashMap::new())),
            config_tx,
            current_hash: Arc::new(RwLock::new(EMPTY_CONFIG_HASH.to_string())),
            current_hashes: Arc::new(RwLock::new(ConfigHashes::default())),
            current_payload: Arc::new(RwLock::new(None)),
            config_version: AtomicU64::new(0),
        }
    }

    /// Register a new DP connection — 注册新 DP 连接
    pub async fn register_dp(&self, info: DataPlaneInfo) -> broadcast::Receiver<Arc<Vec<u8>>> {
        let id = info.id;
        self.data_planes.write().await.insert(id, info);
        tracing::info!("DP registered: {}", id);
        self.config_tx.subscribe()
    }

    /// Unregister a DP connection — 注销 DP 连接
    pub async fn unregister_dp(&self, id: &Uuid) {
        self.data_planes.write().await.remove(id);
        tracing::info!("DP unregistered: {}", id);
    }

    /// Update DP sync status and config hash — 更新 DP 同步状态和配置哈希
    pub async fn update_dp_status(&self, id: &Uuid, config_hash: &str) {
        if let Some(dp) = self.data_planes.write().await.get_mut(id) {
            dp.config_hash = config_hash.to_string();
            dp.last_seen = Utc::now();

            let current = self.current_hash.read().await;
            dp.sync_status = if config_hash == current.as_str() {
                SyncStatus::Normal
            } else {
                SyncStatus::Unknown
            };
        }
    }

    /// Push config to all connected DPs — 推送配置给所有已连接的 DP
    ///
    /// Serializes + compresses once, broadcasts Arc<Vec<u8>> (zero-copy) — 序列化+压缩一次，广播 Arc<Vec<u8>>（零拷贝）
    pub async fn push_config(&self, config_table: &Value) -> Result<(), ClusterError> {
        let hashes = calculate_config_hash(config_table);
        let payload = protocol::build_v1_payload(config_table, &hashes.config, &hashes)?;

        *self.current_hash.write().await = hashes.config.clone();
        *self.current_hashes.write().await = hashes;

        let payload = Arc::new(payload);
        // Store current payload for V2 get_delta — 存储当前负载供 V2 get_delta 使用
        *self.current_payload.write().await = Some(Arc::clone(&payload));
        // Increment global config version — 递增全局配置版本号
        self.config_version.fetch_add(1, Ordering::SeqCst);
        // Broadcast to all subscribers; ignore error if no receivers — 广播给所有订阅者；无接收者时忽略错误
        let _ = self.config_tx.send(payload);

        let dp_count = self.data_planes.read().await.len();
        tracing::info!("配置已推送给 {} 个 DP", dp_count);
        Ok(())
    }

    /// Get current config payload (compressed V1 format) — 获取当前配置负载（V1 压缩格式）
    pub async fn current_payload(&self) -> Option<Arc<Vec<u8>>> {
        self.current_payload.read().await.clone()
    }

    /// Get current global config version — 获取当前全局配置版本号
    pub fn config_version(&self) -> u64 {
        self.config_version.load(Ordering::SeqCst)
    }

    /// Subscribe to config broadcast channel — 订阅配置广播通道
    pub fn subscribe_config(&self) -> broadcast::Receiver<Arc<Vec<u8>>> {
        self.config_tx.subscribe()
    }

    /// Get current config hash — 获取当前配置哈希
    pub async fn current_hash(&self) -> String {
        self.current_hash.read().await.clone()
    }

    /// Get connected DP list for /clustering/data-planes API — 获取已连接 DP 列表（用于 Admin API）
    pub async fn list_data_planes(&self) -> Vec<DataPlaneInfo> {
        self.data_planes.read().await.values().cloned().collect()
    }

    /// Purge stale DPs that haven't been seen within timeout — 清除超时未响应的 DP
    pub async fn purge_stale_dps(&self, timeout_secs: u64) {
        let cutoff = Utc::now() - chrono::Duration::seconds(timeout_secs as i64);
        let mut dps = self.data_planes.write().await;
        let before = dps.len();
        dps.retain(|_, dp| dp.last_seen > cutoff);
        let purged = before - dps.len();
        if purged > 0 {
            tracing::info!("清除 {} 个超时 DP", purged);
        }
    }
}

impl Default for ControlPlane {
    fn default() -> Self {
        Self::new()
    }
}

// ========== Config Hash Calculation — 配置哈希计算 ==========

/// Calculate multi-level config hash, compatible with Kong's algorithm — 计算多级配置哈希，兼容 Kong 算法
///
/// Rules: — 规则:
/// - null -> "/null/"
/// - string/number -> as-is
/// - boolean -> "true"/"false"
/// - empty table -> "{}"
/// - array -> elements joined by ";"
/// - object -> keys sorted, "key:value;" format
pub fn calculate_config_hash(config_table: &Value) -> ConfigHashes {
    let routes_hash = hash_entity(config_table.get("routes"));
    let services_hash = hash_entity(config_table.get("services"));
    let plugins_hash = hash_entity(config_table.get("plugins"));
    let upstreams_hash = hash_entity(config_table.get("upstreams"));
    let targets_hash = hash_entity(config_table.get("targets"));

    // Rest hash: config without the above fields — 其余哈希: 去掉上述字段的配置
    let rest_hash = {
        let mut rest = config_table.clone();
        if let Some(obj) = rest.as_object_mut() {
            obj.remove("routes");
            obj.remove("services");
            obj.remove("plugins");
            obj.remove("upstreams");
            obj.remove("targets");
        }
        hash_entity(Some(&rest))
    };

    // Final hash: MD5 of concatenated sub-hashes — 最终哈希: 各分量哈希级联后的 MD5
    let combined = format!(
        "{}{}{}{}{}{}",
        routes_hash, services_hash, plugins_hash, upstreams_hash, targets_hash, rest_hash
    );
    let config_hash = md5_hex(combined.as_bytes());

    ConfigHashes {
        config: config_hash,
        routes: routes_hash,
        services: services_hash,
        plugins: plugins_hash,
        upstreams: upstreams_hash,
        targets: targets_hash,
    }
}

/// Hash a single entity group — 哈希单个实体组
/// Public for compatibility testing with Kong Lua — 公开用于与 Kong Lua 兼容性测试
pub fn hash_entity(value: Option<&Value>) -> String {
    let mut buf = String::new();
    match value {
        Some(v) => to_sorted_string(v, &mut buf),
        None => to_sorted_string(&Value::Null, &mut buf),
    }
    md5_hex(buf.as_bytes())
}

/// Serialize value to sorted string (Kong-compatible) — 序列化值为排序字符串（兼容 Kong）
/// Public for compatibility testing — 公开用于兼容性测试
pub fn to_sorted_string(value: &Value, buf: &mut String) {
    match value {
        Value::Null => buf.push_str("/null/"),
        Value::String(s) => buf.push_str(s),
        Value::Number(n) => buf.push_str(&n.to_string()),
        Value::Bool(b) => buf.push_str(if *b { "true" } else { "false" }),
        Value::Array(arr) => {
            if arr.is_empty() {
                buf.push_str("{}");
            } else {
                for item in arr {
                    to_sorted_string(item, buf);
                    buf.push(';');
                }
            }
        }
        Value::Object(map) => {
            if map.is_empty() {
                buf.push_str("{}");
            } else {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for key in keys {
                    buf.push_str(key);
                    buf.push(':');
                    to_sorted_string(&map[key], buf);
                    buf.push(';');
                }
            }
        }
    }
}

/// Compute MD5 hex digest — 计算 MD5 十六进制摘要
pub fn md5_hex(data: &[u8]) -> String {
    format!("{:032x}", md5::compute(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_to_sorted_string_null() {
        let mut buf = String::new();
        to_sorted_string(&Value::Null, &mut buf);
        assert_eq!(buf, "/null/");
    }

    #[test]
    fn test_to_sorted_string_string() {
        let mut buf = String::new();
        to_sorted_string(&json!("hello"), &mut buf);
        assert_eq!(buf, "hello");
    }

    #[test]
    fn test_to_sorted_string_number() {
        let mut buf = String::new();
        to_sorted_string(&json!(42), &mut buf);
        assert_eq!(buf, "42");
    }

    #[test]
    fn test_to_sorted_string_bool() {
        let mut buf = String::new();
        to_sorted_string(&json!(true), &mut buf);
        assert_eq!(buf, "true");
    }

    #[test]
    fn test_to_sorted_string_empty_array() {
        let mut buf = String::new();
        to_sorted_string(&json!([]), &mut buf);
        assert_eq!(buf, "{}");
    }

    #[test]
    fn test_to_sorted_string_array() {
        let mut buf = String::new();
        to_sorted_string(&json!(["a", "b", "c"]), &mut buf);
        assert_eq!(buf, "a;b;c;");
    }

    #[test]
    fn test_to_sorted_string_object_sorted() {
        let mut buf = String::new();
        to_sorted_string(&json!({"z": 1, "a": 2}), &mut buf);
        assert_eq!(buf, "a:2;z:1;");
    }

    #[test]
    fn test_to_sorted_string_empty_object() {
        let mut buf = String::new();
        to_sorted_string(&json!({}), &mut buf);
        assert_eq!(buf, "{}");
    }

    #[test]
    fn test_md5_hex() {
        // MD5("") = d41d8cd98f00b204e9800998ecf8427e
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn test_hash_null_config() {
        let hashes = calculate_config_hash(&Value::Null);
        let null_hash = md5_hex(b"/null/");
        assert_eq!(hashes.routes, null_hash);
        assert_eq!(hashes.services, null_hash);
        assert!(hashes.config.len() == 32);
    }

    #[test]
    fn test_hash_empty_config() {
        let hashes = calculate_config_hash(&json!({}));
        assert!(hashes.config.len() == 32);
    }

    #[test]
    fn test_hash_deterministic() {
        let config = json!({
            "routes": [{"id": "r1", "paths": ["/foo"]}],
            "services": [{"id": "s1", "name": "svc"}],
            "plugins": [],
        });
        let h1 = calculate_config_hash(&config);
        let h2 = calculate_config_hash(&config);
        assert_eq!(h1, h2, "Hash should be deterministic");
    }

    #[test]
    fn test_hash_changes_with_config() {
        let config1 = json!({"routes": [{"id": "r1"}]});
        let config2 = json!({"routes": [{"id": "r2"}]});
        let h1 = calculate_config_hash(&config1);
        let h2 = calculate_config_hash(&config2);
        assert_ne!(h1.config, h2.config, "Different configs should produce different hashes");
        assert_ne!(h1.routes, h2.routes);
    }

    #[tokio::test]
    async fn test_control_plane_register_dp() {
        let cp = ControlPlane::new();
        let dp_info = DataPlaneInfo {
            id: Uuid::new_v4(),
            ip: "127.0.0.1".to_string(),
            hostname: "dp-1".to_string(),
            version: "3.9.0".to_string(),
            sync_status: SyncStatus::Unknown,
            config_hash: EMPTY_CONFIG_HASH.to_string(),
            last_seen: Utc::now(),
            labels: HashMap::new(),
        };

        let _rx = cp.register_dp(dp_info.clone()).await;
        let dps = cp.list_data_planes().await;
        assert_eq!(dps.len(), 1);
        assert_eq!(dps[0].id, dp_info.id);
    }

    #[tokio::test]
    async fn test_control_plane_push_config() {
        let cp = ControlPlane::new();
        let dp_info = DataPlaneInfo {
            id: Uuid::new_v4(),
            ip: "127.0.0.1".to_string(),
            hostname: "dp-1".to_string(),
            version: "3.9.0".to_string(),
            sync_status: SyncStatus::Unknown,
            config_hash: EMPTY_CONFIG_HASH.to_string(),
            last_seen: Utc::now(),
            labels: HashMap::new(),
        };

        let mut rx = cp.register_dp(dp_info).await;
        let config = json!({"services": [{"id": "s1"}], "routes": []});
        cp.push_config(&config).await.unwrap();

        // Should receive the broadcast — 应该收到广播
        let payload = rx.recv().await.unwrap();
        assert!(!payload.is_empty());

        // Hash should be updated — 哈希应该已更新
        let hash = cp.current_hash().await;
        assert_ne!(hash, EMPTY_CONFIG_HASH);
    }
}

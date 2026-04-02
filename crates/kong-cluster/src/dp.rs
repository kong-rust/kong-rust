//! Data Plane — WebSocket client, config apply, heartbeat, reconnect
//! 数据面 — WebSocket 客户端、配置应用、心跳、重连

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;
use crate::{ClusterError, EMPTY_CONFIG_HASH, ConfigHashes};
use crate::protocol;
use crate::cache::DiskCache;

/// Heartbeat interval (seconds) — 心跳间隔（秒）
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Connection timeout (seconds) — 连接超时（秒）
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Min reconnect delay (seconds) — 最小重连延迟（秒）
const RECONNECT_DELAY_MIN: u64 = 5;

/// Max reconnect delay (seconds) — 最大重连延迟（秒）
const RECONNECT_DELAY_MAX: u64 = 10;

/// DP state — DP 状态
pub struct DataPlane {
    /// CP address (host:port) — CP 地址
    cp_addr: String,
    /// Whether to use TLS (wss://) — 是否使用 TLS (wss://)
    use_tls: bool,
    /// Current config hash — 当前配置哈希
    pub current_hash: Arc<RwLock<String>>,
    /// Current config hashes (multi-level) — 当前多级哈希
    pub current_hashes: Arc<RwLock<ConfigHashes>>,
    /// Disk cache — 磁盘缓存
    disk_cache: DiskCache,
    /// Loaded plugins list — 已加载插件列表
    plugins: Vec<String>,
    /// Node ID — 节点 ID
    node_id: uuid::Uuid,
    /// Hostname — 主机名
    hostname: String,
    /// Whether connected to CP — 是否已连接 CP
    pub connected: Arc<RwLock<bool>>,
    /// Config ready (has received at least one config) — 配置就绪（已收到至少一个配置）
    pub config_ready: Arc<RwLock<bool>>,
}

/// Config update callback — 配置更新回调
pub type ConfigCallback = Arc<dyn Fn(serde_json::Value, String, ConfigHashes) -> Result<(), String> + Send + Sync>;

impl DataPlane {
    pub fn new(
        cp_addr: &str,
        prefix: &str,
        plugins: Vec<String>,
        node_id: uuid::Uuid,
        hostname: String,
    ) -> Self {
        Self::with_tls(cp_addr, prefix, plugins, node_id, hostname, false)
    }

    /// Create with explicit TLS setting — 创建并指定是否启用 TLS
    pub fn with_tls(
        cp_addr: &str,
        prefix: &str,
        plugins: Vec<String>,
        node_id: uuid::Uuid,
        hostname: String,
        use_tls: bool,
    ) -> Self {
        Self {
            cp_addr: cp_addr.to_string(),
            use_tls,
            current_hash: Arc::new(RwLock::new(EMPTY_CONFIG_HASH.to_string())),
            current_hashes: Arc::new(RwLock::new(ConfigHashes::default())),
            disk_cache: DiskCache::new(prefix),
            plugins,
            node_id,
            hostname,
            connected: Arc::new(RwLock::new(false)),
            config_ready: Arc::new(RwLock::new(false)),
        }
    }

    /// Try to load from disk cache — 尝试从磁盘缓存加载
    pub async fn try_load_from_cache(&self) -> Option<(serde_json::Value, String)> {
        self.disk_cache.load()
    }

    /// Get CP address — 获取 CP 地址
    pub fn cp_addr(&self) -> &str {
        &self.cp_addr
    }

    /// Get current config hash — 获取当前配置哈希
    pub async fn get_current_hash(&self) -> String {
        self.current_hash.read().await.clone()
    }

    /// Check if DP is connected to CP — 检查 DP 是否已连接 CP
    pub async fn is_connected(&self) -> bool {
        *self.connected.read().await
    }

    /// Check if config is ready — 检查配置是否就绪
    pub async fn is_config_ready(&self) -> bool {
        *self.config_ready.read().await
    }

    /// Mark config as applied — 标记配置已应用
    pub async fn mark_config_applied(&self, config: &serde_json::Value, hash: &str, hashes: ConfigHashes) {
        *self.current_hash.write().await = hash.to_string();
        *self.current_hashes.write().await = hashes;
        *self.config_ready.write().await = true;

        // Save to disk cache — 保存到磁盘缓存
        if let Err(e) = self.disk_cache.save(config, hash) {
            tracing::warn!("磁盘缓存保存失败: {}", e);
        }
    }

    /// Whether TLS is enabled — 是否启用了 TLS
    pub fn use_tls(&self) -> bool {
        self.use_tls
    }

    /// Build WebSocket URL for V1 — 构建 V1 WebSocket URL
    pub fn ws_url_v1(&self) -> String {
        let scheme = if self.use_tls { "wss" } else { "ws" };
        format!(
            "{}://{}/v1/outlet?node_id={}&node_hostname={}&node_version={}",
            scheme, self.cp_addr, self.node_id, self.hostname, env!("CARGO_PKG_VERSION")
        )
    }

    /// Build basic_info message — 构建 basic_info 消息
    pub fn basic_info_message(&self) -> Result<Message, ClusterError> {
        let info = protocol::BasicInfo::new(self.plugins.clone());
        let json = serde_json::to_vec(&info)?;
        Ok(Message::Binary(json.into()))
    }

    /// Build PING message with current config hash — 构建带配置哈希的 PING 消息
    pub async fn ping_message(&self) -> Message {
        let hash = self.current_hash.read().await;
        Message::Ping(hash.as_bytes().to_vec().into())
    }

    /// Get reconnect delay with jitter — 获取带抖动的重连延迟
    pub fn reconnect_delay() -> Duration {
        use rand::Rng;
        let delay = rand::thread_rng().gen_range(RECONNECT_DELAY_MIN..=RECONNECT_DELAY_MAX);
        Duration::from_secs(delay)
    }

    /// Get ping interval — 获取心跳间隔
    pub fn ping_interval() -> Duration {
        PING_INTERVAL
    }

    /// Get connect timeout — 获取连接超时
    pub fn connect_timeout() -> Duration {
        CONNECT_TIMEOUT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_url_v1() {
        let dp = DataPlane::new(
            "127.0.0.1:8005",
            "/tmp/kong-test",
            vec!["key-auth".to_string()],
            uuid::Uuid::nil(),
            "test-dp".to_string(),
        );
        let url = dp.ws_url_v1();
        assert!(url.starts_with("ws://127.0.0.1:8005/v1/outlet"));
        assert!(url.contains("node_id="));
        assert!(url.contains("node_hostname=test-dp"));

        // TLS mode should use wss:// — TLS 模式应使用 wss://
        let dp_tls = DataPlane::with_tls(
            "127.0.0.1:8005",
            "/tmp/kong-test",
            vec!["key-auth".to_string()],
            uuid::Uuid::nil(),
            "test-dp".to_string(),
            true,
        );
        let url_tls = dp_tls.ws_url_v1();
        assert!(url_tls.starts_with("wss://127.0.0.1:8005/v1/outlet"));
    }

    #[test]
    fn test_reconnect_delay_range() {
        for _ in 0..100 {
            let delay = DataPlane::reconnect_delay();
            assert!(delay >= Duration::from_secs(5));
            assert!(delay <= Duration::from_secs(10));
        }
    }

    #[tokio::test]
    async fn test_dp_initial_state() {
        let dp = DataPlane::new(
            "127.0.0.1:8005",
            "/tmp/kong-test",
            vec![],
            uuid::Uuid::new_v4(),
            "test".to_string(),
        );
        assert_eq!(dp.get_current_hash().await, EMPTY_CONFIG_HASH);
        assert!(!dp.is_connected().await);
        assert!(!dp.is_config_ready().await);
    }

    #[tokio::test]
    async fn test_dp_mark_config_applied() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dp = DataPlane::new(
            "127.0.0.1:8005",
            tmp.path().to_str().unwrap(),
            vec![],
            uuid::Uuid::new_v4(),
            "test".to_string(),
        );

        let config = serde_json::json!({"services": []});
        let hash = "abcdef1234567890abcdef1234567890";
        let hashes = ConfigHashes { config: hash.to_string(), ..Default::default() };

        dp.mark_config_applied(&config, hash, hashes).await;
        assert_eq!(dp.get_current_hash().await, hash);
        assert!(dp.is_config_ready().await);

        // Should be cached to disk — 应该已缓存到磁盘
        let cached = dp.try_load_from_cache().await;
        assert!(cached.is_some());
    }
}

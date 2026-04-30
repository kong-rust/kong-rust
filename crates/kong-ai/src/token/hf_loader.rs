//! HuggingFace tokenizer.json loader — 异步下载 + 磁盘缓存 + 单飞 + 首次降级不阻塞
//! Async download + disk cache + single-flight + non-blocking first-touch fallback
//!
//! 设计要点 / Design highlights:
//! 1. **Try-fast 路径**:已在内存中加载 → 同步返回 Loaded;否则返回 None,本次降级
//!    Hot path returns synchronously when tokenizer is in memory; otherwise None and falls back
//! 2. **磁盘缓存优先**:首次访问时同步加载磁盘缓存(若存在),无需触发网络
//!    Disk cache is consulted before network
//! 3. **后台下载**:磁盘也无 → spawn 一次下载任务,返回 Pending;后续请求看到 Pending 仍降级,
//!    直到下载完成后第一次 try_get 命中并切换到 Loaded
//!    Network downloads happen in spawned tasks; concurrent calls share the in-flight task
//! 4. **单飞合并**:同一 repo 多并发请求只触发一次下载(DashMap entry API)
//!    Single-flight via DashMap entry API
//! 5. **离线模式**:offline=true 时不发起网络下载,只读磁盘缓存
//!    Offline mode skips network, reads disk only
//! 6. **可注入下载器**:`HfDownloader` trait 让单测用 Mock 替换网络
//!    Pluggable HfDownloader trait lets tests substitute the network

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use kong_core::error::{KongError, Result};

/// 单个 repo 的加载状态机 — per-repo state machine
#[derive(Default)]
enum LoadState {
    /// 尚未尝试加载 — never touched
    #[default]
    Empty,
    /// 后台下载中(单飞)— download in flight (single-flight)
    Pending,
    /// 已加载,可同步使用 — ready for synchronous tokenization
    Loaded(Arc<tokenizers::Tokenizer>),
    /// 永久失败(下载或解析错误);字符串保留供日志和调试 — payload kept for logging
    Failed(#[allow(dead_code)] String),
}

/// 单个 repo 的 cell;状态用 RwLock 保护以支持并发查询
struct TokenizerCell {
    state: RwLock<LoadState>,
    /// 已 spawn 过下载任务的标志,避免重复 spawn
    /// Flag indicating a download task has been spawned (single-flight guard)
    spawned: AtomicBool,
}

impl TokenizerCell {
    fn new() -> Self {
        Self {
            state: RwLock::new(LoadState::Empty),
            spawned: AtomicBool::new(false),
        }
    }
}

/// 下载器抽象 — 把 tokenizer.json 写到目标路径
/// Downloader abstraction — write tokenizer.json bytes to the target path atomically
#[async_trait]
pub trait HfDownloader: Send + Sync {
    /// 下载指定 repo 的 tokenizer.json 内容到目标路径
    /// 实现者应做原子写入(写入临时文件再 rename)以避免半文件
    async fn download_tokenizer(&self, repo_id: &str, target: &Path) -> Result<()>;
}

/// 默认下载器:HTTP GET HuggingFace 公共 URL,reqwest 实现
/// Default HTTP downloader — GET https://huggingface.co/<repo>/resolve/main/tokenizer.json
pub struct HttpHfDownloader {
    client: reqwest::Client,
    /// HF 主机 base URL,默认 https://huggingface.co;测试可重定向到 mock server
    base_url: String,
}

impl HttpHfDownloader {
    pub fn new(timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            base_url: "https://huggingface.co".to_string(),
        }
    }

    /// 自定义 base URL — 单测用
    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }
}

#[async_trait]
impl HfDownloader for HttpHfDownloader {
    async fn download_tokenizer(&self, repo_id: &str, target: &Path) -> Result<()> {
        let url = format!(
            "{}/{}/resolve/main/tokenizer.json",
            self.base_url.trim_end_matches('/'),
            repo_id
        );
        debug!("hf-loader: downloading {}", url);

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| KongError::InternalError(format!("hf download request failed: {}", e)))?;

        if !resp.status().is_success() {
            return Err(KongError::InternalError(format!(
                "hf download HTTP {} for {}",
                resp.status(),
                repo_id
            )));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| KongError::InternalError(format!("hf download body read: {}", e)))?;

        write_atomic(target, &bytes).await
    }
}

/// 原子写入 — 写入临时文件后 rename,避免下载中断留下半文件
async fn write_atomic(target: &Path, bytes: &[u8]) -> Result<()> {
    let parent = target.parent().ok_or_else(|| {
        KongError::InternalError(format!(
            "hf cache target has no parent: {}",
            target.display()
        ))
    })?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| KongError::InternalError(format!("hf cache mkdir: {}", e)))?;

    let tmp = target.with_extension("tmp");
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|e| KongError::InternalError(format!("hf cache write: {}", e)))?;
    tokio::fs::rename(&tmp, target)
        .await
        .map_err(|e| KongError::InternalError(format!("hf cache rename: {}", e)))?;
    Ok(())
}

/// HuggingFace tokenizer 加载器
pub struct HfLoader {
    /// 磁盘缓存目录 — 每个 repo 一个子目录:`<cache_dir>/<repo_id>/tokenizer.json`
    cache_dir: PathBuf,
    /// 离线模式:只读磁盘,不触发下载
    offline: bool,
    /// repo_id → TokenizerCell
    cells: DashMap<String, Arc<TokenizerCell>>,
    /// 下载器(可注入)
    downloader: Arc<dyn HfDownloader>,
}

impl HfLoader {
    /// 默认构造:HTTP 下载器 + 5s 超时(单次下载超时,与 per-request deadline 区分)
    pub fn new(cache_dir: PathBuf, offline: bool) -> Self {
        Self::with_downloader(
            cache_dir,
            offline,
            Arc::new(HttpHfDownloader::new(Duration::from_secs(5))),
        )
    }

    /// 注入式构造 — 测试或自定义下载源用
    pub fn with_downloader(
        cache_dir: PathBuf,
        offline: bool,
        downloader: Arc<dyn HfDownloader>,
    ) -> Self {
        Self {
            cache_dir,
            offline,
            cells: DashMap::new(),
            downloader,
        }
    }

    /// 同步快速路径:返回已加载的 tokenizer,或返回 None 触发本次降级
    /// 副作用:首次访问时尝试磁盘缓存;若磁盘也无,在 spawn 单飞下载任务后返回 None
    /// Hot path: returns the loaded tokenizer or None (which triggers caller fallback).
    /// Side effects: on first touch, synchronously tries the disk cache, and if absent,
    /// spawns a single-flight download task before returning None.
    pub fn try_get(self: &Arc<Self>, repo_id: &str) -> Option<Arc<tokenizers::Tokenizer>> {
        let cell = self.cell(repo_id);

        // 1. 检查内存状态(读锁;写锁占用极短 → 此次降级,下次重试)
        match cell.state.try_read() {
            Ok(guard) => match &*guard {
                LoadState::Loaded(tok) => return Some(tok.clone()),
                LoadState::Pending => return None, // 后台下载中,本次降级
                LoadState::Failed(_) => return None, // 永久失败
                LoadState::Empty => {}             // 继续走磁盘 / 下载流程
            },
            Err(_) => return None, // 写锁占用 = 状态切换中 → 本次降级
        }

        // 2. 尝试同步加载磁盘缓存
        if let Some(tok) = self.try_load_from_disk(repo_id) {
            if let Ok(mut guard) = cell.state.try_write() {
                if !matches!(*guard, LoadState::Loaded(_)) {
                    *guard = LoadState::Loaded(tok.clone());
                }
            }
            return Some(tok);
        }

        // 3. 离线模式:磁盘也无,直接降级
        if self.offline {
            return None;
        }

        // 4. 触发后台下载(单飞:CAS 抢标志)
        if cell
            .spawned
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.spawn_download(repo_id.to_string(), cell.clone());
        }

        None
    }

    /// async 版本 — 用于不在乎延迟的场景(目前只供测试 wait-for-load)
    /// Async wait variant — useful in tests; production hot path uses try_get
    pub async fn get_or_wait(
        self: &Arc<Self>,
        repo_id: &str,
        wait: Duration,
    ) -> Option<Arc<tokenizers::Tokenizer>> {
        // 第一次先 try_get(可能直接命中或触发 spawn)
        if let Some(t) = self.try_get(repo_id) {
            return Some(t);
        }
        let cell = self.cell(repo_id);
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let LoadState::Loaded(t) = &*cell.state.read().await {
                return Some(t.clone());
            }
            if let LoadState::Failed(_) = &*cell.state.read().await {
                return None;
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn cell(&self, repo_id: &str) -> Arc<TokenizerCell> {
        self.cells
            .entry(repo_id.to_string())
            .or_insert_with(|| Arc::new(TokenizerCell::new()))
            .clone()
    }

    fn cache_path(&self, repo_id: &str) -> PathBuf {
        // 用 / 替换为 __ 防止 repo_id 含目录分隔符破坏 cache 布局
        let safe = repo_id.replace('/', "__");
        self.cache_dir.join(safe).join("tokenizer.json")
    }

    fn try_load_from_disk(&self, repo_id: &str) -> Option<Arc<tokenizers::Tokenizer>> {
        let path = self.cache_path(repo_id);
        if !path.exists() {
            return None;
        }
        match tokenizers::Tokenizer::from_file(&path) {
            Ok(t) => Some(Arc::new(t)),
            Err(e) => {
                warn!(
                    "hf-loader: failed to load cached tokenizer.json for {}: {}",
                    repo_id, e
                );
                None
            }
        }
    }

    fn spawn_download(self: &Arc<Self>, repo_id: String, cell: Arc<TokenizerCell>) {
        let loader = self.clone();
        tokio::spawn(async move {
            // 抢写锁标记 Pending(便于后续观察者读到 Pending 而非 Empty)
            {
                let mut guard = cell.state.write().await;
                if matches!(*guard, LoadState::Empty) {
                    *guard = LoadState::Pending;
                }
            }

            let target = loader.cache_path(&repo_id);
            match loader.downloader.download_tokenizer(&repo_id, &target).await {
                Ok(_) => match tokenizers::Tokenizer::from_file(&target) {
                    Ok(t) => {
                        let mut guard = cell.state.write().await;
                        *guard = LoadState::Loaded(Arc::new(t));
                        debug!("hf-loader: loaded {}", repo_id);
                    }
                    Err(e) => {
                        let mut guard = cell.state.write().await;
                        *guard = LoadState::Failed(format!("parse error: {}", e));
                        warn!("hf-loader: parse failure for {}: {}", repo_id, e);
                    }
                },
                Err(e) => {
                    let mut guard = cell.state.write().await;
                    *guard = LoadState::Failed(format!("download error: {}", e));
                    warn!("hf-loader: download failure for {}: {}", repo_id, e);
                }
            }
        });
    }
}

impl Default for HfLoader {
    fn default() -> Self {
        let cache = default_cache_dir();
        Self::new(cache, false)
    }
}

/// 默认 cache 目录:`$KONG_TOKENIZER_CACHE_DIR` > `$XDG_CACHE_HOME/kong-rust/tokenizers`
/// > `$HOME/.cache/kong-rust/tokenizers` > `/tmp/kong-rust/tokenizers`
pub fn default_cache_dir() -> PathBuf {
    if let Ok(p) = std::env::var("KONG_TOKENIZER_CACHE_DIR") {
        return PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(p).join("kong-rust").join("tokenizers");
    }
    if let Ok(p) = std::env::var("HOME") {
        return PathBuf::from(p)
            .join(".cache")
            .join("kong-rust")
            .join("tokenizers");
    }
    PathBuf::from("/tmp/kong-rust/tokenizers")
}

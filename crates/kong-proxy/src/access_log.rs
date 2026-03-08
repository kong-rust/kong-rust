//! Async access log writer — 异步 Access Log 写入器
//!
//! Hot path only does channel try_send (lock-free, nanosecond-level), — 热路径仅做 channel try_send（无锁、纳秒级），
//! background task batch-receives and flushes once, reducing disk IO frequency. — 后台任务批量 recv + 一次性 flush，减少磁盘 IO 频率。
//! Drops log lines when channel is full (acceptable for API gateways, should not backpressure request processing). — channel 满时丢弃日志行（API 网关场景日志丢失可接受，不应反压请求处理）。

use std::io::Write;

use tokio::sync::mpsc;

/// Channel buffer size — channel 缓冲区大小
const CHANNEL_BUFFER_SIZE: usize = 8192;

/// Async access log writer — 异步 Access Log 写入器
///
/// Clone copies the Sender; shared by HTTP and Stream proxies. — Clone 即复制 Sender，HTTP 和 Stream 代理共用。
/// Background task exits naturally when all Senders are dropped. — 后台任务在所有 Sender drop 后自然退出。
#[derive(Clone)]
pub struct AccessLogWriter {
    tx: mpsc::Sender<String>,
}

impl AccessLogWriter {
    /// Create writer and start background flush task — 创建写入器并启动后台 flush 任务
    ///
    /// - Must be called within a tokio runtime (requires spawn) — 必须在 tokio runtime 内调用（需要 spawn）
    /// - Returns None on file open failure — 返回 None 表示文件打开失败
    pub fn new(path: &str) -> Option<Self> {
        if path == "off" {
            return None;
        }

        let log_path = std::path::Path::new(path);
        if let Some(dir) = log_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }

        let file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Access log 文件打开失败: {} ({})", path, e);
                return None;
            }
        };

        let (tx, rx) = mpsc::channel(CHANNEL_BUFFER_SIZE);
        let writer = std::io::BufWriter::new(file);

        tokio::spawn(flush_task(rx, writer));
        tracing::info!("Access log 异步写入器已启动: {}", path);

        Some(Self { tx })
    }

    /// Write a log line (non-blocking) — 写入一行日志（非阻塞）
    ///
    /// Drops when channel is full, does not block caller. — channel 满时丢弃，不阻塞调用者。
    pub fn write(&self, line: String) {
        if let Err(_) = self.tx.try_send(line) {
            // Channel full or closed, drop log line — channel 满或已关闭，丢弃日志
            tracing::trace!("Access log channel 已满，丢弃日志行");
        }
    }
}

/// Background flush task: batch-receive log lines and write to file — 后台 flush 任务：批量接收日志行并写入文件
async fn flush_task(mut rx: mpsc::Receiver<String>, mut writer: std::io::BufWriter<std::fs::File>) {
    // Pre-allocate batch buffer — 预分配批量缓冲区
    let mut batch = Vec::with_capacity(64);

    loop {
        // Wait for first message (blocking, returns None when channel closes) — 等待第一条消息（阻塞式，channel 关闭时返回 None）
        let first = rx.recv().await;
        match first {
            Some(line) => batch.push(line),
            None => break, // All senders dropped, exit — 所有 sender 已 drop，退出
        }

        // Drain as many as possible (non-blocking), batch write — 尽量多取（非阻塞），批量写入
        while batch.len() < 256 {
            match rx.try_recv() {
                Ok(line) => batch.push(line),
                Err(_) => break,
            }
        }

        // Batch write to file — 批量写入文件
        for line in batch.drain(..) {
            let _ = writer.write_all(line.as_bytes());
        }
        let _ = writer.flush();
    }

    // Flush remaining data before exit — 退出前 flush 残余数据
    let _ = writer.flush();
    tracing::info!("Access log flush 任务退出");
}

//! 异步 Access Log 写入器
//!
//! 热路径仅做 channel try_send（无锁、纳秒级），
//! 后台任务批量 recv + 一次性 flush，减少磁盘 IO 频率。
//! channel 满时丢弃日志行（API 网关场景日志丢失可接受，不应反压请求处理）。

use std::io::Write;

use tokio::sync::mpsc;

/// channel 缓冲区大小
const CHANNEL_BUFFER_SIZE: usize = 8192;

/// 异步 Access Log 写入器
///
/// Clone 即复制 Sender，HTTP 和 Stream 代理共用。
/// 后台任务在所有 Sender drop 后自然退出。
#[derive(Clone)]
pub struct AccessLogWriter {
    tx: mpsc::Sender<String>,
}

impl AccessLogWriter {
    /// 创建写入器并启动后台 flush 任务
    ///
    /// - 必须在 tokio runtime 内调用（需要 spawn）
    /// - 返回 None 表示文件打开失败
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

    /// 写入一行日志（非阻塞）
    ///
    /// channel 满时丢弃，不阻塞调用者。
    pub fn write(&self, line: String) {
        if let Err(_) = self.tx.try_send(line) {
            // channel 满或已关闭，丢弃日志
            tracing::trace!("Access log channel 已满，丢弃日志行");
        }
    }
}

/// 后台 flush 任务：批量接收日志行并写入文件
async fn flush_task(mut rx: mpsc::Receiver<String>, mut writer: std::io::BufWriter<std::fs::File>) {
    // 预分配批量缓冲区
    let mut batch = Vec::with_capacity(64);

    loop {
        // 等待第一条消息（阻塞式，channel 关闭时返回 None）
        let first = rx.recv().await;
        match first {
            Some(line) => batch.push(line),
            None => break, // 所有 sender 已 drop，退出
        }

        // 尽量多取（非阻塞），批量写入
        while batch.len() < 256 {
            match rx.try_recv() {
                Ok(line) => batch.push(line),
                Err(_) => break,
            }
        }

        // 批量写入文件
        for line in batch.drain(..) {
            let _ = writer.write_all(line.as_bytes());
        }
        let _ = writer.flush();
    }

    // 退出前 flush 残余数据
    let _ = writer.flush();
    tracing::info!("Access log flush 任务退出");
}

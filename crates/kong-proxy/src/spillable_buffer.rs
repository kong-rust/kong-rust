//! Spillable buffer — in-memory buffer that spills to disk when threshold exceeded — 可溢出缓冲区 — 超过阈值时自动溢出到磁盘
//!
//! Protects against OOM when buffering large request/response bodies — 防止大 body 缓冲时 OOM

use std::io::{Read, Write};

/// Default memory threshold (10MB) — 默认内存阈值（10MB）
const DEFAULT_THRESHOLD: usize = 10 * 1024 * 1024;

/// Buffer state — in-memory or spilled to temp file — 缓冲区状态 — 内存或磁盘临时文件
enum BufferState {
    /// In-memory buffer — 内存缓冲
    Memory(Vec<u8>),
    /// Spilled to temp file — 已溢出到临时文件
    File {
        file: tempfile::NamedTempFile,
        len: usize,
    },
}

/// A buffer that stores data in memory up to a threshold, then spills to a temp file — 缓冲区：数据量低于阈值时存内存，超过后溢出到临时文件
pub struct SpillableBuffer {
    state: BufferState,
    threshold: usize,
}

impl SpillableBuffer {
    /// Create a new spillable buffer with default threshold (10MB) — 创建默认阈值（10MB）的可溢出缓冲区
    pub fn new() -> Self {
        Self {
            state: BufferState::Memory(Vec::new()),
            threshold: DEFAULT_THRESHOLD,
        }
    }

    /// Create with custom threshold — 创建自定义阈值的缓冲区
    #[allow(dead_code)]
    pub fn with_threshold(threshold: usize) -> Self {
        Self {
            state: BufferState::Memory(Vec::new()),
            threshold,
        }
    }

    /// Append data to the buffer, spilling to disk if threshold exceeded — 追加数据，超过阈值时溢出到磁盘
    pub fn extend(&mut self, data: &[u8]) {
        match &mut self.state {
            BufferState::Memory(buf) => {
                if buf.len() + data.len() > self.threshold {
                    // Spill to temp file — 溢出到临时文件
                    match tempfile::NamedTempFile::new() {
                        Ok(mut file) => {
                            let total_len = buf.len() + data.len();
                            if let Err(e) = file.write_all(buf) {
                                tracing::error!("写入临时文件失败: {}", e);
                                // Fallback: keep in memory — 回退：保持内存
                                buf.extend_from_slice(data);
                                return;
                            }
                            if let Err(e) = file.write_all(data) {
                                tracing::error!("写入临时文件失败: {}", e);
                                buf.extend_from_slice(data);
                                return;
                            }
                            tracing::debug!(
                                "Body 缓冲溢出到磁盘: {} bytes -> {}",
                                total_len,
                                file.path().display()
                            );
                            self.state = BufferState::File {
                                file,
                                len: total_len,
                            };
                        }
                        Err(e) => {
                            tracing::error!("创建临时文件失败: {}, 保持内存缓冲", e);
                            buf.extend_from_slice(data);
                        }
                    }
                } else {
                    buf.extend_from_slice(data);
                }
            }
            BufferState::File { file, len } => {
                if let Err(e) = file.write_all(data) {
                    tracing::error!("写入临时文件失败: {}", e);
                }
                *len += data.len();
            }
        }
    }

    /// Consume the buffer and return all data as Vec<u8> — 消费缓冲区，返回完整数据
    pub fn finish(self) -> Vec<u8> {
        match self.state {
            BufferState::Memory(buf) => buf,
            BufferState::File { mut file, len } => {
                use std::io::Seek;
                let mut buf = Vec::with_capacity(len);
                if let Err(e) = file.seek(std::io::SeekFrom::Start(0)) {
                    tracing::error!("临时文件 seek 失败: {}", e);
                    return buf;
                }
                if let Err(e) = file.read_to_end(&mut buf) {
                    tracing::error!("读取临时文件失败: {}", e);
                }
                buf
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_buffer() {
        let mut buf = SpillableBuffer::with_threshold(1024);
        buf.extend(b"hello ");
        buf.extend(b"world");
        let data = buf.finish();
        assert_eq!(data, b"hello world");
    }

    #[test]
    fn test_spill_to_disk() {
        let mut buf = SpillableBuffer::with_threshold(10);
        buf.extend(b"12345");
        buf.extend(b"67890AB"); // 12 bytes > 10 threshold
        let data = buf.finish();
        assert_eq!(data, b"1234567890AB");
    }

    #[test]
    fn test_spill_then_extend() {
        let mut buf = SpillableBuffer::with_threshold(5);
        buf.extend(b"123456"); // 6 > 5, spills immediately
        buf.extend(b"789"); // appends to file
        let data = buf.finish();
        assert_eq!(data, b"123456789");
    }
}

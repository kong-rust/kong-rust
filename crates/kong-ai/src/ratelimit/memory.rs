//! 内存固定窗口限流器 — in-memory fixed-window rate limiter

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 内存固定窗口限流器
pub struct MemoryRateLimiter {
    windows: DashMap<String, WindowEntry>,
    window_duration: Duration,
}

struct WindowEntry {
    start: Instant,
    count: AtomicU64,
}

impl MemoryRateLimiter {
    /// 创建新的内存限流器，window_duration 为窗口时长
    pub fn new(window_duration: Duration) -> Self {
        Self {
            windows: DashMap::new(),
            window_duration,
        }
    }
}

impl super::RateLimiter for MemoryRateLimiter {
    fn check(&self, key: &str, limit: u64) -> (bool, u64) {
        let now = Instant::now();

        // 尝试获取已有窗口
        if let Some(entry) = self.windows.get(key) {
            // 窗口过期则重置
            if now.duration_since(entry.start) >= self.window_duration {
                drop(entry);
                self.windows.insert(
                    key.to_string(),
                    WindowEntry {
                        start: now,
                        count: AtomicU64::new(0),
                    },
                );
                return (true, 0);
            }
            let current = entry.count.load(Ordering::Relaxed);
            return (current < limit, current);
        }

        // 不存在则创建新窗口
        self.windows.insert(
            key.to_string(),
            WindowEntry {
                start: now,
                count: AtomicU64::new(0),
            },
        );
        (true, 0)
    }

    fn increment(&self, key: &str, amount: u64) {
        let now = Instant::now();

        if let Some(entry) = self.windows.get(key) {
            // 窗口过期则重置并设为 amount
            if now.duration_since(entry.start) >= self.window_duration {
                drop(entry);
                self.windows.insert(
                    key.to_string(),
                    WindowEntry {
                        start: now,
                        count: AtomicU64::new(amount),
                    },
                );
                return;
            }
            entry.count.fetch_add(amount, Ordering::Relaxed);
            return;
        }

        // 不存在则创建并设为 amount
        self.windows.insert(
            key.to_string(),
            WindowEntry {
                start: now,
                count: AtomicU64::new(amount),
            },
        );
    }
}

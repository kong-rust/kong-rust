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

    /// 获取或创建窗口，过期则原子重置。返回 entry 的引用守卫。
    /// 使用 entry() API 保证读-检测-修改在同一个锁守卫内完成，避免竞态。
    fn get_or_reset(&self, key: &str, now: Instant) -> dashmap::mapref::one::Ref<'_, String, WindowEntry> {
        // 先尝试快速路径：key 已存在且未过期
        if let Some(entry) = self.windows.get(key) {
            if now.duration_since(entry.start) < self.window_duration {
                return entry;
            }
        }
        // 慢路径：使用 entry() API 原子创建或重置
        self.windows.entry(key.to_string())
            .and_modify(|e| {
                if now.duration_since(e.start) >= self.window_duration {
                    e.start = now;
                    e.count.store(0, Ordering::Relaxed);
                }
            })
            .or_insert_with(|| WindowEntry {
                start: now,
                count: AtomicU64::new(0),
            });
        // entry() 返回的是 OccupiedEntry/VacantEntry，我们需要 Ref，所以再 get 一次
        // 此时 key 一定存在，且窗口已正确初始化
        self.windows.get(key).unwrap()
    }
}

impl super::RateLimiter for MemoryRateLimiter {
    fn check(&self, key: &str, limit: u64) -> (bool, u64) {
        let now = Instant::now();
        let entry = self.get_or_reset(key, now);
        let current = entry.count.load(Ordering::Relaxed);
        (current < limit, current)
    }

    fn check_and_increment(&self, key: &str, limit: u64, amount: u64) -> (bool, u64) {
        let now = Instant::now();
        let entry = self.get_or_reset(key, now);
        // 使用 CAS 循环实现原子的 check-and-increment
        loop {
            let current = entry.count.load(Ordering::Relaxed);
            if current.saturating_add(amount) > limit {
                return (false, current);
            }
            // 尝试原子递增
            match entry.count.compare_exchange_weak(
                current,
                current + amount,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return (true, current + amount),
                Err(_) => continue, // 被其他线程修改，重试
            }
        }
    }

    fn decrement(&self, key: &str, amount: u64) {
        let now = Instant::now();
        let entry = self.get_or_reset(key, now);
        // 使用 CAS 防止下溢
        loop {
            let current = entry.count.load(Ordering::Relaxed);
            let new_val = current.saturating_sub(amount);
            match entry.count.compare_exchange_weak(
                current,
                new_val,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(_) => continue,
            }
        }
    }

    fn increment(&self, key: &str, amount: u64) {
        let now = Instant::now();
        let entry = self.get_or_reset(key, now);
        entry.count.fetch_add(amount, Ordering::Relaxed);
    }
}

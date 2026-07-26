//! AI 实时配额存储。

pub mod clock;
pub mod memory;
pub mod metrics;
pub mod orchestration;
pub mod store;
pub mod types;

pub use clock::{ManualRateLimitClock, RateLimitClock, SystemRateLimitClock};
pub use memory::{
    MemoryCleanupReport, MemoryRateLimitConfig, MemoryRateLimitConfigError, MemoryRateLimitStore,
};
pub use metrics::RateLimitStoreStatsSnapshot;
pub use orchestration::{admit_with_recovery, settle_with_recovery};
pub use store::{RateLimitStore, RateLimitStoreError, RateLimitStoreErrorKind};
pub use types::*;

/// 旧插件迁移期间保留的同步限流器接口。
pub trait RateLimiter: Send + Sync {
    /// 只读查询当前计数是否超限，返回 (是否放行, 当前计数)
    /// Read-only check: returns (allowed, current_count) without modifying state.
    fn check(&self, key: &str, limit: u64) -> (bool, u64);

    /// 原子检查+递增：如果未超限则递增 amount 并返回 (true, 递增后计数)，
    /// 如果已超限则不递增并返回 (false, 当前计数)
    /// Atomic check-and-increment: if under limit, increment by amount and return (true, new_count);
    /// if over limit, do not increment and return (false, current_count).
    fn check_and_increment(&self, key: &str, limit: u64, amount: u64) -> (bool, u64);

    /// 减少计数（用于退还多扣的 tokens）— decrement count (for returning over-estimated tokens)
    fn decrement(&self, key: &str, amount: u64);

    /// 增加计数 — increment count
    fn increment(&self, key: &str, amount: u64);
}

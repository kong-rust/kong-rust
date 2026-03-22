//! AI 速率限制器 — 基于 Token 计数和请求数的限流

pub mod memory;

/// 限流器 trait — 支持固定窗口计数
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

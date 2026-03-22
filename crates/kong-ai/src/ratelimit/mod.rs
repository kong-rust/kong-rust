//! AI 速率限制器 — 基于 Token 计数和请求数的限流

pub mod memory;

/// 限流器 trait — 支持固定窗口计数
pub trait RateLimiter: Send + Sync {
    /// 检查是否超限，返回 (是否放行, 当前计数)
    fn check(&self, key: &str, limit: u64) -> (bool, u64);
    /// 增加计数
    fn increment(&self, key: &str, amount: u64);
}

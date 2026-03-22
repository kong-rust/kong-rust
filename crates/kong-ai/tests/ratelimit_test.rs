//! 限流器单元测试

use kong_ai::ratelimit::memory::MemoryRateLimiter;
use kong_ai::ratelimit::RateLimiter;
use std::time::Duration;

#[test]
fn test_rpm_limit_allows_under_limit() {
    let limiter = MemoryRateLimiter::new(Duration::from_secs(60));

    // 限制为 10 RPM，当前为 0，应放行
    let (allowed, count) = limiter.check("test:rpm", 10);
    assert!(allowed);
    assert_eq!(count, 0);

    // 增加 5 次
    for _ in 0..5 {
        limiter.increment("test:rpm", 1);
    }

    // 当前为 5，仍在限制内
    let (allowed, count) = limiter.check("test:rpm", 10);
    assert!(allowed);
    assert_eq!(count, 5);
}

#[test]
fn test_rpm_limit_blocks_at_limit() {
    let limiter = MemoryRateLimiter::new(Duration::from_secs(60));

    // 增加到限制值
    for _ in 0..10 {
        limiter.increment("test:rpm", 1);
    }

    // 已达限制，应拒绝
    let (allowed, count) = limiter.check("test:rpm", 10);
    assert!(!allowed);
    assert_eq!(count, 10);

    // 超过限制值也应拒绝
    limiter.increment("test:rpm", 1);
    let (allowed, count) = limiter.check("test:rpm", 10);
    assert!(!allowed);
    assert_eq!(count, 11);
}

#[test]
fn test_tpm_increment_and_check() {
    let limiter = MemoryRateLimiter::new(Duration::from_secs(60));

    // 预扣 100 tokens
    limiter.increment("test:tpm", 100);
    let (allowed, count) = limiter.check("test:tpm", 1000);
    assert!(allowed);
    assert_eq!(count, 100);

    // 再预扣 900 tokens，达到限制
    limiter.increment("test:tpm", 900);
    let (allowed, count) = limiter.check("test:tpm", 1000);
    assert!(!allowed);
    assert_eq!(count, 1000);
}

#[test]
fn test_window_expires_and_resets() {
    // 使用极短窗口（10ms）
    let limiter = MemoryRateLimiter::new(Duration::from_millis(10));

    // 填满计数
    limiter.increment("test:rpm", 100);
    let (allowed, _) = limiter.check("test:rpm", 10);
    assert!(!allowed);

    // 等待窗口过期
    std::thread::sleep(Duration::from_millis(20));

    // 过期后应重置
    let (allowed, count) = limiter.check("test:rpm", 10);
    assert!(allowed);
    assert_eq!(count, 0);
}

#[test]
fn test_different_keys_independent() {
    let limiter = MemoryRateLimiter::new(Duration::from_secs(60));

    limiter.increment("key_a:rpm", 100);
    limiter.increment("key_b:rpm", 5);

    let (allowed_a, count_a) = limiter.check("key_a:rpm", 10);
    let (allowed_b, count_b) = limiter.check("key_b:rpm", 10);

    assert!(!allowed_a);
    assert_eq!(count_a, 100);
    assert!(allowed_b);
    assert_eq!(count_b, 5);
}

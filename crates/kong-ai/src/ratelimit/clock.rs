//! RateLimitStore 使用的权威时钟抽象。

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// Store 内部单调时间。它没有跨进程语义。
pub type MonoTime = Duration;

#[derive(Clone, Copy, Debug)]
pub struct RateLimitNow {
    pub mono: MonoTime,
    pub wall: SystemTime,
}

/// 同一次读取同时返回单调时间和墙上时间，避免窗口快照来自两个时点。
pub trait RateLimitClock: Send + Sync {
    fn now(&self) -> RateLimitNow;
}

/// 进程内系统时钟。
pub struct SystemRateLimitClock {
    started_mono: Instant,
    started_wall: SystemTime,
}

impl SystemRateLimitClock {
    pub fn new() -> Self {
        Self {
            started_mono: Instant::now(),
            started_wall: SystemTime::now(),
        }
    }
}

impl Default for SystemRateLimitClock {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitClock for SystemRateLimitClock {
    fn now(&self) -> RateLimitNow {
        let mono = self.started_mono.elapsed();
        RateLimitNow {
            mono,
            wall: self
                .started_wall
                .checked_add(mono)
                .unwrap_or(SystemTime::UNIX_EPOCH),
        }
    }
}

/// 测试用手工时钟。
pub struct ManualRateLimitClock {
    state: Mutex<ManualClockState>,
}

struct ManualClockState {
    mono: MonoTime,
    wall: SystemTime,
}

impl ManualRateLimitClock {
    pub fn new(wall: SystemTime) -> Self {
        Self {
            state: Mutex::new(ManualClockState {
                mono: Duration::ZERO,
                wall,
            }),
        }
    }

    pub fn advance(&self, duration: Duration) {
        let mut state = self.state.lock().expect("manual rate limit clock poisoned");
        state.mono = state
            .mono
            .checked_add(duration)
            .expect("manual rate limit monotonic time overflow");
        state.wall = state
            .wall
            .checked_add(duration)
            .expect("manual rate limit wall time overflow");
    }
}

impl Default for ManualRateLimitClock {
    fn default() -> Self {
        Self::new(SystemTime::UNIX_EPOCH)
    }
}

impl RateLimitClock for ManualRateLimitClock {
    fn now(&self) -> RateLimitNow {
        let state = self.state.lock().expect("manual rate limit clock poisoned");
        RateLimitNow {
            mono: state.mono,
            wall: state.wall,
        }
    }
}

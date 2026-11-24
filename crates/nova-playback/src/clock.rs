// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Nova contributors

//! 可注入的时钟。

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 时间来源。
///
/// 播放核心里的每个时间判断（400 ms 转圈、15 秒卡死、上报间隔、重试退避）都通过它取当前时间，
/// 因此测试可以完全确定性地推进时间，而不必 `sleep`。
pub trait Clock: Send + Sync {
    /// 当前时刻（单调递增）。
    fn now(&self) -> Instant;
}

/// 真实时钟。
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl SystemClock {
    /// 构造真实时钟。
    pub fn new() -> Self {
        Self
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// 手动时钟：只有显式 [`advance`](ManualClock::advance) 才会走。
///
/// 生产代码不应使用；它是测试与其他 crate 测试的公共设施（引擎实现方也可以用它写自己的测试）。
#[derive(Debug)]
pub struct ManualClock {
    base: Instant,
    elapsed: Mutex<Duration>,
}

impl ManualClock {
    /// 以"现在"为起点构造。
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
        }
    }

    /// 前进一段时间。
    pub fn advance(&self, delta: Duration) {
        let mut guard = lock(&self.elapsed);
        *guard = guard.saturating_add(delta);
    }

    /// 自起点以来推进了多少。
    pub fn elapsed(&self) -> Duration {
        *lock(&self.elapsed)
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.base + self.elapsed()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        // 互斥量中毒说明别处 panic 过；状态本身仍然可用，这里不 panic。
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_only_moves_when_advanced() {
        let clock = ManualClock::new();
        let first = clock.now();
        assert_eq!(clock.now(), first);
        clock.advance(Duration::from_millis(400));
        assert_eq!(clock.now(), first + Duration::from_millis(400));
        assert_eq!(clock.elapsed(), Duration::from_millis(400));
    }

    #[test]
    fn system_clock_is_monotonic() {
        let clock = SystemClock::new();
        assert!(clock.now() >= clock.now() - Duration::from_secs(1));
    }
}

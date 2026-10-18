//! 可注入时钟。
//!
//! 生产路径使用 [`SystemClock`]；测试必须使用确定性时钟，禁止在状态机里直接读
//! 系统时间（否则无法验证“不忙循环/游标只前进”）。时钟返回值一律为 Unix 毫秒，
//! 负值视为时钟故障，由各状态机显式拒绝（`INVALID_TARGET`）。

use std::time::{SystemTime, UNIX_EPOCH};

/// 时间来源。
pub trait Clock {
    /// 当前 Unix 毫秒。
    fn now_unix_ms(&self) -> i64;
}

/// 固定时钟：值不变，适合一次性断言。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedClock {
    now_unix_ms: i64,
}

impl FixedClock {
    /// 构造固定时钟。
    pub const fn new(now_unix_ms: i64) -> Self {
        Self { now_unix_ms }
    }

    /// 修改当前时间（测试里显式推进时间）。
    pub fn set(&mut self, now_unix_ms: i64) {
        self.now_unix_ms = now_unix_ms;
    }

    /// 前进 `step_ms`（饱和）。
    pub fn advance(&mut self, step_ms: i64) {
        self.now_unix_ms = self.now_unix_ms.saturating_add(step_ms);
    }
}

impl Clock for FixedClock {
    fn now_unix_ms(&self) -> i64 {
        self.now_unix_ms
    }
}

/// 系统 UTC 时钟（生产入口）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
            Err(_) => 0,
        }
    }
}

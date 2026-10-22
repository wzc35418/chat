//! 自适应网络预检：**只在最近一次网络失败之后的窗口内**才做一次短探活。
//!
//! 背景（见 docs/SOAK_2026-10-23.md 第十节）：无条件预检会让公网链路每条消息多付一次往返，
//! 甚至出现“节点是好的但预检超时”的假失败（实测 9/10）。所以这里改成失败驱动：
//! - 健康时：**完全不预检**，零额外往返；
//! - 刚失败过：窗口内每次请求前先探一次 /v1/health（8 秒超时），把“链路已死”的发现时间压到秒级；
//! - 一旦成功：窗口立即关闭。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 失败窗口长度：60 秒。
const WINDOW: Duration = Duration::from_secs(60);
/// 探活超时：8 秒（公网高 RTT 下 3 秒会误判）。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

static LAST_FAILURE: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

fn cell() -> &'static Mutex<Option<Instant>> {
    LAST_FAILURE.get_or_init(|| Mutex::new(None))
}

/// 现在是否处于“刚失败过”的窗口内。
pub(crate) fn should_probe() -> bool {
    match cell().lock() {
        Ok(guard) => guard.map(|at| at.elapsed() < WINDOW).unwrap_or(false),
        Err(_) => false,
    }
}

/// 记录一次网络失败（打开窗口）。
pub(crate) fn note_failure() {
    if let Ok(mut guard) = cell().lock() {
        *guard = Some(Instant::now());
    }
}

/// 记录一次成功（关闭窗口）。
pub(crate) fn note_success() {
    if let Ok(mut guard) = cell().lock() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_opens_on_failure_and_closes_on_success() {
        note_success();
        assert!(!should_probe(), "healthy state must not probe");
        note_failure();
        assert!(should_probe(), "after a failure the window must be open");
        note_success();
        assert!(!should_probe(), "a success must close the window");
    }

    #[test]
    fn probe_timeout_is_public_and_sane() {
        assert!(PROBE_TIMEOUT >= Duration::from_secs(5));
        assert!(PROBE_TIMEOUT <= Duration::from_secs(15));
    }
}

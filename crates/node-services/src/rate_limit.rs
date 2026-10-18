//! API-005：节点 HTTP 层基础限流（固定窗口，进程内计数）。
//!
//! 位置约定：**只在 node-api 的 handler 里调用**，服务层单测与节点内部调用不经过限流；
//! 因此领域函数的行为不会被限流器“污染”，也不会因为测试并发而互相影响。
//!
//! 维度（与 api.md 的实现记录一致）：
//!
//! - 已登记设备：每设备请求数 + 每设备请求正文字节数；
//! - 账户：同一账户的多台设备共享每账户请求数预算；
//! - 匿名端点（挑战签发/设备注册）：本进程一个全局匿名预算。**不读任何代理头**，
//!   因此伪造 `X-Forwarded-For` 对本节点完全无效（逐 IP 维度尚未实现，见 api.md 的限制）。
//!
//! 所有计数都按“固定窗口”重置；超限返回 [`NodeError::RateLimited`] 并带 `retry_after_ms`，
//! 绝不静默放行，也不把限流伪装成其它错误。
use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::{NodeError, NodeResult};
use crate::limits::RateLimitPolicy;

/// 一个预算桶的身份。设备与账户是两个独立桶（一次已登记请求同时占用两者）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RateLimitKey {
    /// 匿名端点（挑战/注册）的全局桶。
    Anonymous,
    /// 每设备桶。
    Device(String),
    /// 每账户桶。
    Account(String),
}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    window_start: u64,
    requests: u32,
    bytes: u64,
}

/// 进程内固定窗口限流器（线程安全、桶表有界）。
pub struct RateLimiter {
    policy: RateLimitPolicy,
    buckets: Mutex<HashMap<RateLimitKey, Bucket>>,
}

impl RateLimiter {
    /// 用策略建立限流器；策略本身不合法时显式失败。
    pub fn new(policy: RateLimitPolicy) -> NodeResult<Self> {
        policy.validate()?;
        Ok(Self {
            policy,
            buckets: Mutex::new(HashMap::new()),
        })
    }

    /// 当前策略。
    pub fn policy(&self) -> &RateLimitPolicy {
        &self.policy
    }

    /// 当前桶数量（观测/测试用）。
    pub fn bucket_count(&self) -> usize {
        self.buckets.lock().map(|buckets| buckets.len()).unwrap_or(0)
    }

    /// 已登记设备的一次请求：设备桶与账户桶必须**同时**通过，才算放行。
    pub fn check_device(
        &self,
        device_id: &str,
        account_id: &str,
        bytes: u64,
        now: u64,
    ) -> NodeResult<()> {
        self.check(
            &[
                RateLimitKey::Device(device_id.to_owned()),
                RateLimitKey::Account(account_id.to_owned()),
            ],
            bytes,
            now,
        )
    }

    /// 匿名端点的一次请求：只占全局匿名桶。
    pub fn check_anonymous(&self, bytes: u64, now: u64) -> NodeResult<()> {
        self.check(&[RateLimitKey::Anonymous], bytes, now)
    }

    fn check(&self, keys: &[RateLimitKey], bytes: u64, now: u64) -> NodeResult<()> {
        if !self.policy.enabled {
            return Ok(());
        }
        let window = self.policy.window_seconds;
        let window_start = now - now % window;
        let window_end = window_start.saturating_add(window);
        // 至少 1 毫秒，避免客户端在窗口边界忙等。
        let retry_after_ms = window_end.saturating_sub(now).max(1).saturating_mul(1000);
        let mut buckets = self
            .buckets
            .lock()
            .map_err(|_| NodeError::StorageUnavailable)?;
        // 桶表有界：先丢掉上一个窗口的桶；仍满则拒绝新键（fail-closed，绝不无界增长）。
        if buckets.len() >= self.policy.max_buckets {
            buckets.retain(|_, bucket| bucket.window_start == window_start);
            if buckets.len() >= self.policy.max_buckets {
                return Err(NodeError::RateLimited { retry_after_ms });
            }
        }
        // 先检查全部键，任一超限就整体拒绝（不做部分扣减）。
        for key in keys {
            let (limit_requests, limit_bytes) = self.limits_for(key);
            let (requests, used) = match buckets.get(key) {
                Some(bucket) if bucket.window_start == window_start => {
                    (bucket.requests, bucket.bytes)
                }
                _ => (0, 0),
            };
            if requests.saturating_add(1) > limit_requests {
                return Err(NodeError::RateLimited { retry_after_ms });
            }
            if bytes > 0 && used.saturating_add(bytes) > limit_bytes {
                return Err(NodeError::RateLimited { retry_after_ms });
            }
        }
        // 统一扣减。
        for key in keys {
            let bucket = buckets.entry(key.clone()).or_insert(Bucket {
                window_start,
                requests: 0,
                bytes: 0,
            });
            if bucket.window_start != window_start {
                bucket.window_start = window_start;
                bucket.requests = 0;
                bucket.bytes = 0;
            }
            bucket.requests = bucket.requests.saturating_add(1);
            bucket.bytes = bucket.bytes.saturating_add(bytes);
        }
        Ok(())
    }

    fn limits_for(&self, key: &RateLimitKey) -> (u32, u64) {
        match key {
            RateLimitKey::Anonymous => (self.policy.max_requests_anonymous, u64::MAX),
            RateLimitKey::Device(_) => (
                self.policy.max_requests_per_device,
                self.policy.max_bytes_per_device,
            ),
            // 账户维度只限请求数（字节按设备计，避免多设备互相拖累）。
            RateLimitKey::Account(_) => (self.policy.max_requests_per_account, u64::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RateLimitPolicy {
        RateLimitPolicy {
            enabled: true,
            window_seconds: 60,
            max_requests_per_device: 2,
            max_requests_per_account: 3,
            max_requests_anonymous: 5,
            max_bytes_per_device: 10,
            max_buckets: 16,
        }
    }

    fn limiter() -> RateLimiter {
        RateLimiter::new(policy()).expect("limiter")
    }

    fn retry_after(error: &NodeError) -> u64 {
        match error {
            NodeError::RateLimited { retry_after_ms } => *retry_after_ms,
            other => panic!("expected RATE_LIMITED, got {}", other.code()),
        }
    }

    #[test]
    fn device_budget_is_enforced_and_resets_with_the_window() {
        let limiter = limiter();
        let now = 1_700_000_000;
        assert!(limiter.check_device("dev-1", "acct-1", 0, now).is_ok());
        assert!(limiter.check_device("dev-1", "acct-1", 0, now + 1).is_ok());
        let error = limiter
            .check_device("dev-1", "acct-1", 0, now + 2)
            .expect_err("third request in the window is limited");
        assert_eq!(error.code(), "RATE_LIMITED");
        assert_eq!(error.safe_message(), "rate limited");
        // 窗口内剩余时间就是建议的重试等待（至少 1 秒，最长一个窗口）。
        let wait = retry_after(&error);
        assert!(wait >= 1_000 && wait <= 60_000, "retry_after_ms={wait}");
        // 换设备不受影响，下一个窗口恢复。
        assert!(limiter.check_device("dev-2", "acct-1", 0, now + 2).is_ok());
        let next_window = now + 60;
        assert!(limiter.check_device("dev-1", "acct-1", 0, next_window).is_ok());
    }

    #[test]
    fn account_budget_is_shared_across_devices() {
        let limiter = limiter();
        let now = 1_700_000_000;
        assert!(limiter.check_device("dev-a", "acct-1", 0, now).is_ok());
        assert!(limiter.check_device("dev-b", "acct-1", 0, now).is_ok());
        assert!(limiter.check_device("dev-c", "acct-1", 0, now).is_ok());
        let error = limiter
            .check_device("dev-d", "acct-1", 0, now)
            .expect_err("account budget is exhausted");
        assert_eq!(error.code(), "RATE_LIMITED");
        // 另一个账户不受影响。
        assert!(limiter.check_device("dev-d", "acct-2", 0, now).is_ok());
    }

    #[test]
    fn request_bytes_are_counted_per_device() {
        let limiter = limiter();
        let now = 1_700_000_000;
        assert!(limiter.check_device("dev-1", "acct-1", 6, now).is_ok());
        let error = limiter
            .check_device("dev-1", "acct-1", 6, now)
            .expect_err("byte budget is exhausted");
        assert_eq!(error.code(), "RATE_LIMITED");
        // 大请求按字节计：即使请求数还有余量也会被拒绝。
        let error = limiter
            .check_device("dev-1", "acct-1", 11, now)
            .expect_err("oversized request exceeds the byte budget");
        assert_eq!(error.code(), "RATE_LIMITED");
    }

    #[test]
    fn anonymous_requests_share_one_global_budget() {
        let limiter = limiter();
        let now = 1_700_000_000;
        for _ in 0..5 {
            assert!(limiter.check_anonymous(0, now).is_ok());
        }
        let error = limiter
            .check_anonymous(0, now)
            .expect_err("anonymous budget is exhausted");
        assert_eq!(error.code(), "RATE_LIMITED");
        // 已登记设备的预算与匿名预算互不影响。
        assert!(limiter.check_device("dev-1", "acct-1", 0, now).is_ok());
    }

    #[test]
    fn disabled_policy_allows_everything() {
        let limiter = RateLimiter::new(RateLimitPolicy {
            enabled: false,
            ..policy()
        })
        .expect("limiter");
        let now = 1_700_000_000;
        for _ in 0..100 {
            assert!(limiter.check_device("dev-1", "acct-1", 1_000, now).is_ok());
            assert!(limiter.check_anonymous(1_000, now).is_ok());
        }
        assert_eq!(limiter.bucket_count(), 0);
    }

    #[test]
    fn bucket_table_stays_bounded_and_fails_closed() {
        let limiter = limiter();
        let now = 1_700_000_000;
        // 每个设备两个键（device + account），16 个桶意味着 8 台设备正好填满。
        for index in 0..8 {
            let device = format!("dev-{index}");
            let account = format!("acct-{index}");
            assert!(limiter.check_device(&device, &account, 0, now).is_ok());
        }
        assert_eq!(limiter.bucket_count(), 16);
        // 桶表满且都是当前窗口：显式拒绝新键（不无界增长，也不静默放行）。
        let error = limiter
            .check_device("dev-new", "acct-new", 0, now)
            .expect_err("bucket table is full");
        assert_eq!(error.code(), "RATE_LIMITED");
        // 下一个窗口会清理旧桶，新键重新可放行。
        assert!(limiter.check_device("dev-new", "acct-new", 0, now + 60).is_ok());
    }

    #[test]
    fn invalid_policy_is_rejected() {
        assert!(RateLimiter::new(RateLimitPolicy {
            window_seconds: 0,
            ..policy()
        })
        .is_err());
        assert!(RateLimiter::new(RateLimitPolicy {
            max_requests_per_account: 1,
            max_requests_per_device: 2,
            ..policy()
        })
        .is_err());
        assert!(RateLimiter::new(RateLimitPolicy {
            max_buckets: 4,
            ..policy()
        })
        .is_err());
    }
}

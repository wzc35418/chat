//! 服务与时钟注入。
//!
//! 时间一律由调用方显式传入服务层，HTTP 层通过 Clock 取当前时间：
//! 生产用系统时钟，测试用固定时钟推进过期，不用“本地猜测”替代真实时间参数。
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_node_services::error::{NodeError, NodeResult};
use chat_node_services::cleanup::CleanupStats;
use chat_node_services::limits::RateLimitPolicy;
use chat_node_services::rate_limit::RateLimiter;
use chat_node_services::services::NodeService;

use crate::error::ApiError;

/// 节点时钟抽象。
pub trait Clock: Send + Sync {
    /// 当前 Unix 秒。
    fn now_unix(&self) -> u64;
}

/// 系统时钟。
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0)
    }
}

/// 测试用可控时钟。
#[derive(Debug, Default)]
pub struct FixedClock {
    seconds: AtomicU64,
}

impl FixedClock {
    /// 以给定时间建立固定时钟。
    pub fn new(seconds: u64) -> Self {
        Self {
            seconds: AtomicU64::new(seconds),
        }
    }

    /// 直接设置时间。
    pub fn set(&self, seconds: u64) {
        self.seconds.store(seconds, Ordering::SeqCst);
    }

    /// 前进若干秒。
    pub fn advance(&self, seconds: u64) {
        self.seconds.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now_unix(&self) -> u64 {
        self.seconds.load(Ordering::SeqCst)
    }
}

/// axum 共享状态：单连接节点服务 + 时钟 + HTTP 层限流器。
#[derive(Clone)]
pub struct AppState {
    service: Arc<Mutex<NodeService>>,
    clock: Arc<dyn Clock>,
    rate_limiter: Arc<RateLimiter>,
}

impl AppState {
    /// 用显式时钟建立状态；限流器取节点策略（策略在 NodeService::open 时已校验）。
    pub fn new(service: NodeService, clock: Arc<dyn Clock>) -> Self {
        let policy = service.policy().rate_limit.clone();
        // 防御式回退：策略不合法时退化为“关闭限流”，而不是 panic 或静默放行未知配置。
        let rate_limiter = RateLimiter::new(policy).unwrap_or_else(|_| {
            RateLimiter::new(RateLimitPolicy {
                enabled: false,
                ..RateLimitPolicy::default()
            })
            .expect("disabled rate limit policy is always valid")
        });
        Self {
            service: Arc::new(Mutex::new(service)),
            clock,
            rate_limiter: Arc::new(rate_limiter),
        }
    }

    /// 用显式限流器建立状态（测试用固定窗口/紧预算）。
    pub fn with_rate_limiter(
        service: NodeService,
        clock: Arc<dyn Clock>,
        rate_limiter: RateLimiter,
    ) -> Self {
        Self {
            service: Arc::new(Mutex::new(service)),
            clock,
            rate_limiter: Arc::new(rate_limiter),
        }
    }

    /// 用系统时钟建立状态（生产路径）。
    pub fn with_system_clock(service: NodeService) -> Self {
        Self::new(service, Arc::new(SystemClock))
    }

    /// 当前 Unix 秒。
    pub fn now(&self) -> u64 {
        self.clock.now_unix()
    }

    /// 单请求正文上限，取自节点策略。
    pub fn max_request_body_bytes(&self) -> usize {
        self.service
            .lock()
            .map(|service| service.policy().max_request_body_bytes)
            .unwrap_or(chat_node_services::limits::MAX_HTTP_BODY_BYTES)
    }

    /// NODE-016：回收过期资源（库内过期数据 + 进程内限流桶），返回合并后的统计。
    ///
    /// 库内清理由服务层在一个事务里完成；限流桶在进程内，因此在这里合并。
    pub fn collect_expired(&self, now: u64) -> Result<CleanupStats, ApiError> {
        let mut stats = self.with_service(|service| service.collect_expired(now))?;
        stats.expired_rate_limit_buckets =
            u64::try_from(self.rate_limiter.collect_expired(now)).unwrap_or(0);
        Ok(stats)
    }

    /// 当前限流器（观测用）。
    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.rate_limiter
    }

    /// API-005：匿名端点的一次请求（挑战签发/设备注册）。
    ///
    /// 只在 HTTP 层调用；超限返回 429 + RATE_LIMITED，并带 retry_after。
    pub fn limit_anonymous(&self, bytes: u64) -> Result<(), ApiError> {
        self.rate_limiter
            .check_anonymous(bytes, self.now())
            .map_err(ApiError::from_node)
    }

    /// API-005：已登记设备的一次请求（同时占设备桶与账户桶）。
    pub fn limit_device(&self, device_id: &str, account_id: &str, bytes: u64) -> Result<(), ApiError> {
        self.rate_limiter
            .check_device(device_id, account_id, bytes, self.now())
            .map_err(ApiError::from_node)
    }

    /// 节点公开身份（node_id + 签名公钥）；两者都不是秘密，供 /v1/health 使用。
    pub fn node_identity(&self) -> Result<(String, [u8; 32]), ApiError> {
        self.with_service(|service| Ok((service.node_id().to_owned(), service.node_public_key())))
    }

    /// 在锁内执行一次服务调用；锁中毒映射为 STORAGE_FAILURE，绝不当作成功。
    pub fn with_service<T>(
        &self,
        operation: impl FnOnce(&mut NodeService) -> NodeResult<T>,
    ) -> Result<T, ApiError> {
        let mut service = self
            .service
            .lock()
            .map_err(|_| ApiError::from_node(NodeError::StorageUnavailable))?;
        operation(&mut service).map_err(ApiError::from_node)
    }
}
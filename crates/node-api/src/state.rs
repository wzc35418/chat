//! 服务与时钟注入。
//!
//! 时间一律由调用方显式传入服务层，HTTP 层通过 Clock 取当前时间：
//! 生产用系统时钟，测试用固定时钟推进过期，不用“本地猜测”替代真实时间参数。
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_node_services::error::{NodeError, NodeResult};
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

/// axum 共享状态：单连接节点服务 + 时钟。
#[derive(Clone)]
pub struct AppState {
    service: Arc<Mutex<NodeService>>,
    clock: Arc<dyn Clock>,
}

impl AppState {
    /// 用显式时钟建立状态。
    pub fn new(service: NodeService, clock: Arc<dyn Clock>) -> Self {
        Self {
            service: Arc::new(Mutex::new(service)),
            clock,
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

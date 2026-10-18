//! SR 包：常驻同步运行时 —— 把 [`crate::sync_worker`] 的单轮拉取包成可运行、可停止的有界循环。
//!
//! 形状：一个专属线程 + current-thread tokio 运行时；每轮做**至多一次**拉取决策：
//!
//! - 成功（应用了条目 / 已到 head）→ 按决策给出的间隔等待下一轮；
//! - `Resync` → 立刻转一次 [`run_catchup_once`]（追赶计划按序分页），缺口绝不静默跳过；
//! - 失败 → 按 sync 的退避语义等待（等待时长由 `PullPolicy::failure_delay_for` 决定，本层不 sleep 在调用里）；
//! - 预算耗尽 / 明确停止 / **不可重试错误**（PROTOCOL-014 的 `retryable()` 为假，例如授权或验签失败）→ 进入 `Stopped`。
//!
//! 不占满 CPU：每轮之间都用条件变量等待（可被 `stop()` 立即唤醒），等待时长夹在
//! `round_wait_min_ms..round_wait_max_ms`；循环里没有任何忙等或自旋。
//!
//! 可观测性：[`SyncRuntime::status`] 返回一份**只读快照**（`state` / `applied_total` / `last_error_code` /
//! `next_attempt_at_ms` / `resync_required`），界面可以低频轮询而不碰数据库、也不持锁做 I/O。
//!
//! 设备身份：任务书给的入口参数里没有设备签名钥，但 `NodeClient` 必须有它（一次性挑战 + 设备签名）。
//! 因此入口是 [`SyncRuntimeConfig`]：`store_path` / `master_key`（hex 或 32 字节）/ `endpoint` / `group_id` / `policy`
//! 与显式注入的 [`SyncDevice`]；其余语义与任务书一致。
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chat_node_client::NodeClient;
use chat_node_services::canonical::hex_decode;
use chat_protocol::{ErrorCode, GroupId};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};
use chat_sync::clock::{Clock, SystemClock};
use chat_sync::error::{Result as SyncResult, SyncError};
use chat_sync::pull::PullPolicy;

use crate::sync_worker::{load_pull_state, run_catchup_once_at, run_pull_once_at};

/// 一轮之间允许的最小等待（毫秒）：既防止忙等，也保证 `stop()` 后状态能及时可见。
pub const DEFAULT_ROUND_WAIT_MIN_MS: u64 = 50;
/// 一轮之间允许的最大等待（毫秒）。
pub const DEFAULT_ROUND_WAIT_MAX_MS: u64 = 5_000;
/// 连续失败多少轮后停止自动重试（与 sync 的失败预算一致）。
pub const DEFAULT_MAX_FAILURES: u32 = 8;

/// 循环状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// 空闲：上一轮成功，等下一轮。
    Idle,
    /// 正在拉取/追赶。
    Pulling,
    /// 退避等待中（上一轮失败或节点要求稍后再来）。
    Backoff,
    /// 已停止（收到 stop 信号、预算耗尽或不可重试错误）。
    Stopped,
}

/// 只读状态快照（供界面轮询，不含任何密钥或正文）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncStatusView {
    /// 当前状态。
    pub state: SyncState,
    /// 本进程生命周期内累计应用的条目数。
    pub applied_total: u64,
    /// 最近一次失败的稳定错误码（PROTOCOL-014 文本）。
    pub last_error_code: Option<&'static str>,
    /// 下一次允许拉取的时间（Unix 毫秒）。
    pub next_attempt_at_ms: i64,
    /// 是否出现过缺口、需要重新对齐（交给追赶计划/上层）。
    pub resync_required: bool,
    /// 已执行的轮数（诊断）。
    pub rounds: u64,
    /// 停止原因（稳定文本；正常 stop 为 `stop_requested`）。
    pub stopped_reason: Option<&'static str>,
}

impl SyncStatusView {
    fn initial(now_ms: i64) -> Self {
        Self {
            state: SyncState::Idle,
            applied_total: 0,
            last_error_code: None,
            next_attempt_at_ms: now_ms,
            resync_required: false,
            rounds: 0,
            stopped_reason: None,
        }
    }

    /// 是否已停止。
    pub fn is_stopped(&self) -> bool {
        self.state == SyncState::Stopped
    }
}

/// 运行时的可克隆状态句柄（stop 之后仍可读最后状态）。
#[derive(Clone)]
pub struct SyncStatusHandle {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for SyncStatusHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 只打印快照（不含任何密钥或正文）。
        formatter
            .debug_struct("SyncStatusHandle")
            .field("status", &self.view())
            .finish()
    }
}

impl SyncStatusHandle {
    /// 读取当前快照。
    pub fn view(&self) -> SyncStatusView {
        self.shared.snapshot()
    }
}

struct Shared {
    status: Mutex<SyncStatusView>,
    wake: Condvar,
    stop: AtomicBool,
}

impl Shared {
    fn snapshot(&self) -> SyncStatusView {
        match self.status.lock() {
            Ok(status) => status.clone(),
            // 锁中毒只影响观测：返回初始快照而不是 panic。
            Err(_) => SyncStatusView::initial(0),
        }
    }

    fn update(&self, mutate: impl FnOnce(&mut SyncStatusView)) {
        if let Ok(mut status) = self.status.lock() {
            mutate(&mut status);
        }
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// 等待下一轮；返回 `true` 表示收到 stop 信号。
    fn wait_round(&self, timeout_ms: u64) -> bool {
        if self.stopping() {
            return true;
        }
        let Ok(mut status) = self.status.lock() else {
            // 锁中毒：退化成有界 sleep，不忙等。
            thread::sleep(Duration::from_millis(timeout_ms.min(DEFAULT_ROUND_WAIT_MAX_MS)));
            return self.stopping();
        };
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if self.stopping() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return self.stopping();
            }
            let remaining = deadline.saturating_duration_since(now);
            match self.wake.wait_timeout(status, remaining) {
                Ok((guard, _)) => status = guard,
                Err(_) => return self.stopping(),
            }
        }
    }
}


/// 循环参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncRuntimePolicy {
    /// 一轮之间的最小等待（毫秒，≥1）。
    pub round_wait_min_ms: u64,
    /// 一轮之间的最大等待（毫秒，≥ 最小值且 ≤ 300000）。
    pub round_wait_max_ms: u64,
    /// 连续失败多少轮后停止（≥1）。
    pub max_failures: u32,
}

impl Default for SyncRuntimePolicy {
    fn default() -> Self {
        Self {
            round_wait_min_ms: DEFAULT_ROUND_WAIT_MIN_MS,
            round_wait_max_ms: DEFAULT_ROUND_WAIT_MAX_MS,
            max_failures: DEFAULT_MAX_FAILURES,
        }
    }
}

impl SyncRuntimePolicy {
    /// 有界校验：越界显式拒绝，不夹取。
    pub fn validate(&self) -> SyncResult<()> {
        if self.round_wait_min_ms == 0 || self.round_wait_max_ms > 300_000 {
            return Err(invalid_target("round wait bounds are out of range"));
        }
        if self.round_wait_max_ms < self.round_wait_min_ms {
            return Err(invalid_target("round wait max must not be below min"));
        }
        if self.max_failures == 0 {
            return Err(invalid_target("max failures must be at least 1"));
        }
        Ok(())
    }

    fn clamp(&self, wait_ms: u64) -> u64 {
        wait_ms.clamp(self.round_wait_min_ms, self.round_wait_max_ms)
    }
}

/// 运行时配置。
#[derive(Debug)]
pub struct SyncRuntimeConfig {
    /// 正式库路径（运行时自己打开、自己关闭）。
    pub store_path: PathBuf,
    /// 库主密钥（32 字节；进入 `ProtectedStorageKey`）。
    pub master_key: [u8; 32],
    /// 已构造好的节点客户端（固定端点 + 固定节点公钥 + 设备身份/签名钥）。
    ///
    /// 之所以传客户端而不是“端点 + 设备钥”：core 不直接依赖 ed25519-dalek，
    /// 设备签名钥由上层（core 之外的密钥托管方）构造后注入；本层只消费它。
    pub client: NodeClient,
    /// 要同步的群（PROTOCOL-001 群文本）。
    pub group_id: String,
    /// 循环参数。
    pub policy: SyncRuntimePolicy,
    /// 拉取决策策略（页大小/退避/预算）。
    pub pull_policy: PullPolicy,
}

impl SyncRuntimeConfig {
    /// 最小配置：库路径 + 主密钥 hex + 节点客户端（含固定公钥与设备身份）+ 群。
    pub fn new(
        store_path: impl Into<PathBuf>,
        master_key_hex: &str,
        client: NodeClient,
        group_id: impl Into<String>,
    ) -> SyncResult<Self> {
        let master_key = hex_decode::<32>("store master key", master_key_hex).map_err(|_| {
            SyncError::new(
                ErrorCode::InvalidTarget,
                "master key must be 64 hex characters",
            )
        })?;
        Ok(Self {
            store_path: store_path.into(),
            master_key,
            client,
            group_id: group_id.into(),
            policy: SyncRuntimePolicy::default(),
            pull_policy: PullPolicy::DEFAULT,
        })
    }

    /// 注入循环参数。
    pub fn with_policy(mut self, policy: SyncRuntimePolicy) -> Self {
        self.policy = policy;
        self
    }

    fn validate(&self) -> SyncResult<()> {
        if self.store_path.as_os_str().is_empty() {
            return Err(invalid_target("store path must not be empty"));
        }
        if self.group_id.is_empty() {
            return Err(invalid_target("sync runtime requires a group id"));
        }
        if self.client.endpoint().node_public_key().is_none() {
            return Err(invalid_target(
                "sync runtime requires a pinned node public key",
            ));
        }
        if self.pull_policy.validate().is_err() {
            return Err(invalid_target("pull policy is out of range"));
        }
        self.policy.validate()?;
        GroupId::parse(&self.group_id)
            .map_err(|_| invalid_target("group id must be PROTOCOL-001 group text"))?;
        Ok(())
    }
}

/// 常驻同步运行时。
pub struct SyncRuntime {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<SyncResult<Store>>>,
}

impl SyncRuntime {
    /// 启动循环：独立线程 + current-thread 运行时，立刻返回。
    ///
    /// 启动前会做三件同步检查（失败立即返回，不留下半启动的线程）：
    /// 配置有界校验、群在库里有拉取游标（否则 `INVALID_CURSOR`）、库能打开。
    pub fn start(config: SyncRuntimeConfig) -> SyncResult<Self> {
        config.validate()?;
        let now = SystemClock.now_unix_ms();
        let shared = Arc::new(Shared {
            status: Mutex::new(SyncStatusView::initial(now)),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        // 启动前先确认游标存在（把“配置错误”留在启动时，而不是变成循环里的停止原因）。
        {
            let store = open_store(
                StoreOptions::new(config.store_path.clone(), KeyProvenance::Raw32),
                ProtectedStorageKey::from_bytes(config.master_key),
            )
            .map_err(|_| storage_error())?;
            if load_pull_state(&store, &config.group_id)?.is_none() {
                return Err(SyncError::new(
                    ErrorCode::InvalidCursor,
                    "pull cursor is not initialized for this group",
                ));
            }
        }
        let thread_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("chat-sync-runtime".to_owned())
            .spawn(move || run_loop(config, thread_shared))
            .map_err(|_| SyncError::new(ErrorCode::NodeUnavailable, "sync thread could not start"))?;
        Ok(Self {
            shared,
            handle: Some(handle),
        })
    }

    /// 当前状态快照。
    pub fn status(&self) -> SyncStatusView {
        self.shared.snapshot()
    }

    /// 可克隆的状态句柄（stop 之后仍能读最后状态）。
    pub fn status_handle(&self) -> SyncStatusHandle {
        SyncStatusHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// 请求停止并等待线程退出；返回循环自己打开的库句柄（调用方可继续读/写）。
    ///
    /// 停止是协作式的：等待中的条件变量会被立刻唤醒，最多等当前这一轮的网络请求结束。
    pub fn stop(mut self) -> SyncResult<Store> {
        self.shared.request_stop();
        self.shared.update(|status| {
            if status.state != SyncState::Stopped {
                status.state = SyncState::Stopped;
                status.stopped_reason = Some("stop_requested");
            }
        });
        match self.handle.take() {
            Some(handle) => handle
                .join()
                .map_err(|_| SyncError::new(ErrorCode::InternalFailure, "sync thread panicked"))?,
            None => Err(SyncError::new(ErrorCode::InternalFailure, "sync thread already joined")),
        }
    }
}

impl Drop for SyncRuntime {
    fn drop(&mut self) {
        // 兜底：调用方忘了 stop 也不会让线程泄漏（不强等结果，只保证退出）。
        self.shared.request_stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// 循环主体（在专属线程上跑，持有自己的库句柄与节点客户端）。
fn run_loop(config: SyncRuntimeConfig, shared: Arc<Shared>) -> SyncResult<Store> {
    let mut store = open_store(
        StoreOptions::new(config.store_path.clone(), KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(config.master_key),
    )
    .map_err(|_| storage_error())?;
    let client = config.client;
    let group_id = config.group_id.clone();
    let mut failures = 0_u32;
    let mut applied_total = 0_u64;
    loop {
        if shared.stopping() {
            finish(&shared, "stop_requested");
            return Ok(store);
        }
        let now = SystemClock.now_unix_ms();
        shared.update(|status| {
            status.state = SyncState::Pulling;
            status.rounds = status.rounds.saturating_add(1);
        });
        // 一轮 = 一次 run_pull_once（决策 + 至多一页）；Resync 时立刻转一次 catchup。
        let wait_ms = match run_pull_once_at(&mut store, &client, &group_id, now) {
            Ok(outcome) => {
                applied_total = applied_total.saturating_add(outcome.applied);
                failures = 0;
                shared.update(|status| {
                    status.applied_total = applied_total;
                    status.last_error_code = None;
                });
                if outcome.resync_required {
                    shared.update(|status| status.resync_required = true);
                    match run_catchup_once_at(&mut store, &client, &group_id, now) {
                        Ok(catchup) => {
                            applied_total = applied_total.saturating_add(catchup.applied);
                            // 追赶后仍有上层动作（epoch 重建/等待控制结果）时，缺口依旧存在。
                            let still_pending =
                                catchup.awaiting.is_some() || !catchup.upper_actions.is_empty();
                            shared.update(|status| {
                                status.applied_total = applied_total;
                                status.resync_required = still_pending || !catchup.caught_up;
                            });
                            config.policy.clamp(config.policy.round_wait_min_ms)
                        }
                        Err(error) => {
                            shared.update(|status| {
                                status.last_error_code = Some(error.code().as_str());
                            });
                            if !error.retryable() {
                                finish(&shared, "unretryable_error");
                                return Ok(store);
                            }
                            failures = failures.saturating_add(1);
                            if failures > config.policy.max_failures {
                                finish(&shared, "failure_budget_exhausted");
                                return Ok(store);
                            }
                            config.policy.clamp(config.pull_policy.failure_delay_for(failures) as u64)
                        }
                    }
                } else if let Some(reason) = outcome.stopped {
                    // 决策核心的失败预算是终态：直接停，不无限重试。
                    let label = match reason {
                        chat_sync::pull::StopReason::FailureBudgetExhausted => {
                            "failure_budget_exhausted"
                        }
                    };
                    finish(&shared, label);
                    return Ok(store);
                } else {
                    config
                        .policy
                        .clamp(outcome.backoff_ms.unwrap_or(config.policy.round_wait_min_ms))
                }
            }
            Err(error) => {
                shared.update(|status| {
                    status.last_error_code = Some(error.code().as_str());
                });
                // 不可重试（授权/验签/冲突等）→ 明确终止，不再自动重试。
                if !error.retryable() {
                    finish(&shared, "unretryable_error");
                    return Ok(store);
                }
                failures = failures.saturating_add(1);
                if failures > config.policy.max_failures {
                    finish(&shared, "failure_budget_exhausted");
                    return Ok(store);
                }
                config.policy.clamp(config.pull_policy.failure_delay_for(failures) as u64)
            }
        };
        let next_attempt = SystemClock.now_unix_ms().saturating_add(wait_ms as i64);
        shared.update(|status| {
            status.state = if wait_ms > config.policy.round_wait_min_ms {
                SyncState::Backoff
            } else {
                SyncState::Idle
            };
            status.next_attempt_at_ms = next_attempt;
        });
        if shared.wait_round(wait_ms) {
            finish(&shared, "stop_requested");
            return Ok(store);
        }
    }
}

fn finish(shared: &Shared, reason: &'static str) {
    shared.update(|status| {
        status.state = SyncState::Stopped;
        status.stopped_reason = Some(reason);
    });
}

fn invalid_target(detail: &'static str) -> SyncError {
    SyncError::new(ErrorCode::InvalidTarget, detail)
}

fn storage_error() -> SyncError {
    // 只保留冻结码，不回显路径/SQL 细节。
    SyncError::new(ErrorCode::StorageFailure, "sync runtime could not open the store")
}

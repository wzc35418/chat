//! 常驻同步（SYNC-003 SyncRuntime）的 UniFFI 门面。
//!
//! 职责边界：
//!
//! - `SyncFfi::open(...)` 只做**同步校验 + 装配**：打开正式加密库读持久身份种子、派生设备签名钥、
//!   构造固定端点/固定节点公钥的 NodeClient、确认该群的拉取游标已初始化；**不启动**循环。
//!   游标未初始化 → `INVALID_STATE_TRANSITION`（拉取起点必须来自可信回执，不能拿节点自我声明的 head）。
//! - `start()` 才真正启动常驻循环（独立线程 + 独立运行时）；`stop()` 协作式停止并保留最后状态；
//!   `status()` 随时可读（未启动时是 idle）。
//! - `shutdown()` 幂等：先停循环再标记关闭，之后所有调用返回 `CORE_CLOSED`。
//!
//! 状态全部是计数/稳定码/时间戳，不含任何密钥或正文。
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::sync_runtime::{
    SyncRuntime, SyncRuntimeConfig, SyncState, SyncStatusHandle, SyncStatusView,
};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, StoreOptions, open_store};

use crate::local_store::FfiError;

/// 统一错误投影：错误码一律取各层自己的**冻结码**（storage 的 `STORAGE_*`、sync 的
/// `INVALID_CURSOR` / `NODE_UNAVAILABLE` / `STALE_EPOCH` …），说明只放脱敏文本。
fn failure(code: &'static str, reason: impl Into<String>) -> FfiError {
    FfiError::Store {
        code: code.to_owned(),
        reason: reason.into(),
    }
}

fn core_closed() -> FfiError {
    FfiError::Store {
        code: "CORE_CLOSED".to_owned(),
        reason: "the sync runtime object is closed".to_owned(),
    }
}

fn invalid_input(reason: &str) -> FfiError {
    FfiError::Store {
        code: "INVALID_INPUT".to_owned(),
        reason: reason.to_owned(),
    }
}

fn invalid_state(reason: &str) -> FfiError {
    FfiError::Store {
        code: "INVALID_STATE_TRANSITION".to_owned(),
        reason: reason.to_owned(),
    }
}

fn poisoned(what: &str) -> FfiError {
    FfiError::Store {
        code: "INTERNAL_FAILURE".to_owned(),
        reason: format!("{what} is poisoned"),
    }
}

/// 主密钥（64 位十六进制 → 32 字节）；错误密钥绝不被“修正”。
fn decode_master_key(text: &str) -> Result<[u8; 32], FfiError> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return Err(invalid_input("master key must be exactly 64 hexadecimal characters"));
    }
    let mut key = [0_u8; 32];
    for (index, slot) in key.iter_mut().enumerate() {
        let high = decode_nibble(bytes[index * 2])?;
        let low = decode_nibble(bytes[index * 2 + 1])?;
        *slot = (high << 4) | low;
    }
    Ok(key)
}

fn decode_nibble(byte: u8) -> Result<u8, FfiError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(invalid_input("master key must be hexadecimal")),
    }
}

/// 一次性拉取的结果投影（SYNC 单轮：拉一页 → 验签 → 应用 → 前进游标）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncOnceFfi {
    /// 本页拉到的条目数（取自应用报告：applied + skipped；取不到时退化为 applied）。
    pub fetched: u64,
    /// 真正验签并应用（消息/Commit/提案）的条目数。
    pub applied: u64,
    /// 被跳过的条目数（stale + unsupported）。
    pub skipped: u64,
    /// 本轮结束后的游标（没有新条目/失败时保持不变）。
    pub next_cursor: u64,
    /// 是否已经追平（没有待重对齐、也没有建议退避）。
    pub settled: bool,
}

/// 同步状态投影（供界面轮询；不含密钥或正文）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncStatusFfi {
    /// 状态：`idle` / `pulling` / `backoff` / `stopped`。
    pub state: String,
    /// 本进程生命周期内累计应用的条目数。
    pub applied_total: u64,
    /// 最近一次失败的稳定错误码（PROTOCOL-014 文本）。
    pub last_error_code: Option<String>,
    /// 下一次允许拉取的时间（Unix 毫秒；退避期间是未来时间）。
    pub next_attempt_at_ms: i64,
    /// 是否出现过缺口、需要重新对齐（交给追赶计划/上层）。
    pub resync_required: bool,
    /// 已执行的轮数（诊断）。
    pub rounds: u64,
    /// 停止原因（正常 stop 为 `stop_requested`）。
    pub stopped_reason: Option<String>,
}

fn state_code(state: SyncState) -> &'static str {
    match state {
        SyncState::Idle => "idle",
        SyncState::Pulling => "pulling",
        SyncState::Backoff => "backoff",
        SyncState::Stopped => "stopped",
    }
}

fn status_ffi(view: &SyncStatusView) -> SyncStatusFfi {
    SyncStatusFfi {
        state: state_code(view.state).to_owned(),
        applied_total: view.applied_total,
        last_error_code: view.last_error_code.map(str::to_owned),
        next_attempt_at_ms: view.next_attempt_at_ms,
        resync_required: view.resync_required,
        rounds: view.rounds,
        stopped_reason: view.stopped_reason.map(str::to_owned),
    }
}

fn idle_status() -> SyncStatusFfi {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    SyncStatusFfi {
        state: "idle".to_owned(),
        applied_total: 0,
        last_error_code: None,
        next_attempt_at_ms: now,
        resync_required: false,
        rounds: 0,
        stopped_reason: None,
    }
}

struct Inner {
    runtime: Option<SyncRuntime>,
    last: Option<SyncStatusHandle>,
    closed: bool,
}

/// 常驻同步 FFI 对象。
#[derive(uniffi::Object)]
pub struct SyncFfi {
    database_path: String,
    master_key_hex: String,
    endpoint_url: String,
    node_id: String,
    node_public_key_hex: String,
    group_id: String,
    inner: Mutex<Inner>,
}

impl SyncFfi {
    fn lock(&self) -> Result<MutexGuard<'_, Inner>, FfiError> {
        self.inner.lock().map_err(|_| poisoned("sync runtime"))
    }

    /// 装配 NodeClient（设备身份来自库里的持久种子）。
    fn build_client(&self) -> Result<NodeClient, FfiError> {
        let master_key = decode_master_key(&self.master_key_hex)?;
        let store = open_store(
            StoreOptions::new(&self.database_path, KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(master_key),
        )
        .map_err(|error| failure(error.code(), error.to_string()))?;
        let seeds = store
            .load_identity_seeds()
            .map_err(|error| failure(error.code(), error.to_string()))?
            .ok_or_else(|| invalid_state("the local identity is not enrolled in this database"))?;
        let device_seed: [u8; 32] = seeds.device_seed.as_slice().try_into().map_err(|_| {
            FfiError::Store {
                code: "STATE_CORRUPTED".to_owned(),
                reason: "the persisted device seed is not 32 bytes".to_owned(),
            }
        })?;
        let signing_key = chat_protocol::signing_key_from_seed(&device_seed);
        let mut endpoint =
            NodeEndpoint::new(&self.endpoint_url, &self.node_id).map_err(|_| {
                invalid_input("node endpoint must be a parseable http(s) url")
            })?;
        endpoint = endpoint.with_node_public_key(decode_node_public_key(&self.node_public_key_hex)?);
        // 游标必须已初始化，否则不构造运行时（拉取起点来自可信回执，不能问节点要）。
        match chat_core::sync_worker::load_pull_state(&store, &self.group_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(invalid_state(
                    "pull cursor is not initialized for this group",
                ));
            }
            Err(error) => {
                return Err(failure(error.code().as_str(), error.to_string()));
            }
        }
        drop(store);
        NodeClient::new(endpoint, seeds.device_id.clone(), signing_key).map_err(|error| {
            FfiError::Store {
                code: "INVALID_TARGET".to_owned(),
                // 带上底层原因：TLS 指纹不匹配 / 端点不自洽都会在这里显形，而不是只说"连不上"。
                reason: format!(
                    "node client could not be created from the persisted device identity: {}",
                    error.detail()
                ),
            }
        })
    }

    /// 每次 start 都重建配置（NodeClient 被运行时消费，不能复用）。
    fn prepare_config(&self) -> Result<SyncRuntimeConfig, FfiError> {
        let client = self.build_client()?;
        SyncRuntimeConfig::new(
            &self.database_path,
            &self.master_key_hex,
            client,
            &self.group_id,
        )
        .map_err(|error| failure(error.code().as_str(), error.to_string()))
    }

    fn current_status(&self) -> Result<SyncStatusFfi, FfiError> {
        let inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        if let Some(runtime) = inner.runtime.as_ref() {
            return Ok(status_ffi(&runtime.status()));
        }
        Ok(match inner.last.as_ref() {
            Some(handle) => status_ffi(&handle.view()),
            None => idle_status(),
        })
    }
}

fn decode_node_public_key(text: &str) -> Result<[u8; 32], FfiError> {
    let trimmed = text.trim();
    if trimmed.len() != 64 {
        return Err(invalid_input(
            "node public key must be exactly 64 hexadecimal characters",
        ));
    }
    let bytes = trimmed.as_bytes();
    let mut key = [0_u8; 32];
    for (index, slot) in key.iter_mut().enumerate() {
        let high = decode_nibble(bytes[index * 2])?;
        let low = decode_nibble(bytes[index * 2 + 1])?;
        *slot = (high << 4) | low;
    }
    Ok(key)
}

#[uniffi::export]
impl SyncFfi {
    /// 打开常驻同步对象：装配 NodeClient 并校验拉取游标，**不启动**循环。
    ///
    /// - 库必须已存在且主密钥正确（否则 storage 的冻结码）；
    /// - 身份必须已入库（否则 `INVALID_STATE_TRANSITION`）；
    /// - `node_public_key_hex` 必填（固定节点公钥，运行时不接受未固定节点）；
    /// - 该群的拉取游标未初始化 → `INVALID_STATE_TRANSITION`。
    #[uniffi::constructor]
    pub fn open(
        database_path: String,
        master_key_hex: String,
        endpoint_url: String,
        node_id: String,
        node_public_key_hex: String,
        group_id: String,
    ) -> Result<Arc<Self>, FfiError> {
        if database_path.trim().is_empty() {
            return Err(invalid_input("database path must not be empty"));
        }
        if endpoint_url.trim().is_empty() || node_id.trim().is_empty() {
            return Err(invalid_input("node endpoint and node id are required"));
        }
        if group_id.trim().is_empty() {
            return Err(invalid_input("group id is required"));
        }
        let object = Self {
            database_path,
            master_key_hex,
            endpoint_url,
            node_id,
            node_public_key_hex,
            group_id,
            inner: Mutex::new(Inner {
                runtime: None,
                last: None,
                closed: false,
            }),
        };
        // 装配即校验（身份/端点/公钥/游标），不留下半打开的对象。
        let _ = object.build_client()?;
        Ok(Arc::new(object))
    }

    /// 启动常驻同步循环（幂等失败：已在运行 → `INVALID_STATE_TRANSITION`）。
    pub fn start(&self) -> Result<SyncStatusFfi, FfiError> {
        let mut inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        if inner.runtime.is_some() {
            return Err(invalid_state("the sync runtime is already running"));
        }
        inner.last = None;
        drop(inner);
        let config = self.prepare_config()?;
        let runtime = SyncRuntime::start(config)
            .map_err(|error| failure(error.code().as_str(), error.to_string()))?;
        let status = status_ffi(&runtime.status());
        let mut inner = self.lock()?;
        inner.runtime = Some(runtime);
        Ok(status)
    }

    /// 请求停止并等待循环退出；未运行时幂等返回当前状态。
    pub fn stop(&self) -> Result<SyncStatusFfi, FfiError> {
        let runtime = {
            let mut inner = self.lock()?;
            if inner.closed {
                return Err(core_closed());
            }
            inner.runtime.take()
        };
        let Some(runtime) = runtime else {
            return self.current_status();
        };
        let handle = runtime.status_handle();
        let store = runtime
            .stop()
            .map_err(|error| failure(error.code().as_str(), error.to_string()))?;
        // 运行时把库句柄交回：直接释放，之后 start 会自己重新开库。
        drop(store);
        let view = handle.view();
        let mut inner = self.lock()?;
        inner.last = Some(handle);
        Ok(status_ffi(&view))
    }

    /// 当前状态（未启动 = idle；已停止 = 最后状态）。
    pub fn status(&self) -> Result<SyncStatusFfi, FfiError> {
        self.current_status()
    }

    /// 是否仍处于打开状态。
    /// 一次性拉取：拉一页 → 验签 → 应用 → 前进游标，返回本页结果。
    ///
    /// 单写者约定：常驻循环在运行时**拒绝**（`INVALID_STATE_TRANSITION`），调用方必须先 `stop()`。
    /// 语义：没有新条目（`applied=0` 且无跳过）时游标不前进、不写库；
    /// 网络/节点错误显式透出（`NODE_UNAVAILABLE` 等）；应用失败时游标不前进（由 core 保证）。
    pub fn sync_once(&self) -> Result<SyncOnceFfi, FfiError> {
        let inner = self.lock()?;
        if inner.closed {
            return Err(core_closed());
        }
        if inner.runtime.is_some() {
            return Err(invalid_state(
                "a sync loop is running; stop it before a one-shot pull",
            ));
        }
        let client = self.build_client()?;
        let mut store = open_store(
            StoreOptions::new(&self.database_path, KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(decode_master_key(&self.master_key_hex)?),
        )
        .map_err(|error| failure(error.code(), error.to_string()))?;
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        let (outcome, report) =
            chat_core::sync_worker::run_pull_once_reporting_at(&mut store, &client, &self.group_id, now_ms)
                .map_err(|error| failure(error.code().as_str(), error.to_string()))?;
        let skipped = report.skipped();
        let fetched = outcome.applied.saturating_add(skipped);
        // `settled` 表示“不需要上层做事”：没有待重对齐、拉取循环也没有终止。
        // `backoff_ms` 只是“下次多久再拉”的建议（已到 head 时也会给），不算未追平。
        let settled = !outcome.resync_required && outcome.stopped.is_none();
        Ok(SyncOnceFfi {
            fetched,
            applied: outcome.applied,
            skipped,
            next_cursor: outcome.cursor_after,
            settled,
        })
    }

    pub fn is_open(&self) -> bool {
        match self.inner.lock() {
            Ok(inner) => !inner.closed,
            Err(_) => false,
        }
    }

    /// 绑定的群（PROTOCOL-001 群文本）。
    pub fn group_id(&self) -> String {
        self.group_id.clone()
    }

    /// 关闭：先停循环再标记关闭；幂等，之后所有调用返回 `CORE_CLOSED`。
    pub fn shutdown(&self) {
        let runtime = match self.inner.lock() {
            Ok(mut inner) => {
                if inner.closed {
                    return;
                }
                inner.closed = true;
                inner.runtime.take()
            }
            Err(_) => return,
        };
        if let Some(runtime) = runtime {
            let handle = runtime.status_handle();
            if let Ok(store) = runtime.stop() {
                drop(store);
            }
            if let Ok(mut inner) = self.inner.lock() {
                inner.last = Some(handle);
            }
        }
    }
}

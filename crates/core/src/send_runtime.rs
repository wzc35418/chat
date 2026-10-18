//! SND：把发送编排接到**真实节点**与**持久 MLS 群状态**（CORE-007 执行侧的接线层）。
//!
//! 分层（与 docs/implementation/core.md 的实现记录一致）：
//!
//! - 本模块只做“装配 + 显式失败”：从正式加密库恢复 [MlsDevice]（CRYPTO-004/010 的
//!   `export_to_store` / `import_from_store`）、从持久身份取设备签名钥、按节点配置建
//!   [NodeClient]，再交给 [SendService]（编排/状态机/事件）与 [NodeClientSubmitter]（NODE-006）；
//! - 状态机、幂等、三态与事件都在既有层里，本模块**不重写**任何发送语义，也不做本地推断；
//! - 未配置节点、本机身份未入库、MLS 群未加入或有歧义时一律显式失败（冻结错误码），
//!   绝不伪造 Accepted。
//!
//! 标识映射（本切片的临时约定，后续由持久“会话→群”映射取代）：
//!
//! - 协议群 ID = `grp:1:sha256("chat/send/protocol-group/v1" || MLS 群键)`：发送链路可完全
//!   由持久 MLS 群状态推导，Kotlin 侧不需要额外传群参数；
//! - MLS 群键（`mls_group_key`）= MLS group id 的小写十六进制（crypto 的 `MlsDevice::group_key`）；
//!   未显式给出时只在“本库恰好一个持久 MLS 群状态”时自动绑定，0 个或多个都显式失败。
//!
//! 已知限制：MLS 群键的发现用了一条**只读**查询（`mls_group_state.group_id` 是不透明
//! 明文列，不含任何密封内容）；正式的多群列表 API 属 storage 的后续任务。加密一次后
//! 不可重发（同 SubmissionId 复用耐久密文）由编排层保证，本模块不重新加密。
use std::fmt;

use chat_crypto::engine::MlsDevice;
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_protocol::signing::signing_key_from_seed;
use chat_protocol::{GroupId, OperationId};
use chat_sync::clock::SystemClock;
use chat_sync::{ErrorCode, SyncError};
use chat_storage::formal::Store;
use sha2::{Digest, Sha256};

use crate::core_events::{EventBus, SendService, MAX_EVENT_BATCH};
use crate::node_submitter::NodeClientSubmitter;
use crate::send_orchestrator::{
    CryptoEngine, DispatchOutcome, EncryptedPayload, PrepareOutcome, ReconcileReport, SendResult,
};
use crate::service::DraftStatusView;

/// 发送链路的固定调度者标识（进入 outbox 租约语义；同一进程内稳定）。
pub const SEND_WORKER: &str = "core-send-worker";

/// 协议群 ID 派生域。
const PROTOCOL_GROUP_DOMAIN: &[u8] = b"chat/send/protocol-group/v1";
/// MLS 群键允许的最大长度（与 storage 的不透明 group_id 列一致）。
pub const MAX_MLS_GROUP_KEY_BYTES: usize = 128;
/// 本机设备密钥种子字节数。
const DEVICE_SEED_BYTES: usize = 32;

/// 节点配置：只保存，不发请求（`configure_send_node` 的落库形式）。
#[derive(Clone, PartialEq, Eq)]
pub struct SendNodeConfig {
    /// 节点 HTTP(S) 基地址（例如 `http://127.0.0.1:8080`）。
    pub endpoint_url: String,
    /// 节点标识（建端点时校验）。
    pub node_id: String,
    /// 可选的固定节点签名公钥（32 字节）；给出时客户端会校验节点回执签名。
    pub node_public_key: Option<[u8; 32]>,
}

impl fmt::Debug for SendNodeConfig {
    /// 只暴露地址与节点标识（公钥只报长度）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SendNodeConfig")
            .field("endpoint_url", &self.endpoint_url)
            .field("node_id", &self.node_id)
            .field(
                "node_public_key_len",
                &self.node_public_key.map(|_| DEVICE_SEED_BYTES),
            )
            .finish()
    }
}

impl SendNodeConfig {
    /// 校验并构造配置（不联网）：端点必须是可解析的 http(s) 地址，节点标识非空，
    /// 公钥给出时必须是 64 位十六进制。
    pub fn new(
        endpoint_url: impl Into<String>,
        node_id: impl Into<String>,
        node_public_key_hex: Option<&str>,
    ) -> SendResult<Self> {
        let endpoint_url = endpoint_url.into();
        let node_id = node_id.into();
        if node_id.trim().is_empty() || node_id.len() > 128 {
            return Err(SyncError::new(
                ErrorCode::InvalidTarget,
                "node id must be a non-empty bounded identifier",
            ));
        }
        NodeEndpoint::new(&endpoint_url, &node_id).map_err(|_| {
            SyncError::new(
                ErrorCode::InvalidTarget,
                "node endpoint must be a parseable http(s) url",
            )
        })?;
        let node_public_key = match node_public_key_hex {
            Some(text) => Some(decode_hex_32(text).ok_or_else(|| {
                SyncError::new(
                    ErrorCode::InvalidSignature,
                    "node public key must be 64 hexadecimal characters",
                )
            })?),
            None => None,
        };
        Ok(Self {
            endpoint_url,
            node_id,
            node_public_key,
        })
    }
}

/// 解析 64 位十六进制为 32 字节（失败即可，不做部分接受）。
fn decode_hex_32(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut output = [0u8; 32];
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    for (index, pair) in pairs.iter().enumerate() {
        let high = decode_nibble(pair[0])?;
        let low = decode_nibble(pair[1])?;
        output[index] = (high << 4) | low;
    }
    Some(output)
}

fn decode_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 协议群 ID：由 MLS 群键确定性派生（PROTOCOL-001 `grp:1:<hex>` 文本）。
pub fn protocol_group_id_for(mls_group_key: &str) -> SendResult<String> {
    if mls_group_key.is_empty() || mls_group_key.len() > MAX_MLS_GROUP_KEY_BYTES {
        return Err(SyncError::new(
            ErrorCode::InvalidTarget,
            "mls group key must be a bounded identifier",
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(PROTOCOL_GROUP_DOMAIN);
    hasher.update(mls_group_key.as_bytes());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(hasher.finalize().as_slice());
    Ok(GroupId::from_bytes(bytes).encode())
}

/// 发送链路单步类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendStep {
    /// 准备一次尝试（必要时恰好加密一次）。
    Prepare,
    /// 提交一次尝试（只走三态）。
    Dispatch,
    /// 重建被拒绝的尝试（必须由新的 prepare 产生新 SubmissionId）。
    Retry,
    /// 按 SubmissionId 对账（结果未知时唯一合法动作）。
    Reconcile,
}

impl SendStep {
    /// 稳定文本（不含正文/密钥）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Dispatch => "dispatch",
            Self::Retry => "retry",
            Self::Reconcile => "reconcile",
        }
    }
}

/// 单步结果视图：状态 + SubmissionId + 尝试序号（FFI 直接投影，不含正文）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendStepView {
    /// 草稿 ID。
    pub draft_id: String,
    /// 有效状态：queued / prepared / submitted / accepted / rejected / unknown / cancelled / failed。
    pub status: String,
    /// 最新尝试的 SubmissionId（尚无尝试时为空）。
    pub submission_id: Option<String>,
    /// 最新尝试的重建序号（0 表示首次尝试）。
    pub attempt_index: Option<u32>,
    /// 静态说明（不含正文/密钥/URL）。
    pub detail: String,
    /// 该结果是否已确定（`false` = 仍需对账，禁止重发）。
    pub settled: bool,
}

/// 恢复出来的 MLS 加密引擎：真实 OpenMLS 设备 + 持久群状态。
pub struct MlsDeviceEngine {
    device: MlsDevice,
    group_key: String,
}

impl fmt::Debug for MlsDeviceEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MlsDeviceEngine")
            .field("group_key", &self.group_key)
            .finish_non_exhaustive()
    }
}

impl MlsDeviceEngine {
    /// 从正式加密库恢复：群状态不存在时显式 `INVALID_STATE_TRANSITION`，
    /// 存在但无法重建/自检不一致时显式 `STATE_CORRUPTED`（绝不退化成空群）。
    pub fn restore(store: &Store, mls_group_key: &str) -> anyhow::Result<Self> {
        if mls_group_key.is_empty() || mls_group_key.len() > MAX_MLS_GROUP_KEY_BYTES {
            return Err(anyhow::Error::new(SyncError::new(
                ErrorCode::InvalidTarget,
                "mls group key must be a bounded identifier",
            )));
        }
        let persisted = store
            .load_mls_group_state(mls_group_key)
            .map_err(anyhow::Error::new)?;
        if persisted.is_none() {
            return Err(anyhow::Error::new(SyncError::new(
                ErrorCode::InvalidStateTransition,
                "mls group is not joined in this database",
            )));
        }
        let device = MlsDevice::import_from_store(store, mls_group_key).map_err(|_| {
            anyhow::Error::new(SyncError::new(
                ErrorCode::StateCorrupted,
                "persisted mls group state could not be restored",
            ))
        })?;
        let restored_key = device.group_key().map_err(|_| {
            anyhow::Error::new(SyncError::new(
                ErrorCode::StateCorrupted,
                "restored mls device has no group",
            ))
        })?;
        if restored_key != mls_group_key {
            return Err(anyhow::Error::new(SyncError::new(
                ErrorCode::StateCorrupted,
                "restored mls group does not match the requested group",
            )));
        }
        Ok(Self {
            device,
            group_key: mls_group_key.to_owned(),
        })
    }

    /// 绑定的 MLS 群键。
    pub fn group_key(&self) -> &str {
        &self.group_key
    }
}

impl CryptoEngine for MlsDeviceEngine {
    fn epoch(&self, _conversation_id: &str) -> SendResult<u64> {
        self.device.epoch().map_err(|_| {
            SyncError::new(
                ErrorCode::StateCorrupted,
                "mls group epoch is unavailable",
            )
        })
    }

    fn encrypt(
        &mut self,
        _conversation_id: &str,
        _draft_id: &str,
        body: &str,
    ) -> SendResult<EncryptedPayload> {
        // 先用当前 epoch，再加密；MLS 加密本身不推进 epoch，因此两者一致。
        let mls_epoch = self.epoch("")?;
        let ciphertext = self.device.encrypt(body).map_err(|_| {
            SyncError::new(ErrorCode::StateCorrupted, "mls encryption failed")
        })?;
        Ok(EncryptedPayload {
            mls_epoch,
            ciphertext,
        })
    }
}

/// 打开运行时失败的显式结果：`store` 只在“还没被 [SendService] 消费”时返回，
/// 便于调用方把独占写句柄放回去（消费后句柄已随服务释放）。
pub struct SendOpenError {
    /// 失败原因（保留冻结码：storage/身份/同步错误都在里面）。
    pub error: anyhow::Error,
    /// 未被消费的 Store 句柄（`None` = 已被服务消费）。
    pub store: Option<Store>,
}

impl fmt::Debug for SendOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SendOpenError")
            .field("message", &self.error.to_string())
            .field("store_returned", &self.store.is_some())
            .finish()
    }
}

impl fmt::Display for SendOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.error)
    }
}

impl std::error::Error for SendOpenError {}

/// 装配完成的端口（先于消费 Store 的所有可失败步骤）。
struct PreparedPorts {
    engine: MlsDeviceEngine,
    submitter: NodeClientSubmitter,
    group_key: String,
    group_id: String,
    device_id: String,
}

/// 发送运行时：真实节点 + 持久 MLS 群状态上的 [SendService]。
pub struct SendRuntime {
    service: SendService<NodeClientSubmitter, MlsDeviceEngine, SystemClock>,
    group_key: String,
    group_id: String,
    device_id: String,
}

impl fmt::Debug for SendRuntime {
    /// 只暴露群/设备标识与计数级信息；引擎、客户端与密钥都不进日志。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SendRuntime")
            .field("group_key", &self.group_key)
            .field("group_id", &self.group_id)
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

impl SendRuntime {
    /// 用持久库 + 节点配置装配运行时。
    ///
    /// 可失败步骤（身份种子、MLS 群恢复、节点客户端、提交端口）都在**借用** Store 的阶段
    /// 完成，失败时把句柄原样还给调用方；只有最后一步 [SendService::new] 会消费句柄。
    pub fn open(
        store: Store,
        config: &SendNodeConfig,
        mls_group_key: Option<&str>,
    ) -> Result<Self, SendOpenError> {
        let prepared = match prepare_ports(&store, config, mls_group_key) {
            Ok(prepared) => prepared,
            Err(error) => {
                return Err(SendOpenError {
                    error,
                    store: Some(store),
                });
            }
        };
        let PreparedPorts {
            engine,
            submitter,
            group_key,
            group_id,
            device_id,
        } = prepared;
        match SendService::new(store, submitter, engine, SystemClock, SEND_WORKER) {
            Ok(service) => Ok(Self {
                service,
                group_key,
                group_id,
                device_id,
            }),
            // 句柄已被消费：这是本地不变式失败，调用方会看到显式错误而不是假成功。
            Err(error) => Err(SendOpenError { error, store: None }),
        }
    }

    /// 绑定的 MLS 群键。
    pub fn group_key(&self) -> &str {
        &self.group_key
    }

    /// 派生的协议群 ID（PROTOCOL-001 文本）。
    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    /// 绑定的设备 ID（来自持久身份）。
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// 事件总线（只读）。
    pub fn events(&self) -> &EventBus {
        self.service.events()
    }

    /// 按序号读取事件（只读、幂等）。
    pub fn poll_events(&self, since_seq: u64, limit: u32) -> anyhow::Result<crate::core_events::EventBatch> {
        self.service.poll_events(since_seq, limit)
    }

    /// 当前草稿真实状态（无正文）。
    pub fn get_draft_status(&self, draft_id: &str) -> anyhow::Result<Option<DraftStatusView>> {
        self.service.get_draft_status(draft_id)
    }

    /// CORE-007 begin：写发送意图并返回 `draft_id`。
    pub fn begin_send(
        &mut self,
        conversation_id: &str,
        operation_id: &str,
        body: &str,
    ) -> anyhow::Result<String> {
        OperationId::parse(operation_id).map_err(|_| {
            anyhow::Error::new(SyncError::new(
                ErrorCode::InvalidId,
                "operation id must be PROTOCOL-001 operation text",
            ))
        })?;
        self.service.begin_send(conversation_id, operation_id, body)
    }

    /// CORE-007 prepare：必要时恰好加密一次。
    pub fn prepare(&mut self, draft_id: &str) -> anyhow::Result<PrepareOutcome> {
        self.service.prepare(draft_id)
    }

    /// CORE-007 dispatch：只走三态（绝不把未知当成功）。
    pub fn dispatch(&mut self, draft_id: &str) -> anyhow::Result<DispatchOutcome> {
        self.service.dispatch(draft_id)
    }

    /// CORE-008 retry：拒绝后重建（新 SubmissionId）。
    pub fn retry_draft(&mut self, draft_id: &str) -> anyhow::Result<PrepareOutcome> {
        self.service.retry_draft(draft_id)
    }

    /// CORE-008 reconcile：按 SubmissionId 对账。
    pub fn reconcile(&mut self, draft_id: &str) -> anyhow::Result<ReconcileReport> {
        self.service.reconcile(draft_id)
    }

    /// 单步执行 + 状态投影（FFI 的 send_prepare/dispatch/retry/reconcile 共用）。
    pub fn step(&mut self, draft_id: &str, step: SendStep) -> anyhow::Result<SendStepView> {
        let (detail, settled) = match step {
            SendStep::Prepare => describe_prepare(self.service.prepare(draft_id)?, false),
            SendStep::Retry => describe_prepare(self.service.retry_draft(draft_id)?, true),
            SendStep::Dispatch => describe_dispatch(self.service.dispatch(draft_id)?),
            SendStep::Reconcile => describe_reconcile(self.service.reconcile(draft_id)?),
        };
        let status = self
            .service
            .get_draft_status(draft_id)?
            .ok_or_else(|| {
                anyhow::Error::new(SyncError::new(
                    ErrorCode::InvalidTarget,
                    "draft disappeared while the send step was running",
                ))
            })?;
        Ok(SendStepView {
            draft_id: status.draft_id,
            status: status.status,
            submission_id: status.submission_id,
            attempt_index: status.attempt_index,
            detail,
            settled,
        })
    }

    /// 取回 Store 句柄（成功后由调用方重新持有）。
    pub fn into_store(self) -> Option<Store> {
        self.service.into_store()
    }
}

/// 装配端口：全部可失败步骤都在消费 Store 之前完成。
fn prepare_ports(
    store: &Store,
    config: &SendNodeConfig,
    mls_group_key: Option<&str>,
) -> anyhow::Result<PreparedPorts> {
    let seeds = store
        .load_identity_seeds()
        .map_err(anyhow::Error::new)?
        .ok_or_else(|| {
            anyhow::Error::new(SyncError::new(
                ErrorCode::InvalidStateTransition,
                "the local identity is not enrolled in this database",
            ))
        })?;
    let device_seed = device_seed_bytes(&seeds.device_seed)?;
    // 设备签名钥与持久身份同源（identity 用同一派生函数），因此节点看到的设备 ID 一致。
    let signing_key = signing_key_from_seed(&device_seed);
    let group_key = match mls_group_key {
        Some(key) => key.to_owned(),
        None => discover_single_group_key(store)?,
    };
    let group_id = protocol_group_id_for(&group_key)?;
    let engine = MlsDeviceEngine::restore(store, &group_key)?;
    let mut endpoint = NodeEndpoint::new(&config.endpoint_url, &config.node_id).map_err(|_| {
        anyhow::Error::new(SyncError::new(
            ErrorCode::InvalidTarget,
            "node endpoint must be a parseable http(s) url",
        ))
    })?;
    if let Some(public_key) = config.node_public_key {
        endpoint = endpoint.with_node_public_key(public_key);
    }
    let client = NodeClient::new(endpoint, seeds.device_id.clone(), signing_key).map_err(|_| {
        anyhow::Error::new(SyncError::new(
            ErrorCode::InvalidTarget,
            "node client could not be created from the persisted device identity",
        ))
    })?;
    let submitter = NodeClientSubmitter::new(client, group_id.clone()).map_err(anyhow::Error::new)?;
    Ok(PreparedPorts {
        engine,
        submitter,
        group_key,
        group_id,
        device_id: seeds.device_id.clone(),
    })
}

/// 取设备种子（必须正好 32 字节；长度不对说明身份记录已损坏）。
fn device_seed_bytes(seed: &[u8]) -> anyhow::Result<[u8; DEVICE_SEED_BYTES]> {
    let bytes: [u8; DEVICE_SEED_BYTES] = seed.try_into().map_err(|_| {
        anyhow::Error::new(SyncError::new(
            ErrorCode::StateCorrupted,
            "persisted device seed has an unexpected length",
        ))
    })?;
    Ok(bytes)
}

/// 自动绑定：本库恰好一个持久 MLS 群状态时返回其群键。
///
/// 只读 `mls_group_state.group_id`（不透明明文列）；0 个 = 未加入群，多个 = 歧义，
/// 两种情况都显式失败而不是随便挑一个。
fn discover_single_group_key(store: &Store) -> anyhow::Result<String> {
    let mut statement = store
        .connection()
        .prepare("SELECT group_id FROM mls_group_state ORDER BY group_id LIMIT 2")
        .map_err(|_| {
            anyhow::Error::new(SyncError::new(
                ErrorCode::StorageFailure,
                "mls group state table could not be read",
            ))
        })?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| {
            anyhow::Error::new(SyncError::new(
                ErrorCode::StorageFailure,
                "mls group state table could not be read",
            ))
        })?;
    let mut keys = Vec::new();
    for row in rows {
        keys.push(row.map_err(|_| {
            anyhow::Error::new(SyncError::new(
                ErrorCode::StorageFailure,
                "mls group state row could not be read",
            ))
        })?);
    }
    match keys.len() {
        1 => Ok(keys.remove(0)),
        0 => Err(anyhow::Error::new(SyncError::new(
            ErrorCode::InvalidStateTransition,
            "no mls group is joined in this database",
        ))),
        _ => Err(anyhow::Error::new(SyncError::new(
            ErrorCode::InvalidTarget,
            "several mls groups are joined; the send group must be selected explicitly",
        ))),
    }
}

/// prepare/retry 的静态说明。
fn describe_prepare(outcome: PrepareOutcome, retry: bool) -> (String, bool) {
    let prefix = if retry { "retry: " } else { "" };
    match outcome {
        PrepareOutcome::Prepared {
            attempt_index,
            already_prepared,
            ..
        } => {
            let detail = if already_prepared {
                format!("{prefix}an attempt is already prepared (attempt {attempt_index})")
            } else {
                format!("{prefix}prepared attempt {attempt_index}")
            };
            (detail, true)
        }
        PrepareOutcome::AwaitingReconciliation { .. } => (
            format!("{prefix}node result is still unknown; reconcile before resending"),
            false,
        ),
        PrepareOutcome::WaitingForLease { lease_until_ms, .. } => (
            format!("{prefix}another worker holds the send lease until {lease_until_ms}"),
            false,
        ),
        PrepareOutcome::AlreadyAccepted { .. } => {
            (format!("{prefix}the message is already accepted"), true)
        }
        PrepareOutcome::Abandoned { .. } => (format!("{prefix}the draft is cancelled"), true),
    }
}

/// dispatch 的静态说明。
fn describe_dispatch(outcome: DispatchOutcome) -> (String, bool) {
    match outcome {
        DispatchOutcome::Accepted { idempotent, .. } => {
            let detail = if idempotent {
                "node accepted the message earlier (idempotent replay)".to_owned()
            } else {
                "node accepted the message".to_owned()
            };
            (detail, true)
        }
        DispatchOutcome::Unknown { .. } => (
            "node result is unknown; reconcile by submission id before resending".to_owned(),
            false,
        ),
        DispatchOutcome::Rejected { .. } => (
            "node rejected the attempt; retry rebuilds it with a new submission id".to_owned(),
            true,
        ),
        DispatchOutcome::WaitingForLease { until_ms, .. } => (
            format!("the send job is not claimable before {until_ms}"),
            false,
        ),
        DispatchOutcome::Abandoned { .. } => ("the draft is cancelled".to_owned(), true),
    }
}

/// reconcile 的静态说明。
fn describe_reconcile(report: ReconcileReport) -> (String, bool) {
    if report.awaiting_reconciliation {
        (
            "reconcile: the node result is still unknown; do not resend".to_owned(),
            false,
        )
    } else if report.draft_repaired {
        ("reconcile: the draft was repaired to the persisted attempt state".to_owned(), true)
    } else {
        ("reconcile: local state is consistent".to_owned(), true)
    }
}

/// 事件批上限（与内核事件总线一致），供调用方一次性取完本次产生的事件。
pub const SEND_EVENT_BATCH: u32 = MAX_EVENT_BATCH;

//! CRYPTO-005~009：群成员控制提交、对端条目处理、pending 合并/丢弃与 Welcome 加入。
//!
//! 语义要点（与 docs/implementation/crypto.md、TRANSACTIONS.md 的 T2/T3 对齐）：
//!
//! - **prepare_control 只“准备”**：OpenMLS 进入 PendingCommit，本地 epoch 不前进；
//!   返回的 [PendingControl] 绑定 group_id、base_epoch、target_epoch 与提交摘要，
//!   pending 分支随群状态快照持久化到正式加密库（T2：pending 与正式状态分开）。
//! - **合并需要接受证据**：只有 [ControlAcceptance]（绑定同一 group/epoch/摘要）才允许
//!   [MlsDevice::merge_accepted_pending] 合并；合并成功才推进 epoch 并清除 pending。
//! - **失败不推进 epoch**：证据不符/无 pending 时在合并前拒绝，epoch 不变；合并本身或
//!   控制结果校验失败时让设备 fail-closed（丢弃内存群句柄），调用方必须从已提交的
//!   正式存储重新 import，绝不静默继续。
//! - **discard 需要拒绝证据**：只有 [ControlRejection] 匹配时才回滚 pending，并把回滚后的
//!   已提交快照写回正式库；旧 epoch 的密钥状态不变，可以继续加解密。
//! - **process_verified_entry 先校验后解密**：群、epoch、外层声称的作者身份逐项核对；
//!   epoch 落后 -> STALE_EPOCH，超前 -> MISSING_LOG_ENTRIES；对端 Commit 在校验预期
//!   成员变化后才合并，不一致时停止该群分支。
//! - **join_from_welcome**：用 Welcome 建群、核对群绑定（group_id/epoch/成员凭据）后
//!   立即写正式库；拒绝时不留下半加入的群状态。
//!
//! 已知限制（P0 切片）：不做账户/设备级授权（DOMAIN-004/IDENTITY 未接入），不做外层
//! 日志与候选签名（T3 由 storage 的 STORAGE-008 负责），不做 pending Commit 与并发
//! 胜出 Commit 的分叉合并（竞争时要求先显式 discard/merge）。

use chat_storage::formal::{StorageError, Store as FormalStore};
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use sha2::{Digest, Sha256};

use crate::engine::{MlsDevice, MlsExportSummary};

/// 单次 add 操作的成员数上限（P0 有界保护；正式群上限由 domain 策略决定）。
pub const MAX_ADDED_MEMBERS: usize = 32;
/// 单个序列化控制提交的上限（字节）。
pub const MAX_COMMIT_BYTES: usize = chat_protocol::wire::MAX_FRAME_BYTES;
/// 单个序列化 Welcome 的上限（字节）。
pub const MAX_WELCOME_BYTES: usize = 256 * 1024;
/// pending 元数据编码版本。
const PENDING_FORMAT_VERSION: u8 = 1;
/// pending 提交摘要的域分隔标签。
const PENDING_DIGEST_DOMAIN: &[u8] = b"chat/crypto/pending-control/v1";

/// 控制提交种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlOperationKind {
    /// 新增成员（产生 Commit + Welcome）。
    AddMembers,
    /// 移除成员（产生 Commit）。
    RemoveMembers,
    /// 更新本机 leaf（产生 Commit）。
    SelfUpdate,
}

impl ControlOperationKind {
    /// 稳定编码（进入 pending 元数据，不可改）。
    pub const fn code(self) -> &'static str {
        match self {
            Self::AddMembers => "add_members",
            Self::RemoveMembers => "remove_members",
            Self::SelfUpdate => "self_update",
        }
    }

    fn wire_tag(self) -> u8 {
        match self {
            Self::AddMembers => 0,
            Self::RemoveMembers => 1,
            Self::SelfUpdate => 2,
        }
    }

    fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::AddMembers),
            1 => Some(Self::RemoveMembers),
            2 => Some(Self::SelfUpdate),
            _ => None,
        }
    }
}

/// 请求准备的控制操作。输入是“已验证的业务操作”的 MLS 投影：
/// KeyPackage 会在这里再做一次 MLS 验签与成员去重，Remove 目标按凭据身份定位。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlOperation {
    /// 新增成员；每个元素是一个序列化的 KeyPackage。
    AddMembers {
        /// 序列化的 KeyPackage 列表。
        key_packages: Vec<Vec<u8>>,
    },
    /// 移除成员；元素是已验证的成员凭据身份（BasicCredential 内容）。
    RemoveMembers {
        /// 要移除的成员凭据身份。
        members: Vec<String>,
    },
    /// 更新本机 leaf（密钥轮换）。
    SelfUpdate,
    /// 本机自移除（主动退出群）：只移除本机设备，生成真实的 RemoveMembers 提交。
    ///
    /// 这是比普通 `RemoveMembers` 更窄的能力：不接受任何成员列表，只能作用于本机；
    /// 普通 `RemoveMembers` 仍显式拒绝移除本机（安全边界不变）。
    SelfRemoval,
}

/// 已准备但未被接受的控制提交。只含可发送的密文材料与公开绑定信息，不含密钥。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingControl {
    /// MLS 群 ID（小写十六进制）。
    pub group_id: String,
    /// 创建该提交时的本地 epoch。
    pub base_epoch: u64,
    /// 该提交被接受并合并后应到达的 epoch（恒为 base_epoch + 1）。
    pub target_epoch: u64,
    /// 操作种类。
    pub operation: ControlOperationKind,
    /// 预期新增成员数。
    pub added: u32,
    /// 预期移除成员数。
    pub removed: u32,
    /// 序列化的 MLS Commit。
    pub commit: Vec<u8>,
    /// 序列化的 MLS Welcome（Add 操作才有）。
    pub welcome: Option<Vec<u8>>,
    /// 提交摘要（绑定 group/epochs/Commit 字节）。
    pub commit_digest: [u8; 32],
}

impl PendingControl {
    /// 构造“已接受”证据（由节点/协调者回执转成；这里只做绑定）。
    pub fn acceptance(&self) -> ControlAcceptance {
        ControlAcceptance {
            group_id: self.group_id.clone(),
            base_epoch: self.base_epoch,
            target_epoch: self.target_epoch,
            commit_digest: self.commit_digest,
        }
    }

    /// 构造“明确拒绝”证据（绑定同一提交与 epoch）。
    pub fn rejection(&self, reason: &'static str) -> ControlRejection {
        ControlRejection {
            group_id: self.group_id.clone(),
            base_epoch: self.base_epoch,
            commit_digest: self.commit_digest,
            reason,
        }
    }

    /// 本机当前 epoch 是否仍停在 pending 的 base_epoch。
    pub fn binds_epoch(&self, epoch: u64) -> bool {
        self.base_epoch == epoch
    }
}


/// 本机自移除提案（MLS 里唯一合法的"主动退出"原语）。
///
/// MLS 1.0 禁止成员提交移除自己的 Commit（OpenMLS 直接拒绝），自移除必须：
/// 1. 本机生成"移除本机 leaf"的提案（本结构返回的 `proposal`，密文形式的 MLS 提案消息）；
/// 2. 由群里另一名成员把该提案包进 Commit；
/// 3. 本机处理那条 Commit（`process_verified_entry` 返回 `self_removed = true`）后
///    才真正离群，此后不能加解密。
///
/// 本结构只暴露可转发的提案字节与绑定信息，不含任何密钥；epoch 不因它推进。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSelfRemoval {
    /// MLS 群 ID（小写十六进制）。
    pub group_id: String,
    /// 生成提案时的本地 epoch（提案必须在该 epoch 被提交覆盖）。
    pub base_epoch: u64,
    /// 预期移除的成员数（恒为 1：只有本机）。
    pub removed: u32,
    /// 是否携带 Welcome（恒为 false：自移除不移入任何成员）。
    pub has_welcome: bool,
    /// 序列化的 MLS 提案消息（交给另一成员提交）。
    pub proposal: Vec<u8>,
    /// 提案摘要（绑定 group/base_epoch/提案字节，便于对端核对）。
    pub proposal_digest: [u8; 32],
}

/// 自移除提案摘要的域分隔标签。
const SELF_REMOVAL_DIGEST_DOMAIN: &[u8] = b"chat/crypto/self-removal-proposal/v1";

fn self_removal_digest(group_id: &str, base_epoch: u64, proposal: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(SELF_REMOVAL_DIGEST_DOMAIN);
    hasher.update(group_id.as_bytes());
    hasher.update(base_epoch.to_be_bytes());
    hasher.update(proposal);
    let digest = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(digest.as_slice());
    bytes
}

/// 节点/协调者已接受该控制提交的证据。只含公开绑定字段，不含密钥。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlAcceptance {
    /// MLS 群 ID（小写十六进制）。
    pub group_id: String,
    /// 提交基于的 epoch。
    pub base_epoch: u64,
    /// 接受后应到达的 epoch。
    pub target_epoch: u64,
    /// 提交摘要。
    pub commit_digest: [u8; 32],
}

/// 明确拒绝该控制提交的证据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlRejection {
    /// MLS 群 ID（小写十六进制）。
    pub group_id: String,
    /// 提交基于的 epoch。
    pub base_epoch: u64,
    /// 提交摘要。
    pub commit_digest: [u8; 32],
    /// 拒绝原因（稳定短语，只用于诊断与状态展示）。
    pub reason: &'static str,
}

/// 合并成功的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergedControl {
    /// MLS 群 ID。
    pub group_id: String,
    /// 合并前的 epoch。
    pub old_epoch: u64,
    /// 合并后的 epoch。
    pub new_epoch: u64,
    /// 操作种类。
    pub operation: ControlOperationKind,
    /// 合并后写入正式库的快照摘要。
    pub exported: MlsExportSummary,
}

/// 丢弃 pending 成功的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscardedControl {
    /// MLS 群 ID。
    pub group_id: String,
    /// 回滚后仍停留的 epoch（等于 pending 的 base_epoch）。
    pub epoch: u64,
    /// 被丢弃的操作种类。
    pub operation: ControlOperationKind,
    /// 拒绝原因。
    pub reason: &'static str,
    /// 回滚后写入正式库的快照摘要。
    pub exported: MlsExportSummary,
}

/// 对端 Commit 的预期成员变化（来自已验证的外层控制元数据）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitExpectation {
    /// 预期新增成员数。
    pub added: u32,
    /// 预期移除成员数。
    pub removed: u32,
    /// 预期更新（leaf 变更）数。
    pub updated: u32,
}

/// 已通过外层验证的群条目（CRYPTO-006 输入）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedGroupEntry {
    /// 该条目所属 MLS 群 ID（小写十六进制，已由外层日志绑定）。
    pub group_id: String,
    /// 该条目声明的 epoch（外层日志序号绑定；必须与 MLS 消息内 epoch 一致）。
    pub epoch: u64,
    /// 外层已验证的作者凭据身份。
    pub sender_identity: String,
    /// 对端 Commit 的预期成员变化（应用消息为 None）。
    pub expectation: Option<CommitExpectation>,
    /// MLS 消息字节（应用消息或 Commit）。
    pub payload: Vec<u8>,
}

/// 处理对端条目的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessedEntry {
    /// 应用消息已认证解密。
    Application {
        /// 已认证的作者身份。
        sender_identity: String,
        /// 消息所属 epoch。
        epoch: u64,
        /// 明文（只交给可信核心路径，不进入普通 DTO/日志）。
        plaintext: Vec<u8>,
    },
    /// 独立提案已暂存（等待后续 Commit 覆盖）。
    Proposal {
        /// 已认证的作者身份。
        sender_identity: String,
        /// 消息所属 epoch。
        epoch: u64,
    },
    /// 对端 Commit 已校验并合并。
    Commit {
        /// 已认证的作者身份。
        sender_identity: String,
        /// 合并前 epoch。
        old_epoch: u64,
        /// 合并后 epoch。
        new_epoch: u64,
        /// 实际新增成员数。
        added: u32,
        /// 实际移除成员数。
        removed: u32,
        /// 实际更新数。
        updated: u32,
        /// 该 Commit 是否移除了本设备。
        self_removed: bool,
    },
    /// 收到的 Commit 是本机 pending 提交的回显：应改用 merge_accepted_pending。
    OwnPendingCommit {
        /// 该提交所属 epoch。
        epoch: u64,
    },
}


/// 独立提案的种类（P0 控制切片只支持 Remove/自移除提案）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalKind {
    /// 移除成员（含"本机自移除"提案：目标就是本机）。
    Remove,
}

impl ProposalKind {
    /// 稳定编码（进入 DTO/事件文本）。
    pub const fn code(self) -> &'static str {
        match self {
            Self::Remove => "remove",
        }
    }
}

/// 处理对端独立提案的结果（epoch 不变；提案进入本地 proposal store，等待 Commit 覆盖）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalOutcome {
    /// 提案种类。
    pub kind: ProposalKind,
    /// 已认证的提案作者身份。
    pub by: String,
    /// 提案所属 epoch。
    pub epoch: u64,
    /// 提案的目标成员身份（Remove 提案的目标；自移除提案的目标就是作者本机）。
    pub targets: Vec<String>,
    /// 处理后本地 proposal store 里待提交的提案数。
    pub proposals_pending: u32,
}

/// Welcome 加入的预期绑定（由已验证的邀请/控制日志提供）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WelcomeAcceptance {
    /// 期望的 MLS 群 ID（小写十六进制）。
    pub group_id: String,
    /// 期望加入的 epoch。
    pub expected_epoch: u64,
    /// 期望的成员凭据身份集合（顺序无关）。
    pub members: Vec<String>,
}

/// Welcome 加入成功的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinedGroup {
    /// MLS 群 ID。
    pub group_id: String,
    /// 加入后的 epoch。
    pub epoch: u64,
    /// 成员凭据身份（顺序与 MLS 树一致）。
    pub members: Vec<String>,
    /// 加入后写入正式库的快照摘要。
    pub exported: MlsExportSummary,
}

/// 控制层稳定错误码。安全校验失败必须显式失败，绝不继续推进。
#[derive(Debug)]
pub enum ControlError {
    /// 设备尚未加入群（或已被 fail-closed）。
    GroupNotInitialized,
    /// 设备已经加入过群。
    GroupAlreadyInitialized,
    /// 本机已经有未决控制提交。
    PendingCommitExists {
        /// 未决提交基于的 epoch。
        base_epoch: u64,
    },
    /// 没有未决控制提交可供合并/丢弃。
    NoPendingCommit,
    /// pending 与证据/本地状态不一致。
    PendingMismatch(&'static str),
    /// epoch 落后（旧状态/重复条目）。
    StaleEpoch {
        /// 本地当前 epoch。
        expected: u64,
        /// 条目声称的 epoch。
        found: u64,
    },
    /// epoch 超前（缺少中间条目）。
    MissingLogEntries {
        /// 本地当前 epoch。
        expected: u64,
        /// 条目声称的 epoch。
        found: u64,
    },
    /// KeyPackage 不合法/重复。
    InvalidKeyPackage(&'static str),
    /// 重复的独立提案（同一提案引用已在 proposal store 里）。
    DuplicateProposal,
    /// 本地没有待提交的提案，无法打包成 Commit。
    NoPendingProposals,
    /// 成员不在群里或身份不唯一。
    UnknownMember(&'static str),
    /// 控制操作本身不合法。
    InvalidControlOperation(&'static str),
    /// 条目的群/epoch/作者/密文不合法。
    InvalidCiphertext(&'static str),
    /// 控制结果与已验证的业务预期不一致。
    InvalidControlResult(&'static str),
    /// Welcome 未被授权或与已验证绑定不一致。
    WelcomeNotAuthorized(&'static str),
    /// 凭据缺失/绑定不一致。
    InvalidCredential(&'static str),
    /// 已提交状态无法证明一致。
    StateCorrupted(&'static str),
    /// 该条目种类不在 P0 控制切片内。
    UnsupportedEntry(&'static str),
    /// 自移除不能用 `prepare_control` 生成提交（MLS 禁止成员提交移除自己），
    /// 必须走 `prepare_self_removal` 生成 SelfRemove/Remove 提案，由另一成员提交。
    SelfRemovalRequiresProposal,
    /// 正式存储写入失败。
    Storage(StorageError),
    /// 合并已发生，但提交后的快照写库失败（epoch 已推进，需要重试导出）。
    PersistAfterMerge(StorageError),
    /// pending 已回滚，但回滚后的快照写库失败（库内仍是 pending 快照，需要重试）。
    PersistAfterDiscard(StorageError),
    /// OpenMLS 或内部加密失败。
    Crypto(String),
}

impl ControlError {
    /// 稳定错误码字符串。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::GroupNotInitialized => "CRYPTO_FAILURE",
            Self::GroupAlreadyInitialized => "INVALID_STATE_TRANSITION",
            Self::PendingCommitExists { .. } => "INVALID_STATE_TRANSITION",
            Self::NoPendingCommit => "PENDING_MISMATCH",
            Self::PendingMismatch(_) => "PENDING_MISMATCH",
            Self::StaleEpoch { .. } => "STALE_EPOCH",
            Self::MissingLogEntries { .. } => "MISSING_LOG_ENTRIES",
            Self::InvalidKeyPackage(_) => "INVALID_KEY_PACKAGE",
            Self::DuplicateProposal => "DUPLICATE_PROPOSAL",
            Self::NoPendingProposals => "NO_PENDING_PROPOSALS",
            Self::UnknownMember(_) => "INVALID_STATE_TRANSITION",
            Self::InvalidControlOperation(_) => "INVALID_STATE_TRANSITION",
            Self::InvalidCiphertext(_) => "INVALID_CIPHERTEXT",
            Self::InvalidControlResult(_) => "INVALID_CONTROL_RESULT",
            Self::WelcomeNotAuthorized(_) => "WELCOME_NOT_AUTHORIZED",
            Self::InvalidCredential(_) => "INVALID_CREDENTIAL",
            Self::StateCorrupted(_) => "STATE_CORRUPTED",
            Self::UnsupportedEntry(_) => "UNSUPPORTED_ENTRY",
            Self::SelfRemovalRequiresProposal => "SELF_REMOVAL_REQUIRES_PROPOSAL",
            Self::Storage(error) | Self::PersistAfterMerge(error) | Self::PersistAfterDiscard(error) => {
                error.code()
            }
            Self::Crypto(_) => "CRYPTO_FAILURE",
        }
    }
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GroupNotInitialized => formatter.write_str("MLS group is not initialized"),
            Self::GroupAlreadyInitialized => formatter.write_str("MLS group is already initialized"),
            Self::PendingCommitExists { base_epoch } => {
                write!(formatter, "a control commit is already pending at epoch {base_epoch}")
            }
            Self::NoPendingCommit => formatter.write_str("there is no pending control commit"),
            Self::PendingMismatch(detail) => write!(formatter, "pending control mismatch: {detail}"),
            Self::StaleEpoch { expected, found } => write!(
                formatter,
                "entry epoch {found} is behind the local epoch {expected}"
            ),
            Self::MissingLogEntries { expected, found } => write!(
                formatter,
                "entry epoch {found} is ahead of the local epoch {expected}; entries are missing"
            ),
            Self::InvalidKeyPackage(detail) => write!(formatter, "invalid key package: {detail}"),
            Self::DuplicateProposal => {
                formatter.write_str("the proposal is already pending in the local proposal store")
            }
            Self::NoPendingProposals => {
                formatter.write_str("there are no pending proposals to commit")
            }
            Self::UnknownMember(detail) => write!(formatter, "unknown member: {detail}"),
            Self::InvalidControlOperation(detail) => {
                write!(formatter, "invalid control operation: {detail}")
            }
            Self::InvalidCiphertext(detail) => write!(formatter, "invalid MLS entry: {detail}"),
            Self::InvalidControlResult(detail) => {
                write!(formatter, "control result mismatch: {detail}")
            }
            Self::WelcomeNotAuthorized(detail) => write!(formatter, "welcome not authorized: {detail}"),
            Self::InvalidCredential(detail) => write!(formatter, "invalid credential: {detail}"),
            Self::StateCorrupted(detail) => write!(formatter, "state corrupted: {detail}"),
            Self::UnsupportedEntry(detail) => write!(formatter, "unsupported entry: {detail}"),
            Self::SelfRemovalRequiresProposal => formatter.write_str(
                "MLS forbids a commit that removes self; prepare a self-removal proposal instead",
            ),
            Self::Storage(error) => write!(formatter, "storage failure: {error}"),
            Self::PersistAfterMerge(error) => {
                write!(formatter, "control commit merged but persisting it failed: {error}")
            }
            Self::PersistAfterDiscard(error) => {
                write!(formatter, "pending discarded but restoring the committed snapshot failed: {error}")
            }
            Self::Crypto(detail) => write!(formatter, "MLS failure: {detail}"),
        }
    }
}

impl std::error::Error for ControlError {}

impl From<StorageError> for ControlError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[(byte >> 4) as usize]));
        text.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    text
}

fn crypto_error(context: &str, error: impl std::fmt::Display) -> ControlError {
    ControlError::Crypto(format!("{context}: {error}"))
}

fn storage_error(error: anyhow::Error) -> ControlError {
    match error.downcast::<StorageError>() {
        Ok(storage) => ControlError::Storage(storage),
        Err(other) => ControlError::Crypto(format!("{other}")),
    }
}

/// pending 提交摘要：绑定 group_id、base/target epoch 与 Commit 字节。
fn control_digest(
    group_id: &str,
    base_epoch: u64,
    target_epoch: u64,
    commit: &[u8],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(PENDING_DIGEST_DOMAIN);
    hasher.update((group_id.len() as u32).to_be_bytes());
    hasher.update(group_id.as_bytes());
    hasher.update(base_epoch.to_be_bytes());
    hasher.update(target_epoch.to_be_bytes());
    hasher.update((commit.len() as u32).to_be_bytes());
    hasher.update(commit);
    let digest = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(digest.as_slice());
    bytes
}

fn push_block(out: &mut Vec<u8>, bytes: &[u8], limit: usize, label: &str) -> Result<(), ControlError> {
    if bytes.len() > limit {
        return Err(ControlError::Crypto(format!(
            "pending control {label} exceeds {limit} bytes"
        )));
    }
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn take_block<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    limit: usize,
    label: &str,
) -> Result<&'a [u8], ControlError> {
    if bytes.len() < *cursor + 4 {
        return Err(ControlError::StateCorrupted(
            "persisted pending control record is truncated",
        ));
    }
    let length = u32::from_be_bytes(
        bytes[*cursor..*cursor + 4]
            .try_into()
            .map_err(|_| ControlError::StateCorrupted("pending control length is malformed"))?,
    ) as usize;
    *cursor += 4;
    if length > limit {
        return Err(ControlError::StateCorrupted(
            "persisted pending control field is too large",
        ));
    }
    if bytes.len() < *cursor + length {
        return Err(ControlError::StateCorrupted(
            "persisted pending control field is truncated",
        ));
    }
    let value = &bytes[*cursor..*cursor + length];
    *cursor += length;
    let _ = label;
    Ok(value)
}

/// 编码 pending 控制元数据（随群状态快照进正式库）。
pub(crate) fn encode_pending_record(record: &PendingControl) -> Result<Vec<u8>, ControlError> {
    let mut out = Vec::with_capacity(128 + record.commit.len());
    out.push(PENDING_FORMAT_VERSION);
    out.push(record.operation.wire_tag());
    out.extend_from_slice(&record.added.to_be_bytes());
    out.extend_from_slice(&record.removed.to_be_bytes());
    out.extend_from_slice(&record.base_epoch.to_be_bytes());
    out.extend_from_slice(&record.target_epoch.to_be_bytes());
    push_block(&mut out, record.group_id.as_bytes(), 128, "group ID")?;
    out.extend_from_slice(&record.commit_digest);
    push_block(&mut out, &record.commit, MAX_COMMIT_BYTES, "commit")?;
    match &record.welcome {
        Some(welcome) => {
            out.push(1);
            push_block(&mut out, welcome, MAX_WELCOME_BYTES, "welcome")?;
        }
        None => out.push(0),
    }
    Ok(out)
}

/// 解码 pending 控制元数据；畸形/越界一律显式失败。
pub(crate) fn decode_pending_record(bytes: &[u8]) -> Result<PendingControl, ControlError> {
    if bytes.len() < 1 + 1 + 4 + 4 + 8 + 8 + 32 + 1 {
        return Err(ControlError::StateCorrupted(
            "persisted pending control record is too short",
        ));
    }
    if bytes[0] != PENDING_FORMAT_VERSION {
        return Err(ControlError::StateCorrupted(
            "unsupported pending control record version",
        ));
    }
    let operation = ControlOperationKind::from_wire_tag(bytes[1]).ok_or(
        ControlError::StateCorrupted("unknown pending control operation tag"),
    )?;
    let mut cursor = 2usize;
    let take_u32 = |cursor: &mut usize| -> Result<u32, ControlError> {
        if bytes.len() < *cursor + 4 {
            return Err(ControlError::StateCorrupted(
                "persisted pending control record is truncated",
            ));
        }
        let value = u32::from_be_bytes(
            bytes[*cursor..*cursor + 4]
                .try_into()
                .map_err(|_| ControlError::StateCorrupted("pending control integer is malformed"))?,
        );
        *cursor += 4;
        Ok(value)
    };
    let added = take_u32(&mut cursor)?;
    let removed = take_u32(&mut cursor)?;
    if bytes.len() < cursor + 16 {
        return Err(ControlError::StateCorrupted(
            "persisted pending control record is truncated",
        ));
    }
    let base_epoch = u64::from_be_bytes(
        bytes[cursor..cursor + 8]
            .try_into()
            .map_err(|_| ControlError::StateCorrupted("pending control epoch is malformed"))?,
    );
    cursor += 8;
    let target_epoch = u64::from_be_bytes(
        bytes[cursor..cursor + 8]
            .try_into()
            .map_err(|_| ControlError::StateCorrupted("pending control epoch is malformed"))?,
    );
    cursor += 8;
    let group_id = String::from_utf8(
        take_block(bytes, &mut cursor, 128, "group ID")?.to_vec(),
    )
    .map_err(|_| ControlError::StateCorrupted("pending control group ID is not UTF-8"))?;
    if bytes.len() < cursor + 32 {
        return Err(ControlError::StateCorrupted(
            "persisted pending control record is truncated",
        ));
    }
    let mut commit_digest = [0u8; 32];
    commit_digest.copy_from_slice(&bytes[cursor..cursor + 32]);
    cursor += 32;
    let commit = take_block(bytes, &mut cursor, MAX_COMMIT_BYTES, "commit")?.to_vec();
    if bytes.len() < cursor + 1 {
        return Err(ControlError::StateCorrupted(
            "persisted pending control record is truncated",
        ));
    }
    let welcome_flag = bytes[cursor];
    cursor += 1;
    let welcome = match welcome_flag {
        0 => None,
        1 => Some(take_block(bytes, &mut cursor, MAX_WELCOME_BYTES, "welcome")?.to_vec()),
        _ => {
            return Err(ControlError::StateCorrupted(
                "persisted pending control record has an invalid welcome flag",
            ));
        }
    };
    if cursor != bytes.len() {
        return Err(ControlError::StateCorrupted(
            "persisted pending control record has trailing bytes",
        ));
    }
    if group_id.is_empty() || commit.is_empty() || target_epoch != base_epoch.saturating_add(1) {
        return Err(ControlError::StateCorrupted(
            "persisted pending control record is inconsistent",
        ));
    }
    Ok(PendingControl {
        group_id,
        base_epoch,
        target_epoch,
        operation,
        added,
        removed,
        commit,
        welcome,
        commit_digest,
    })
}

impl MlsDevice {
    /// 本机当前未决控制提交（与 OpenMLS 的 PendingCommit 状态交叉校验）。
    pub fn pending_control(&self) -> Result<Option<PendingControl>, ControlError> {
        let group = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?;
        match (group.pending_commit().is_some(), self.pending_record()) {
            (false, None) => Ok(None),
            (true, Some(record)) => Ok(Some(record.clone())),
            (true, None) => Err(ControlError::StateCorrupted(
                "MLS group holds a pending commit without recorded control metadata",
            )),
            (false, Some(_)) => Err(ControlError::StateCorrupted(
                "control metadata records a pending commit that MLS does not hold",
            )),
        }
    }

    /// CRYPTO-005：准备一个控制提交（Add/Remove/SelfUpdate/SelfRemoval），保存为 pending 并持久化。
    ///
    /// - `SelfRemoval` 只移除本机设备，生成真实 RemoveMembers 提交（removed = 1、
    ///   无 Welcome）；普通 `RemoveMembers` 仍然拒绝把本机设备列为移除目标。
    /// - pending 的种类记为 `RemoveMembers`（它确实是一条 MLS RemoveMembers 提交），
    ///   调用方用操作 context 区分"主动退出"与"被移除"。
    ///
    /// 关键不变量：本调用**不推进**本地 epoch；只有拿到接受证据并 merge 后才推进。
    ///
    /// 若 pending 快照写库失败，pending 已经在内存与 OpenMLS 状态里 stage 完成，返回
    /// Storage 错误；调用方应重试 export_to_store（同 epoch 快照替换）或丢弃该 pending，
    /// 不得当作“什么都没发生”继续准备第二个提交。
    pub fn prepare_control(
        &mut self,
        operation: ControlOperation,
        store: &mut FormalStore,
    ) -> Result<PendingControl, ControlError> {
        if let Some(existing) = self.pending_record() {
            return Err(ControlError::PendingCommitExists {
                base_epoch: existing.base_epoch,
            });
        }
        let local_identity = self
            .credential_handle()
            .credential
            .serialized_content()
            .to_vec();

        let base_epoch = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?
            .epoch()
            .as_u64();

        let staged = {
            let (provider, signer, group) = self
                .control_parts_mut()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            if group.pending_commit().is_some() {
                return Err(ControlError::PendingMismatch(
                    "the MLS group already holds an untracked pending commit",
                ));
            }
            let members: Vec<Member> = group.members().collect();
            match operation {
                ControlOperation::AddMembers { key_packages } => {
                    if key_packages.is_empty() {
                        return Err(ControlError::InvalidControlOperation(
                            "add members requires at least one key package",
                        ));
                    }
                    if key_packages.len() > MAX_ADDED_MEMBERS {
                        return Err(ControlError::InvalidControlOperation(
                            "too many members in one add operation",
                        ));
                    }
                    let mut validated: Vec<KeyPackage> = Vec::with_capacity(key_packages.len());
                    for bytes in &key_packages {
                        let package = KeyPackageIn::tls_deserialize_exact(bytes.as_slice())
                            .map_err(|_| {
                                ControlError::InvalidKeyPackage(
                                    "key package is not a valid MLS message",
                                )
                            })?
                            .validate(provider.crypto(), ProtocolVersion::Mls10)
                            .map_err(|_| {
                                ControlError::InvalidKeyPackage(
                                    "key package signature or ciphersuite is invalid",
                                )
                            })?;
                        let identity = package.leaf_node().credential().serialized_content();
                        if identity.is_empty() {
                            return Err(ControlError::InvalidCredential(
                                "peer credential is empty",
                            ));
                        }
                        if package.leaf_node().signature_key().as_slice() == signer.public() {
                            return Err(ControlError::InvalidControlOperation(
                                "cannot add the local device",
                            ));
                        }
                        if members.iter().any(|member| {
                            member.signature_key.as_slice()
                                == package.leaf_node().signature_key().as_slice()
                        }) {
                            return Err(ControlError::InvalidControlOperation(
                                "peer is already a group member",
                            ));
                        }
                        if validated.iter().any(|existing: &KeyPackage| {
                            existing.leaf_node().signature_key()
                                == package.leaf_node().signature_key()
                        }) {
                            return Err(ControlError::InvalidKeyPackage(
                                "duplicate key package in the same add operation",
                            ));
                        }
                        validated.push(package);
                    }
                    let (commit, welcome, _) = group
                        .add_members(provider, signer, &validated)
                        .map_err(|error| crypto_error("stage add-members commit", error))?;
                    let commit_bytes = commit
                        .tls_serialize_detached()
                        .map_err(|error| crypto_error("serialize add-members commit", error))?;
                    let welcome_bytes = welcome
                        .tls_serialize_detached()
                        .map_err(|error| crypto_error("serialize welcome", error))?;
                    (
                        commit_bytes,
                        Some(welcome_bytes),
                        ControlOperationKind::AddMembers,
                        validated.len() as u32,
                        0u32,
                    )
                }
                ControlOperation::RemoveMembers {
                    members: identities,
                } => {
                    if identities.is_empty() {
                        return Err(ControlError::InvalidControlOperation(
                            "remove members requires at least one member identity",
                        ));
                    }
                    if identities.len() > MAX_ADDED_MEMBERS {
                        return Err(ControlError::InvalidControlOperation(
                            "too many members in one remove operation",
                        ));
                    }
                    let mut indices: Vec<LeafNodeIndex> = Vec::new();
                    for identity in &identities {
                        let matched: Vec<&Member> = members
                            .iter()
                            .filter(|member| {
                                member.credential.serialized_content() == identity.as_bytes()
                            })
                            .collect();
                        if matched.len() != 1 {
                            return Err(ControlError::UnknownMember(
                                "remove target is not a unique group member",
                            ));
                        }
                        let member = matched[0];
                        if member.credential.serialized_content() == local_identity.as_slice() {
                            return Err(ControlError::InvalidControlOperation(
                                "cannot remove the local device through remove_members",
                            ));
                        }
                        if indices.contains(&member.index) {
                            return Err(ControlError::InvalidControlOperation(
                                "duplicate remove target",
                            ));
                        }
                        indices.push(member.index);
                    }
                    let (commit, welcome, _) = group
                        .remove_members(provider, signer, &indices)
                        .map_err(|error| crypto_error("stage remove-members commit", error))?;
                    let commit_bytes = commit
                        .tls_serialize_detached()
                        .map_err(|error| crypto_error("serialize remove-members commit", error))?;
                    let welcome_bytes = welcome
                        .map(|welcome| welcome.tls_serialize_detached())
                        .transpose()
                        .map_err(|error| crypto_error("serialize welcome", error))?;
                    (
                        commit_bytes,
                        welcome_bytes,
                        ControlOperationKind::RemoveMembers,
                        0u32,
                        indices.len() as u32,
                    )
                }
                ControlOperation::SelfRemoval => {
                    // MLS 1.0 明确禁止"成员提交移除自己"（OpenMLS: "The Commit tried to
                    // remove self from the group. This is not possible."）。自移除只能以
                    // 提案形式发起、由另一成员提交，因此这里把它引导到
                    // `prepare_self_removal`，绝不伪造一条本地自移除提交。
                    return Err(ControlError::SelfRemovalRequiresProposal);
                }
                ControlOperation::SelfUpdate => {
                    let bundle = group
                        .self_update(provider, signer, LeafNodeParameters::default())
                        .map_err(|error| crypto_error("stage self-update commit", error))?;
                    let (commit, welcome, _) = bundle.into_messages();
                    let commit_bytes = commit
                        .tls_serialize_detached()
                        .map_err(|error| crypto_error("serialize self-update commit", error))?;
                    let welcome_bytes = welcome
                        .map(|welcome| welcome.tls_serialize_detached())
                        .transpose()
                        .map_err(|error| crypto_error("serialize welcome", error))?;
                    (
                        commit_bytes,
                        welcome_bytes,
                        ControlOperationKind::SelfUpdate,
                        0u32,
                        0u32,
                    )
                }
            }
        };
        let (commit, welcome, kind, added, removed) = staged;

        // 步骤 3：比对实际 staging 出来的增删集合，拒绝“夹带”未经请求的成员变化。
        let staged_changes = {
            let group = self
                .group_ref()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            let Some(staged) = group.pending_commit() else {
                return Err(ControlError::PendingMismatch(
                    "MLS did not stage a pending commit for the control operation",
                ));
            };
            (
                staged.add_proposals().count() as u32,
                staged.remove_proposals().count() as u32,
            )
        };
        if staged_changes != (added, removed) {
            // 自有提交与请求不一致：清掉 pending 回到 Operational，不写库、不推进 epoch。
            let clear_result = {
                let (provider, _signer, group) = self
                    .control_parts_mut()
                    .map_err(|_| ControlError::GroupNotInitialized)?;
                group.clear_pending_commit(provider.storage())
            };
            let _ = clear_result;
            self.set_pending_record(None);
            return Err(ControlError::InvalidControlResult(
                "staged commit member changes do not match the requested control operation",
            ));
        }

        // 关键不变量：staging 不得改变本地 epoch。
        let epoch_after = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?
            .epoch()
            .as_u64();
        if epoch_after != base_epoch {
            self.abandon_group();
            return Err(ControlError::Crypto(
                "staging a control commit changed the local epoch".to_owned(),
            ));
        }
        if commit.is_empty() || commit.len() > MAX_COMMIT_BYTES {
            self.abandon_group();
            return Err(ControlError::Crypto(
                "staged commit length is outside the supported bounds".to_owned(),
            ));
        }
        if let Some(welcome) = &welcome
            && (welcome.is_empty() || welcome.len() > MAX_WELCOME_BYTES)
        {
            self.abandon_group();
            return Err(ControlError::Crypto(
                "staged welcome length is outside the supported bounds".to_owned(),
            ));
        }
        let group_id = self
            .group_key()
            .map_err(|error| crypto_error("read MLS group id", error))?;
        let target_epoch = base_epoch.saturating_add(1);
        let commit_digest = control_digest(&group_id, base_epoch, target_epoch, &commit);
        let pending = PendingControl {
            group_id,
            base_epoch,
            target_epoch,
            operation: kind,
            added,
            removed,
            commit,
            welcome,
            commit_digest,
        };
        self.set_pending_record(Some(pending.clone()));
        // T2：pending 分支持久化（与正式已提交状态分开，但同一次快照写出）。
        self.export_to_store(store)
            .map_err(storage_error)?;
        Ok(pending)
    }

    /// 本机自移除（CRYPTO-005 的 SelfRemoval 路径）：生成"移除本机 leaf"的 MLS 提案。
    ///
    /// 关键语义（为什么不是 Commit）：
    /// - MLS 1.0 禁止一条 Commit 移除提交者自己（OpenMLS 报
    ///   "The Commit tried to remove self from the group. This is not possible."），
    ///   因此本地**无法**生成合法的自移除提交；
    /// - 唯一合法的自移除原语是"本机对自己发起 Remove 提案"，再由群里另一名成员
    ///   把该提案包进 Commit。本方法返回可转发的提案字节，并把提案写入本地 proposal store
    ///   （随群状态快照一起持久化）；
    /// - 本调用不推进 epoch、不动 pending 控制提交；返回后本机仍在群里（能收发），
    ///   直到对端提交到达并被处理（`self_removed = true`）才真正离群。
    ///
    /// 前置条件：群已初始化、没有 pending 控制提交、本机是群里唯一的自己、epoch 未变。
    pub fn prepare_self_removal(
        &mut self,
        store: &mut FormalStore,
    ) -> Result<PendingSelfRemoval, ControlError> {
        if let Some(existing) = self.pending_record() {
            return Err(ControlError::PendingCommitExists {
                base_epoch: existing.base_epoch,
            });
        }
        let local_identity = self
            .credential_handle()
            .credential
            .serialized_content()
            .to_vec();
        let base_epoch = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?
            .epoch()
            .as_u64();
        let proposal_bytes = {
            let (provider, signer, group) = self
                .control_parts_mut()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            if group.pending_commit().is_some() {
                return Err(ControlError::PendingMismatch(
                    "the MLS group already holds an untracked pending commit",
                ));
            }
            // 本机必须是群里唯一的自己（凭据身份唯一）。
            let local_matches: Vec<Member> = group
                .members()
                .filter(|member| {
                    member.credential.serialized_content() == local_identity.as_slice()
                })
                .collect();
            match local_matches.as_slice() {
                [_] => {}
                [] => {
                    return Err(ControlError::StateCorrupted(
                        "the local device is not a member of the MLS group",
                    ));
                }
                _ => {
                    return Err(ControlError::StateCorrupted(
                        "the local device appears more than once in the MLS group",
                    ));
                }
            }
            // leave_group 生成"移除本机 leaf"的提案（密文形式，符合本工程的 wire format）。
            let proposal = group
                .leave_group(provider, signer)
                .map_err(|error| crypto_error("stage self-removal proposal", error))?;
            proposal
                .tls_serialize_detached()
                .map_err(|error| crypto_error("serialize self-removal proposal", error))?
        };
        if proposal_bytes.is_empty() || proposal_bytes.len() > MAX_COMMIT_BYTES {
            self.abandon_group();
            return Err(ControlError::Crypto(
                "self-removal proposal length is outside the supported bounds".to_owned(),
            ));
        }
        // 关键不变量：生成提案不得改变本地 epoch。
        let epoch_after = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?
            .epoch()
            .as_u64();
        if epoch_after != base_epoch {
            self.abandon_group();
            return Err(ControlError::Crypto(
                "staging a self-removal proposal changed the local epoch".to_owned(),
            ));
        }
        let group_id = self
            .group_key()
            .map_err(|error| crypto_error("read MLS group id", error))?;
        let proposal_digest = self_removal_digest(&group_id, base_epoch, &proposal_bytes);
        // 提案已进本地 proposal store：与群状态同一次快照落库。
        self.export_to_store(store).map_err(storage_error)?;
        Ok(PendingSelfRemoval {
            group_id,
            base_epoch,
            removed: 1,
            has_welcome: false,
            proposal: proposal_bytes,
            proposal_digest,
        })
    }

    /// 把本地 proposal store 里的提案打包成一条 Commit（写 pending，epoch 不推进）。
    ///
    /// 用于"提案式控制流"：对端收到独立提案后，由某个成员把提案提交成 Commit。
    /// - 没有待提交提案 → NO_PENDING_PROPOSALS（显式拒绝，不产生空提交）；
    /// - 已有 pending 控制提交 → PENDING_COMMIT_EXISTS；
    /// - 提交种类按实际增删推导（AddMembers/RemoveMembers/SelfUpdate），removed/added
    ///   为 staged commit 的真实计数；Welcome 只在该提交真的新增成员时存在；
    /// - 只有拿到接受证据并 merge 后 epoch 才推进；本调用不推进 epoch。
    pub fn commit_pending_proposals(
        &mut self,
        store: &mut FormalStore,
    ) -> Result<PendingControl, ControlError> {
        if let Some(existing) = self.pending_record() {
            return Err(ControlError::PendingCommitExists {
                base_epoch: existing.base_epoch,
            });
        }
        let base_epoch = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?
            .epoch()
            .as_u64();
        {
            let group = self
                .group_ref()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            if group.pending_commit().is_some() {
                return Err(ControlError::PendingMismatch(
                    "the MLS group already holds an untracked pending commit",
                ));
            }
            if group.proposal_store().proposals().next().is_none() {
                return Err(ControlError::NoPendingProposals);
            }
        }
        let (commit, welcome) = {
            let (provider, signer, group) = self
                .control_parts_mut()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            let (commit, welcome, _) = group
                .commit_to_pending_proposals(provider, signer)
                .map_err(|error| crypto_error("commit pending proposals", error))?;
            let commit_bytes = commit
                .tls_serialize_detached()
                .map_err(|error| crypto_error("serialize proposal commit", error))?;
            let welcome_bytes = welcome
                .map(|welcome| welcome.tls_serialize_detached())
                .transpose()
                .map_err(|error| crypto_error("serialize welcome", error))?;
            (commit_bytes, welcome_bytes)
        };
        // 真实增删计数取自已 staged 的提交。
        let (added, removed, updated) = {
            let group = self
                .group_ref()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            let Some(staged) = group.pending_commit() else {
                return Err(ControlError::PendingMismatch(
                    "MLS did not stage a pending commit for the pending proposals",
                ));
            };
            (
                staged.add_proposals().count() as u32,
                staged.remove_proposals().count() as u32,
                staged.update_proposals().count() as u32,
            )
        };
        // 关键不变量：打包提案不推进本地 epoch。
        let epoch_after = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?
            .epoch()
            .as_u64();
        if epoch_after != base_epoch {
            self.abandon_group();
            return Err(ControlError::Crypto(
                "staging a proposal commit changed the local epoch".to_owned(),
            ));
        }
        if commit.is_empty() || commit.len() > MAX_COMMIT_BYTES {
            self.abandon_group();
            return Err(ControlError::Crypto(
                "staged proposal commit length is outside the supported bounds".to_owned(),
            ));
        }
        if let Some(welcome) = &welcome
            && (welcome.is_empty() || welcome.len() > MAX_WELCOME_BYTES)
        {
            self.abandon_group();
            return Err(ControlError::Crypto(
                "staged welcome length is outside the supported bounds".to_owned(),
            ));
        }
        let kind = if added > 0 {
            ControlOperationKind::AddMembers
        } else if removed > 0 {
            ControlOperationKind::RemoveMembers
        } else {
            // 只有 update/PSK 类提案时按自更新记录（不新增枚举，避免破坏上层匹配）。
            ControlOperationKind::SelfUpdate
        };
        let group_id = self
            .group_key()
            .map_err(|error| crypto_error("read MLS group id", error))?;
        let target_epoch = base_epoch.saturating_add(1);
        let commit_digest = control_digest(&group_id, base_epoch, target_epoch, &commit);
        let pending = PendingControl {
            group_id,
            base_epoch,
            target_epoch,
            operation: kind,
            added,
            removed,
            commit,
            welcome,
            commit_digest,
        };
        self.set_pending_record(Some(pending.clone()));
        self.export_to_store(store).map_err(storage_error)?;
        let _ = updated;
        Ok(pending)
    }

    /// CRYPTO-007：提交被接受后合并 pending，并消费该提交的密钥。
    ///
    /// 只有绑定同一 group/base_epoch/target_epoch/摘要的接受证据才允许合并；
    /// 合并失败或合并后 epoch 不符时设备 fail-closed，绝不静默推进。
    ///
    /// 若合并后写库失败，返回 PersistAfterMerge：内存已到 target_epoch，库仍是 pending
    /// 快照；调用方必须重试 export_to_store（幂等同 epoch 快照替换）后才能对外发布。
    pub fn merge_accepted_pending(
        &mut self,
        acceptance: &ControlAcceptance,
        store: &mut FormalStore,
    ) -> Result<MergedControl, ControlError> {
        let pending = self
            .pending_record()
            .cloned()
            .ok_or(ControlError::NoPendingCommit)?;
        if acceptance.group_id != pending.group_id
            || acceptance.base_epoch != pending.base_epoch
            || acceptance.target_epoch != pending.target_epoch
            || acceptance.commit_digest != pending.commit_digest
        {
            return Err(ControlError::PendingMismatch(
                "acceptance evidence does not match the pending control commit",
            ));
        }
        let current_epoch = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?
            .epoch()
            .as_u64();
        if current_epoch != pending.base_epoch {
            return Err(ControlError::StaleEpoch {
                expected: pending.base_epoch,
                found: current_epoch,
            });
        }
        let merge_result = {
            let (provider, _signer, group) = self
                .control_parts_mut()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            group.merge_pending_commit(provider)
        };
        if let Err(error) = merge_result {
            self.abandon_group();
            return Err(crypto_error("merge pending commit", error));
        }
        let new_epoch = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?
            .epoch()
            .as_u64();
        if new_epoch != pending.target_epoch {
            self.abandon_group();
            return Err(ControlError::Crypto(format!(
                "merged epoch {new_epoch} does not match the expected target {}",
                pending.target_epoch
            )));
        }
        self.set_pending_record(None);
        let exported = self
            .export_to_store(store)
            .map_err(|error| match error.downcast::<StorageError>() {
                Ok(storage) => ControlError::PersistAfterMerge(storage),
                Err(other) => ControlError::Crypto(format!("{other}")),
            })?;
        Ok(MergedControl {
            group_id: pending.group_id,
            old_epoch: pending.base_epoch,
            new_epoch,
            operation: pending.operation,
            exported,
        })
    }

    /// CRYPTO-008：明确拒绝后丢弃 pending，回滚到上一完整状态并写回正式库。
    ///
    /// 旧 epoch 的密钥状态不受影响，因此丢弃后仍可继续用旧 epoch 加解密。
    ///
    /// 若回滚后的快照写库失败，返回 PersistAfterDiscard：内存已无 pending，库仍是旧
    /// pending 快照；下一次成功导出会覆盖它，或在重开后再次丢弃。
    pub fn discard_rejected_pending(
        &mut self,
        rejection: &ControlRejection,
        store: &mut FormalStore,
    ) -> Result<DiscardedControl, ControlError> {
        let pending = self
            .pending_record()
            .cloned()
            .ok_or(ControlError::NoPendingCommit)?;
        if rejection.group_id != pending.group_id
            || rejection.base_epoch != pending.base_epoch
            || rejection.commit_digest != pending.commit_digest
        {
            return Err(ControlError::PendingMismatch(
                "rejection evidence does not match the pending control commit",
            ));
        }
        // 持久化的 pending 分支必须仍停在 base_epoch；否则不得用旧快照覆盖新状态。
        match store.load_mls_group_state(&pending.group_id) {
            Ok(Some(state)) if state.epoch == pending.base_epoch => {}
            Ok(Some(state)) => {
                return Err(ControlError::StaleEpoch {
                    expected: pending.base_epoch,
                    found: state.epoch,
                });
            }
            Ok(None) => {
                return Err(ControlError::StateCorrupted(
                    "pending control commit has no persisted group snapshot",
                ));
            }
            Err(error) => return Err(ControlError::Storage(error)),
        }
        let clear_result = {
            let (provider, _signer, group) = self
                .control_parts_mut()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            group.clear_pending_commit(provider.storage())
        };
        clear_result.map_err(|error| crypto_error("clear pending commit", error))?;
        let epoch = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?
            .epoch()
            .as_u64();
        if epoch != pending.base_epoch {
            self.abandon_group();
            return Err(ControlError::Crypto(
                "discarding a pending commit changed the local epoch".to_owned(),
            ));
        }
        self.set_pending_record(None);
        let exported = self
            .export_to_store(store)
            .map_err(|error| match error.downcast::<StorageError>() {
                Ok(storage) => ControlError::PersistAfterDiscard(storage),
                Err(other) => ControlError::Crypto(format!("{other}")),
            })?;
        Ok(DiscardedControl {
            group_id: pending.group_id,
            epoch,
            operation: pending.operation,
            reason: rejection.reason,
            exported,
        })
    }

    /// CRYPTO-006：处理已通过外层验证的对端条目（应用消息 / Commit / 提案）。
    ///
    /// 先校验群与 epoch，再交给 OpenMLS 认证解密或 stage；对端 Commit 校验预期成员
    /// 变化后才合并。任何不一致都显式失败，不越过坏条目继续。
    pub fn process_verified_entry(
        &mut self,
        entry: &VerifiedGroupEntry,
    ) -> Result<ProcessedEntry, ControlError> {
        let local_group_id = self
            .group_key()
            .map_err(|error| crypto_error("read MLS group id", error))?;
        if local_group_id != entry.group_id {
            return Err(ControlError::InvalidCiphertext(
                "entry belongs to a different MLS group",
            ));
        }
        let (current_epoch, has_pending_commit) = {
            let group = self
                .group_ref()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            (group.epoch().as_u64(), group.pending_commit().is_some())
        };
        if has_pending_commit != self.pending_record().is_some() {
            return Err(ControlError::StateCorrupted(
                "MLS pending-commit state and recorded control metadata disagree",
            ));
        }
        if entry.epoch > current_epoch {
            return Err(ControlError::MissingLogEntries {
                expected: current_epoch,
                found: entry.epoch,
            });
        }
        if entry.epoch < current_epoch {
            return Err(ControlError::StaleEpoch {
                expected: current_epoch,
                found: entry.epoch,
            });
        }
        let message = MlsMessageIn::tls_deserialize_exact(entry.payload.as_slice()).map_err(|_| {
            ControlError::InvalidCiphertext("entry payload is not an MLS message")
        })?;
        let protocol = message.try_into_protocol_message().map_err(|_| {
            ControlError::InvalidCiphertext("entry payload is not an MLS protocol message")
        })?;
        if hex_lower(protocol.group_id().as_slice()) != local_group_id {
            return Err(ControlError::InvalidCiphertext(
                "MLS message group id does not match the entry group",
            ));
        }
        if protocol.epoch().as_u64() != entry.epoch {
            return Err(ControlError::InvalidCiphertext(
                "MLS message epoch does not match the entry epoch",
            ));
        }
        let is_commit = protocol.content_type() == ContentType::Commit;
        if is_commit && self.pending_record().is_some() {
            // 本机 pending 与对端 Commit 属于同一 epoch 的竞争分支：先显式解决，
            // 不允许静默丢弃 pending（否则会失去可重新投递的候选）。
            return Err(ControlError::PendingMismatch(
                "a control commit is already pending locally; resolve it before processing a competing commit",
            ));
        }
        let processed = {
            let (provider, _signer, group) = self
                .control_parts_mut()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            group.process_message(provider, protocol)
        }
        .map_err(|_| {
            // 认证/解密失败一律按无效密文处理；不回显底层细节，避免给攻击者区分信息。
            // 握手（Commit）失败会消耗该代密钥、无法在本状态重试，因此停止该群分支；
            // 应用消息失败只消耗一个发送方 ratchet 代数，新消息仍可解密，不断分支。
            if is_commit {
                self.abandon_group();
            }
            ControlError::InvalidCiphertext("MLS entry failed authentication or decryption")
        })?;
        if processed.epoch().as_u64() != entry.epoch {
            self.abandon_group();
            return Err(ControlError::InvalidCiphertext(
                "authenticated MLS epoch does not match the entry epoch",
            ));
        }
        if processed.credential().serialized_content() != entry.sender_identity.as_bytes() {
            return Err(ControlError::InvalidCiphertext(
                "entry sender does not match the authenticated MLS sender",
            ));
        }
        let sender_identity = entry.sender_identity.clone();
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(message) => Ok(ProcessedEntry::Application {
                sender_identity,
                epoch: entry.epoch,
                plaintext: message.into_bytes(),
            }),
            ProcessedMessageContent::ProposalMessage(proposal) => {
                let stored = {
                    let (provider, _signer, group) = self
                        .control_parts_mut()
                        .map_err(|_| ControlError::GroupNotInitialized)?;
                    group.store_pending_proposal(provider.storage(), *proposal)
                };
                stored.map_err(|error| crypto_error("store pending proposal", error))?;
                Ok(ProcessedEntry::Proposal {
                    sender_identity,
                    epoch: entry.epoch,
                })
            }
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                let added = staged.add_proposals().count() as u32;
                let removed = staged.remove_proposals().count() as u32;
                let updated = staged.update_proposals().count() as u32;
                let self_removed = staged.self_removed();
                if let Some(expectation) = entry.expectation
                    && (expectation.added != added
                        || expectation.removed != removed
                        || expectation.updated != updated)
                {
                    // 控制结果与已验证业务不一致：停止该群分支，绝不合并。
                    self.abandon_group();
                    return Err(ControlError::InvalidControlResult(
                        "commit member changes do not match the verified control metadata",
                    ));
                }
                let merge_result = {
                    let (provider, _signer, group) = self
                        .control_parts_mut()
                        .map_err(|_| ControlError::GroupNotInitialized)?;
                    group.merge_staged_commit(provider, *staged)
                };
                if let Err(error) = merge_result {
                    self.abandon_group();
                    return Err(crypto_error("merge remote commit", error));
                }
                let new_epoch = self
                    .group_ref()
                    .map_err(|_| ControlError::GroupNotInitialized)?
                    .epoch()
                    .as_u64();
                if new_epoch != entry.epoch.saturating_add(1) {
                    self.abandon_group();
                    return Err(ControlError::Crypto(
                        "merging a remote commit did not advance the epoch by exactly one".to_owned(),
                    ));
                }
                Ok(ProcessedEntry::Commit {
                    sender_identity,
                    old_epoch: entry.epoch,
                    new_epoch,
                    added,
                    removed,
                    updated,
                    self_removed,
                })
            }
            ProcessedMessageContent::OwnPendingCommit => Ok(ProcessedEntry::OwnPendingCommit {
                epoch: entry.epoch,
            }),
            ProcessedMessageContent::ExternalJoinProposalMessage(_) => {
                Err(ControlError::UnsupportedEntry(
                    "external join proposals are not part of the P0 control slice",
                ))
            }
            ProcessedMessageContent::OwnPrivateMessage => Err(ControlError::InvalidCiphertext(
                "entry is an echo of an application message sent by this device",
            )),
        }
    }

    /// 本地 proposal store 里待提交的提案数（epoch 不变；用于展示"等待被提交"状态）。
    pub fn pending_proposal_count(&self) -> Result<u32, ControlError> {
        let group = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?;
        Ok(u32::try_from(group.proposal_store().proposals().count()).unwrap_or(u32::MAX))
    }

    /// 处理对端独立提案（P0 只支持 Remove 提案，含"自移除"提案）。
    ///
    /// 语义：
    /// - 只接受 Proposal 内容类型的 MLS 消息（带 Commit 预期的条目显式拒绝）；
    /// - 群 ID 必须一致、epoch 必须等于本地 epoch（落后 STALE_EPOCH、超前
    ///   MISSING_LOG_ENTRIES）；作者身份必须等于已认证的 MLS 发送者；
    /// - 提案通过认证后存进本地 proposal store，**epoch 不变**，等待后续 Commit 覆盖；
    /// - 重复提案（同一提案引用已在 store 里）→ DUPLICATE_PROPOSAL，不重复入队；
    /// - 其它种类（Add/Update/PSK/...）→ UNSUPPORTED_ENTRY，且不把该提案留在本地
    ///   store（解析后立即撤销，保证不留半状态）。
    pub fn process_proposal_entry(
        &mut self,
        entry: &VerifiedGroupEntry,
    ) -> Result<ProposalOutcome, ControlError> {
        let local_group_id = self
            .group_key()
            .map_err(|error| crypto_error("read MLS group id", error))?;
        if entry.group_id != local_group_id {
            return Err(ControlError::InvalidCiphertext(
                "proposal entry belongs to another MLS group",
            ));
        }
        if entry.expectation.is_some() {
            return Err(ControlError::InvalidControlOperation(
                "a proposal entry must not carry a commit expectation",
            ));
        }
        let local_epoch = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?
            .epoch()
            .as_u64();
        if entry.epoch < local_epoch {
            return Err(ControlError::StaleEpoch {
                expected: local_epoch,
                found: entry.epoch,
            });
        }
        if entry.epoch > local_epoch {
            return Err(ControlError::MissingLogEntries {
                expected: local_epoch,
                found: entry.epoch,
            });
        }
        let protocol = MlsMessageIn::tls_deserialize_exact(entry.payload.as_slice())
            .map_err(|_| ControlError::InvalidCiphertext("proposal is not a valid MLS message"))?
            .try_into_protocol_message()
            .map_err(|_| ControlError::InvalidCiphertext("expected an MLS protocol message"))?;
        if protocol.content_type() != ContentType::Proposal {
            return Err(ControlError::UnsupportedEntry(
                "entry is not an MLS standalone proposal",
            ));
        }
        // 现有提案（发送者 + 提案内容）：用于去重（重复提案不重复入队）。
        // openmls 的 proposal_reference 是 crate 私有 API，这里用公开的 sender()/proposal() 比较。
        let existing_proposals: Vec<(Sender, Proposal)> = self
            .group_ref()
            .map_err(|_| ControlError::GroupNotInitialized)?
            .proposal_store()
            .proposals()
            .map(|stored| (stored.sender().clone(), stored.proposal().clone()))
            .collect();
        let processed = {
            let (provider, _signer, group) = self
                .control_parts_mut()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            group.process_message(provider, protocol)
        }
        .map_err(|_| {
            // 提案认证失败不改变 epoch，也不写入本地 store；不停止群分支。
            ControlError::InvalidCiphertext("MLS proposal failed authentication or decryption")
        })?;
        if processed.epoch().as_u64() != entry.epoch {
            return Err(ControlError::InvalidCiphertext(
                "authenticated MLS epoch does not match the proposal entry epoch",
            ));
        }
        if processed.credential().serialized_content() != entry.sender_identity.as_bytes() {
            return Err(ControlError::InvalidCiphertext(
                "entry sender does not match the authenticated MLS sender",
            ));
        }
        let by = entry.sender_identity.clone();
        let ProcessedMessageContent::ProposalMessage(proposal) = processed.into_content() else {
            return Err(ControlError::UnsupportedEntry(
                "entry is not a standalone proposal message",
            ));
        };
        let duplicate = existing_proposals.iter().any(|(sender, stored)| {
            *sender == *proposal.sender() && stored == proposal.proposal()
        });
        if duplicate {
            return Err(ControlError::DuplicateProposal);
        }
        // 只支持 Remove 提案（自移除提案也是 Remove，目标是作者本机）。
        let target_leaf = match proposal.proposal() {
            Proposal::Remove(remove) => remove.removed(),
            _ => {
                return Err(ControlError::UnsupportedEntry(
                    "only remove proposals are supported in the P0 control slice",
                ));
            }
        };
        let target = {
            let group = self
                .group_ref()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            group
                .members()
                .find(|member| member.index == target_leaf)
                .map(|member| {
                    String::from_utf8_lossy(member.credential.serialized_content()).into_owned()
                })
        }
        .ok_or(ControlError::StateCorrupted(
            "remove proposal targets a leaf that is not a group member",
        ))?;
        // 入队（提案进 proposal store 与 provider storage；epoch 不变）。
        {
            let (provider, _signer, group) = self
                .control_parts_mut()
                .map_err(|_| ControlError::GroupNotInitialized)?;
            group
                .store_pending_proposal(provider.storage(), *proposal)
                .map_err(|error| crypto_error("store pending proposal", error))?;
        }
        let proposals_pending = self.pending_proposal_count()?;
        Ok(ProposalOutcome {
            kind: ProposalKind::Remove,
            by,
            epoch: entry.epoch,
            targets: vec![target],
            proposals_pending,
        })
    }

    /// CRYPTO-009：用 Welcome 加入群、核对绑定并立即持久化。
    ///
    /// 拒绝时清理刚写入 provider 的群状态，不留下半加入的群；被拒绝的 Welcome 已经
    /// 消耗本机 KeyPackage 私有材料，必须重新获取 KeyPackage 与邀请。
    pub fn join_from_welcome(
        &mut self,
        welcome: &[u8],
        acceptance: &WelcomeAcceptance,
        store: &mut FormalStore,
    ) -> Result<JoinedGroup, ControlError> {
        if self.group_ref().is_ok() {
            return Err(ControlError::GroupAlreadyInitialized);
        }
        if welcome.is_empty() || welcome.len() > MAX_WELCOME_BYTES {
            return Err(ControlError::WelcomeNotAuthorized(
                "welcome length is outside the supported bounds",
            ));
        }
        let local_identity = self
            .credential_handle()
            .credential
            .serialized_content()
            .to_vec();
        let (group, group_id, epoch, members) = {
            let provider = self.provider();
            let parsed = MlsMessageIn::tls_deserialize_exact(welcome).map_err(|_| {
                ControlError::WelcomeNotAuthorized("welcome is not a valid MLS message")
            })?;
            let MlsMessageBodyIn::Welcome(welcome) = parsed.extract() else {
                return Err(ControlError::WelcomeNotAuthorized(
                    "payload is not an MLS Welcome",
                ));
            };
            let config = MlsGroupJoinConfig::builder()
                .use_ratchet_tree_extension(true)
                .build();
            let staged = StagedWelcome::new_from_welcome(provider, &config, welcome, None)
                .map_err(|_| {
                    ControlError::WelcomeNotAuthorized(
                        "welcome cannot be staged for this device (used or foreign key package)",
                    )
                })?;
            let group = staged.into_group(provider).map_err(|_| {
                ControlError::WelcomeNotAuthorized("welcome cannot be joined by this device")
            })?;
            let group_id = hex_lower(group.group_id().as_slice());
            let epoch = group.epoch().as_u64();
            let members: Vec<String> = group
                .members()
                .map(|member| {
                    String::from_utf8_lossy(member.credential.serialized_content()).into_owned()
                })
                .collect();
            let mut expected = acceptance.members.clone();
            expected.sort();
            expected.dedup();
            let mut actual = members.clone();
            actual.sort();
            actual.dedup();
            let local_is_member = members
                .iter()
                .any(|identity| identity.as_bytes() == local_identity.as_slice());
            let authorized = group_id == acceptance.group_id
                && epoch == acceptance.expected_epoch
                && !expected.is_empty()
                && actual == expected
                && local_is_member;
            if !authorized {
                // 拒绝时移除刚写入 provider 的群状态，不留半加入的群。
                let mut group = group;
                let _ = group.delete(provider.storage());
                return Err(ControlError::WelcomeNotAuthorized(
                    "welcome does not match the verified group binding",
                ));
            }
            (group, group_id, epoch, members)
        };
        self.set_group(group);
        let exported = self.export_to_store(store).map_err(storage_error)?;
        Ok(JoinedGroup {
            group_id,
            epoch,
            members,
            exported,
        })
    }
}

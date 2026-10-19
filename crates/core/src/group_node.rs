//! GN 包：控制提交先拿**节点回执**、再接受合并（GROUPS-003 的节点侧闭环）。
//!
//! 背景：在此之前 `groups::accept_pending_control` 只用**本地持久 pending** 作为接受证据
//! （groups.md 明确写着“本地接受 ≠ 节点回执”）。本模块把节点回执接进接受路径：
//!
//! 1. [`submit_pending_control`]：把库里 pending 的 MLS Commit 作为**控制候选条目**提交到节点
//!    （`kind = control`，正文就是序列化的 Commit），返回节点签名回执；
//! 2. [`accept_pending_control_with_receipt`]：先校验回执确实绑定**这一个** pending 提交
//!    （group / submission / candidate 摘要 / seq / log_hash / kind / 密文逐字节一致 / 回执签名），
//!    通过后才调用既有 `groups::accept_pending_control` 合并；不通过显式失败且**不推进 epoch**。
//!
//! 同步语义：本 crate 的公开 API 是同步的（FFI 侧不能假设调用者在 async 上下文里），
//! 因此这里用 `std::thread::scope` + 独立 current-thread 运行时把异步客户端调用桥成同步：
//! 每次治理提交起一个短命线程、跑完即回收；即使调用方本身在一个 tokio 运行时里也不会冲突。
//! 控制提交是低频人为操作，这个代价可接受（见 groups.md 的限制说明）。
//!
//! 已验证的边界：提交幂等（同一 pending 重试只会返回同一回执，不产生第二条日志）、
//! 伪造/错绑定回执一律拒绝、节点不可达显式 `NODE_UNAVAILABLE` 且本地状态不变。
use std::fmt;

use chat_crypto::control::{ControlOperationKind, PendingControl};
use chat_domain::MemberRole;
use chat_crypto::engine::MlsDevice;
use chat_node_client::{NodeClient, NodeClientError, NodeClientErrorKind};
use chat_node_services::canonical::{hex_decode, hex_encode, sha256_concat};
use chat_node_services::group::{
    AppendResult, CandidateControlMetadata, GroupCandidate, KIND_APPLICATION, KIND_CONTROL,
    SignedLogHead,
    log_chain_hash,
};
use chat_protocol::{ErrorCode, GroupId, SubmissionId};
use chat_storage::formal::Store;

use crate::groups::{self, GroupError, GroupView};

/// 控制提交标识的派生域：同一 (群, 提交摘要, base_epoch) 恒得同一个 SubmissionId。
/// 这让“重复提交同一个 pending”在节点侧天然幂等（NODE-006 按 submission 去重）。
const CONTROL_SUBMISSION_DOMAIN: &[u8] = b"chat/core/control-submission/v1";
/// 节点允许的单条候选密文上限（与节点默认策略 `max_candidate_ciphertext_bytes` 一致）；
/// 超过时本地先拒绝，避免把明显不会被接受的候选送到节点。
const MAX_CONTROL_CANDIDATE_BYTES: usize = 16 * 1024;
/// 对账时最多翻多少页节点日志（每页上限取节点策略允许的最大值 50）。
const MAX_RECONCILE_PAGES: usize = 20;
const RECONCILE_PAGE_LIMIT: u32 = 50;

/// 控制提交/回执路径的稳定错误（码取自 PROTOCOL-014 冻结表或既有层，不新造协议码）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlReceiptError {
    code: &'static str,
    detail: String,
}

impl ControlReceiptError {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    /// 稳定错误码字符串。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 脱敏说明（不含正文、密钥或节点内部细节）。
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for ControlReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for ControlReceiptError {}

impl From<GroupError> for ControlReceiptError {
    fn from(error: GroupError) -> Self {
        Self::new(error.code(), error.detail().to_owned())
    }
}

impl From<NodeClientError> for ControlReceiptError {
    fn from(error: NodeClientError) -> Self {
        // 传输/超时在本层语义就是“节点这次没拿到提交”，统一映射成 NODE_UNAVAILABLE；
        // 其余错误保留客户端已分类的稳定码（权限/冲突/协议违规等）。
        let code = match error.kind() {
            NodeClientErrorKind::Network | NodeClientErrorKind::Timeout => {
                ErrorCode::NodeUnavailable.as_str()
            }
            _ => error.code().as_str(),
        };
        Self::new(code, "the node did not accept this control submission")
    }
}

/// 控制提交结果类型。
pub type ControlReceiptResult<T> = Result<T, ControlReceiptError>;

/// 一次控制提交的**完整证据**：节点回执 + 被提交的候选原文 + 提交时的节点 head 快照。
///
/// 接受时必须同时提供这三部分，才能证明“回执绑定的就是这个 pending 提交”
/// （只看回执字段无法排除“拿 A 的回执去接受 B 的提交”）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlReceipt {
    /// 节点签名回执（seq / candidate_hash / previous_hash / log_hash / 签名）。
    pub receipt: AppendResult,
    /// 实际提交的候选原文（kind = control，正文是 pending 的 Commit）。
    pub candidate: GroupCandidate,
    /// 提交时读到的节点 head（CAS 依据与节点身份来源）。
    pub head: SignedLogHead,
}

impl ControlReceipt {
    /// 节点分配的提交标识（PROTOCOL-001 submission 文本）。
    pub fn submission_id(&self) -> &str {
        &self.receipt.submission_id
    }

    /// 节点日志序号。
    pub fn seq(&self) -> u64 {
        self.receipt.seq
    }

    /// 节点日志链摘要（该条目之后的 head 值）。
    pub fn log_hash(&self) -> &str {
        &self.receipt.log_hash
    }

    /// 候选正文（= 被提交的 Commit 字节）。
    pub fn commit_bytes(&self) -> ControlReceiptResult<Vec<u8>> {
        chat_node_services::canonical::decode_hex_bytes(
            "control candidate payload",
            &self.candidate.encrypted_payload,
            MAX_CONTROL_CANDIDATE_BYTES,
        )
        .map_err(|error| ControlReceiptError::new(error.code(), "control candidate payload"))
    }
}

/// 把库里 pending 的控制提交作为控制候选条目提交到节点（**兼容入口**）。
///
/// 它只在能从本地快照**证明唯一语义**时提交：追加成员 → `invite`、移除成员 → `remove`、
/// 本地仍是群主的 `SelfUpdate` → `set_role`（本切片里 `SelfUpdate` 的另一产出者是转让，
/// 而转让会把发起人降级，所以“仍是群主”排除了它）。
///
/// 其余情形（例如转让后的 `SelfUpdate`、或未来新增的 `SelfUpdate` 治理操作）**显式失败**
/// （`PROTOCOL_UNSUPPORTED`），要求调用方改用 `submit_pending_control_with_intent` 声明语义——
/// 绝不退化成“看起来像 set_role”。
pub fn submit_pending_control(
    device: &MlsDevice,
    store: &Store,
    group_id: &str,
    client: &NodeClient,
) -> ControlReceiptResult<ControlReceipt> {
    let view = groups::group_snapshot(store, group_id)?;
    let pending = require_pending(device)?;
    let intent = infer_control_intent(&view, &pending)?;
    submit_with_intent(device, store, group_id, client, &intent)
}

/// 与 `submit_pending_control` 相同，但由调用方**显式声明**提交语义（转让/退出/解散必须走这里）。
pub fn submit_pending_control_with_intent(
    device: &MlsDevice,
    store: &Store,
    group_id: &str,
    client: &NodeClient,
    intent: &ControlIntent,
) -> ControlReceiptResult<ControlReceipt> {
    submit_with_intent(device, store, group_id, client, intent)
}

fn submit_with_intent(
    device: &MlsDevice,
    store: &Store,
    group_id: &str,
    client: &NodeClient,
    intent: &ControlIntent,
) -> ControlReceiptResult<ControlReceipt> {
    GroupId::parse(group_id).map_err(|_| {
        ControlReceiptError::new(ErrorCode::InvalidId.as_str(), "group id must be PROTOCOL-001 group text")
    })?;
    // 本地证据：快照 + 内存设备上的 pending 必须互相吻合。
    let view = groups::group_snapshot(store, group_id)?;
    let pending = require_pending(device)?;
    check_binds_group(&view, &pending)?;
    if pending.commit.len() > MAX_CONTROL_CANDIDATE_BYTES {
        return Err(ControlReceiptError::new(
            ErrorCode::ContentTooLarge.as_str(),
            "the control commit exceeds the node candidate limit",
        ));
    }
    // 公开元数据：先解析语义，任何不符都在触网之前失败。
    let metadata = resolve_control_metadata(&view, &pending, intent, client.device_id())?;
    // 节点 head：候选的 CAS 值只来自这里（含节点身份，用于事后复核回执）。
    let head = fetch_head(client, group_id)?;
    if head.group_id != group_id {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidReceiptScope.as_str(),
            "node head is for another group",
        ));
    }
    let submission_id = control_submission_id(group_id, &pending);
    let candidate = GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: submission_id.clone(),
        group_id: group_id.to_owned(),
        author_device_id: client.device_id().to_owned(),
        kind: KIND_CONTROL.to_owned(),
        expected_control_head: head.control_head.clone(),
        policy_version: head.policy_version,
        mls_epoch: head.mls_epoch,
        encrypted_payload: hex_encode(&pending.commit),
        // PROTOCOL-004 要求控制候选**必须**带公开元数据（缺了会被协议层判为非法候选）。
        control_metadata: Some(metadata),
    };
    match append_candidate(client, &candidate)? {
        Ok(receipt) => {
            verify_receipt_binding(group_id, &pending, &candidate, &receipt, &head)?;
            Ok(ControlReceipt {
                receipt,
                candidate,
                head,
            })
        }
        // 同一 SubmissionId 但候选字节不同 = 重试（节点 head 已因我们自己的条目前进）。
        // 这时按 submission 从节点日志**对账**取回原回执，而不是伪造成“又提交成功了一次”。
        Err(error) if error.node_code() == Some("SUBMISSION_ID_CONFLICT") => {
            let existing = reconcile_submission(client, group_id, &candidate.submission_id)?
                .ok_or_else(|| ControlReceiptError::from(error))?;
            let fresh_head = fetch_head(client, group_id)?;
            verify_receipt_binding(group_id, &pending, &existing.0, &existing.1, &fresh_head)?;
            Ok(ControlReceipt {
                receipt: existing.1,
                candidate: existing.0,
                head: fresh_head,
            })
        }
        Err(error) => Err(ControlReceiptError::from(error)),
    }
}

/// 取节点 head（异步客户端 → 同步桥）。
fn fetch_head(client: &NodeClient, group_id: &str) -> ControlReceiptResult<SignedLogHead> {
    block_on_node(client, |client| {
        Box::pin(async move {
            client
                .get_head(group_id)
                .await
                .map_err(ControlReceiptError::from)
        })
    })
}

/// 提交候选（保留客户端的原始错误：本层要按节点码区分“冲突 = 可对账”和真失败）。
///
/// 外层错误只会是桥本身失败（运行时/线程起不来），内层是节点的真实答复。
fn append_candidate(
    client: &NodeClient,
    candidate: &GroupCandidate,
) -> ControlReceiptResult<Result<AppendResult, NodeClientError>> {
    block_on_node(client, |client| {
        let candidate = candidate.clone();
        Box::pin(async move { Ok(client.append_entry(&candidate).await) })
    })
}

/// 按 submission 在节点日志里对账，取回（候选原文，回执）。
fn reconcile_submission(
    client: &NodeClient,
    group_id: &str,
    submission_id: &str,
) -> ControlReceiptResult<Option<(GroupCandidate, AppendResult)>> {
    let mut after = 0_u64;
    for _ in 0..MAX_RECONCILE_PAGES {
        let page = block_on_node(client, |client| {
            Box::pin(async move {
                client
                    .fetch_log(group_id, after, RECONCILE_PAGE_LIMIT)
                    .await
                    .map_err(ControlReceiptError::from)
            })
        })?;
        if let Some(entry) = page
            .entries
            .iter()
            .find(|entry| entry.receipt.submission_id == submission_id)
        {
            return Ok(Some((entry.candidate.candidate.clone(), entry.receipt.clone())));
        }
        if !page.has_more || page.next_after <= after {
            break;
        }
        after = page.next_after;
    }
    Ok(None)
}

/// 先校验回执绑定、再接受并合并该群当前 pending 控制提交。
///
/// 校验项（任一不满足都返回显式错误，**不推进 epoch、不清空 pending**）：
///
/// 1. 本地仍有 pending，且快照 epoch 停在 pending 的 base_epoch、群键一致；
/// 2. `receipt.group_id` / `candidate.group_id` 都等于本群，`receipt.submission_id` 等于候选的；
/// 3. `candidate.kind == control`，且候选正文**逐字节等于**本 pending 的 Commit（密文摘要一致）；
/// 4. `receipt.author_device_id` 等于候选作者（不接受代别人签的条目）；
/// 5. 回执与候选摘要、日志链自洽：`candidate_hash` 用本地候选重算，`log_hash` 按
///    `log_chain_hash(group, seq, previous_hash, candidate_hash)` 重算，两者都必须与回执一致；
/// 6. 回执节点身份与 head 快照一致，且回执签名在该节点公钥下可验证。
///
/// 全部通过后才调用 `groups::accept_pending_control`（既有合并语义：epoch 前进、pending 清空、
/// 策略快照写入，失败不留半更新）。
pub fn accept_pending_control_with_receipt(
    device: &mut MlsDevice,
    store: &mut Store,
    group_id: &str,
    receipt: &ControlReceipt,
) -> ControlReceiptResult<GroupView> {
    GroupId::parse(group_id).map_err(|_| {
        ControlReceiptError::new(ErrorCode::InvalidId.as_str(), "group id must be PROTOCOL-001 group text")
    })?;
    let view = groups::group_snapshot(store, group_id)?;
    let pending = require_pending(device)?;
    check_binds_group(&view, &pending)?;
    verify_receipt_binding(
        group_id,
        &pending,
        &receipt.candidate,
        &receipt.receipt,
        &receipt.head,
    )?;
    let accepted = groups::accept_pending_control(device, store, group_id)?;
    // 合并后必须真的落在目标 epoch 且 pending 清空；否则按损坏处理（不回滚已提交的合并）。
    if accepted.epoch != pending.target_epoch || accepted.pending.is_some() {
        return Err(ControlReceiptError::new(
            ErrorCode::StateCorrupted.as_str(),
            "the merged group state does not match the control commit target epoch",
        ));
    }
    Ok(accepted)
}

/// 取本机设备的 pending 控制提交（没有就是显式失败，绝不用“空提交”占位）。
fn require_pending(device: &MlsDevice) -> ControlReceiptResult<PendingControl> {
    device
        .pending_control()
        .map_err(|error| ControlReceiptError::new(error.code(), "pending control commit is unavailable"))?
        .ok_or_else(|| {
            ControlReceiptError::new(
                "PENDING_MISMATCH",
                "no control commit is persisted for this group",
            )
        })
}

/// 快照 epoch / MLS 群键必须与 pending 吻合（防止跨群或跨 epoch 的错配）。
fn check_binds_group(view: &GroupView, pending: &PendingControl) -> ControlReceiptResult<()> {
    if view.mls_group_key != pending.group_id {
        return Err(ControlReceiptError::new(
            ErrorCode::StateCorrupted.as_str(),
            "the pending control commit belongs to another MLS group",
        ));
    }
    if view.epoch != pending.base_epoch {
        return Err(ControlReceiptError::new(
            ErrorCode::StaleEpoch.as_str(),
            "the persisted group epoch does not match the pending control commit",
        ));
    }
    Ok(())
}

/// 控制提交的**公开语义声明**：决定节点元数据里写哪种操作。
///
/// 为什么必须由调用方声明：CRYPTO-005 的 `ControlOperationKind` 只有 `AddMembers` /
/// `RemoveMembers` / `SelfUpdate` 三种，而 `SelfUpdate` 同时承载改角色、转让群主、解散等多种
/// 治理语义。**节点元数据不能靠猜**——猜错会让公开日志把“转让群主”写成“改角色”。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlIntent {
    /// 追加成员（邀请）。无具体目标 → 目标摘要用提交摘要。
    Invite,
    /// 移除成员。给了目标凭据就用它的摘要，否则用提交摘要（旧行为）。
    RemoveMember { target_credential: Option<String> },
    /// 改角色。同上。
    SetRole { target_credential: Option<String> },
    /// 转让群主：目标凭据摘要 + 本地设备必须已被降级（证明这次提交确实是转让）。
    TransferOwnership { target_credential: String },
    /// 本机退出（自移除）：只有 `RemoveMembers` 且恰好移除 1 人时成立，目标 = 本机摘要。
    LeaveSelf,
    /// 解散群：映射为 `dissolve`（PROTOCOL-004 有该枚举）。
    Dissolve,
}

impl ControlIntent {
    /// 该声明在给定 pending 上是否成立（不成立就显式失败，绝不“宽容映射”）。
    fn compatible_with(&self, view: &GroupView, pending: &PendingControl) -> bool {
        match self {
            Self::Invite => pending.operation == ControlOperationKind::AddMembers,
            Self::RemoveMember { .. } => pending.operation == ControlOperationKind::RemoveMembers,
            Self::LeaveSelf => {
                pending.operation == ControlOperationKind::RemoveMembers && pending.removed == 1
            }
            Self::SetRole { .. } => pending.operation == ControlOperationKind::SelfUpdate,
            Self::TransferOwnership { .. } => {
                // 转让会把发起人降级：本地角色仍是群主就不可能是转让。
                pending.operation == ControlOperationKind::SelfUpdate
                    && view.role != MemberRole::Owner
            }
            Self::Dissolve => pending.operation == ControlOperationKind::SelfUpdate,
        }
    }

    /// 节点元数据里的操作文本（必须是节点 `control_operation` 认识的那 8 个之一）。
    fn node_operation(&self) -> &'static str {
        match self {
            Self::Invite => "invite",
            Self::RemoveMember { .. } | Self::LeaveSelf => "remove",
            Self::SetRole { .. } => "set_role",
            Self::TransferOwnership { .. } => "transfer_ownership",
            Self::Dissolve => "dissolve",
        }
    }

    /// 目标摘要：声明了目标凭据就用它的摘要，否则退回提交摘要（公开可算、不泄露身份）。
    fn target_digest(&self, pending: &PendingControl, local_device_id: &str) -> String {
        let credential = match self {
            Self::RemoveMember {
                target_credential: Some(credential),
            }
            | Self::SetRole {
                target_credential: Some(credential),
            }
            | Self::TransferOwnership {
                target_credential: credential,
            } => Some(credential.as_str()),
            // 自移除的目标就是本机设备。
            Self::LeaveSelf => Some(local_device_id),
            // 没有更具体的目标：用提交摘要（与旧行为一致，且不额外泄露）。
            _ => None,
        };
        match credential {
            Some(credential) => hex_encode(&control_target_digest(credential)),
            None => hex_encode(&pending.commit_digest),
        }
    }
}

/// 目标凭据摘要的派生域（公开层只留摘要，不泄露成员凭据原文）。
const CONTROL_TARGET_DOMAIN: &[u8] = b"chat/core/control-target/v1";

fn control_target_digest(credential: &str) -> [u8; 32] {
    sha256_concat(&[CONTROL_TARGET_DOMAIN, credential.as_bytes()])
}
// ---------------------------------------------------------------- 提案投递（GROUPS-013）

/// 用**持久身份**派生设备签名钥并构造节点客户端（端点/节点 ID/节点公钥全部显式）。
///
/// 身份未入库 → `INVALID_STATE_TRANSITION`；端点不可解析/公钥格式错 → `INVALID_TARGET`。
pub fn build_node_client(
    store: &Store,
    endpoint_url: &str,
    node_id: &str,
    node_public_key_hex: Option<&str>,
) -> ControlReceiptResult<NodeClient> {
    let seeds = store
        .load_identity_seeds()
        .map_err(|_| {
            ControlReceiptError::new(
                ErrorCode::StorageFailure.as_str(),
                "the persisted identity could not be read",
            )
        })?
        .ok_or_else(|| {
            ControlReceiptError::new(
                ErrorCode::InvalidStateTransition.as_str(),
                "the local identity is not enrolled in this database",
            )
        })?;
    let device_seed: [u8; 32] = seeds.device_seed.as_slice().try_into().map_err(|_| {
        ControlReceiptError::new(
            ErrorCode::StateCorrupted.as_str(),
            "the persisted device seed is not 32 bytes",
        )
    })?;
    let signing_key = chat_protocol::signing_key_from_seed(&device_seed);
    let mut endpoint = chat_node_client::NodeEndpoint::new(endpoint_url, node_id).map_err(|_| {
        ControlReceiptError::new(
            ErrorCode::InvalidTarget.as_str(),
            "node endpoint must be a parseable http(s) url",
        )
    })?;
    if let Some(hex_key) = node_public_key_hex {
        let key: [u8; 32] = hex_decode::<32>("node public key", hex_key.trim()).map_err(|_| {
            ControlReceiptError::new(
                ErrorCode::InvalidTarget.as_str(),
                "node public key must be 32 bytes of lowercase hex",
            )
        })?;
        endpoint = endpoint.with_node_public_key(key);
    }
    NodeClient::new(endpoint, seeds.device_id.clone(), signing_key).map_err(|_| {
        ControlReceiptError::new(
            ErrorCode::InvalidTarget.as_str(),
            "node client could not be created from the persisted device identity",
        )
    })
}

/// 提案条目的提交标识派生域。
const PROPOSAL_SUBMISSION_DOMAIN: &[u8] = b"chat/core/proposal-submission/v1";

/// 把独立提案作为**节点条目**提交，返回真实回执（绝不伪造）。
///
/// 语义：
/// - 候选以**应用条目**（kind = `application`）写入节点日志：节点不解析提案内容，也不为提案做
///   公开语义声明（PROTOCOL-004 没有“独立提案”的控制操作值）；提案的 MLS 认证由收侧
///   `groups::group_process_proposal` 完成；
/// - `submission_id` 由 (群, 提案摘要, base_epoch) 确定性派生，重复提交幂等（节点按提交 ID 去重）；
/// - CAS 字段只取自节点 head（不本地推断）；回执的节点身份与日志链绑定由本层复核。
pub fn submit_proposal_entry(
    client: &NodeClient,
    group_id: &str,
    proposal: &[u8],
    base_epoch: u64,
) -> ControlReceiptResult<ProposalSubmission> {
    GroupId::parse(group_id).map_err(|_| {
        ControlReceiptError::new(
            ErrorCode::InvalidId.as_str(),
            "group id must be PROTOCOL-001 group text",
        )
    })?;
    if proposal.is_empty() || proposal.len() > MAX_CONTROL_CANDIDATE_BYTES {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidCandidate.as_str(),
            "proposal bytes are outside the supported bounds",
        ));
    }
    let head = fetch_head(client, group_id)?;
    if head.group_id != group_id {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidReceiptScope.as_str(),
            "node head is for another group",
        ));
    }
    let digest = sha256_concat(&[
        PROPOSAL_SUBMISSION_DOMAIN,
        group_id.as_bytes(),
        &base_epoch.to_be_bytes(),
        proposal,
    ]);
    let candidate = GroupCandidate {
        protocol_major: 1,
        protocol_minor: 0,
        submission_id: SubmissionId::from_bytes(digest).encode(),
        group_id: group_id.to_owned(),
        author_device_id: client.device_id().to_owned(),
        kind: KIND_APPLICATION.to_owned(),
        expected_control_head: head.control_head.clone(),
        policy_version: head.policy_version,
        mls_epoch: head.mls_epoch,
        encrypted_payload: hex_encode(proposal),
        control_metadata: None,
    };
    let receipt = match append_candidate(client, &candidate)? {
        Ok(receipt) => receipt,
        // 同一提交 ID 的重复提交：按 submission 从节点日志对账取回原回执。
        Err(error) if error.node_code() == Some("SUBMISSION_ID_CONFLICT") => {
            reconcile_submission(client, group_id, &candidate.submission_id)?
                .map(|(_, receipt)| receipt)
                .ok_or_else(|| ControlReceiptError::from(error))?
        }
        Err(error) => return Err(ControlReceiptError::from(error)),
    };
    if receipt.group_id != group_id
        || receipt.submission_id != candidate.submission_id
        || receipt.author_device_id != candidate.author_device_id
    {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidReceiptScope.as_str(),
            "the receipt does not bind this proposal submission",
        ));
    }
    if receipt.node_id != head.node_id || receipt.node_public_key != head.node_public_key {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidReceiptScope.as_str(),
            "the receipt was issued by another node",
        ));
    }
    let node_public_key = hex_decode::<32>("node public key", &head.node_public_key)
        .map_err(|error| ControlReceiptError::new(error.code(), "node public key is malformed"))?;
    receipt
        .verify(&node_public_key)
        .map_err(|error| ControlReceiptError::new(error.code(), "the node receipt signature is invalid"))?;
    Ok(ProposalSubmission {
        submission_id: receipt.submission_id,
        seq: receipt.seq,
        log_hash: receipt.log_hash,
        node_id: receipt.node_id,
    })
}

/// 提案提交回执的公开投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalSubmission {
    /// 节点分配的提交标识。
    pub submission_id: String,
    /// 节点日志序号。
    pub seq: u64,
    /// 该条目之后的日志链摘要。
    pub log_hash: String,
    /// 节点身份。
    pub node_id: String,
}


/// 把“提交 + 公开语义声明”解析成节点元数据；不成立就显式失败。
///
/// 失败码：声明与提交形态不符 → `INVALID_CANDIDATE`；调用方**没有声明**（无 intent 的兼容入口
/// 推断不出唯一语义）→ `PROTOCOL_UNSUPPORTED`（显式“无法表达”，绝不退化成 set_role）。
pub fn resolve_control_metadata(
    view: &GroupView,
    pending: &PendingControl,
    intent: &ControlIntent,
    local_device_id: &str,
) -> ControlReceiptResult<CandidateControlMetadata> {
    if !intent.compatible_with(view, pending) {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidCandidate.as_str(),
            "the declared control intent is incompatible with the pending commit",
        ));
    }
    // 提交携带成员凭据：同步侧凭这些凭据把真实 MLS 增删写回成员集合（旧快照缺凭据时留空，
    // 由同步侧显式计为无法应用，绝不伪造身份）。
    let (added_credentials, removed_credentials) = submitted_credentials(view, pending, intent);
    Ok(CandidateControlMetadata {
        operation: intent.node_operation().to_owned(),
        target_digest: intent.target_digest(pending, local_device_id),
        added_credentials,
        removed_credentials,
    })
}

/// 从本地快照证明本次提交的新增/移除成员凭据。
///
/// 只有数量与 pending 计数完全一致时才填（否则返回空列表，保留旧的“无法证明”语义）。
fn submitted_credentials(
    view: &GroupView,
    pending: &PendingControl,
    intent: &ControlIntent,
) -> (Vec<String>, Vec<String>) {
    match intent {
        ControlIntent::Invite => {
            let added: Vec<String> = view
                .pending_members
                .iter()
                .filter(|member| member.pending)
                .map(|member| member.credential.clone())
                .collect();
            if u32::try_from(added.len()).ok() == Some(pending.added) {
                (added, Vec::new())
            } else {
                (Vec::new(), Vec::new())
            }
        }
        ControlIntent::RemoveMember { .. } | ControlIntent::LeaveSelf => {
            let removed: Vec<String> = view
                .pending_removed
                .iter()
                .map(|member| member.credential.clone())
                .collect();
            if u32::try_from(removed.len()).ok() == Some(pending.removed) {
                (Vec::new(), removed)
            } else {
                (Vec::new(), Vec::new())
            }
        }
        _ => (Vec::new(), Vec::new()),
    }
}

/// 无 intent 的兼容入口能推断出的语义（只覆盖**唯一可能**的形态，其余显式失败）。
fn infer_control_intent(
    view: &GroupView,
    pending: &PendingControl,
) -> ControlReceiptResult<ControlIntent> {
    match pending.operation {
        // 追加成员只有一个语义。
        ControlOperationKind::AddMembers => Ok(ControlIntent::Invite),
        // 移除成员只有一个语义（是否“自移除”无法从 pending 证明 → 不声明目标）。
        ControlOperationKind::RemoveMembers => Ok(ControlIntent::RemoveMember {
            target_credential: None,
        }),
        ControlOperationKind::SelfUpdate => {
            // 本地设备仍是群主：转让会把发起人降级，因此只可能是改角色（本切片 SelfUpdate 的
            // 另一种产出者是转让）。已经不是群主 → 无法区分“转让”与“管理员改角色”。
            if view.role == MemberRole::Owner {
                Ok(ControlIntent::SetRole {
                    target_credential: None,
                })
            } else {
                Err(ControlReceiptError::new(
                    ErrorCode::ProtocolUnsupported.as_str(),
                    "the control commit has no unambiguous public operation; declare the intent explicitly",
                ))
            }
        }
    }
}

/// 确定性提交标识：同一 pending 的重试必须得到同一个 SubmissionId（节点据此幂等去重）。
fn control_submission_id(group_id: &str, pending: &PendingControl) -> String {
    let digest = sha256_concat(&[
        CONTROL_SUBMISSION_DOMAIN,
        group_id.as_bytes(),
        &pending.base_epoch.to_be_bytes(),
        &pending.target_epoch.to_be_bytes(),
        &pending.commit_digest,
    ]);
    SubmissionId::from_bytes(digest).encode()
}

/// 回执必须绑定“这一个”候选与“这一条”日志。
fn verify_receipt_binding(
    group_id: &str,
    pending: &PendingControl,
    candidate: &GroupCandidate,
    receipt: &AppendResult,
    head: &SignedLogHead,
) -> ControlReceiptResult<()> {
    if candidate.kind != KIND_CONTROL {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidCandidate.as_str(),
            "the submitted candidate is not a control entry",
        ));
    }
    if candidate.group_id != group_id || receipt.group_id != group_id {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidReceiptScope.as_str(),
            "the receipt is for another group",
        ));
    }
    if receipt.submission_id != candidate.submission_id {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidReceiptScope.as_str(),
            "the receipt does not bind this submission",
        ));
    }
    if receipt.author_device_id != candidate.author_device_id {
        return Err(ControlReceiptError::new(
            ErrorCode::PermissionDenied.as_str(),
            "the receipt was issued for another author",
        ));
    }
    // 密文一致：候选正文必须逐字节等于本 pending 的 Commit。
    if candidate.encrypted_payload != hex_encode(&pending.commit) {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidCandidate.as_str(),
            "the submitted ciphertext is not this pending control commit",
        ));
    }
    // 节点身份：回执必须来自提交时看到的那个节点。
    if receipt.node_id != head.node_id || receipt.node_public_key != head.node_public_key {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidReceiptScope.as_str(),
            "the receipt was issued by another node",
        ));
    }
    let node_public_key = hex_decode::<32>("node public key", &head.node_public_key)
        .map_err(|error| ControlReceiptError::new(error.code(), "node public key is malformed"))?;
    receipt
        .verify(&node_public_key)
        .map_err(|error| ControlReceiptError::new(error.code(), "the node receipt signature is invalid"))?;
    // 候选摘要：用本地候选重算，必须等于回执里的 candidate_hash。
    let local_hash = candidate
        .to_unsigned(MAX_CONTROL_CANDIDATE_BYTES)
        .map_err(|error| ControlReceiptError::new(error.code(), "control candidate is malformed"))?
        .candidate_digest()
        .map_err(|_| {
            ControlReceiptError::new(
                ErrorCode::InvalidCandidate.as_str(),
                "control candidate digest could not be computed",
            )
        })?;
    if hex_encode(local_hash.as_bytes()) != receipt.candidate_hash {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidReceiptScope.as_str(),
            "the receipt does not bind this candidate",
        ));
    }
    // 日志链：seq + previous_hash + candidate_hash 必须重算出回执里的 log_hash。
    let previous_hash = hex_decode::<32>("previous hash", &receipt.previous_hash)
        .map_err(|error| ControlReceiptError::new(error.code(), "previous hash is malformed"))?;
    let mut candidate_hash = [0_u8; 32];
    hex_decode_into(local_hash.as_bytes(), &mut candidate_hash)?;
    let expected_log_hash = log_chain_hash(group_id, receipt.seq, &previous_hash, &candidate_hash);
    if hex_encode(&expected_log_hash) != receipt.log_hash {
        return Err(ControlReceiptError::new(
            ErrorCode::InvalidReceiptScope.as_str(),
            "the receipt log hash does not bind this entry",
        ));
    }
    Ok(())
}

fn hex_decode_into(source: &[u8], target: &mut [u8; 32]) -> ControlReceiptResult<()> {
    if source.len() != target.len() {
        return Err(ControlReceiptError::new(
            ErrorCode::StateCorrupted.as_str(),
            "candidate digest has an unexpected length",
        ));
    }
    target.copy_from_slice(source);
    Ok(())
}

/// 桥用 future 类型：显式带上客户端借用生命周期（否则无法把 `&NodeClient` 借进异步块）。
type NodeFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// 同步桥：在一个独立线程 + 独立 current-thread 运行时里跑异步客户端调用。
///
/// 用 `std::thread::scope` 借出 `&NodeClient`（不需要 `'static`、不需要 Clone），
/// 并且即使调用方线程本身在某个 tokio 运行时里也不会嵌套 panic。
fn block_on_node<'a, T>(
    client: &'a NodeClient,
    operation: impl FnOnce(&'a NodeClient) -> NodeFuture<'a, ControlReceiptResult<T>> + Send,
) -> ControlReceiptResult<T>
where
    T: Send + 'a,
{
    std::thread::scope(|scope| {
        let handle = scope.spawn(move || -> ControlReceiptResult<T> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| {
                    ControlReceiptError::new(
                        ErrorCode::NodeUnavailable.as_str(),
                        "the node bridge runtime could not be built",
                    )
                })?;
            runtime.block_on(operation(client))
        });
        handle.join().map_err(|_| {
            ControlReceiptError::new(
                ErrorCode::NodeUnavailable.as_str(),
                "the node bridge thread panicked",
            )
        })?
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat_protocol::DeviceId;

    fn pending(kind: ControlOperationKind, added: u32, removed: u32) -> PendingControl {
        PendingControl {
            group_id: "ab".repeat(16),
            base_epoch: 1,
            target_epoch: 2,
            operation: kind,
            added,
            removed,
            commit: vec![1, 2, 3, 4],
            welcome: None,
            commit_digest: [7_u8; 32],
        }
    }

    /// 只关心 `role`（转让的兼容性检查用它）。
    fn view(role: MemberRole) -> GroupView {
        GroupView {
            group_id: GroupId::from_bytes([0x11; 32]).encode(),
            name: "meta".into(),
            epoch: 1,
            policy_version: 2,
            control_head: "00".repeat(32),
            role,
            created_at_ms: 0,
            mls_group_key: "ab".repeat(16),
            state_version: 1,
            members: Vec::new(),
            pending_members: Vec::new(),
            pending_removed: Vec::new(),
            pending: None,
            last_control_cleared: None,
            dissolved: false,
            dissolved_at_ms: None,
            left_at_ms: None,
            leaving_at_ms: None,
            revoked_devices: Vec::new(),
        }
    }

    fn local() -> String {
        "dev:1:".to_owned() + &"55".repeat(32)
    }

    #[test]
    fn submission_ids_are_deterministic_and_bound_to_the_commit() {
        let group = "grp:1:".to_owned() + &"11".repeat(32);
        let first = control_submission_id(&group, &pending(ControlOperationKind::SelfUpdate, 0, 0));
        let again = control_submission_id(&group, &pending(ControlOperationKind::SelfUpdate, 0, 0));
        assert_eq!(first, again, "same pending must reuse the same submission id");
        assert!(SubmissionId::parse(&first).is_ok());
        let other_group = "grp:1:".to_owned() + &"22".repeat(32);
        assert_ne!(
            first,
            control_submission_id(&other_group, &pending(ControlOperationKind::SelfUpdate, 0, 0))
        );
        let mut moved = pending(ControlOperationKind::SelfUpdate, 0, 0);
        moved.base_epoch = 5;
        assert_ne!(first, control_submission_id(&group, &moved));
        let mut other_commit = pending(ControlOperationKind::SelfUpdate, 0, 0);
        other_commit.commit_digest = [9_u8; 32];
        assert_ne!(first, control_submission_id(&group, &other_commit));
    }

    /// 六种声明各自映射到哪个节点操作文本。
    #[test]
    fn intents_map_to_the_expected_node_operations() {
        let owner_view = view(MemberRole::Owner);
        let demoted_view = view(MemberRole::Member);
        let cases: Vec<(ControlIntent, &str, MemberRole, PendingControl)> = vec![
            (
                ControlIntent::Invite,
                "invite",
                MemberRole::Owner,
                pending(ControlOperationKind::AddMembers, 1, 0),
            ),
            (
                ControlIntent::RemoveMember {
                    target_credential: Some("bob".into()),
                },
                "remove",
                MemberRole::Owner,
                pending(ControlOperationKind::RemoveMembers, 0, 1),
            ),
            (
                ControlIntent::LeaveSelf,
                "remove",
                MemberRole::Member,
                pending(ControlOperationKind::RemoveMembers, 0, 1),
            ),
            (
                ControlIntent::SetRole {
                    target_credential: Some("bob".into()),
                },
                "set_role",
                MemberRole::Owner,
                pending(ControlOperationKind::SelfUpdate, 0, 0),
            ),
            (
                ControlIntent::TransferOwnership {
                    target_credential: "bob".into(),
                },
                "transfer_ownership",
                MemberRole::Member,
                pending(ControlOperationKind::SelfUpdate, 0, 0),
            ),
            (
                ControlIntent::Dissolve,
                "dissolve",
                MemberRole::Owner,
                pending(ControlOperationKind::SelfUpdate, 0, 0),
            ),
        ];
        for (intent, expected, role, pending) in cases {
            let constructed = match role {
                MemberRole::Owner => &owner_view,
                _ => &demoted_view,
            };
            let metadata = resolve_control_metadata(constructed, &pending, &intent, &local())
                .unwrap_or_else(|error| panic!("{intent:?} must resolve: {}", error.code()));
            assert_eq!(metadata.operation, expected, "intent {intent:?}");
        }
    }

    /// 目标摘要：声明了目标凭据就用它的摘要；自移除用本机；否则退回提交摘要。
    #[test]
    fn target_digests_bind_the_declared_target() {
        let owner_view = view(MemberRole::Owner);
        let self_update = pending(ControlOperationKind::SelfUpdate, 0, 0);
        let with_target = resolve_control_metadata(
            &owner_view,
            &self_update,
            &ControlIntent::SetRole {
                target_credential: Some("bob".into()),
            },
            &local(),
        )
        .expect("resolves");
        assert_eq!(
            with_target.target_digest,
            hex_encode(&control_target_digest("bob"))
        );
        let without_target = resolve_control_metadata(
            &owner_view,
            &self_update,
            &ControlIntent::SetRole { target_credential: None },
            &local(),
        )
        .expect("resolves");
        assert_eq!(without_target.target_digest, hex_encode(&self_update.commit_digest));
        // 目标不同 → 摘要不同（不可跨目标重放）。
        let other = resolve_control_metadata(
            &owner_view,
            &self_update,
            &ControlIntent::SetRole {
                target_credential: Some("carol".into()),
            },
            &local(),
        )
        .expect("resolves");
        assert_ne!(with_target.target_digest, other.target_digest);
        // 自移除的目标是本机设备摘要。
        let removal = pending(ControlOperationKind::RemoveMembers, 0, 1);
        let leave = resolve_control_metadata(
            &view(MemberRole::Member),
            &removal,
            &ControlIntent::LeaveSelf,
            &local(),
        )
        .expect("resolves");
        assert_eq!(leave.operation, "remove");
        assert_eq!(leave.target_digest, hex_encode(&control_target_digest(&local())));
    }

    /// 声明与提交形态不符 → 显式 INVALID_CANDIDATE（绝不硬套）。
    #[test]
    fn incompatible_intents_fail_explicitly() {
        let owner_view = view(MemberRole::Owner);
        let demoted_view = view(MemberRole::Member);
        let add = pending(ControlOperationKind::AddMembers, 1, 0);
        let remove = pending(ControlOperationKind::RemoveMembers, 0, 1);
        let self_update = pending(ControlOperationKind::SelfUpdate, 0, 0);
        let transfer = ControlIntent::TransferOwnership {
            target_credential: "bob".into(),
        };
        let cases: Vec<(&str, &ControlIntent, &PendingControl, &GroupView)> = vec![
            (
                "set_role on an add commit",
                &ControlIntent::SetRole { target_credential: None },
                &add,
                &owner_view,
            ),
            (
                "invite on a self update",
                &ControlIntent::Invite,
                &self_update,
                &owner_view,
            ),
            ("transfer on a remove commit", &transfer, &remove, &demoted_view),
            (
                "leave on a self update",
                &ControlIntent::LeaveSelf,
                &self_update,
                &demoted_view,
            ),
            (
                "dissolve on an add commit",
                &ControlIntent::Dissolve,
                &add,
                &owner_view,
            ),
        ];
        for (label, intent, pending, view) in cases {
            let error = resolve_control_metadata(view, pending, intent, &local())
                .expect_err(&format!("{label} must not resolve"));
            assert_eq!(error.code(), "INVALID_CANDIDATE", "{label}");
        }
        // 自移除要求恰好移除 1 人。
        let wide_removal = pending(ControlOperationKind::RemoveMembers, 0, 2);
        assert_eq!(
            resolve_control_metadata(
                &demoted_view,
                &wide_removal,
                &ControlIntent::LeaveSelf,
                &local()
            )
            .expect_err("two removals are not a self removal")
            .code(),
            "INVALID_CANDIDATE"
        );
        // 转让要求本地已被降级（本地仍是群主 → 不可能是一次转让）。
        assert_eq!(
            resolve_control_metadata(
                &owner_view,
                &self_update,
                &ControlIntent::TransferOwnership {
                    target_credential: "bob".into(),
                },
                &local()
            )
            .expect_err("the owner cannot transfer to themselves")
            .code(),
            "INVALID_CANDIDATE"
        );
    }

    /// 无 intent 的兼容入口：只在唯一形态下推断，转让后的提交**显式失败**。
    #[test]
    fn legacy_inference_never_degrades_a_transfer_to_set_role() {
        assert_eq!(
            infer_control_intent(&view(MemberRole::Owner), &pending(ControlOperationKind::AddMembers, 1, 0))
                .expect("invite"),
            ControlIntent::Invite
        );
        assert_eq!(
            infer_control_intent(
                &view(MemberRole::Owner),
                &pending(ControlOperationKind::RemoveMembers, 0, 1)
            )
            .expect("remove"),
            ControlIntent::RemoveMember {
                target_credential: None
            }
        );
        // 仍是群主的 self update：只可能是改角色（旧行为，兼容 group_submit_control）。
        assert_eq!(
            infer_control_intent(&view(MemberRole::Owner), &pending(ControlOperationKind::SelfUpdate, 0, 0))
                .expect("set_role"),
            ControlIntent::SetRole {
                target_credential: None
            }
        );
        // 已经被降级的 self update：无法区分“转让”与“管理员改角色” → 显式 UNSUPPORTED。
        let error = infer_control_intent(
            &view(MemberRole::Member),
            &pending(ControlOperationKind::SelfUpdate, 0, 0),
        )
        .expect_err("ambiguous self update must fail");
        assert_eq!(error.code(), "PROTOCOL_UNSUPPORTED");
    }

    /// 映射出来的元数据必须是 PROTOCOL-004 能编码的合法控制元数据。
    #[test]
    fn mapped_metadata_is_expressible_by_protocol_004() {
        let owner_view = view(MemberRole::Owner);
        let demoted_view = view(MemberRole::Member);
        let cases: Vec<(ControlIntent, MemberRole, PendingControl)> = vec![
            (
                ControlIntent::Invite,
                MemberRole::Owner,
                pending(ControlOperationKind::AddMembers, 1, 0),
            ),
            (
                ControlIntent::RemoveMember { target_credential: None },
                MemberRole::Owner,
                pending(ControlOperationKind::RemoveMembers, 0, 1),
            ),
            (
                ControlIntent::LeaveSelf,
                MemberRole::Member,
                pending(ControlOperationKind::RemoveMembers, 0, 1),
            ),
            (
                ControlIntent::SetRole { target_credential: None },
                MemberRole::Owner,
                pending(ControlOperationKind::SelfUpdate, 0, 0),
            ),
            (
                ControlIntent::TransferOwnership {
                    target_credential: "bob".into(),
                },
                MemberRole::Member,
                pending(ControlOperationKind::SelfUpdate, 0, 0),
            ),
            (
                ControlIntent::Dissolve,
                MemberRole::Owner,
                pending(ControlOperationKind::SelfUpdate, 0, 0),
            ),
        ];
        for (intent, role, pending) in cases {
            let constructed = if role == MemberRole::Owner {
                &owner_view
            } else {
                &demoted_view
            };
            let metadata = resolve_control_metadata(constructed, &pending, &intent, &local())
                .unwrap_or_else(|error| panic!("{intent:?} must resolve: {}", error.code()));
            let candidate = GroupCandidate {
                protocol_major: 1,
                protocol_minor: 0,
                submission_id: SubmissionId::from_bytes([0x22; 32]).encode(),
                group_id: GroupId::from_bytes([0x11; 32]).encode(),
                author_device_id: DeviceId::from_bytes([0x33; 32]).encode(),
                kind: KIND_CONTROL.to_owned(),
                expected_control_head: "00".repeat(32),
                policy_version: 1,
                mls_epoch: 0,
                encrypted_payload: hex_encode(b"commit"),
                control_metadata: Some(metadata),
            };
            candidate
                .to_unsigned(MAX_CONTROL_CANDIDATE_BYTES)
                .unwrap_or_else(|error| {
                    panic!("{intent:?} metadata must be protocol-expressible: {}", error.code())
                });
        }
    }
}


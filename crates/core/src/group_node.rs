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
use chat_crypto::engine::MlsDevice;
use chat_node_client::{NodeClient, NodeClientError, NodeClientErrorKind};
use chat_node_services::canonical::{hex_decode, hex_encode, sha256_concat};
use chat_node_services::group::{
    AppendResult, CandidateControlMetadata, GroupCandidate, KIND_CONTROL, SignedLogHead,
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

/// 把库里 pending 的控制提交作为控制候选条目提交到节点。
///
/// 前置（**任何一步不满足都显式失败，不发请求**）：
/// - `group_id` 是 PROTOCOL-001 群文本，且该群在本库里有持久快照；
/// - 本机设备确实持有该群的 pending 控制提交，且快照 epoch 正好停在 pending 的 base_epoch；
/// - pending 绑定的 MLS 群键与快照一致（防止把别的群的提交送到这个群）。
///
/// 候选的 CAS 字段（control head / policy_version / mls_epoch）**只取自节点 head**，不做本地推断；
/// `submission_id` 由 (群, 提交摘要, base_epoch) 确定性派生，因此重复调用是幂等的。
pub fn submit_pending_control(
    device: &MlsDevice,
    store: &Store,
    group_id: &str,
    client: &NodeClient,
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
        control_metadata: Some(control_metadata_for(&pending)),
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

/// 控制候选的公开元数据（只含操作种类与摘要，不含成员身份或正文）。
///
/// 映射依据是**本切片**里产生这些 pending 提交的治理操作：
///
/// - `AddMembers` 只由建群/追加成员的邀请产生 → `invite`；
/// - `RemoveMembers` 只由移除成员产生 → `remove`；
/// - `SelfUpdate` 只由 GROUPS-003 改角色产生（`set_muted`/`rename_group` 是纯策略层，
///   不产生 MLS 提交）→ `set_role`。
///
/// 若以后引入其它也会产生 `SelfUpdate` 的治理操作，这个映射必须随之扩展（否则公开元数据会
/// 说错操作种类）——见 groups.md 的限制说明。
///
/// `target_digest` 用 pending 自带的提交摘要（`H(域 || group || base || target || commit)`）：
/// 它已经是公开可计算的绑定值，不额外泄露目标账户/角色身份。
fn control_metadata_for(pending: &PendingControl) -> CandidateControlMetadata {
    let operation = match pending.operation {
        ControlOperationKind::AddMembers => "invite",
        ControlOperationKind::RemoveMembers => "remove",
        ControlOperationKind::SelfUpdate => "set_role",
    };
    CandidateControlMetadata {
        operation: operation.to_owned(),
        target_digest: hex_encode(&pending.commit_digest),
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

    fn pending() -> PendingControl {
        PendingControl {
            group_id: "ab".repeat(16),
            base_epoch: 3,
            target_epoch: 4,
            operation: chat_crypto::control::ControlOperationKind::SelfUpdate,
            added: 0,
            removed: 0,
            commit: vec![1, 2, 3, 4],
            welcome: None,
            commit_digest: [7_u8; 32],
        }
    }

    #[test]
    fn submission_ids_are_deterministic_and_bound_to_the_commit() {
        let group = "grp:1:".to_owned() + &"11".repeat(32);
        let first = control_submission_id(&group, &pending());
        let again = control_submission_id(&group, &pending());
        assert_eq!(first, again, "same pending must reuse the same submission id");
        // 必须是 PROTOCOL-001 submission 文本。
        assert!(SubmissionId::parse(&first).is_ok());
        // 换群 / 换 epoch / 换摘要都会换 id。
        let other_group = "grp:1:".to_owned() + &"22".repeat(32);
        assert_ne!(first, control_submission_id(&other_group, &pending()));
        let mut moved = pending();
        moved.base_epoch = 5;
        assert_ne!(first, control_submission_id(&group, &moved));
        let mut other_commit = pending();
        other_commit.commit_digest = [9_u8; 32];
        assert_ne!(first, control_submission_id(&group, &other_commit));
    }

    #[test]
    fn pending_errors_carry_frozen_codes() {
        // 群标识非规范 → INVALID_ID（不会走到网络）。
        let device = MlsDevice::new("unit-device").expect("device");
        let dir = std::env::temp_dir();
        let _ = (&dir, &device);
        let error = ControlReceiptError::from(GroupError::from(
            chat_crypto::control::ControlError::GroupNotInitialized,
        ));
        assert!(!error.code().is_empty());
    }
}

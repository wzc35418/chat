//! PROTOCOL-005/007：候选外层验签与日志条目哈希链校验。
//!
//! 边界：
//!
//! - `verify_candidate` 只验证「外层作者签名 + 字段绑定」；MLS 内层解密/权限验证仍必需，
//!   因此输出类型 `VerifiedCandidate` 不会被当作已认证消息。
//! - `verify_log_entry` 只验证节点回执与日志哈希链连续性；业务权限与 MLS 结果由上层负责，
//!   未通过前不得提交本地游标。
//! - 两个函数都是纯验证：不写正式群状态、不产生部分结果。

use crate::canonical::{ProtocolVersion, WireField, WireValue, encode_canonical, field};
use crate::digest::{ControlHead, Digest};
use crate::error::{ProtocolError, Result};
use crate::ids::{AccountId, CoordinatorId, DeviceId, GroupId};
use crate::signing::{
    CandidateKind, UnsignedCandidate, candidate_signing_bytes, receipt_signing_bytes,
    verify_ed25519,
};

/// 日志条目哈希用途域。
pub const LOG_ENTRY_HASH_DOMAIN: &[u8] = b"chat/p1/log-entry-hash/v1";
/// 创世日志序号（尚无条目）。
pub const GENESIS_SEQUENCE: u64 = 0;
const SCHEMA_VERSION: u64 = 1;

/// 已由上层验证过的设备凭据。
///
/// 上层（identity）必须先完成账户根、设备绑定、期限与撤销检查，再构造本类型；
/// `revoked` 为 `true` 时任何候选都不允许通过（fail closed）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedCredential {
    /// 账户。
    pub account_id: AccountId,
    /// 设备。
    pub device_id: DeviceId,
    /// 设备签名公钥（Ed25519）。
    pub device_public_key: [u8; 32],
    /// 设备授权版本。
    pub credential_version: u64,
    /// 是否已撤销。
    pub revoked: bool,
}

/// 候选必须匹配的本机群上下文。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupContext {
    /// 群。
    pub group_id: GroupId,
    /// 当前正式控制头。
    pub control_head: ControlHead,
    /// 当前策略版本。
    pub policy_version: u64,
    /// 当前 MLS epoch。
    pub mls_epoch: u64,
}

/// 带作者签名的完整候选。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedCandidate {
    /// 未签名部分。
    pub unsigned: UnsignedCandidate,
    /// 作者设备签名（64 字节 Ed25519）。
    pub author_signature: [u8; 64],
}

/// 外层验签通过的候选。构造入口只有 [`verify_candidate`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedCandidate {
    unsigned: UnsignedCandidate,
    author_account: AccountId,
    candidate_digest: Digest,
}

impl VerifiedCandidate {
    /// 未签名候选内容。
    pub const fn unsigned(&self) -> &UnsignedCandidate {
        &self.unsigned
    }

    /// 凭据确认的作者账户。
    pub const fn author_account(&self) -> AccountId {
        self.author_account
    }

    /// 候选作者设备。
    pub const fn author_device(&self) -> DeviceId {
        self.unsigned.author_device_id
    }

    /// 目标群。
    pub const fn group_id(&self) -> GroupId {
        self.unsigned.group_id
    }

    /// 候选类别。
    pub const fn kind(&self) -> CandidateKind {
        self.unsigned.kind
    }

    /// 固定密文提交 ID。
    pub const fn submission_id(&self) -> crate::ids::SubmissionId {
        self.unsigned.submission_id
    }

    /// 期望控制头。
    pub const fn expected_control_head(&self) -> ControlHead {
        self.unsigned.expected_control_head
    }

    /// 期望策略版本。
    pub const fn policy_version(&self) -> u64 {
        self.unsigned.policy_version
    }

    /// 期望 MLS epoch。
    pub const fn mls_epoch(&self) -> u64 {
        self.unsigned.mls_epoch
    }

    /// 密文。
    pub fn encrypted_payload(&self) -> &[u8] {
        &self.unsigned.encrypted_payload
    }

    /// 候选全文摘要（节点回执与日志链使用）。
    pub const fn candidate_digest(&self) -> Digest {
        self.candidate_digest
    }
}

/// PROTOCOL-005：验证候选外层作者签名与字段绑定。
///
/// 顺序：撤销检查 → 设备绑定 → 版本 → 群绑定 → 控制状态/epoch 绑定 →
/// 作者签名 → 控制元数据单操作（由规范编码校验）。
/// 设备绑定、群绑定、签名篡改都映射 `INVALID_SIGNATURE`；控制状态陈旧使用冻结的
/// `STALE_CONTROL_HEAD`/`STALE_EPOCH`，让调用方先同步而不是把陈旧当作伪造。
pub fn verify_candidate(
    candidate: SignedCandidate,
    credential: VerifiedCredential,
    context: GroupContext,
) -> Result<VerifiedCandidate> {
    if credential.revoked {
        return Err(ProtocolError::DeviceRevoked);
    }
    let unsigned = candidate.unsigned;
    // 设备与账户绑定：候选只能由凭据确认的设备签名。
    if credential.device_id != unsigned.author_device_id {
        return Err(ProtocolError::InvalidSignature);
    }
    if !unsigned.protocol_version.is_supported() {
        return Err(ProtocolError::UnsupportedProtocolVersion);
    }
    // 群绑定：同一签名不能重放到其它群。
    if unsigned.group_id != context.group_id {
        return Err(ProtocolError::InvalidSignature);
    }
    if unsigned.expected_control_head != context.control_head
        || unsigned.policy_version != context.policy_version
    {
        return Err(ProtocolError::StaleControlHead);
    }
    if unsigned.mls_epoch != context.mls_epoch {
        return Err(ProtocolError::StaleEpoch);
    }
    let signing_bytes = candidate_signing_bytes(&unsigned)?;
    verify_ed25519(
        &credential.device_public_key,
        signing_bytes.as_bytes(),
        &candidate.author_signature,
    )?;
    Ok(VerifiedCandidate {
        candidate_digest: Digest::sha256(signing_bytes.as_bytes()),
        author_account: credential.account_id,
        unsigned,
    })
}

/// 本机已验证日志头。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedHead {
    /// 群。
    pub group_id: GroupId,
    /// 当前控制头。
    pub control_head: ControlHead,
    /// 当前策略版本。
    pub policy_version: u64,
    /// 当前 MLS epoch。
    pub mls_epoch: u64,
    /// 已验证日志序号；创世为 0。
    pub log_sequence: u64,
    /// 已验证日志链尾摘要；创世为全零。
    pub log_hash: Digest,
}

impl VerifiedHead {
    /// 创世头：无条目、全零链尾。
    pub const fn genesis(
        group_id: GroupId,
        control_head: ControlHead,
        policy_version: u64,
        mls_epoch: u64,
    ) -> Self {
        Self {
            group_id,
            control_head,
            policy_version,
            mls_epoch,
            log_sequence: GENESIS_SEQUENCE,
            log_hash: Digest::ZERO,
        }
    }
}

/// 固定的协调节点公钥。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinnedNodeKey {
    /// 节点身份。
    pub coordinator_id: CoordinatorId,
    /// 已固定信任的 Ed25519 公钥。
    pub public_key: [u8; 32],
}

/// 节点已接受但尚未被本机验证的日志条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedLogEntry {
    /// 已通过 [`verify_candidate`] 的候选（作者验签已发生）。
    pub candidate: VerifiedCandidate,
    /// 节点分配的群序号（从 1 开始）。
    pub group_sequence: u64,
    /// 前一条日志摘要（创世为全零）。
    pub previous_log_hash: Digest,
    /// 节点回执签名。
    pub coordinator_signature: [u8; 64],
}

impl AcceptedLogEntry {
    /// 由已验证候选构造条目（作者验签由类型保证）。
    pub fn from_verified_candidate(
        candidate: VerifiedCandidate,
        group_sequence: u64,
        previous_log_hash: Digest,
        coordinator_signature: [u8; 64],
    ) -> Self {
        Self {
            candidate,
            group_sequence,
            previous_log_hash,
            coordinator_signature,
        }
    }
}

/// 验证通过的日志条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedLogEntry {
    entry: AcceptedLogEntry,
    log_hash: Digest,
}

impl VerifiedLogEntry {
    /// 原始条目。
    pub const fn entry(&self) -> &AcceptedLogEntry {
        &self.entry
    }

    /// 候选全文摘要。
    pub const fn candidate_digest(&self) -> Digest {
        self.entry.candidate.candidate_digest()
    }

    /// 群序号。
    pub const fn group_sequence(&self) -> u64 {
        self.entry.group_sequence
    }

    /// 前序日志摘要。
    pub const fn previous_log_hash(&self) -> Digest {
        self.entry.previous_log_hash
    }

    /// 本条日志链摘要；下一条的 `previous_log_hash` 必须等于它。
    pub const fn log_hash(&self) -> Digest {
        self.log_hash
    }

    /// 本条生效后的链尾摘要。
    pub const fn next_previous_log_hash(&self) -> Digest {
        self.log_hash
    }
}

/// 冻结的日志条目摘要：覆盖群、序号、前序摘要、候选摘要与节点签名。
pub fn log_entry_hash(
    group: GroupId,
    group_sequence: u64,
    previous_log_hash: Digest,
    candidate_digest: Digest,
    coordinator_signature: &[u8; 64],
) -> Result<Digest> {
    if group_sequence == GENESIS_SEQUENCE {
        return Err(ProtocolError::InvalidSequence);
    }
    let body = WireValue::structure([
        WireField::new(field::SCHEMA_VERSION, WireValue::U64(SCHEMA_VERSION)),
        WireField::new(field::GROUP_ID, WireValue::id_bytes(group.as_bytes())),
        WireField::new(field::RECEIPT_SEQ, WireValue::U64(group_sequence)),
        WireField::new(
            field::PREVIOUS_LOG_HASH,
            WireValue::digest(previous_log_hash),
        ),
        WireField::new(field::CANDIDATE_DIGEST, WireValue::digest(candidate_digest)),
        WireField::new(
            field::LOG_COORDINATOR_SIGNATURE,
            WireValue::Bytes(coordinator_signature.to_vec()),
        ),
    ]);
    let encoded = encode_canonical(&body, ProtocolVersion::V1)?;
    let mut preimage = Vec::with_capacity(LOG_ENTRY_HASH_DOMAIN.len() + encoded.len());
    preimage.extend_from_slice(LOG_ENTRY_HASH_DOMAIN);
    preimage.extend_from_slice(encoded.as_bytes());
    Ok(Digest::sha256(&preimage))
}

/// PROTOCOL-007：验证日志条目回执与哈希链连续性。
///
/// 顺序：群绑定 → 控制状态/epoch 绑定 → 序号连续性（跳号 `MISSING_LOG_ENTRIES`、
/// 重复序号 `LOG_CONFLICT`）→ 前序哈希连续 → 用固定节点公钥验证回执 →
/// 计算本条目链摘要。任何失败都不返回部分验证结果。
pub fn verify_log_entry(
    entry: AcceptedLogEntry,
    expected: &VerifiedHead,
    node_key: &PinnedNodeKey,
) -> Result<VerifiedLogEntry> {
    // 群绑定：条目签名不能跨群复用。
    if entry.candidate.group_id() != expected.group_id {
        return Err(ProtocolError::InvalidSignature);
    }
    if entry.candidate.expected_control_head() != expected.control_head
        || entry.candidate.policy_version() != expected.policy_version
    {
        return Err(ProtocolError::StaleControlHead);
    }
    if entry.candidate.mls_epoch() != expected.mls_epoch {
        return Err(ProtocolError::StaleEpoch);
    }
    // 序号连续性。
    let expected_sequence = expected
        .log_sequence
        .checked_add(1)
        .ok_or(ProtocolError::InvalidSequence)?;
    if entry.group_sequence > expected_sequence {
        return Err(ProtocolError::MissingLogEntries);
    }
    if entry.group_sequence < expected_sequence {
        return Err(ProtocolError::LogConflict);
    }
    // 前序哈希连续：创世条目必须引用全零头，其后必须引用本机已验证链尾。
    if expected.log_sequence == GENESIS_SEQUENCE {
        if entry.previous_log_hash != Digest::ZERO {
            return Err(ProtocolError::LogConflict);
        }
    } else if entry.previous_log_hash != expected.log_hash {
        return Err(ProtocolError::LogConflict);
    }
    // 节点回执：只信任固定公钥。
    let receipt = receipt_signing_bytes(
        entry.candidate.candidate_digest(),
        entry.group_sequence,
        entry.previous_log_hash,
        expected.group_id,
    )?;
    verify_ed25519(
        &node_key.public_key,
        receipt.as_bytes(),
        &entry.coordinator_signature,
    )?;
    let log_hash = log_entry_hash(
        expected.group_id,
        entry.group_sequence,
        entry.previous_log_hash,
        entry.candidate.candidate_digest(),
        &entry.coordinator_signature,
    )?;
    Ok(VerifiedLogEntry { entry, log_hash })
}

#[cfg(test)]
pub(crate) mod test_support {
    //! 仅供 crate 内测试使用：固定种子密钥与候选构造。

    use super::*;
    use crate::canonical::ProtocolVersion;
    use crate::signing::{
        ControlMetadata, ControlOperation, SigningKey, sign_bytes, signing_key_from_seed,
    };

    pub(crate) const DEVICE_SEED: [u8; 32] = [0x11; 32];
    pub(crate) const OTHER_SEED: [u8; 32] = [0x22; 32];
    pub(crate) const NODE_SEED: [u8; 32] = [0x33; 32];

    pub(crate) fn signing_key(seed: &[u8; 32]) -> SigningKey {
        signing_key_from_seed(seed)
    }

    pub(crate) fn credential(seed: &[u8; 32]) -> VerifiedCredential {
        let key = signing_key(seed);
        VerifiedCredential {
            account_id: AccountId::from_bytes([0xa1; 32]),
            device_id: DeviceId::from_bytes([0xb2; 32]),
            device_public_key: key.verifying_key().to_bytes(),
            credential_version: 3,
            revoked: false,
        }
    }

    pub(crate) fn context() -> GroupContext {
        GroupContext {
            group_id: GroupId::from_bytes([0x77; 32]),
            control_head: ControlHead::from_bytes([0x44; 32]),
            policy_version: 7,
            mls_epoch: 9,
        }
    }

    pub(crate) fn unsigned_candidate() -> UnsignedCandidate {
        UnsignedCandidate {
            protocol_version: ProtocolVersion::V1,
            submission_id: crate::ids::SubmissionId::from_bytes([0x55; 32]),
            group_id: context().group_id,
            author_device_id: credential(&DEVICE_SEED).device_id,
            kind: CandidateKind::Application,
            expected_control_head: context().control_head,
            policy_version: context().policy_version,
            mls_epoch: context().mls_epoch,
            encrypted_payload: b"ciphertext".to_vec(),
            control_metadata: None,
        }
    }

    pub(crate) fn signed_candidate() -> SignedCandidate {
        sign_with(&unsigned_candidate(), &DEVICE_SEED)
    }

    pub(crate) fn sign_with(unsigned: &UnsignedCandidate, seed: &[u8; 32]) -> SignedCandidate {
        let key = signing_key(seed);
        let bytes = candidate_signing_bytes(unsigned).expect("fixture must encode");
        SignedCandidate {
            unsigned: unsigned.clone(),
            author_signature: sign_bytes(&key, bytes.as_bytes()),
        }
    }

    pub(crate) fn control_candidate() -> UnsignedCandidate {
        let mut unsigned = unsigned_candidate();
        unsigned.kind = CandidateKind::Control;
        unsigned.control_metadata = Some(ControlMetadata {
            operation: ControlOperation::Invite,
            target_digest: Digest::from_bytes([0x66; 32]),
            domain_operation: None,
            added_credentials: Vec::new(),
            removed_credentials: Vec::new(),
            revoked_devices: Vec::new(),
        });
        unsigned
    }

    pub(crate) fn verified_application() -> VerifiedCandidate {
        verify_candidate(signed_candidate(), credential(&DEVICE_SEED), context())
            .expect("fixture candidate must verify")
    }

    pub(crate) fn verified_control() -> VerifiedCandidate {
        verify_candidate(
            sign_with(&control_candidate(), &DEVICE_SEED),
            credential(&DEVICE_SEED),
            context(),
        )
        .expect("fixture control candidate must verify")
    }

    pub(crate) fn node_key() -> PinnedNodeKey {
        PinnedNodeKey {
            coordinator_id: CoordinatorId::from_bytes([0xc3; 32]),
            public_key: signing_key(&NODE_SEED).verifying_key().to_bytes(),
        }
    }

    pub(crate) fn entry_with_sequence(sequence: u64, previous: Digest) -> AcceptedLogEntry {
        let candidate = verified_application();
        let receipt = receipt_signing_bytes(
            candidate.candidate_digest(),
            sequence,
            previous,
            context().group_id,
        )
        .expect("receipt preimage");
        let signature = sign_bytes(&signing_key(&NODE_SEED), receipt.as_bytes());
        AcceptedLogEntry {
            candidate,
            group_sequence: sequence,
            previous_log_hash: previous,
            coordinator_signature: signature,
        }
    }

    pub(crate) fn head() -> VerifiedHead {
        VerifiedHead::genesis(
            context().group_id,
            context().control_head,
            context().policy_version,
            context().mls_epoch,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::error::ErrorCode;
    use crate::signing::{CandidateKind, sign_bytes, signing_key_from_seed};

    #[test]
    fn valid_candidate_verifies_with_bindings() {
        let verified =
            verify_candidate(signed_candidate(), credential(&DEVICE_SEED), context()).unwrap();
        assert_eq!(verified.author_account(), AccountId::from_bytes([0xa1; 32]));
        assert_eq!(verified.author_device(), DeviceId::from_bytes([0xb2; 32]));
        assert_eq!(verified.group_id(), context().group_id);
        assert_eq!(verified.kind(), CandidateKind::Application);
        assert_eq!(verified.policy_version(), 7);
        assert_eq!(verified.mls_epoch(), 9);
        assert_eq!(verified.expected_control_head(), context().control_head);
        let signing = candidate_signing_bytes(verified.unsigned()).unwrap();
        assert_eq!(
            verified.candidate_digest(),
            Digest::sha256(signing.as_bytes())
        );
    }

    #[test]
    fn revoked_device_and_wrong_device_are_rejected() {
        let mut revoked = credential(&DEVICE_SEED);
        revoked.revoked = true;
        assert_eq!(
            verify_candidate(signed_candidate(), revoked, context()).unwrap_err(),
            ProtocolError::DeviceRevoked
        );
        // 候选声明了别的设备。
        let mut other_device = unsigned_candidate();
        other_device.author_device_id = DeviceId::from_bytes([0x99; 32]);
        let signed = sign_with(&other_device, &DEVICE_SEED);
        assert_eq!(
            verify_candidate(signed, credential(&DEVICE_SEED), context()).unwrap_err(),
            ProtocolError::InvalidSignature
        );
    }

    #[test]
    fn signature_from_another_key_or_tampered_fields_is_rejected() {
        // 用另一把密钥签名。
        let forged = sign_with(&unsigned_candidate(), &OTHER_SEED);
        assert_eq!(
            verify_candidate(forged, credential(&DEVICE_SEED), context()).unwrap_err(),
            ProtocolError::InvalidSignature
        );
        // 签名后篡改 epoch/head/密文。
        let original = signing_key_from_seed(&DEVICE_SEED);
        let bytes = candidate_signing_bytes(&unsigned_candidate()).unwrap();
        let signature = sign_bytes(&original, bytes.as_bytes());
        // 只改「不属于上下文绑定」的字段时必然不能通过签名；改 epoch/head/policy
        // 会先命中 STALE_*（见下一个用例），不能被误报成签名伪造。
        for mutate in [
            |c: &mut UnsignedCandidate| c.encrypted_payload.push(0),
            |c: &mut UnsignedCandidate| {
                c.submission_id = crate::ids::SubmissionId::from_bytes([0x99; 32])
            },
            |c: &mut UnsignedCandidate| c.group_id = GroupId::from_bytes([0x99; 32]),
            |c: &mut UnsignedCandidate| c.author_device_id = DeviceId::from_bytes([0x98; 32]),
        ] {
            let mut unsigned = unsigned_candidate();
            mutate(&mut unsigned);
            let candidate = SignedCandidate {
                unsigned,
                author_signature: signature,
            };
            assert_eq!(
                verify_candidate(candidate, credential(&DEVICE_SEED), context()).unwrap_err(),
                ProtocolError::InvalidSignature
            );
        }
    }

    #[test]
    fn cross_group_and_stale_context_are_rejected() {
        let mut other_group = context();
        other_group.group_id = GroupId::from_bytes([0x99; 32]);
        assert_eq!(
            verify_candidate(signed_candidate(), credential(&DEVICE_SEED), other_group)
                .unwrap_err(),
            ProtocolError::InvalidSignature
        );
        let mut stale_head = context();
        stale_head.control_head = ControlHead::from_bytes([0x99; 32]);
        assert_eq!(
            verify_candidate(signed_candidate(), credential(&DEVICE_SEED), stale_head).unwrap_err(),
            ProtocolError::StaleControlHead
        );
        let mut stale_policy = context();
        stale_policy.policy_version += 1;
        assert_eq!(
            verify_candidate(signed_candidate(), credential(&DEVICE_SEED), stale_policy)
                .unwrap_err(),
            ProtocolError::StaleControlHead
        );
        let mut stale_epoch = context();
        stale_epoch.mls_epoch += 1;
        assert_eq!(
            verify_candidate(signed_candidate(), credential(&DEVICE_SEED), stale_epoch)
                .unwrap_err(),
            ProtocolError::StaleEpoch
        );
        // 候选自身声明不支持版本。
        let mut unsupported = unsigned_candidate();
        unsupported.protocol_version = ProtocolVersion { major: 2, minor: 0 };
        let candidate = SignedCandidate {
            unsigned: unsupported,
            author_signature: [0u8; 64],
        };
        assert_eq!(
            verify_candidate(candidate, credential(&DEVICE_SEED), context()).unwrap_err(),
            ProtocolError::UnsupportedProtocolVersion
        );
    }

    #[test]
    fn application_candidate_cannot_smuggle_control_metadata() {
        let mut unsigned = unsigned_candidate();
        unsigned.control_metadata = Some(crate::signing::ControlMetadata {
            operation: crate::signing::ControlOperation::Invite,
            target_digest: Digest::from_bytes([1; 32]),
            domain_operation: None,
            added_credentials: Vec::new(),
            removed_credentials: Vec::new(),
            revoked_devices: Vec::new(),
        });
        // 候选本身已不合法：规范编码阶段就拒绝，签名永远不会被检查。
        let candidate = SignedCandidate {
            unsigned,
            author_signature: [0u8; 64],
        };
        assert_eq!(
            verify_candidate(candidate, credential(&DEVICE_SEED), context()).unwrap_err(),
            ProtocolError::InvalidCandidate
        );
    }

    #[test]
    fn log_entry_hash_is_deterministic_and_field_sensitive() {
        let candidate = Digest::from_bytes([1; 32]);
        let previous = Digest::from_bytes([2; 32]);
        let group = GroupId::from_bytes([3; 32]);
        let signature = [7u8; 64];
        let base = log_entry_hash(group, 1, previous, candidate, &signature).unwrap();
        assert_eq!(
            base,
            log_entry_hash(group, 1, previous, candidate, &signature).unwrap()
        );
        assert_ne!(
            base,
            log_entry_hash(group, 2, previous, candidate, &signature).unwrap()
        );
        assert_ne!(
            base,
            log_entry_hash(group, 1, Digest::from_bytes([9; 32]), candidate, &signature).unwrap()
        );
        assert_ne!(
            base,
            log_entry_hash(group, 1, previous, Digest::from_bytes([9; 32]), &signature).unwrap()
        );
        let mut other_signature = signature;
        other_signature[0] ^= 0xff;
        assert_ne!(
            base,
            log_entry_hash(group, 1, previous, candidate, &other_signature).unwrap()
        );
        assert_eq!(
            log_entry_hash(group, 0, previous, candidate, &signature).unwrap_err(),
            ProtocolError::InvalidSequence
        );
        assert_eq!(base.to_hex().len(), 64);
    }

    #[test]
    fn log_chain_accepts_contiguous_entries_only() {
        let entry = entry_with_sequence(1, Digest::ZERO);
        let verified = verify_log_entry(entry, &head(), &node_key()).unwrap();
        assert_eq!(verified.group_sequence(), 1);
        assert_eq!(verified.next_previous_log_hash(), verified.log_hash());
        // 第二条必须引用第一条的链尾。
        let second = entry_with_sequence(2, verified.log_hash());
        let verified_second =
            verify_log_entry(second, &head_after(&verified), &node_key()).unwrap();
        assert_eq!(verified_second.group_sequence(), 2);
        // 跳号 → MISSING_LOG_ENTRIES。
        let skipped = entry_with_sequence(3, verified.log_hash());
        assert_eq!(
            verify_log_entry(skipped, &head_after(&verified), &node_key()).unwrap_err(),
            ProtocolError::MissingLogEntries
        );
        // 重复序号 → LOG_CONFLICT。
        let duplicate = entry_with_sequence(1, Digest::ZERO);
        assert_eq!(
            verify_log_entry(duplicate, &head_after(&verified), &node_key()).unwrap_err(),
            ProtocolError::LogConflict
        );
        // 前序摘要不符 → LOG_CONFLICT。
        let wrong_previous = entry_with_sequence(2, Digest::from_bytes([9; 32]));
        assert_eq!(
            verify_log_entry(wrong_previous, &head_after(&verified), &node_key()).unwrap_err(),
            ProtocolError::LogConflict
        );
    }

    fn head_after(entry: &VerifiedLogEntry) -> VerifiedHead {
        VerifiedHead {
            log_sequence: entry.group_sequence(),
            log_hash: entry.log_hash(),
            ..head()
        }
    }

    #[test]
    fn wrong_node_key_and_tampered_receipt_are_rejected() {
        let entry = entry_with_sequence(1, Digest::ZERO);
        let mut other_key = node_key();
        other_key.public_key = signing_key_from_seed(&[0x44; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            verify_log_entry(entry.clone(), &head(), &other_key).unwrap_err(),
            ProtocolError::InvalidSignature
        );
        // 篡改节点签名。
        let mut tampered = entry;
        tampered.coordinator_signature[0] ^= 0xff;
        assert_eq!(
            verify_log_entry(tampered, &head(), &node_key()).unwrap_err(),
            ProtocolError::InvalidSignature
        );
        // 签名后篡改候选密文（作者签名不受影响，但候选摘要与回执不再匹配）。
        let mut entry = entry_with_sequence(1, Digest::ZERO);
        entry.candidate = {
            let mut unsigned = unsigned_candidate();
            unsigned.encrypted_payload.push(0);
            verify_candidate(
                sign_with(&unsigned, &DEVICE_SEED),
                credential(&DEVICE_SEED),
                context(),
            )
            .unwrap()
        };
        assert_eq!(
            verify_log_entry(entry, &head(), &node_key()).unwrap_err(),
            ProtocolError::InvalidSignature
        );
    }

    #[test]
    fn cross_group_and_stale_log_heads_are_rejected() {
        let entry = entry_with_sequence(1, Digest::ZERO);
        let mut other_group = head();
        other_group.group_id = GroupId::from_bytes([0x99; 32]);
        assert_eq!(
            verify_log_entry(entry.clone(), &other_group, &node_key()).unwrap_err(),
            ProtocolError::InvalidSignature
        );
        let mut stale = head();
        stale.control_head = ControlHead::from_bytes([0x99; 32]);
        assert_eq!(
            verify_log_entry(entry.clone(), &stale, &node_key()).unwrap_err(),
            ProtocolError::StaleControlHead
        );
        let mut stale_epoch = head();
        stale_epoch.mls_epoch += 1;
        assert_eq!(
            verify_log_entry(entry, &stale_epoch, &node_key()).unwrap_err(),
            ProtocolError::StaleEpoch
        );
        // 创世条目必须引用全零前序。
        let wrong_genesis = entry_with_sequence(1, Digest::from_bytes([9; 32]));
        assert_eq!(
            verify_log_entry(wrong_genesis, &head(), &node_key()).unwrap_err(),
            ProtocolError::LogConflict
        );
    }

    #[test]
    fn verification_failures_map_to_frozen_codes() {
        let mut revoked = credential(&DEVICE_SEED);
        revoked.revoked = true;
        assert_eq!(
            verify_candidate(signed_candidate(), revoked, context())
                .unwrap_err()
                .code(),
            ErrorCode::DeviceRevoked
        );
        let mut stale = context();
        stale.mls_epoch += 1;
        assert_eq!(
            verify_candidate(signed_candidate(), credential(&DEVICE_SEED), stale)
                .unwrap_err()
                .code(),
            ErrorCode::StaleEpoch
        );
        let entry = entry_with_sequence(5, Digest::ZERO);
        assert_eq!(
            verify_log_entry(entry, &head(), &node_key())
                .unwrap_err()
                .code(),
            ErrorCode::MissingLogEntries
        );
    }
}

//! DOMAIN-008/009/010：发送状态机、送达聚合与重试分类。
//!
//! 状态推进只依据「证据种类」：本机计时器、连接 ACK、信箱 ACK 都不是正式送达证据，
//! 也不允许用它们推断已接受。所有函数是纯函数：不 sleep、不加密、不查网络。

use chat_protocol::{
    AccountId, ConversationId, DeviceId, Digest, ErrorCode, GroupId, MessageId, SubmissionId,
};

use crate::error::{DomainError, Result};

/// 用户可见发送状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendState {
    /// 已持久化意图、尚未获正式接受。
    Queued,
    /// 节点签名回执已验证。
    Accepted,
    /// 对方获准设备已认证并本地持久化回执。
    Delivered,
    /// 已获认证的拒绝（不是本机超时推断）。
    Failed,
}

impl SendState {
    /// 是否终态（不再接受推进证据）。
    pub const fn is_terminal(self) -> bool {
        matches!(self, SendState::Delivered | SendState::Failed)
    }
}

/// 投递证据。只有 `VerifiedAcceptedReceipt`/`AuthenticatedReceipt`/`VerifiedRejection`
/// 能推进状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryEvidence {
    /// 对端连接接收字节：仅网络诊断。
    TransportAck,
    /// 节点信箱已持久保存密文：不算送达。
    MailboxStored,
    /// 已验证的节点签名接受回执。
    VerifiedAcceptedReceipt {
        /// 固定密文提交 ID。
        submission_id: SubmissionId,
        /// 候选全文摘要。
        candidate_digest: Digest,
        /// 群。
        group: GroupId,
        /// 节点分配的群序号（从 1 开始）。
        seq: u64,
    },
    /// 对方获准设备认证并本地持久化后的回执。
    AuthenticatedReceipt {
        /// 正式消息 ID。
        message_id: MessageId,
        /// 对方设备。
        device_id: DeviceId,
    },
    /// 已验证的节点拒绝（正式失败证据）。
    VerifiedRejection {
        /// 节点给出的稳定错误码。
        code: ErrorCode,
    },
    /// 本机超时：不是正式证据，必须显式拒绝。
    LocalTimeout,
}

/// DOMAIN-008：按证据推进发送状态。
///
/// - transport/mailbox ACK 与本机超时被显式拒绝，不会推进状态；
/// - `VerifiedAcceptedReceipt` 只把 QUEUED 推到 ACCEPTED，重复证据幂等；
/// - 送达证据必须已有正式接受关联，禁止 QUEUED 直接跳 DELIVERED；
/// - 已接受后不会被超时降为 FAILED；
/// - FAILED 只能来自已验证拒绝，且是终态。
pub fn transition_send_state(current: SendState, evidence: DeliveryEvidence) -> Result<SendState> {
    match evidence {
        DeliveryEvidence::TransportAck => Err(DomainError::InvalidStateTransition {
            detail: "transport ack is not delivery evidence",
        }),
        DeliveryEvidence::MailboxStored => Err(DomainError::InvalidStateTransition {
            detail: "mailbox ack is not delivery evidence",
        }),
        DeliveryEvidence::LocalTimeout => Err(DomainError::InvalidStateTransition {
            detail: "local timeout is not formal failure evidence",
        }),
        DeliveryEvidence::VerifiedAcceptedReceipt {
            submission_id,
            candidate_digest,
            group,
            seq,
        } => {
            // 回执必须绑定完整的提交/候选/群/序号；缺项或全零视为畸形。
            if submission_id == SubmissionId::from_bytes([0; 32])
                || candidate_digest == Digest::ZERO
                || group == GroupId::from_bytes([0; 32])
                || seq == 0
            {
                return Err(DomainError::InvalidStateTransition {
                    detail: "accepted receipt is not bound to a valid candidate",
                });
            }
            match current {
                SendState::Queued | SendState::Accepted => Ok(SendState::Accepted),
                SendState::Delivered => Ok(SendState::Delivered),
                SendState::Failed => Err(DomainError::InvalidStateTransition {
                    detail: "failed send cannot become accepted",
                }),
            }
        }
        DeliveryEvidence::AuthenticatedReceipt {
            message_id,
            device_id,
        } => {
            if message_id == MessageId::from_bytes([0; 32])
                || device_id == DeviceId::from_bytes([0; 32])
            {
                return Err(DomainError::InvalidStateTransition {
                    detail: "authenticated receipt lacks identity binding",
                });
            }
            match current {
                SendState::Accepted | SendState::Delivered => Ok(SendState::Delivered),
                SendState::Queued => Err(DomainError::InvalidStateTransition {
                    detail: "delivery requires a verified acceptance first",
                }),
                SendState::Failed => Err(DomainError::InvalidStateTransition {
                    detail: "failed send cannot become delivered",
                }),
            }
        }
        DeliveryEvidence::VerifiedRejection { code } => {
            if code.retryable() {
                return Err(DomainError::InvalidStateTransition {
                    detail: "only terminal errors may reject an attempt",
                });
            }
            match current {
                SendState::Queued | SendState::Failed => Ok(SendState::Failed),
                SendState::Accepted | SendState::Delivered => {
                    Err(DomainError::InvalidStateTransition {
                        detail: "accepted send cannot be rejected afterwards",
                    })
                }
            }
        }
    }
}

/// 消息身份。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageIdentity {
    /// 会话。
    pub conversation: ConversationId,
    /// 正式消息 ID。
    pub message_id: MessageId,
    /// 发件账户。
    pub sender: AccountId,
}

/// 消息时点的收件账户与设备。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipientAccount {
    /// 账户。
    pub account_id: AccountId,
    /// 当时获准收件的设备。
    pub devices: Vec<DeviceId>,
}

/// 消息时点的收件范围；不能用今天成员表推断。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipientSnapshot {
    /// 收件账户集合。
    pub accounts: Vec<RecipientAccount>,
}

/// 已认证并本地持久化的送达回执。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedReceipt {
    /// 消息。
    pub message_id: MessageId,
    /// 送达账户。
    pub account_id: AccountId,
    /// 送达设备。
    pub device_id: DeviceId,
}

/// 单设备明细。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceDelivery {
    /// 账户。
    pub account_id: AccountId,
    /// 设备。
    pub device_id: DeviceId,
    /// 是否已送达。
    pub delivered: bool,
}

/// 单账户明细。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountDelivery {
    /// 账户。
    pub account_id: AccountId,
    /// 该账户是否至少一台设备送达。
    pub delivered: bool,
    /// 已送达设备数。
    pub delivered_devices: usize,
    /// 收件设备总数。
    pub devices: usize,
}

/// 送达汇总。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliverySummary {
    /// 按 (账户, 设备) 排序的明细。
    pub per_device: Vec<DeviceDelivery>,
    /// 按账户排序的明细。
    pub per_account: Vec<AccountDelivery>,
    /// 对方已送达账户数。
    pub delivered_accounts: usize,
    /// 对方收件账户总数。
    pub total_accounts: usize,
    /// 未送达账户。
    pub missing_accounts: Vec<AccountId>,
    /// 已送达设备数。
    pub delivered_devices: usize,
    /// 收件设备总数。
    pub total_devices: usize,
}

/// DOMAIN-009：聚合送达回执。
///
/// 只统计消息时点获准收件的设备；按 (消息, 设备) 去重；按账户 OR 聚合；
/// 排除发件人账户；超出收件快照的回执显式 `INVALID_RECEIPT_SCOPE`。
pub fn aggregate_delivery(
    message: MessageIdentity,
    receipts: &[VerifiedReceipt],
    members: &RecipientSnapshot,
) -> Result<DeliverySummary> {
    // 校验收件快照自身：账户唯一、设备不跨账户共享。
    let mut accounts: Vec<AccountId> = Vec::with_capacity(members.accounts.len());
    let mut all_devices: Vec<DeviceId> = Vec::new();
    for account in &members.accounts {
        if accounts.contains(&account.account_id) {
            return Err(DomainError::InvalidReceiptScope {
                detail: "duplicate recipient account",
            });
        }
        accounts.push(account.account_id);
        for device in &account.devices {
            if all_devices.contains(device) {
                return Err(DomainError::InvalidReceiptScope {
                    detail: "duplicate recipient device",
                });
            }
            all_devices.push(*device);
        }
    }

    // 排除发件人账户。
    let mut recipients: Vec<RecipientAccount> = members
        .accounts
        .iter()
        .filter(|account| account.account_id != message.sender)
        .cloned()
        .collect();
    recipients.sort_by_key(|account| account.account_id);

    // 只保留该消息、属于收件设备集合的回执；按设备去重。
    let mut delivered: Vec<(AccountId, DeviceId)> = Vec::new();
    for receipt in receipts {
        if receipt.message_id != message.message_id {
            return Err(DomainError::InvalidReceiptScope {
                detail: "receipt belongs to another message",
            });
        }
        // 本人其他设备的回执不属于「对方」，静默排除但不计数。
        if receipt.account_id == message.sender {
            continue;
        }
        let Some(account) = recipients
            .iter()
            .find(|account| account.account_id == receipt.account_id)
        else {
            return Err(DomainError::InvalidReceiptScope {
                detail: "receipt from non-recipient account",
            });
        };
        if !account.devices.contains(&receipt.device_id) {
            return Err(DomainError::InvalidReceiptScope {
                detail: "receipt from device not allowed at message time",
            });
        }
        let pair = (receipt.account_id, receipt.device_id);
        if !delivered.contains(&pair) {
            delivered.push(pair);
        }
    }

    let mut per_device: Vec<DeviceDelivery> = Vec::new();
    let mut per_account: Vec<AccountDelivery> = Vec::new();
    let mut missing_accounts: Vec<AccountId> = Vec::new();
    let mut delivered_accounts = 0usize;
    let mut delivered_devices = 0usize;
    let mut total_devices = 0usize;
    for account in &recipients {
        let mut devices: Vec<DeviceId> = account.devices.clone();
        devices.sort();
        devices.dedup();
        let mut account_delivered = false;
        let mut account_delivered_devices = 0usize;
        for device in &devices {
            let hit = delivered.iter().any(|(account_id, device_id)| {
                *account_id == account.account_id && device_id == device
            });
            if hit {
                account_delivered = true;
                account_delivered_devices += 1;
                delivered_devices += 1;
            }
            total_devices += 1;
            per_device.push(DeviceDelivery {
                account_id: account.account_id,
                device_id: *device,
                delivered: hit,
            });
        }
        if account_delivered {
            delivered_accounts += 1;
        } else {
            missing_accounts.push(account.account_id);
        }
        per_account.push(AccountDelivery {
            account_id: account.account_id,
            delivered: account_delivered,
            delivered_devices: account_delivered_devices,
            devices: devices.len(),
        });
    }

    Ok(DeliverySummary {
        per_device,
        per_account,
        delivered_accounts,
        total_accounts: recipients.len(),
        missing_accounts,
        delivered_devices,
        total_devices,
    })
}

/// 操作生命周期状态（供重试分类）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationState {
    /// 已持久化但未投递。
    Prepared,
    /// 已投递、尚未获回执。
    InFlight,
    /// 超时或断连：结果未知，必须先对账。
    AcceptanceUnknown,
    /// 已获正式接受。
    Accepted,
    /// 已终止失败。
    Failed,
}

/// 重试决定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryDecision {
    /// 同 SubmissionId、同字节重发。
    ResendSameBytes,
    /// 先同步补齐，再按新状态重建（重建后是新 SubmissionId）。
    SyncThenRebuild,
    /// 停止自动重试，等用户处置后重建。
    WaitForUser,
    /// 结果未知：按原 SubmissionId 查询，不得重新加密。
    QueryExisting,
    /// 永久失败。
    PermanentFailure,
}

impl RetryDecision {
    /// 是否无需用户参与自动重试。
    pub const fn retries_automatically(self) -> bool {
        matches!(self, RetryDecision::ResendSameBytes)
    }

    /// 是否必须保留原 SubmissionId。
    pub const fn preserves_submission(self) -> bool {
        matches!(
            self,
            RetryDecision::ResendSameBytes | RetryDecision::QueryExisting
        )
    }

    /// 是否必须先同步。
    pub const fn requires_sync(self) -> bool {
        matches!(self, RetryDecision::SyncThenRebuild)
    }

    /// 是否需要用户先处置。
    pub const fn requires_user_action(self) -> bool {
        matches!(self, RetryDecision::WaitForUser)
    }
}

/// DOMAIN-010：按错误分类与操作状态给出重试决定。
///
/// 顺序：结果未知优先对账 → 已接受/已失败不再重试 → 网络类原字节重发 →
/// head/epoch/日志类先同步 → 权限/配额/限额外等用户 → 其余永久失败。
/// 本函数不 sleep、不加密，也不产生新 SubmissionId。
pub fn classify_retry(error: ErrorCode, state: OperationState) -> RetryDecision {
    if state == OperationState::AcceptanceUnknown || error == ErrorCode::UnknownResult {
        return RetryDecision::QueryExisting;
    }
    if state == OperationState::Accepted || state == OperationState::Failed {
        return RetryDecision::PermanentFailure;
    }
    match error {
        ErrorCode::NetworkUnavailable
        | ErrorCode::NodeUnavailable
        | ErrorCode::RateLimited
        | ErrorCode::StorageLocked => RetryDecision::ResendSameBytes,
        ErrorCode::StaleControlHead
        | ErrorCode::StaleEpoch
        | ErrorCode::MissingLogEntries
        | ErrorCode::LogConflict => RetryDecision::SyncThenRebuild,
        // 权限/禁言/设备撤销：同字节不会成功，必须等用户处置。
        ErrorCode::PermissionDenied
        | ErrorCode::DeviceRevoked
        | ErrorCode::NotMember
        | ErrorCode::QuotaExceeded
        | ErrorCode::ContactNotAccepted
        | ErrorCode::ContentTooLarge
        | ErrorCode::GroupLimitExceeded
        | ErrorCode::DeviceLimitExceeded
        | ErrorCode::OwnerTransferRequired => RetryDecision::WaitForUser,
        _ => RetryDecision::PermanentFailure,
    }
}

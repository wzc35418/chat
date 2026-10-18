//! 领域规则：群权限、策略迁移、控制结果、发送状态机、送达聚合与可见性。
//!
//! 模块边界：
//!
//! - [`model`]：正式群快照、成员、操作、容量与授权/策略结果；
//! - [`group`]：DOMAIN-001/002/003/006/007；
//! - [`control`]：DOMAIN-004/005 策略操作应用与控制结果核对；
//! - [`delivery`]：DOMAIN-008/009/010；
//! - [`visibility`]：DOMAIN-011/012；
//! - [`error`]：复用协议层冻结错误码的领域错误。
//!
//! 全部函数是纯规则：不读写数据库、不发网络请求、不推进本地正式成员，也不把
//! 「意图」或「本机计时」当成正式状态。
#![forbid(unsafe_code)]
pub mod rules;

pub mod control;
pub mod delivery;
pub mod error;
pub mod group;
pub mod model;
pub mod visibility;

pub use control::{
    CONTROL_HEAD_DOMAIN, POLICY_OPERATION_DOMAIN, apply_policy_operation, control_head_after,
    control_operation_of, policy_operation_digest, verify_control_result,
};
pub use delivery::{
    AccountDelivery, DeliveryEvidence, DeliverySummary, DeviceDelivery, MessageIdentity,
    OperationState, RecipientAccount, RecipientSnapshot, RetryDecision, SendState, VerifiedReceipt,
    aggregate_delivery, classify_retry, transition_send_state,
};
pub use error::{DomainError, Result};
pub use group::{
    ExitMode, authorize_group_operation, validate_capacity, validate_owner_exit,
    validate_ownership_transfer, validate_send_permission, validate_snapshot_capacity,
};
pub use model::{
    Actor, AuthorizedGroupOperation, AuthorizedSend, Capacity, ConversationKind, CredentialLeaf,
    DeviceSetDiff, GroupLimits, GroupMember, GroupOperation, GroupSnapshot, MAX_MESSAGE_BYTES,
    MemberRole, MembershipWindow, PauseReason, ProposedPolicy, RoleChange, StagedCommit,
    VerifiedControlResult,
};
pub use visibility::{
    MessageAvailability, MessageRef, ReplyReference, SequenceRange, enforce_join_visibility,
    validate_reply_target,
};

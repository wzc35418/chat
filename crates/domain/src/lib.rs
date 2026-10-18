//! 领域规则：群权限、发送状态机、送达聚合与可见性（DOMAIN-001..012 中的纯规则部分）。
//!
//! 模块边界：
//!
//! - [`model`]：正式群快照、成员、操作与授权结果；
//! - [`group`]：DOMAIN-001/002/003/006/007；
//! - [`delivery`]：DOMAIN-008/009/010；
//! - [`visibility`]：DOMAIN-011/012；
//! - [`error`]：复用协议层冻结错误码的领域错误。
//!
//! 全部函数是纯规则：不读写数据库、不发网络请求、不推进本地正式成员，也不把
//! 「意图」或「本机计时」当成正式状态。
#![forbid(unsafe_code)]
pub mod rules;

pub mod delivery;
pub mod error;
pub mod group;
pub mod model;
pub mod visibility;

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
    Actor, AuthorizedGroupOperation, AuthorizedSend, Capacity, ConversationKind, GroupLimits,
    GroupMember, GroupOperation, GroupSnapshot, MAX_MESSAGE_BYTES, MemberRole, MembershipWindow,
    PauseReason, RoleChange,
};
pub use visibility::{
    MessageAvailability, MessageRef, ReplyReference, SequenceRange, enforce_join_visibility,
    validate_reply_target,
};

//! DOMAIN-011/012：回复引用校验与加入可见性。
//!
//! 两者都是纯函数：不隐式下载历史、不读取 blob、不修改成员状态。
//! 可见性只决定「正文/附件内容」范围；控制链恢复材料是另一条范围，必须由调用方
//! 单独授权，绝不能因为需要恢复控制链就公开旧正文密钥。

use chat_protocol::{ConversationId, MessageId};

use crate::error::{DomainError, Result};
use crate::model::MembershipWindow;

/// 被引用消息对调用方的可见性。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageAvailability {
    /// 正文可用。
    Available,
    /// 正文已过期（可保留引用占位）。
    Expired,
    /// 正文已删除（可保留引用占位）。
    Deleted,
    /// 对调用方不可见或从未获准。
    NotVisible,
}

/// 回复目标引用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageRef {
    /// 目标消息所属会话。
    pub conversation: ConversationId,
    /// 目标消息 ID。
    pub message_id: MessageId,
    /// 目标消息可见性。
    pub availability: MessageAvailability,
}

/// 合法回复引用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplyReference {
    /// 会话。
    pub conversation: ConversationId,
    /// 被引用消息 ID。
    pub message_id: MessageId,
    /// 正文不可用，需要显示占位。
    pub content_unavailable: bool,
}

/// DOMAIN-011：校验同会话回复引用。
///
/// 跨会话引用直接拒绝；已过期/已删除正文允许保留引用并标注占位；对调用方不可见
/// 的消息返回 `MESSAGE_NOT_VISIBLE`。字段大小由强类型 ID 固定，不需要额外长度预算。
pub fn validate_reply_target(
    conversation: ConversationId,
    target: MessageRef,
) -> Result<ReplyReference> {
    if target.conversation != conversation {
        return Err(DomainError::InvalidReply {
            detail: "reply target belongs to another conversation",
        });
    }
    match target.availability {
        MessageAvailability::NotVisible => Err(DomainError::MessageNotVisible),
        MessageAvailability::Available => Ok(ReplyReference {
            conversation,
            message_id: target.message_id,
            content_unavailable: false,
        }),
        MessageAvailability::Expired | MessageAvailability::Deleted => Ok(ReplyReference {
            conversation,
            message_id: target.message_id,
            content_unavailable: true,
        }),
    }
}

/// 序号区间（闭区间）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceRange {
    /// 起始序号（从 1 开始）。
    pub start: u64,
    /// 结束序号（含）。
    pub end_inclusive: u64,
}

/// DOMAIN-012：限制内容可见范围。
///
/// 返回 `requested` 与成员窗口的交集：
/// 新设备只能读加入后的消息；离开后不再包含新消息；空交集返回
/// `HISTORY_NOT_AUTHORIZED`，绝不退化为「全部可见」。
/// 附件与正文使用同一序号授权边界，调用方复用本函数。
pub fn enforce_join_visibility(
    member: MembershipWindow,
    requested: SequenceRange,
) -> Result<SequenceRange> {
    member
        .validate()
        .map_err(|_| DomainError::HistoryNotAuthorized {
            detail: "membership window is malformed",
        })?;
    if requested.start == 0 || requested.end_inclusive < requested.start {
        return Err(DomainError::HistoryNotAuthorized {
            detail: "requested sequence range is malformed",
        });
    }
    let upper = match member.left_sequence {
        Some(left) => requested.end_inclusive.min(left - 1),
        None => requested.end_inclusive,
    };
    let lower = requested.start.max(member.joined_sequence);
    if lower > upper {
        return Err(DomainError::HistoryNotAuthorized {
            detail: "requested range is outside the membership window",
        });
    }
    Ok(SequenceRange {
        start: lower,
        end_inclusive: upper,
    })
}

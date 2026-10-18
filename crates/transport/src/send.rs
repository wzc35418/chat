//! TRANSPORT-005 的纯逻辑子集：把应用负载编码成分片并逐片交给注入的
//! [`WireSink`]。
//!
//! 语义：
//!
//! - 会话必须处于可用状态（`Connected`/`Degraded`），否则 `SESSION_NOT_USABLE`；
//! - 负载为空 `MALFORMED_FRAME`，超过会话帧上限 `CONTENT_TOO_LARGE`；
//! - 分片大小由 [`SendBudget::max_fragment_bytes`] 与帧上限的较小值决定，分片数
//!   必须在 `max_records_per_send` 内；
//! - 发送前检查 sink 队列深度：`queued_records + fragments > max_queue_depth` 时
//!   显式 `SEND_BUDGET_EXCEEDED`，不排队等待；
//! - wire 失败分为可重试/致命：可重试失败交给 [`RetryTracker`] 计算退避，预算耗尽
//!   转为致命；成功写完后重置重试计数。本函数不 sleep。
//!
//! 部分写入的语义：失败时已写出的记录保留计数并返回 `sent_records`。上层必须按连接
//! 级失败重建会话（对端重组器随之重置），不得在旧连接上从中间分片续传。

use crate::error::{Result, TransportError};
use crate::frame::{
    FRAME_HARD_MAX_BYTES, Fragment, FrameCodec, FrameLimits, MAX_FRAGMENTS_PER_FRAME_HARD,
};
use crate::retry::{RetryOutcome, RetryTracker};
use crate::session::{SessionLimits, TransportSession};
use crate::wire::{WireFailure, WireSink};

/// 默认分片大小（4 KiB）。
pub const DEFAULT_FRAGMENT_BYTES: usize = 4 * 1024;
/// 单次发送记录数硬上限。
pub const SEND_HARD_MAX_RECORDS: usize = 1024;
/// 发送队列深度硬上限。
pub const SEND_HARD_MAX_QUEUE: usize = 4096;

/// 发送预算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SendBudget {
    /// 单分片字节上限。
    pub max_fragment_bytes: usize,
    /// 单次发送记录数上限。
    pub max_records_per_send: usize,
    /// 允许的 sink 排队记录数上限。
    pub max_queue_depth: usize,
}

impl SendBudget {
    /// 默认预算：4 KiB 分片、最多 64 条记录、队列 128。
    pub const DEFAULT: SendBudget = SendBudget {
        max_fragment_bytes: DEFAULT_FRAGMENT_BYTES,
        max_records_per_send: 64,
        max_queue_depth: 128,
    };

    /// 由会话预算推导：队列深度沿用会话出站队列上限。
    pub const fn from_session_limits(limits: SessionLimits) -> Self {
        Self {
            max_fragment_bytes: DEFAULT_FRAGMENT_BYTES,
            max_records_per_send: Self::DEFAULT.max_records_per_send,
            max_queue_depth: limits.max_outbound_queue,
        }
    }

    /// 有界校验；非法预算显式拒绝。
    pub fn validate(&self) -> Result<()> {
        if self.max_fragment_bytes == 0 || self.max_fragment_bytes > FRAME_HARD_MAX_BYTES {
            return Err(TransportError::InvalidConfig {
                detail: "send maxFragmentBytes must be within 1..=65536",
            });
        }
        if self.max_records_per_send == 0 || self.max_records_per_send > SEND_HARD_MAX_RECORDS {
            return Err(TransportError::InvalidConfig {
                detail: "send maxRecordsPerSend must be within 1..=1024",
            });
        }
        if self.max_queue_depth == 0 || self.max_queue_depth > SEND_HARD_MAX_QUEUE {
            return Err(TransportError::InvalidConfig {
                detail: "send maxQueueDepth must be within 1..=4096",
            });
        }
        Ok(())
    }
}

impl Default for SendBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// 致命原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FatalReason {
    /// wire 端口报告连接已关闭。
    Wire(WireFailure),
    /// 可重试失败超过重试预算。
    RetryBudgetExhausted,
}

/// 发送结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// 全部记录已交给 sink。
    Sent {
        /// 帧编号。
        frame_id: u32,
        /// 分片数。
        fragments: u16,
        /// 写出的记录数。
        records: usize,
    },
    /// 可重试失败：调用方按 `delay_ms` 退避后在**新连接**上重发整帧。
    RetryableFailure {
        /// 帧编号。
        frame_id: u32,
        /// 失败的分片索引。
        fragment_index: u16,
        /// 已写出的记录数（连接级丢弃）。
        sent_records: usize,
        /// 退避时长（毫秒）。
        delay_ms: u64,
        /// 下一次尝试时间（Unix 毫秒）。
        next_attempt_at_ms: u64,
    },
    /// 致命失败：不得自动重试。
    FatalFailure {
        /// 帧编号。
        frame_id: u32,
        /// 失败的分片索引。
        fragment_index: u16,
        /// 已写出的记录数。
        sent_records: usize,
        /// 致命原因。
        reason: FatalReason,
    },
}

impl SendOutcome {
    /// 是否全部写出。
    pub const fn is_sent(&self) -> bool {
        matches!(self, SendOutcome::Sent { .. })
    }

    /// 是否可重试。
    pub const fn is_retryable(&self) -> bool {
        matches!(self, SendOutcome::RetryableFailure { .. })
    }

    /// 是否致命。
    pub const fn is_fatal(&self) -> bool {
        matches!(self, SendOutcome::FatalFailure { .. })
    }
}

/// 校验并编码一帧为规范分片（同步管线与真实 wire 适配器共用）。
pub fn encode_frame_for_send(
    session: &TransportSession,
    frame_id: u32,
    payload: &[u8],
    budget: SendBudget,
) -> Result<Vec<Fragment>> {
    if !session.is_usable() {
        return Err(TransportError::SessionNotUsable);
    }
    budget.validate()?;
    if payload.is_empty() {
        return Err(TransportError::MalformedFrame {
            detail: "empty frame",
        });
    }
    let frame_limit = session.limits().max_frame_bytes;
    if payload.len() > frame_limit {
        return Err(TransportError::ContentTooLarge {
            size: payload.len(),
            limit: frame_limit,
        });
    }
    let fragment = budget.max_fragment_bytes.min(frame_limit);
    let required = frame_limit.div_ceil(fragment);
    if required > budget.max_records_per_send {
        return Err(TransportError::SendBudgetExceeded {
            detail: "maxRecordsPerSend cannot cover maxFrameBytes",
        });
    }
    let codec = FrameCodec::new(FrameLimits {
        max_frame_bytes: frame_limit,
        max_fragment_bytes: fragment,
        max_fragments_per_frame: budget
            .max_records_per_send
            .min(MAX_FRAGMENTS_PER_FRAME_HARD),
    })?;
    let fragments = codec.encode(frame_id, payload)?;
    if fragments.len() > budget.max_records_per_send {
        return Err(TransportError::SendBudgetExceeded {
            detail: "frame fragments exceed maxRecordsPerSend",
        });
    }
    Ok(fragments)
}

/// TRANSPORT-005：编码并逐片发送一帧。
pub fn send_peer_frame<S: WireSink + ?Sized>(
    session: &TransportSession,
    frame_id: u32,
    payload: &[u8],
    sink: &mut S,
    budget: SendBudget,
    retry: &mut RetryTracker,
    now_unix_ms: u64,
) -> Result<SendOutcome> {
    let fragments = encode_frame_for_send(session, frame_id, payload, budget)?;
    let queued = sink.queued_records();
    let queued_after =
        queued
            .checked_add(fragments.len())
            .ok_or(TransportError::SendBudgetExceeded {
                detail: "send queue depth overflow",
            })?;
    if queued_after > budget.max_queue_depth {
        return Err(TransportError::SendBudgetExceeded {
            detail: "send queue depth exceeds maxQueueDepth",
        });
    }

    let mut sent_records = 0usize;
    for (index, fragment) in fragments.iter().enumerate() {
        match sink.write_record(&fragment.wire_bytes()) {
            Ok(()) => sent_records += 1,
            Err(failure) if failure.is_retryable() => {
                return Ok(match retry.record_failure(now_unix_ms) {
                    RetryOutcome::Retry {
                        delay_ms,
                        next_attempt_at_ms,
                        ..
                    } => SendOutcome::RetryableFailure {
                        frame_id,
                        fragment_index: index as u16,
                        sent_records,
                        delay_ms,
                        next_attempt_at_ms,
                    },
                    RetryOutcome::Stop { .. } => SendOutcome::FatalFailure {
                        frame_id,
                        fragment_index: index as u16,
                        sent_records,
                        reason: FatalReason::RetryBudgetExhausted,
                    },
                });
            }
            Err(failure) => {
                return Ok(SendOutcome::FatalFailure {
                    frame_id,
                    fragment_index: index as u16,
                    sent_records,
                    reason: FatalReason::Wire(failure),
                });
            }
        }
    }
    retry.reset();
    Ok(SendOutcome::Sent {
        frame_id,
        fragments: fragments.len() as u16,
        records: sent_records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionLimits;
    use crate::state::ConnectionEvent;
    use crate::wire::WireSink;
    use chat_protocol::{
        Capabilities, CapabilityLimits, DeviceId, ProtocolVersion, negotiate_capabilities,
    };

    fn endpoint(seed: u8) -> crate::session::EndpointId {
        crate::session::EndpointId::from_bytes([seed; 32])
    }

    pub(crate) fn device(seed: u8) -> DeviceId {
        DeviceId::from_bytes([seed; 32])
    }

    pub(crate) fn connected_session(frame_limit: usize) -> TransportSession {
        let caps = Capabilities::new(
            vec![ProtocolVersion::V1],
            vec!["e2ee.v1".to_owned(), "mls.v1".to_owned()],
            CapabilityLimits::new(frame_limit, frame_limit, frame_limit),
        );
        let negotiated = negotiate_capabilities(caps.clone(), caps).unwrap();
        let mut session = TransportSession::open(crate::session::SessionConfig {
            local_endpoint: endpoint(1),
            local_device: device(0xa1),
            peer_endpoint: endpoint(2),
            peer_device: device(0xb2),
            negotiated: Some(negotiated),
            now_unix_ms: 0,
            limits: SessionLimits {
                max_frame_bytes: frame_limit,
                ..SessionLimits::DEFAULT
            },
        })
        .unwrap();
        session.apply(ConnectionEvent::Connect).unwrap();
        session.apply(ConnectionEvent::Connected).unwrap();
        session
    }

    struct MemorySink {
        records: Vec<Vec<u8>>,
        queued: usize,
        fail_at: Option<(usize, WireFailure)>,
    }

    impl MemorySink {
        fn new() -> Self {
            Self {
                records: Vec::new(),
                queued: 0,
                fail_at: None,
            }
        }
    }

    impl WireSink for MemorySink {
        fn write_record(&mut self, record: &[u8]) -> core::result::Result<(), WireFailure> {
            if let Some((at, failure)) = self.fail_at
                && self.records.len() == at
            {
                return Err(failure);
            }
            self.records.push(record.to_vec());
            Ok(())
        }

        fn queued_records(&self) -> usize {
            self.queued
        }
    }

    fn tracker(max_attempts: u32, jitter: u64) -> RetryTracker {
        RetryTracker::new(crate::retry::RetryPolicy {
            base_delay_ms: 100,
            max_delay_ms: 1_000,
            max_attempts,
            jitter_max_ms: jitter,
            jitter_seed: 1,
        })
        .unwrap()
    }

    #[test]
    fn sends_single_and_multi_fragment_frames() {
        let session = connected_session(16 * 1024);
        let mut sink = MemorySink::new();
        let mut retry = tracker(3, 0);
        let outcome = send_peer_frame(
            &session,
            1,
            b"hello",
            &mut sink,
            SendBudget::DEFAULT,
            &mut retry,
            1_000,
        )
        .unwrap();
        assert_eq!(
            outcome,
            SendOutcome::Sent {
                frame_id: 1,
                fragments: 1,
                records: 1
            }
        );
        assert_eq!(sink.records.len(), 1);
        // 10 KiB / 4 KiB 分片 = 3 片。
        let payload = vec![7u8; 10 * 1024];
        let outcome = send_peer_frame(
            &session,
            2,
            &payload,
            &mut sink,
            SendBudget::DEFAULT,
            &mut retry,
            1_000,
        )
        .unwrap();
        assert_eq!(
            outcome,
            SendOutcome::Sent {
                frame_id: 2,
                fragments: 3,
                records: 3
            }
        );
        assert_eq!(sink.records.len(), 4);
    }

    #[test]
    fn rejects_unusable_session_and_invalid_payloads() {
        let session = connected_session(4 * 1024);
        let mut sink = MemorySink::new();
        let mut retry = tracker(3, 0);
        assert_eq!(
            send_peer_frame(
                &session,
                1,
                b"",
                &mut sink,
                SendBudget::DEFAULT,
                &mut retry,
                0
            )
            .unwrap_err()
            .code(),
            "MALFORMED_FRAME"
        );
        let too_big = vec![0u8; 4 * 1024 + 1];
        assert_eq!(
            send_peer_frame(
                &session,
                1,
                &too_big,
                &mut sink,
                SendBudget::DEFAULT,
                &mut retry,
                0
            )
            .unwrap_err()
            .code(),
            "CONTENT_TOO_LARGE"
        );
        // 会话未连接。
        let mut disconnected = TransportSession::open(crate::session::SessionConfig {
            local_endpoint: endpoint(1),
            local_device: device(0xa1),
            peer_endpoint: endpoint(2),
            peer_device: device(0xb2),
            negotiated: None,
            now_unix_ms: 0,
            limits: SessionLimits::DEFAULT,
        })
        .unwrap();
        assert_eq!(
            send_peer_frame(
                &disconnected,
                1,
                b"x",
                &mut sink,
                SendBudget::DEFAULT,
                &mut retry,
                0
            )
            .unwrap_err()
            .code(),
            "SESSION_NOT_USABLE"
        );
        // 非法预算。
        let mut invalid = SendBudget::DEFAULT;
        invalid.max_queue_depth = 0;
        assert_eq!(
            send_peer_frame(&session, 1, b"x", &mut sink, invalid, &mut retry, 0)
                .unwrap_err()
                .code(),
            "INVALID_TRANSPORT_CONFIG"
        );
        let _ = disconnected.apply(ConnectionEvent::Close);
    }

    #[test]
    fn queue_and_record_budgets_are_enforced() {
        let session = connected_session(16 * 1024);
        let mut sink = MemorySink::new();
        let mut retry = tracker(3, 0);
        // 队列深度只剩 1 条，但帧需要 2 条。
        sink.queued = 1;
        let partially = SendBudget {
            max_queue_depth: 2,
            ..SendBudget::DEFAULT
        };
        let payload = vec![1u8; 4 * 1024 + 1];
        assert_eq!(
            send_peer_frame(&session, 1, &payload, &mut sink, partially, &mut retry, 0)
                .unwrap_err()
                .code(),
            "SEND_BUDGET_EXCEEDED"
        );
        // 记录数预算不足：16 KiB 帧需要 4 片，但只允许 2 条。
        let tight_records = SendBudget {
            max_records_per_send: 2,
            ..SendBudget::DEFAULT
        };
        assert_eq!(
            send_peer_frame(&session, 1, b"x", &mut sink, tight_records, &mut retry, 0)
                .unwrap_err()
                .code(),
            "SEND_BUDGET_EXCEEDED"
        );
    }

    #[test]
    fn wire_failures_map_to_retry_or_fatal() {
        let session = connected_session(16 * 1024);
        let mut retry = tracker(2, 0);
        // 第 2 条记录可重试失败：给出退避。
        let mut sink = MemorySink::new();
        sink.fail_at = Some((1, WireFailure::Unavailable));
        let outcome = send_peer_frame(
            &session,
            1,
            &vec![1u8; 5 * 1024],
            &mut sink,
            SendBudget::DEFAULT,
            &mut retry,
            1_000,
        )
        .unwrap();
        match outcome {
            SendOutcome::RetryableFailure {
                sent_records,
                fragment_index,
                delay_ms,
                next_attempt_at_ms,
                ..
            } => {
                assert_eq!(sent_records, 1);
                assert_eq!(fragment_index, 1);
                assert_eq!(delay_ms, 100);
                assert_eq!(next_attempt_at_ms, 1_100);
            }
            other => panic!("expected retryable, got {other:?}"),
        }
        // 可重试失败但预算耗尽 → 致命。
        let mut sink = MemorySink::new();
        sink.fail_at = Some((0, WireFailure::Unavailable));
        let mut exhausted = tracker(1, 0);
        let first = send_peer_frame(
            &session,
            2,
            &vec![2u8; 5 * 1024],
            &mut sink,
            SendBudget::DEFAULT,
            &mut exhausted,
            0,
        )
        .unwrap();
        assert!(first.is_retryable());
        let second = send_peer_frame(
            &session,
            2,
            &vec![2u8; 5 * 1024],
            &mut sink,
            SendBudget::DEFAULT,
            &mut exhausted,
            0,
        )
        .unwrap();
        assert_eq!(
            second,
            SendOutcome::FatalFailure {
                frame_id: 2,
                fragment_index: 0,
                sent_records: 0,
                reason: FatalReason::RetryBudgetExhausted
            }
        );
        // 连接关闭 → 致命且不消耗重试预算。
        let mut sink = MemorySink::new();
        sink.fail_at = Some((0, WireFailure::Closed));
        let mut fresh = tracker(3, 0);
        let outcome = send_peer_frame(
            &session,
            3,
            &vec![3u8; 5 * 1024],
            &mut sink,
            SendBudget::DEFAULT,
            &mut fresh,
            0,
        )
        .unwrap();
        assert_eq!(
            outcome,
            SendOutcome::FatalFailure {
                frame_id: 3,
                fragment_index: 0,
                sent_records: 0,
                reason: FatalReason::Wire(WireFailure::Closed)
            }
        );
        assert_eq!(fresh.attempts(), 0);
        // 成功写出会重置重试计数。
        let mut sink = MemorySink::new();
        let mut reset = tracker(3, 0);
        sink.fail_at = Some((0, WireFailure::Unavailable));
        assert!(
            send_peer_frame(
                &session,
                4,
                b"x",
                &mut sink,
                SendBudget::DEFAULT,
                &mut reset,
                0
            )
            .unwrap()
            .is_retryable()
        );
        sink.fail_at = None;
        assert!(
            send_peer_frame(
                &session,
                5,
                b"x",
                &mut sink,
                SendBudget::DEFAULT,
                &mut reset,
                0
            )
            .unwrap()
            .is_sent()
        );
        assert_eq!(reset.attempts(), 0);
    }
}

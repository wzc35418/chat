//! TRANSPORT-001~004 纯逻辑核心的端到端验收。
//!
//! 覆盖：会话建立与协商上限、连接状态机迁移矩阵与幂等、帧编解码/分片重组的边界与
//! 拒绝路径、确定性指数退避与预算耗尽；全程无网络、无 sleep。

use chat_protocol::{
    Capabilities, CapabilityLimits, DeviceId, ProtocolVersion, negotiate_capabilities,
};
use chat_transport::{
    ConnectionEvent, ConnectionState, EndpointId, FragmentAssembler, FrameCodec, FrameLimits,
    RetryOutcome, RetryPolicy, RetryStopReason, RetryTracker, SessionConfig, SessionLimits,
    TransitionTableCheck, TransportSession, open_session,
};

fn endpoint(seed: u8) -> EndpointId {
    EndpointId::from_bytes([seed; 32])
}

fn device(seed: u8) -> DeviceId {
    DeviceId::from_bytes([seed; 32])
}

fn capabilities(max_frame: usize) -> Capabilities {
    Capabilities::new(
        vec![ProtocolVersion::V1],
        vec!["e2ee.v1".to_owned(), "mls.v1".to_owned()],
        CapabilityLimits::new(max_frame, max_frame, max_frame),
    )
}

fn session(max_frame: usize, limits: SessionLimits) -> TransportSession {
    let negotiated =
        negotiate_capabilities(capabilities(max_frame), capabilities(max_frame)).unwrap();
    open_session(SessionConfig {
        local_endpoint: endpoint(1),
        local_device: device(0xa1),
        peer_endpoint: endpoint(2),
        peer_device: device(0xb2),
        negotiated: Some(negotiated),
        now_unix_ms: 1_700_000_000_000,
        limits,
    })
    .unwrap()
}

#[test]
fn session_frame_retry_flow_is_closed_loop() {
    let limits = SessionLimits::DEFAULT;
    let mut session = session(64 * 1024, limits);
    assert_eq!(session.state(), ConnectionState::Disconnected);
    assert_eq!(
        session.apply(ConnectionEvent::Connect),
        Ok(ConnectionState::Connecting)
    );
    assert_eq!(
        session.apply(ConnectionEvent::Connected),
        Ok(ConnectionState::Connected)
    );
    assert!(session.is_usable());

    // 帧经规范化分片、线格式、严格重组回到原字节。
    let codec = FrameCodec::new(FrameLimits::DEFAULT).unwrap();
    let frame: Vec<u8> = (0..9_000).map(|index| (index % 251) as u8).collect();
    let wires = codec.encode_wire(1, &frame).unwrap();
    assert_eq!(wires.len(), 3);
    let mut assembler = FragmentAssembler::new(FrameLimits::DEFAULT).unwrap();
    let mut rebuilt = None;
    for wire in &wires {
        let fragment = codec.decode_wire(wire).unwrap();
        if let Some(complete) = assembler.push(fragment).unwrap() {
            rebuilt = Some(complete);
        }
    }
    assert_eq!(rebuilt.as_deref(), Some(frame.as_slice()));

    // 连接降级仍可传帧，恢复后正常；重试预算耗尽后 Stop。
    assert_eq!(
        session.apply(ConnectionEvent::Degrade),
        Ok(ConnectionState::Degraded)
    );
    assert!(session.is_usable());
    assert_eq!(
        session.apply(ConnectionEvent::Recover),
        Ok(ConnectionState::Connected)
    );

    let flaky = RetryPolicy {
        max_attempts: 2,
        jitter_max_ms: 0,
        ..RetryPolicy::DEFAULT
    };
    let mut tracker = RetryTracker::new(flaky).unwrap();
    let mut delays = Vec::new();
    loop {
        match tracker.record_failure(1_000) {
            RetryOutcome::Retry {
                attempt, delay_ms, ..
            } => {
                assert_eq!(attempt, delays.len() as u32 + 1);
                delays.push(delay_ms);
            }
            RetryOutcome::Stop { reason } => {
                assert_eq!(reason, RetryStopReason::BudgetExhausted);
                break;
            }
        }
    }
    assert_eq!(delays, vec![1_000, 2_000]);

    assert_eq!(
        session.apply(ConnectionEvent::Disconnect),
        Ok(ConnectionState::Disconnected)
    );
    assert_eq!(
        session.apply(ConnectionEvent::Close),
        Ok(ConnectionState::Closed)
    );
    assert_eq!(
        session
            .apply(ConnectionEvent::Connected)
            .unwrap_err()
            .code(),
        "INVALID_TRANSITION"
    );
}

#[test]
fn transition_matrix_is_complete_and_unique() {
    let stats = TransitionTableCheck::verify().expect("transition table must be consistent");
    assert_eq!(stats.states, 5);
    assert_eq!(stats.events, 6);
    assert_eq!(stats.legal, 19);
    assert_eq!(stats.illegal, 11);
    assert_eq!(stats.legal + stats.illegal, 30);
    // 合法迁移都指向已知状态。
    for (from, event, to) in chat_transport::TRANSITIONS {
        assert!(ConnectionState::ALL.contains(&from));
        assert!(ConnectionEvent::ALL.contains(&event));
        assert!(ConnectionState::ALL.contains(&to));
    }
}

#[test]
fn repeated_session_events_are_idempotent() {
    let mut session = session(64 * 1024, SessionLimits::DEFAULT);
    assert_eq!(
        session.apply(ConnectionEvent::Connect),
        Ok(ConnectionState::Connecting)
    );
    assert_eq!(
        session.apply(ConnectionEvent::Connect),
        Ok(ConnectionState::Connecting)
    );
    assert_eq!(
        session.apply(ConnectionEvent::Connected),
        Ok(ConnectionState::Connected)
    );
    assert_eq!(
        session.apply(ConnectionEvent::Connected),
        Ok(ConnectionState::Connected)
    );
    assert_eq!(
        session.apply(ConnectionEvent::Degrade),
        Ok(ConnectionState::Degraded)
    );
    assert_eq!(
        session.apply(ConnectionEvent::Degrade),
        Ok(ConnectionState::Degraded)
    );
    assert_eq!(
        session.apply(ConnectionEvent::Close),
        Ok(ConnectionState::Closed)
    );
    assert_eq!(
        session.apply(ConnectionEvent::Close),
        Ok(ConnectionState::Closed)
    );
}

#[test]
fn sessions_cannot_connect_without_negotiation_or_enlarge_limits() {
    // 未协商的会话可以建立，但不能发起连接。
    let mut unnegotiated = open_session(SessionConfig {
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
        unnegotiated
            .apply(ConnectionEvent::Connect)
            .unwrap_err()
            .code(),
        "CAPABILITY_NOT_NEGOTIATED"
    );
    // 会话帧上限大于协商上限 → CAPABILITY_MISMATCH。
    let negotiated =
        negotiate_capabilities(capabilities(8 * 1024), capabilities(8 * 1024)).unwrap();
    let mut limits = SessionLimits::DEFAULT;
    limits.max_frame_bytes = 16 * 1024;
    let error = open_session(SessionConfig {
        local_endpoint: endpoint(1),
        local_device: device(0xa1),
        peer_endpoint: endpoint(2),
        peer_device: device(0xb2),
        negotiated: Some(negotiated),
        now_unix_ms: 0,
        limits,
    })
    .unwrap_err();
    assert_eq!(error.code(), "CAPABILITY_MISMATCH");
    assert_eq!(
        error.frozen_code(),
        Some(chat_protocol::ErrorCode::ProtocolUnsupported)
    );
}

#[test]
fn frame_wire_record_rejects_stale_duplicate_and_out_of_order() {
    let limits = FrameLimits {
        max_frame_bytes: 32,
        max_fragment_bytes: 8,
        max_fragments_per_frame: 4,
    };
    let codec = FrameCodec::new(limits).unwrap();
    let wires = codec.encode_wire(10, &[7u8; 20]).unwrap();
    assert_eq!(wires.len(), 3);
    let fragments: Vec<_> = wires
        .iter()
        .map(|wire| codec.decode_wire(wire).unwrap())
        .collect();
    // 先完成帧 10。
    let mut assembler = FragmentAssembler::new(limits).unwrap();
    assert_eq!(assembler.push(fragments[0].clone()).unwrap(), None);
    assert_eq!(assembler.push(fragments[1].clone()).unwrap(), None);
    assert_eq!(
        assembler.push(fragments[2].clone()).unwrap(),
        Some(vec![7u8; 20])
    );
    // 旧帧迟到 → STALE_FRAME。
    assert_eq!(
        assembler.push(fragments[0].clone()).unwrap_err().code(),
        "STALE_FRAME"
    );
    // 重复分片 → DUPLICATE_FRAGMENT。
    let next = codec
        .decode_wire(&codec.encode_wire(11, &[9u8; 12]).unwrap()[0])
        .unwrap();
    assert_eq!(assembler.push(next.clone()).unwrap(), None);
    assert_eq!(
        assembler.push(next).unwrap_err().code(),
        "DUPLICATE_FRAGMENT"
    );
    // 未完成帧收到更旧的帧同样拒绝。
    let older = codec.decode_wire(&wires[1]).unwrap();
    assert_eq!(assembler.push(older).unwrap_err().code(), "STALE_FRAME");
}

#[test]
fn frame_decode_rejects_oversize_without_allocation() {
    let codec = FrameCodec::new(FrameLimits::DEFAULT).unwrap();
    let valid = codec.encode_wire(1, b"abc").unwrap().remove(0);
    let mut huge_total = valid.clone();
    huge_total[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        codec.decode_wire(&huge_total).unwrap_err().code(),
        "CONTENT_TOO_LARGE"
    );
    // 长度前缀声称 1 GiB：先比长度，不分配。
    let mut huge_prefix = valid.clone();
    huge_prefix[0..4].copy_from_slice(&(1024u32 * 1024 * 1024).to_be_bytes());
    assert_eq!(
        codec.decode_wire(&huge_prefix).unwrap_err().code(),
        "MALFORMED_FRAME"
    );
    // 声明 0 长度的记录。
    assert_eq!(
        codec.decode_wire(&0u32.to_be_bytes()).unwrap_err().code(),
        "MALFORMED_FRAME"
    );
}

#[test]
fn retry_sequence_is_deterministic_and_bounded() {
    fn sequence(policy: RetryPolicy, failures: u32) -> Vec<u64> {
        let mut tracker = RetryTracker::new(policy).unwrap();
        let mut delays = Vec::new();
        for _ in 0..failures {
            if let RetryOutcome::Retry { delay_ms, .. } = tracker.record_failure(0) {
                delays.push(delay_ms);
            }
        }
        delays
    }
    let policy = RetryPolicy::DEFAULT;
    let first = sequence(policy, 8);
    assert_eq!(first, sequence(policy, 8));
    assert!(first.iter().all(|delay| *delay <= policy.max_delay_ms));
    assert!(first.iter().all(|delay| *delay >= policy.base_delay_ms / 2));
    // 预算耗尽后 Stop。
    let mut tracker = RetryTracker::new(policy).unwrap();
    for _ in 0..policy.max_attempts {
        assert!(matches!(
            tracker.record_failure(0),
            RetryOutcome::Retry { .. }
        ));
    }
    assert!(matches!(
        tracker.record_failure(0),
        RetryOutcome::Stop {
            reason: RetryStopReason::BudgetExhausted
        }
    ));
    // jitter=0 时严格指数。
    let exact = RetryPolicy {
        base_delay_ms: 100,
        max_delay_ms: 1_000,
        max_attempts: 10,
        jitter_max_ms: 0,
        jitter_seed: 0,
    };
    assert_eq!(sequence(exact, 6), vec![100, 200, 400, 800, 1_000, 1_000]);
}

#[test]
fn error_codes_are_registered_with_protocol_relations() {
    let malformed = chat_transport::TransportError::MalformedFrame { detail: "x" };
    assert_eq!(malformed.code(), "MALFORMED_FRAME");
    assert_eq!(
        malformed.frozen_code(),
        Some(chat_protocol::ErrorCode::MalformedFrame)
    );
    let transition = chat_transport::TransportError::InvalidTransition;
    assert_eq!(transition.code(), "INVALID_TRANSITION");
    assert_eq!(transition.frozen_code(), None);
    assert!(!transition.retryable());
}

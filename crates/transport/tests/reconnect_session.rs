//! TRANSPORT-009：真实重连接入 Session（纯逻辑 + 真回环 iroh）。
//!
//! 覆盖：断线 → 重连计划 → 断线期间发送显式失败 → 重连复位（保留出站序号与重放窗口）→
//! 按序重放 → 窗口满失败；另含真 iroh 断线→重连→继续发送。
use std::time::Duration;

use chat_protocol::{
    Capabilities, CapabilityLimits, DeviceId, ProtocolVersion, negotiate_capabilities,
};
use chat_transport::{
    ConnectionEvent, EndpointConfig, EndpointId, SendBudget, SendFrameRequest, SendOutcome,
    SessionConfig, SessionLimits, TransportSession, start_endpoint,
};

const FRAME_LIMIT: usize = 8 * 1024;
const FRAGMENT: usize = 2 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

fn device(seed: u8) -> DeviceId {
    DeviceId::from_bytes([seed; 32])
}

fn session(local: u8, peer: u8) -> TransportSession {
    let caps = Capabilities::new(
        vec![ProtocolVersion::V1],
        vec!["e2ee.v1".to_owned(), "mls.v1".to_owned()],
        CapabilityLimits::new(FRAME_LIMIT, FRAME_LIMIT, FRAME_LIMIT),
    );
    let negotiated = negotiate_capabilities(caps.clone(), caps).expect("negotiation");
    TransportSession::open(SessionConfig {
        local_endpoint: EndpointId::from_bytes([local; 32]),
        local_device: device(0xa1),
        peer_endpoint: EndpointId::from_bytes([peer; 32]),
        peer_device: device(0xb2),
        negotiated: Some(negotiated),
        now_unix_ms: 0,
        limits: SessionLimits {
            max_frame_bytes: FRAME_LIMIT,
            ..SessionLimits::DEFAULT
        },
    })
    .expect("session")
}

fn connected(local: u8, peer: u8) -> TransportSession {
    let mut session = session(local, peer);
    session.apply(ConnectionEvent::Connect).expect("connect");
    session.apply(ConnectionEvent::Connected).expect("connected");
    session
}

fn send_budget() -> SendBudget {
    SendBudget {
        max_fragment_bytes: FRAGMENT,
        ..SendBudget::DEFAULT
    }
}

fn retry() -> chat_transport::RetryTracker {
    chat_transport::RetryTracker::new(chat_transport::RetryPolicy {
        base_delay_ms: 50,
        max_delay_ms: 1_000,
        max_attempts: 3,
        jitter_max_ms: 0,
        jitter_seed: 0,
    })
    .expect("retry")
}

#[test]
fn disconnect_blocks_sending_and_reconnect_preserves_seq_and_replay_window() {
    let mut session = connected(21, 22);
    assert!(session.is_usable());
    assert!(session.ensure_sendable().is_ok());
    // 出站序号从 0 开始，并进入重放窗口。
    let first = session.allocate_outgoing_frame().expect("seq 0");
    let second = session.allocate_outgoing_frame().expect("seq 1");
    assert_eq!((first, second), (0, 1));
    assert_eq!(session.next_seq(), 2);
    assert_eq!(session.replay_frames(), vec![0, 1]);

    // 断线：状态机到 Disconnected、错误码记录、发送显式失败。
    let state = session.on_disconnected("CONNECTION_CLOSED", 1_000);
    assert!(!session.is_usable());
    assert_eq!(state.last_error_code, Some("CONNECTION_CLOSED"));
    assert!(state.next_attempt_ms > 1_000, "a retry is scheduled");
    assert_eq!(
        session.ensure_sendable().expect_err("disconnected").code(),
        "SESSION_DISCONNECTED"
    );
    // 断线期间不能分配新序号（也不该悄悄丢掉窗口内容）。
    assert_eq!(session.next_seq(), 2);
    assert_eq!(session.replay_frames(), vec![0, 1]);

    // 重连计划：预算内返回调度，只算不拨号。
    let plan = session.reconnect_plan(1_100).expect("plan within budget");
    assert_eq!(plan.attempt, 1);
    assert!(plan.delay_ms > 0);
    assert_eq!(session.reconnect_state().attempts, 1);

    // 重连成功：计数清零，**出站序号与重放窗口保留**。
    let state = session.mark_reconnected().expect("reconnected");
    assert_eq!(state, chat_transport::ConnectionState::Connected);
    assert!(session.is_usable());
    assert!(session.ensure_sendable().is_ok());
    assert_eq!(session.reconnect_state().attempts, 0);
    assert_eq!(session.next_seq(), 2, "the outbound sequence is preserved");
    assert_eq!(session.replay_frames(), vec![0, 1], "the replay window is preserved");

    // 按序重放 + 对端确认后移出窗口。
    for seq in session.replay_frames() {
        assert!(session.acknowledge_replay(seq));
    }
    assert!(session.replay_frames().is_empty());
    assert!(!session.acknowledge_replay(0), "accepting twice is a no-op");
    // 重连后新序号继续递增（不断档）。
    assert_eq!(session.allocate_outgoing_frame().expect("seq 2"), 2);
}

#[test]
fn replay_window_is_bounded_and_reports_window_full() {
    let mut session = connected(23, 24);
    assert_eq!(session.replay_window(), chat_transport::session::DEFAULT_REPLAY_WINDOW);
    session.set_replay_window(2).expect("small window");
    assert_eq!(session.replay_window(), 2);
    assert_eq!(session.allocate_outgoing_frame().expect("0"), 0);
    assert_eq!(session.allocate_outgoing_frame().expect("1"), 1);
    let error = session
        .allocate_outgoing_frame()
        .expect_err("window full");
    assert_eq!(error.code(), "REPLAY_WINDOW_FULL");
    // 序号没有因为失败而前进（重试同一 seq 即可）。
    assert_eq!(session.next_seq(), 2);
    assert_eq!(session.replay_frames(), vec![0, 1]);
    // 确认一帧后腾出位置。
    assert!(session.acknowledge_replay(0));
    assert_eq!(session.allocate_outgoing_frame().expect("2"), 2);
    // 窗口越界拒绝。
    assert_eq!(
        session.set_replay_window(0).expect_err("zero").code(),
        "INVALID_TRANSPORT_CONFIG"
    );
    assert!(session
        .set_replay_window(chat_transport::session::MAX_REPLAY_WINDOW + 1)
        .is_err());
}

#[test]
fn reconnect_budget_exhaustion_is_explicit_after_the_default_policy() {
    let mut session = connected(25, 26);
    // DEFAULT 预算是 5 次：前 5 次给出调度，第 6 次显式失败。
    for attempt in 1..=5u32 {
        session.on_disconnected("PEER_UNREACHABLE", u64::from(attempt) * 1_000);
        let plan = session
            .reconnect_plan(u64::from(attempt) * 1_000)
            .expect("within budget");
        assert_eq!(plan.attempt, attempt);
    }
    session.on_disconnected("PEER_UNREACHABLE", 6_000);
    let error = session.reconnect_plan(6_000).expect_err("budget exhausted");
    assert_eq!(error.code(), "RECONNECT_BUDGET_EXHAUSTED");
    let state = session.reconnect_state();
    assert_eq!(state.attempts, 5, "the rejected attempt is not counted");
    assert_eq!(state.last_error_code, Some("RECONNECT_BUDGET_EXHAUSTED"));
    assert!(state.next_attempt_ms > 6_000);
}

#[tokio::test]
async fn real_loopback_disconnect_then_reconnect_continues_sending() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let alice = start_endpoint(EndpointConfig::loopback().with_secret_key([31u8; 32]))
            .await
            .expect("alice endpoint");
        let bob = start_endpoint(EndpointConfig::loopback().with_secret_key([32u8; 32]))
            .await
            .expect("bob endpoint");
        let mut alice_session = TransportSession::open(SessionConfig {
            local_endpoint: alice.node_id(),
            local_device: device(0xa1),
            peer_endpoint: bob.node_id(),
            peer_device: device(0xb2),
            negotiated: None,
            now_unix_ms: 0,
            limits: SessionLimits {
                max_frame_bytes: FRAME_LIMIT,
                ..SessionLimits::DEFAULT
            },
        })
        .expect("session");
        // 未协商不能连接（既有语义保持不变）。
        assert_eq!(
            alice_session
                .apply(ConnectionEvent::Connect)
                .expect_err("not negotiated")
                .code(),
            "CAPABILITY_NOT_NEGOTIATED"
        );
        let mut alice_session = {
            let mut fresh = session(41, 42);
            // 端点/设备需要与真实 iroh 身份一致：重建一个绑定的会话。
            fresh = TransportSession::open(SessionConfig {
                local_endpoint: alice.node_id(),
                local_device: device(0xa1),
                peer_endpoint: bob.node_id(),
                peer_device: device(0xb2),
                negotiated: fresh.negotiated().cloned(),
                now_unix_ms: 0,
                limits: SessionLimits {
                    max_frame_bytes: FRAME_LIMIT,
                    ..SessionLimits::DEFAULT
                },
            })
            .expect("bound session");
            fresh.apply(ConnectionEvent::Connect).expect("connect");
            fresh.apply(ConnectionEvent::Connected).expect("connected");
            fresh
        };
        let mut bob_session = TransportSession::open(SessionConfig {
            local_endpoint: bob.node_id(),
            local_device: device(0xb2),
            peer_endpoint: alice.node_id(),
            peer_device: device(0xa1),
            negotiated: alice_session.negotiated().cloned(),
            now_unix_ms: 0,
            limits: SessionLimits {
                max_frame_bytes: FRAME_LIMIT,
                ..SessionLimits::DEFAULT
            },
        })
        .expect("bob session");
        bob_session.apply(ConnectionEvent::Connect).expect("bob connect");
        bob_session.apply(ConnectionEvent::Connected).expect("bob connected");

        let mut accepts = bob.accept_loop(4).expect("accept loop");
        let alice_conn = alice
            .connect(bob.address().expect("address"), IO_TIMEOUT)
            .await
            .expect("alice connects");
        let _bob_conn = accepts.next(IO_TIMEOUT).await.expect("bob accepts");
        let mut stream = alice_conn.open_stream(IO_TIMEOUT).await.expect("stream");
        let mut retry = retry();
        let seq0 = alice_session.allocate_outgoing_frame().expect("seq 0");
        let sent = stream
            .send_frame(
                SendFrameRequest {
                    session: &alice_session,
                    frame_id: u32::try_from(seq0).expect("frame id"),
                    payload: b"before-disconnect",
                    budget: send_budget(),
                    retry: &mut retry,
                    now_unix_ms: 0,
                },
                IO_TIMEOUT,
            )
            .await
            .expect("send before disconnect");
        assert_eq!(
            sent,
            SendOutcome::Sent {
                frame_id: 0,
                fragments: 1,
                records: 1,
            }
        );

        // 断线：关闭连接 → 会话记录 + 发送显式失败。
        alice_conn.close(b"test disconnect");
        let state = alice_session.on_disconnected("CONNECTION_CLOSED", 1_000);
        assert_eq!(state.last_error_code, Some("CONNECTION_CLOSED"));
        assert_eq!(
            alice_session.ensure_sendable().expect_err("disconnected").code(),
            "SESSION_DISCONNECTED"
        );

        // 重连计划 + 标记成功，保留序号与重放窗口。
        alice_session.reconnect_plan(1_100).expect("plan");
        // 旧的 accept 循环要先释放，否则它会抢走新连接（同一个端点只能有一个消费者）。
        drop(accepts);
        let mut accepts = bob.accept_loop(4).expect("accept loop 2");
        let alice_conn2 = alice
            .connect(bob.address().expect("address"), IO_TIMEOUT)
            .await
            .expect("alice reconnects");
        let _bob_conn2 = accepts.next(IO_TIMEOUT).await.expect("bob accepts 2");
        alice_session.mark_reconnected().expect("reconnected");
        assert!(alice_session.is_usable());
        assert_eq!(alice_session.next_seq(), 1, "sequence preserved");
        assert_eq!(alice_session.replay_frames(), vec![0]);

        // 重连后继续发送新帧（序号不断档）。
        let seq1 = alice_session.allocate_outgoing_frame().expect("seq 1");
        assert!(alice_session.acknowledge_replay(0));
        let mut stream2 = alice_conn2.open_stream(IO_TIMEOUT).await.expect("stream 2");
        let sent = stream2
            .send_frame(
                SendFrameRequest {
                    session: &alice_session,
                    frame_id: u32::try_from(seq1).expect("frame id"),
                    payload: b"after-reconnect",
                    budget: send_budget(),
                    retry: &mut retry,
                    now_unix_ms: 0,
                },
                IO_TIMEOUT,
            )
            .await
            .expect("send after reconnect");
        assert!(sent.is_sent());
        assert_eq!(alice_session.replay_frames(), vec![1]);
    })
    .await
    .expect("test-level timeout: reconnect flow hung");
}


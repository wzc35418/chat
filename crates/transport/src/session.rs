//! TRANSPORT-001 的纯逻辑子集：会话建立与输入校验。
//!
//! 会话记录本机端点/设备、对端端点/设备、能力协商结果与初始连接状态。本模块不做
//! 真实网络 I/O：端点启动仍由 [`crate::network`] 的 P0 loopback 实现承载，真正接入
//! iroh 生产路径属于后续阶段。
//!
//! 校验顺序：预算 → 端点/设备标识 → 互斥关系 → 能力协商结果（版本、必需能力、
//! 帧上限不得放大）。任何失败都不构造会话对象。

use chat_protocol::{DeviceId, REQUIRED_FEATURES, SessionCapabilities};

use crate::error::{Result, TransportError};
use crate::frame::{FRAME_HARD_MAX_BYTES, FrameLimits};
use crate::reconnect::{
    ReconnectAttempts, ReconnectError, ReconnectSchedule, ReconnectState, RetryPolicy,
};
use crate::state::{ConnectionEvent, ConnectionState, ConnectionStateMachine};

/// 端点标识字节数（iroh EndpointId 为 32 字节公钥）。
pub const ENDPOINT_ID_BYTES: usize = 32;
/// 入站/出站队列硬上限。
pub const SESSION_HARD_MAX_QUEUE: usize = 4096;
/// 重连尝试硬上限。
pub const SESSION_HARD_MAX_RECONNECT_ATTEMPTS: u32 = 100;
/// 握手超时硬上限（60 秒）。
pub const SESSION_HARD_MAX_HANDSHAKE_MS: u64 = 60_000;
/// 重放窗口默认大小（未确认帧上限；TRANSPORT-009 追加）。
pub const DEFAULT_REPLAY_WINDOW: usize = 16;
/// 重放窗口硬上限（TRANSPORT-009 追加）。
pub const MAX_REPLAY_WINDOW: usize = 256;

/// 传输端点标识。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EndpointId([u8; ENDPOINT_ID_BYTES]);

impl EndpointId {
    /// 由固定 32 字节公钥构造。
    pub const fn from_bytes(bytes: [u8; ENDPOINT_ID_BYTES]) -> Self {
        Self(bytes)
    }

    /// 只读字节。
    pub const fn as_bytes(&self) -> &[u8; ENDPOINT_ID_BYTES] {
        &self.0
    }

    /// 是否全零（非法标识）。
    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; ENDPOINT_ID_BYTES]
    }

    /// 小写十六进制。
    pub fn to_hex(self) -> String {
        const HEX: [u8; 16] = *b"0123456789abcdef";
        let mut text = String::with_capacity(ENDPOINT_ID_BYTES * 2);
        for byte in self.0 {
            text.push(char::from(HEX[(byte >> 4) as usize]));
            text.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
        text
    }
}

impl core::fmt::Debug for EndpointId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "EndpointId({})", self.to_hex())
    }
}

impl core::fmt::Display for EndpointId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// 会话级预算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionLimits {
    /// 单帧字节上限（同时受协商结果约束）。
    pub max_frame_bytes: usize,
    /// 入站队列长度上限。
    pub max_inbound_queue: usize,
    /// 出站队列长度上限。
    pub max_outbound_queue: usize,
    /// 最大重连尝试次数（交给 [`crate::retry`] 的预算）。
    pub max_reconnect_attempts: u32,
    /// 握手超时预算（毫秒）。
    pub handshake_timeout_ms: u64,
}

impl SessionLimits {
    /// 默认预算。
    pub const DEFAULT: SessionLimits = SessionLimits {
        max_frame_bytes: 64 * 1024,
        max_inbound_queue: 64,
        max_outbound_queue: 64,
        max_reconnect_attempts: 5,
        handshake_timeout_ms: 10_000,
    };

    /// 有界校验；非法预算显式拒绝。
    pub fn validate(&self) -> Result<()> {
        if self.max_frame_bytes == 0 || self.max_frame_bytes > FRAME_HARD_MAX_BYTES {
            return Err(TransportError::InvalidConfig {
                detail: "maxFrameBytes must be within 1..=65536",
            });
        }
        if self.max_inbound_queue == 0 || self.max_inbound_queue > SESSION_HARD_MAX_QUEUE {
            return Err(TransportError::InvalidConfig {
                detail: "maxInboundQueue must be within 1..=4096",
            });
        }
        if self.max_outbound_queue == 0 || self.max_outbound_queue > SESSION_HARD_MAX_QUEUE {
            return Err(TransportError::InvalidConfig {
                detail: "maxOutboundQueue must be within 1..=4096",
            });
        }
        if self.max_reconnect_attempts == 0
            || self.max_reconnect_attempts > SESSION_HARD_MAX_RECONNECT_ATTEMPTS
        {
            return Err(TransportError::InvalidConfig {
                detail: "maxReconnectAttempts must be within 1..=100",
            });
        }
        if self.handshake_timeout_ms == 0
            || self.handshake_timeout_ms > SESSION_HARD_MAX_HANDSHAKE_MS
        {
            return Err(TransportError::InvalidConfig {
                detail: "handshakeTimeoutMs must be within 1..=60000",
            });
        }
        Ok(())
    }

    /// 对应的帧预算。
    pub const fn frame_limits(&self) -> FrameLimits {
        FrameLimits {
            max_frame_bytes: self.max_frame_bytes,
            max_fragment_bytes: self.max_frame_bytes,
            max_fragments_per_frame: 1,
        }
    }
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// 会话建立输入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    /// 本机端点。
    pub local_endpoint: EndpointId,
    /// 本机设备。
    pub local_device: DeviceId,
    /// 对端端点。
    pub peer_endpoint: EndpointId,
    /// 对端设备。
    pub peer_device: DeviceId,
    /// 能力协商结果；`None` 表示尚未协商（此时拒绝发起连接）。
    pub negotiated: Option<SessionCapabilities>,
    /// 建立时间（Unix 毫秒，仅记录）。
    pub now_unix_ms: u64,
    /// 会话预算。
    pub limits: SessionLimits,
}

/// 已建立的传输会话（纯逻辑，不含 socket）。
#[derive(Clone, Debug, PartialEq)]
pub struct TransportSession {
    local_endpoint: EndpointId,
    local_device: DeviceId,
    peer_endpoint: EndpointId,
    peer_device: DeviceId,
    negotiated: Option<SessionCapabilities>,
    created_at_unix_ms: u64,
    limits: SessionLimits,
    machine: ConnectionStateMachine,
    // ---- TRANSPORT-009 追加：重连调度与重放窗口（不改变既有字段语义） ----
    /// 重连尝试状态机（计数/错误码/下次尝试时间）。
    reconnect: ReconnectAttempts,
    /// 重放窗口大小（未确认帧上限，默认 [`DEFAULT_REPLAY_WINDOW`]）。
    replay_window: usize,
    /// 待重放的出站序号（按入队顺序）。
    replay_queue: Vec<u64>,
    /// 下一个出站帧序号（重连后**保持**，从不断档）。
    next_outgoing_seq: u64,
}

impl TransportSession {
    /// 校验输入并建立会话；初始状态固定为 `Disconnected`。
    pub fn open(config: SessionConfig) -> Result<Self> {
        config.limits.validate()?;
        if config.local_endpoint.is_zero() || config.peer_endpoint.is_zero() {
            return Err(TransportError::InvalidConfig {
                detail: "endpoint id must be non-zero",
            });
        }
        if config.local_endpoint == config.peer_endpoint {
            return Err(TransportError::InvalidConfig {
                detail: "local and peer endpoints must differ",
            });
        }
        if config.local_device == DeviceId::from_bytes([0; 32])
            || config.peer_device == DeviceId::from_bytes([0; 32])
        {
            return Err(TransportError::InvalidConfig {
                detail: "device id must be non-zero",
            });
        }
        if config.local_device == config.peer_device {
            return Err(TransportError::InvalidConfig {
                detail: "local and peer devices must differ",
            });
        }
        if let Some(negotiated) = &config.negotiated {
            validate_negotiation(negotiated, config.limits)?;
        }
        // 默认重连策略是常量且已校验；构造失败说明策略常量被改坏，按配置错误显式失败。
        let reconnect = ReconnectAttempts::new(RetryPolicy::DEFAULT).map_err(|_| {
            TransportError::InvalidConfig {
                detail: "default reconnect policy is invalid",
            }
        })?;
        Ok(Self {
            local_endpoint: config.local_endpoint,
            local_device: config.local_device,
            peer_endpoint: config.peer_endpoint,
            peer_device: config.peer_device,
            negotiated: config.negotiated,
            created_at_unix_ms: config.now_unix_ms,
            limits: config.limits,
            machine: ConnectionStateMachine::new(ConnectionState::Disconnected),
            reconnect,
            replay_window: DEFAULT_REPLAY_WINDOW,
            replay_queue: Vec::new(),
            next_outgoing_seq: 0,
        })
    }

    /// 当前连接状态。
    pub const fn state(&self) -> ConnectionState {
        self.machine.state()
    }

    /// 是否已有可用连接。
    pub const fn is_usable(&self) -> bool {
        self.machine.state().is_usable()
    }

    /// 能力协商结果。
    pub const fn negotiated(&self) -> Option<&SessionCapabilities> {
        self.negotiated.as_ref()
    }

    /// 会话预算。
    pub const fn limits(&self) -> SessionLimits {
        self.limits
    }

    /// 建立时间。
    pub const fn created_at_unix_ms(&self) -> u64 {
        self.created_at_unix_ms
    }

    /// 本机端点。
    pub const fn local_endpoint(&self) -> EndpointId {
        self.local_endpoint
    }

    /// 对端端点。
    pub const fn peer_endpoint(&self) -> EndpointId {
        self.peer_endpoint
    }

    /// 本机设备。
    pub const fn local_device(&self) -> DeviceId {
        self.local_device
    }

    /// 对端设备。
    pub const fn peer_device(&self) -> DeviceId {
        self.peer_device
    }

    /// 施加连接事件。
    ///
    /// 发起 `Connect` 前必须有协商结果，否则 `CAPABILITY_NOT_NEGOTIATED`；
    /// 其余迁移规则见 [`crate::state`]。
    pub fn apply(&mut self, event: ConnectionEvent) -> Result<ConnectionState> {
        if event == ConnectionEvent::Connect && self.negotiated.is_none() {
            return Err(TransportError::CapabilityNotNegotiated);
        }
        self.machine.apply(event)
    }

    // ---------------------------------------------------------------- TRANSPORT-009（追加）

    /// 断线检测：把状态机推进到 `Disconnected`，并记录错误码与退避计划。
    ///
    /// 不消耗重连预算（[`Self::reconnect_plan`] 才消耗）；重复调用幂等。
    pub fn on_disconnected(&mut self, code: &'static str, now_ms: u64) -> ReconnectState {
        if !self.machine.state().is_terminal() {
            let _ = self.machine.apply(ConnectionEvent::Disconnect);
        }
        self.reconnect.on_disconnected(code, now_ms)
    }

    /// 计算一次重连计划：预算内返回调度（延迟 + 下次尝试时间），超预算返回显式错误码。
    ///
    /// 只算调度，不重建连接；真实拨号由宿主完成，成功后调用 [`Self::mark_reconnected`]。
    pub fn reconnect_plan(
        &mut self,
        now_ms: u64,
    ) -> core::result::Result<ReconnectSchedule, ReconnectError> {
        self.reconnect.next_attempt(now_ms)
    }

    /// 标记重连成功：清空尝试计数与错误，**保留**出站序号与待重放帧。
    pub fn mark_reconnected(&mut self) -> Result<ConnectionState> {
        self.machine.apply(ConnectionEvent::Connect)?;
        let state = self.machine.apply(ConnectionEvent::Connected)?;
        self.reconnect.record_success();
        Ok(state)
    }

    /// 重连调度状态快照（attempts / last_error_code / next_attempt_ms）。
    pub fn reconnect_state(&self) -> ReconnectState {
        self.reconnect.state()
    }

    /// 断线期间发送必须显式失败（`SESSION_DISCONNECTED`）；可用时返回 Ok。
    pub fn ensure_sendable(&self) -> Result<()> {
        if self.machine.state().is_usable() {
            return Ok(());
        }
        Err(TransportError::SessionDisconnected {
            detail: "reconnect before sending frames",
        })
    }

    /// 下一个出站帧序号（尚未分配）。
    pub const fn next_seq(&self) -> u64 {
        self.next_outgoing_seq
    }

    /// 分配下一个出站帧序号并登记到重放窗口（满 → `REPLAY_WINDOW_FULL`）。
    pub fn allocate_outgoing_frame(&mut self) -> Result<u64> {
        let seq = self.next_outgoing_seq;
        self.queue_frame_for_replay(seq)?;
        self.next_outgoing_seq = self.next_outgoing_seq.saturating_add(1);
        Ok(seq)
    }

    /// 把某个出站序号登记进重放窗口（按调用顺序排队；窗口满 → `REPLAY_WINDOW_FULL`）。
    pub fn queue_frame_for_replay(&mut self, seq: u64) -> Result<()> {
        if self.replay_queue.len() >= self.replay_window {
            return Err(TransportError::ReplayWindowFull {
                limit: self.replay_window,
            });
        }
        self.replay_queue.push(seq);
        Ok(())
    }

    /// 当前待重放的出站序号（按入队顺序）。
    pub fn replay_frames(&self) -> Vec<u64> {
        self.replay_queue.clone()
    }

    /// 对端确认后移出重放窗口（返回是否真的移除了）。
    pub fn acknowledge_replay(&mut self, seq: u64) -> bool {
        match self.replay_queue.iter().position(|value| *value == seq) {
            Some(index) => {
                self.replay_queue.remove(index);
                true
            }
            None => false,
        }
    }

    /// 重放窗口大小（默认 [`DEFAULT_REPLAY_WINDOW`]）。
    pub const fn replay_window(&self) -> usize {
        self.replay_window
    }

    /// 调整重放窗口（1..=[`MAX_REPLAY_WINDOW`]；已排队条目按新上限截断最旧的）。
    pub fn set_replay_window(&mut self, window: usize) -> Result<()> {
        if window == 0 || window > MAX_REPLAY_WINDOW {
            return Err(TransportError::InvalidConfig {
                detail: "replay window must be within 1..=256",
            });
        }
        self.replay_window = window;
        if self.replay_queue.len() > window {
            let excess = self.replay_queue.len() - window;
            self.replay_queue.drain(..excess);
        }
        Ok(())
    }
}

fn validate_negotiation(negotiated: &SessionCapabilities, limits: SessionLimits) -> Result<()> {
    if !negotiated.protocol_version().is_supported() {
        return Err(TransportError::CapabilityMismatch {
            detail: "negotiated protocol version is not supported",
        });
    }
    for required in REQUIRED_FEATURES {
        if !negotiated.supports(required) {
            return Err(TransportError::CapabilityMismatch {
                detail: "negotiated capabilities miss a required feature",
            });
        }
    }
    if limits.max_frame_bytes > negotiated.limits().max_frame_bytes {
        return Err(TransportError::CapabilityMismatch {
            detail: "session frame limit exceeds the negotiated limit",
        });
    }
    Ok(())
}

/// TRANSPORT-001：建立会话（等价于 [`TransportSession::open`]）。
pub fn open_session(config: SessionConfig) -> Result<TransportSession> {
    TransportSession::open(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat_protocol::{Capabilities, CapabilityLimits, ProtocolVersion, negotiate_capabilities};

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

    fn negotiated(max_frame: usize) -> SessionCapabilities {
        negotiate_capabilities(capabilities(max_frame), capabilities(max_frame)).unwrap()
    }

    fn base_config() -> SessionConfig {
        SessionConfig {
            local_endpoint: endpoint(1),
            local_device: device(0xa1),
            peer_endpoint: endpoint(2),
            peer_device: device(0xb2),
            negotiated: Some(negotiated(64 * 1024)),
            now_unix_ms: 1_700_000_000_000,
            limits: SessionLimits::DEFAULT,
        }
    }

    #[test]
    fn opens_session_in_disconnected_state() {
        let session = open_session(base_config()).unwrap();
        assert_eq!(session.state(), ConnectionState::Disconnected);
        assert!(!session.is_usable());
        assert!(session.negotiated().is_some());
        assert_eq!(session.local_endpoint(), endpoint(1));
        assert_eq!(session.peer_endpoint(), endpoint(2));
        assert_eq!(session.local_device(), device(0xa1));
        assert_eq!(session.peer_device(), device(0xb2));
        assert_eq!(session.created_at_unix_ms(), 1_700_000_000_000);
        assert_eq!(session.limits(), SessionLimits::DEFAULT);
    }

    #[test]
    fn rejects_invalid_session_inputs() {
        // 未协商也能建立会话，但不能发起连接。
        let mut unnegotiated = base_config();
        unnegotiated.negotiated = None;
        let mut session = open_session(unnegotiated).unwrap();
        assert_eq!(
            session.apply(ConnectionEvent::Connect).unwrap_err().code(),
            "CAPABILITY_NOT_NEGOTIATED"
        );
        // 零/相同标识。
        let mut invalid = base_config();
        invalid.local_endpoint = endpoint(0);
        assert_eq!(
            open_session(invalid).unwrap_err().code(),
            "INVALID_TRANSPORT_CONFIG"
        );
        let mut invalid = base_config();
        invalid.peer_endpoint = endpoint(1);
        assert!(open_session(invalid).is_err());
        let mut invalid = base_config();
        invalid.local_device = device(0);
        assert!(open_session(invalid).is_err());
        let mut invalid = base_config();
        invalid.peer_device = device(0xa1);
        assert!(open_session(invalid).is_err());
    }

    #[test]
    fn session_limits_are_bounded() {
        let mut invalid = base_config();
        invalid.limits.max_frame_bytes = 0;
        assert_eq!(
            open_session(invalid).unwrap_err().code(),
            "INVALID_TRANSPORT_CONFIG"
        );
        let mut invalid = base_config();
        invalid.limits.max_frame_bytes = FRAME_HARD_MAX_BYTES + 1;
        assert!(open_session(invalid).is_err());
        let mut invalid = base_config();
        invalid.limits.max_inbound_queue = SESSION_HARD_MAX_QUEUE + 1;
        assert!(open_session(invalid).is_err());
        let mut invalid = base_config();
        invalid.limits.max_outbound_queue = 0;
        assert!(open_session(invalid).is_err());
        let mut invalid = base_config();
        invalid.limits.max_reconnect_attempts = SESSION_HARD_MAX_RECONNECT_ATTEMPTS + 1;
        assert!(open_session(invalid).is_err());
        let mut invalid = base_config();
        invalid.limits.handshake_timeout_ms = 0;
        assert!(open_session(invalid).is_err());
        // 默认预算合法。
        SessionLimits::DEFAULT.validate().unwrap();
    }

    #[test]
    fn negotiated_frame_limit_cannot_be_enlarged() {
        let mut config = base_config();
        config.negotiated = Some(negotiated(8 * 1024));
        config.limits.max_frame_bytes = 16 * 1024;
        assert_eq!(
            open_session(config).unwrap_err().code(),
            "CAPABILITY_MISMATCH"
        );
        // 等于或更小都允许。
        let mut config = base_config();
        config.negotiated = Some(negotiated(8 * 1024));
        config.limits.max_frame_bytes = 8 * 1024;
        assert!(open_session(config).is_ok());
    }

    #[test]
    fn session_lifecycle_uses_the_state_machine() {
        let mut session = open_session(base_config()).unwrap();
        assert_eq!(
            session.apply(ConnectionEvent::Connect),
            Ok(ConnectionState::Connecting)
        );
        assert_eq!(
            session.apply(ConnectionEvent::Connected),
            Ok(ConnectionState::Connected)
        );
        assert!(session.is_usable());
        assert_eq!(
            session.apply(ConnectionEvent::Degrade),
            Ok(ConnectionState::Degraded)
        );
        assert!(session.is_usable());
        assert_eq!(
            session.apply(ConnectionEvent::Recover),
            Ok(ConnectionState::Connected)
        );
        assert_eq!(
            session.apply(ConnectionEvent::Disconnect),
            Ok(ConnectionState::Disconnected)
        );
        assert_eq!(
            session.apply(ConnectionEvent::Close),
            Ok(ConnectionState::Closed)
        );
        // 终态后非法事件显式拒绝。
        assert_eq!(
            session.apply(ConnectionEvent::Connect).unwrap_err().code(),
            "INVALID_TRANSITION"
        );
        assert_eq!(
            session.apply(ConnectionEvent::Close),
            Ok(ConnectionState::Closed)
        );
    }

    #[test]
    fn endpoint_id_rendering_is_canonical() {
        let id = endpoint(0xab);
        assert_eq!(id.to_hex().len(), 64);
        assert_eq!(id.to_hex(), "ab".repeat(32));
        assert_eq!(format!("{id}"), id.to_hex());
        assert!(!id.is_zero());
    }
}

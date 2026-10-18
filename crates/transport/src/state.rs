//! TRANSPORT-002 的纯逻辑子集：连接状态机。
//!
//! 状态：`Disconnected → Connecting → Connected → Degraded → Closed`。
//!
//! 规则：
//!
//! - 合法迁移必须在冻结迁移表内；表外迁移返回 `INVALID_TRANSITION`；
//! - 重复事件幂等：同一状态再次收到「已经生效」的事件不改变状态、不重复产生副作用
//!   （例如 `Connecting + Connect → Connecting`、`Closed + Close → Closed`）；
//! - `Closed` 是终态：除重复 `Close` 外一律拒绝；
//! - 状态机本身不持有连接对象，不执行网络 I/O。

use crate::error::{Result, TransportError};

/// 连接状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConnectionState {
    /// 尚未连接。
    Disconnected,
    /// 正在连接/握手。
    Connecting,
    /// 已连接、路径正常。
    Connected,
    /// 已连接但质量降级（路径切换/丢包升高），仍可传帧。
    Degraded,
    /// 已关闭（终态）。
    Closed,
}

impl ConnectionState {
    /// 全部状态（迁移矩阵测试用）。
    pub const ALL: [ConnectionState; 5] = [
        ConnectionState::Disconnected,
        ConnectionState::Connecting,
        ConnectionState::Connected,
        ConnectionState::Degraded,
        ConnectionState::Closed,
    ];

    /// 冻结名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            ConnectionState::Disconnected => "Disconnected",
            ConnectionState::Connecting => "Connecting",
            ConnectionState::Connected => "Connected",
            ConnectionState::Degraded => "Degraded",
            ConnectionState::Closed => "Closed",
        }
    }

    /// 是否终态（不能再迁移到其它状态）。
    pub const fn is_terminal(self) -> bool {
        matches!(self, ConnectionState::Closed)
    }

    /// 是否已有可用连接（可发帧）。
    pub const fn is_usable(self) -> bool {
        matches!(self, ConnectionState::Connected | ConnectionState::Degraded)
    }
}

/// 连接事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConnectionEvent {
    /// 发起连接。
    Connect,
    /// 连接/握手成功。
    Connected,
    /// 路径降级。
    Degrade,
    /// 从降级恢复。
    Recover,
    /// 断开（可重连）。
    Disconnect,
    /// 关闭（终态）。
    Close,
}

impl ConnectionEvent {
    /// 全部事件（迁移矩阵测试用）。
    pub const ALL: [ConnectionEvent; 6] = [
        ConnectionEvent::Connect,
        ConnectionEvent::Connected,
        ConnectionEvent::Degrade,
        ConnectionEvent::Recover,
        ConnectionEvent::Disconnect,
        ConnectionEvent::Close,
    ];

    /// 冻结名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            ConnectionEvent::Connect => "Connect",
            ConnectionEvent::Connected => "Connected",
            ConnectionEvent::Degrade => "Degrade",
            ConnectionEvent::Recover => "Recover",
            ConnectionEvent::Disconnect => "Disconnect",
            ConnectionEvent::Close => "Close",
        }
    }
}

/// 冻结迁移表：`(当前状态, 事件, 下一状态)`。自环表示幂等重放。
pub const TRANSITIONS: [(ConnectionState, ConnectionEvent, ConnectionState); 19] = [
    // Disconnected
    (
        ConnectionState::Disconnected,
        ConnectionEvent::Connect,
        ConnectionState::Connecting,
    ),
    (
        ConnectionState::Disconnected,
        ConnectionEvent::Disconnect,
        ConnectionState::Disconnected,
    ),
    (
        ConnectionState::Disconnected,
        ConnectionEvent::Close,
        ConnectionState::Closed,
    ),
    // Connecting
    (
        ConnectionState::Connecting,
        ConnectionEvent::Connect,
        ConnectionState::Connecting,
    ),
    (
        ConnectionState::Connecting,
        ConnectionEvent::Connected,
        ConnectionState::Connected,
    ),
    (
        ConnectionState::Connecting,
        ConnectionEvent::Disconnect,
        ConnectionState::Disconnected,
    ),
    (
        ConnectionState::Connecting,
        ConnectionEvent::Close,
        ConnectionState::Closed,
    ),
    // Connected
    (
        ConnectionState::Connected,
        ConnectionEvent::Connect,
        ConnectionState::Connected,
    ),
    (
        ConnectionState::Connected,
        ConnectionEvent::Connected,
        ConnectionState::Connected,
    ),
    (
        ConnectionState::Connected,
        ConnectionEvent::Degrade,
        ConnectionState::Degraded,
    ),
    (
        ConnectionState::Connected,
        ConnectionEvent::Recover,
        ConnectionState::Connected,
    ),
    (
        ConnectionState::Connected,
        ConnectionEvent::Disconnect,
        ConnectionState::Disconnected,
    ),
    (
        ConnectionState::Connected,
        ConnectionEvent::Close,
        ConnectionState::Closed,
    ),
    // Degraded
    (
        ConnectionState::Degraded,
        ConnectionEvent::Degrade,
        ConnectionState::Degraded,
    ),
    (
        ConnectionState::Degraded,
        ConnectionEvent::Connected,
        ConnectionState::Connected,
    ),
    (
        ConnectionState::Degraded,
        ConnectionEvent::Recover,
        ConnectionState::Connected,
    ),
    (
        ConnectionState::Degraded,
        ConnectionEvent::Disconnect,
        ConnectionState::Disconnected,
    ),
    (
        ConnectionState::Degraded,
        ConnectionEvent::Close,
        ConnectionState::Closed,
    ),
    (
        ConnectionState::Closed,
        ConnectionEvent::Close,
        ConnectionState::Closed,
    ),
];

/// 计算一次迁移的目标状态。
///
/// 只查冻结表：表外迁移显式 `INVALID_TRANSITION`，不存在“猜测下一状态”。
pub fn next_state(current: ConnectionState, event: ConnectionEvent) -> Result<ConnectionState> {
    TRANSITIONS
        .iter()
        .find(|(from, trigger, _)| *from == current && *trigger == event)
        .map(|(_, _, to)| *to)
        .ok_or(TransportError::InvalidTransition)
}

/// 迁移表自检结果（用于宿主启动自检与测试）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransitionTableCheck {
    /// 状态数。
    pub states: usize,
    /// 事件数。
    pub events: usize,
    /// 合法迁移数。
    pub legal: usize,
    /// 非法迁移数。
    pub illegal: usize,
}

impl TransitionTableCheck {
    /// 校验冻结迁移表自身一致：条目唯一、端点合法、计数与逐格查询一致。
    ///
    /// 表损坏返回 `INVALID_TRANSITION`（宿主必须停止而不是继续使用坏表）。
    pub fn verify() -> Result<Self> {
        let mut seen = std::collections::BTreeSet::new();
        for (from, event, to) in TRANSITIONS {
            if !ConnectionState::ALL.contains(&from)
                || !ConnectionEvent::ALL.contains(&event)
                || !ConnectionState::ALL.contains(&to)
            {
                return Err(TransportError::InvalidTransition);
            }
            if !seen.insert((from, event)) {
                return Err(TransportError::InvalidTransition);
            }
        }
        let mut legal = 0usize;
        for state in ConnectionState::ALL {
            for event in ConnectionEvent::ALL {
                match next_state(state, event) {
                    Ok(_) => legal += 1,
                    Err(TransportError::InvalidTransition) => {}
                    Err(_) => return Err(TransportError::InvalidTransition),
                }
            }
        }
        if legal != TRANSITIONS.len() {
            return Err(TransportError::InvalidTransition);
        }
        let total = ConnectionState::ALL.len() * ConnectionEvent::ALL.len();
        Ok(Self {
            states: ConnectionState::ALL.len(),
            events: ConnectionEvent::ALL.len(),
            legal,
            illegal: total - legal,
        })
    }
}

/// 连接状态机（只持有状态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionStateMachine {
    state: ConnectionState,
}

impl ConnectionStateMachine {
    /// 以初始状态创建（正常流程从 `Disconnected` 开始）。
    pub const fn new(initial: ConnectionState) -> Self {
        Self { state: initial }
    }

    /// 当前状态。
    pub const fn state(&self) -> ConnectionState {
        self.state
    }

    /// 该事件在当前状态下是否合法。
    pub fn can_apply(&self, event: ConnectionEvent) -> bool {
        next_state(self.state, event).is_ok()
    }

    /// 当前状态允许的事件（有序）。
    pub fn legal_events(&self) -> Vec<ConnectionEvent> {
        ConnectionEvent::ALL
            .into_iter()
            .filter(|event| self.can_apply(*event))
            .collect()
    }

    /// 应用事件；非法迁移不改变状态并返回错误。
    pub fn apply(&mut self, event: ConnectionEvent) -> Result<ConnectionState> {
        let next = next_state(self.state, event)?;
        self.state = next;
        Ok(self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_table_matches_the_full_matrix() {
        // 逐格核对：表中声明的都合法且目标一致，表外的都显式拒绝。
        for state in ConnectionState::ALL {
            for event in ConnectionEvent::ALL {
                let expected = TRANSITIONS
                    .iter()
                    .find(|(from, trigger, _)| *from == state && *trigger == event)
                    .map(|(_, _, to)| *to);
                match expected {
                    Some(target) => {
                        assert_eq!(next_state(state, event), Ok(target));
                        let mut machine = ConnectionStateMachine::new(state);
                        assert_eq!(machine.apply(event), Ok(target));
                        assert_eq!(machine.state(), target);
                    }
                    None => {
                        assert_eq!(
                            next_state(state, event),
                            Err(TransportError::InvalidTransition),
                            "{state:?} + {event:?} must be rejected"
                        );
                        let mut machine = ConnectionStateMachine::new(state);
                        assert_eq!(machine.apply(event), Err(TransportError::InvalidTransition));
                        // 失败不改变状态。
                        assert_eq!(machine.state(), state);
                    }
                }
                assert_eq!(
                    ConnectionStateMachine::new(state).can_apply(event),
                    expected.is_some()
                );
            }
        }
    }

    #[test]
    fn happy_path_lifecycle() {
        let mut machine = ConnectionStateMachine::new(ConnectionState::Disconnected);
        assert_eq!(
            machine.apply(ConnectionEvent::Connect),
            Ok(ConnectionState::Connecting)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Connected),
            Ok(ConnectionState::Connected)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Degrade),
            Ok(ConnectionState::Degraded)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Recover),
            Ok(ConnectionState::Connected)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Disconnect),
            Ok(ConnectionState::Disconnected)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Close),
            Ok(ConnectionState::Closed)
        );
    }

    #[test]
    fn repeated_events_are_idempotent() {
        let mut machine = ConnectionStateMachine::new(ConnectionState::Disconnected);
        assert_eq!(
            machine.apply(ConnectionEvent::Connect),
            Ok(ConnectionState::Connecting)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Connect),
            Ok(ConnectionState::Connecting)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Connected),
            Ok(ConnectionState::Connected)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Connected),
            Ok(ConnectionState::Connected)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Degrade),
            Ok(ConnectionState::Degraded)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Degrade),
            Ok(ConnectionState::Degraded)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Disconnect),
            Ok(ConnectionState::Disconnected)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Disconnect),
            Ok(ConnectionState::Disconnected)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Close),
            Ok(ConnectionState::Closed)
        );
        assert_eq!(
            machine.apply(ConnectionEvent::Close),
            Ok(ConnectionState::Closed)
        );
        assert_eq!(machine.state(), ConnectionState::Closed);
    }

    #[test]
    fn closed_is_terminal_except_repeated_close() {
        let mut machine = ConnectionStateMachine::new(ConnectionState::Closed);
        for event in [
            ConnectionEvent::Connect,
            ConnectionEvent::Connected,
            ConnectionEvent::Degrade,
            ConnectionEvent::Recover,
            ConnectionEvent::Disconnect,
        ] {
            assert_eq!(machine.apply(event), Err(TransportError::InvalidTransition));
            assert_eq!(machine.state(), ConnectionState::Closed);
        }
        assert_eq!(
            machine.apply(ConnectionEvent::Close),
            Ok(ConnectionState::Closed)
        );
        assert_eq!(machine.legal_events(), vec![ConnectionEvent::Close]);
        assert!(ConnectionState::Closed.is_terminal());
    }

    #[test]
    fn illegal_transitions_are_explicit() {
        let cases = [
            (ConnectionState::Disconnected, ConnectionEvent::Connected),
            (ConnectionState::Disconnected, ConnectionEvent::Degrade),
            (ConnectionState::Disconnected, ConnectionEvent::Recover),
            (ConnectionState::Connecting, ConnectionEvent::Degrade),
            (ConnectionState::Connecting, ConnectionEvent::Recover),
        ];
        for (state, event) in cases {
            assert_eq!(
                next_state(state, event),
                Err(TransportError::InvalidTransition),
                "{state:?} + {event:?}"
            );
        }
        // Connecting 事件只能在 Connecting/Connected/Degraded 内幂等或前进。
        assert_eq!(
            next_state(ConnectionState::Degraded, ConnectionEvent::Connected),
            Ok(ConnectionState::Connected)
        );
    }

    #[test]
    fn usable_and_terminal_helpers_are_frozen() {
        assert!(ConnectionState::Connected.is_usable());
        assert!(ConnectionState::Degraded.is_usable());
        assert!(!ConnectionState::Connecting.is_usable());
        assert!(!ConnectionState::Disconnected.is_usable());
        assert!(!ConnectionState::Closed.is_usable());
        assert!(ConnectionState::Closed.is_terminal());
        assert!(!ConnectionState::Connected.is_terminal());
    }
}

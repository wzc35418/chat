//! 可注入的 wire 端口（无真实网络 I/O）。
//!
//! 传输层只依赖这两个 trait：真实实现由 QUIC/WS 适配器提供（P0 loopback 在
//! [`crate::network`]）；测试用内存队列即可覆盖丢片、乱序、重复与跨会话场景。
//! 端口失败只表达「暂时不可用」或「已关闭」两类，退避与终止判定由
//! [`crate::send`] / [`crate::retry`] 的显式策略决定。

use crate::session::EndpointId;

/// wire 端口失败类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireFailure {
    /// 暂时不可用/可重试（连接仍在，稍后重试）。
    Unavailable,
    /// 连接已关闭（致命，需重建会话）。
    Closed,
}

impl WireFailure {
    /// 是否可重试。
    pub const fn is_retryable(self) -> bool {
        matches!(self, WireFailure::Unavailable)
    }

    /// 是否致命。
    pub const fn is_fatal(self) -> bool {
        matches!(self, WireFailure::Closed)
    }

    /// 冻结名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            WireFailure::Unavailable => "Unavailable",
            WireFailure::Closed => "Closed",
        }
    }
}

/// 单帧分片写出端口。`record` 是 [`crate::frame::Fragment::wire_bytes`] 的完整记录。
pub trait WireSink {
    /// 写出一条记录。
    fn write_record(&mut self, record: &[u8]) -> core::result::Result<(), WireFailure>;

    /// 当前已排队未写出的记录数（用于发送预算/背压；默认无信息返回 0）。
    fn queued_records(&self) -> usize {
        0
    }
}

/// 逐片读取端口。
pub trait WireSource {
    /// 取下一条记录；`Ok(None)` 表示当前没有数据（不是错误）。
    fn next_record(&mut self) -> core::result::Result<Option<Vec<u8>>, WireFailure>;

    /// 连接认证提供的对端端点标识。传输层用它绑定会话；
    /// 返回 `None` 表示无法认证来源，接收方会显式拒绝（fail closed）。
    fn peer_endpoint(&self) -> Option<EndpointId> {
        None
    }
}

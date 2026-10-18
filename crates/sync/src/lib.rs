//! sync 模块：可靠发送、信箱轮询与退避状态机（本轮交付纯逻辑部分）。
//!
//! 模块边界：
//!
//! - [`clock`]：可注入时钟（测试确定性，生产用系统 UTC 时钟）；
//! - [`ids`]：ID 校验与确定性 SubmissionId 派生（PROTOCOL-001 强类型）；
//! - [`retry`]：SYNC-011 schedule_retry（指数退避 + 抖动 + Retry-After + 预算）；
//! - [`pull`]：SYNC-002 拉取循环决策核心（Pull/Backoff/Resync/Stop，缺口不跳过）；
//! - [`catchup`]：SYNC-008 追赶计划（分页拉取、逐 epoch 重建、等待控制结果）；
//! - [`mailbox`]：SYNC-007 consume_mailbox_page 与轮询状态机（游标/去重/乱序/预算/ACK）；
//! - [`send`]：SYNC-003/004 发送意图调度（同草稿同 SubmissionId、unknown 只对账、租约门）；
//! - [`engine`]：SYNC 编号与本模块实现的映射，以及本轮未实现项的边界说明。
//!
//! 全部函数是纯逻辑或显式状态机：不读网络、不读数据库、不 sleep；网络与持久化
//! 通过 trait/端口注入（见 [`mailbox::MailboxSource`]、[`mailbox::AckSink`]）。
//! 任何安全/顺序校验失败都显式返回冻结错误码（PROTOCOL-014），不模拟成功。
#![forbid(unsafe_code)]
pub mod catchup;
pub mod clock;
pub mod engine;
pub mod error;
pub mod ids;
pub mod mailbox;
pub mod pull;
pub mod retry;
pub mod send;

pub use error::{ErrorCode, Result, SyncError};

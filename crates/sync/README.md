# crates/sync

Rust sync 模块（可靠发送、信箱轮询、日志补齐与生命周期）。

已实现（纯逻辑部分，见 [sync.md](../../docs/implementation/sync.md) 的实现记录）：

- `retry`：SYNC-011 `schedule_retry` 与 `RetryLedger`（指数退避 + 抖动 + Retry-After + 预算）；
- `mailbox`：SYNC-007 单页消费与 `MailboxPoller` 轮询状态机（游标/去重/乱序/预算/ACK）；
- `send`：SYNC-003/004 的 `plan_submission` 调度决策（同草稿同 SubmissionId、unknown 只对账、租约门）；
- `clock`/`ids`/`error`/`engine`：可注入时钟、确定性 SubmissionId 派生、冻结错误码与模块映射。

未实现：SYNC-001/002/005/006/008/009/010/012/013/014（需要真实存储/网络/MLS 接入）。
本 crate 不连网、不连库、不 sleep；网络与持久化通过 trait/端口注入，测试使用确定性假实现。

阶段完成需实际实现和验证；通过本 crate 用例不代表整个 sync 模块完成。

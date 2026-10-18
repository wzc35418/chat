//! SYNC 编号与本模块实现的映射。
//!
//! 计划书 [docs/implementation/sync.md] 用 SYNC-001..014 编号函数。本轮交付
//! “纯逻辑、不连网”的部分，映射如下：
//!
//! | 计划书函数 | 本轮实现 | 说明 |
//! |---|---|---|
//! | SYNC-011 `schedule_retry` | [`crate::retry::schedule_retry`] / [`crate::retry::RetryLedger`] | DOMAIN-010 分类 + 指数退避 + 抖动 + Retry-After + 预算 |
//! | SYNC-007 `consume_mailbox_page` | [`crate::mailbox::MailboxPoller`]（消费、游标、ACK）；单页消费逻辑在 `consume` 内部 | 顺序/去重/乱序/预算/ACK 记账 |
//! | SYNC-003 `prepare_draft_job` / SYNC-004 `submit_attempt_job` | [`crate::send::plan_submission`] | 同草稿同 SubmissionId、unknown 只对账、租约门、成功才释放 |
//! | SYNC-002 的**拉取循环决策核心** | [`crate::pull::mailbox_pull_loop`] | Pull/Backoff/Resync/Stop；缺口→Resync、范围有界、退避封顶、失败预算终止 |
//! | SYNC-008 的**追赶计划核心** | [`crate::catchup::catchup_plan`] | 分页 PullLog、逐 epoch RebuildEpoch、缺控制结果 AwaitControl、超限显式失败 |
//!
//! **编号对照说明（重要）**：计划书的 SYNC-002 是 `run_outbox_once`（outbox 任务批执行），
//! 本轮的 SYNC-002 交付是常驻同步 worker 的**拉取循环决策**（输入本地游标/节点 head/
//! 上次结果/退避状态 → 下一步动作），对应的计划条目是 SYNC-006 步骤 2-3 与 SYNC-008 的内环；
//! outbox 批执行本身仍未实现，SYNC-002 的验收框保持未勾选。同理 SYNC-008 本轮只交付
//! **计划**（不联网、不验签、不应用），逐条应用属于 SYNC-009。
//!
//! 本轮**未实现**（需要真实存储/网络/MLS 接入，且不在本轮写入范围）：
//!
//! - SYNC-001 `enqueue_job`、SYNC-002 `run_outbox_once`（需要 STORAGE-014/015 的真实事务循环）；
//! - SYNC-005 `rebuild_stale_attempt`（需要 CRYPTO-008 清理 pending 分支）；
//! - SYNC-006 `sync_now`、SYNC-008 `catch_up_group` 的网络/验签/应用部分、SYNC-009 `apply_log_page`
//!   （需要 TRANSPORT-007/009 与 PROTOCOL-007 验签、CRYPTO-006/010）；
//! - SYNC-010 `process_delivery_receipt`、SYNC-012/013/014（生命周期/连接/节点配置）。
//!
//! 这些函数的“完成验收”复选框保持未勾选：通过本模块用例不代表整个 sync 模块完成。

pub use crate::{clock, ids, mailbox, retry, send};

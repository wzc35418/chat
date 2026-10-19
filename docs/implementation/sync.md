# 可靠发送、信箱、日志补齐与生命周期

阶段：P1 → P3。状态：函数级设计；SYNC-003/004（发送调度决策）、SYNC-007（信箱顺序/去重/预算/ACK 记账）、SYNC-011（重试/退避策略）、SYNC-002 的拉取循环决策核心（`mailbox_pull_loop`）、SYNC-008 的追赶计划核心（`catchup_plan`）已实现并被自动化用例覆盖，其余函数未开始。目标源码：[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)。

可靠性来自耐久队列，不依赖 Flow 或 WebSocket 是否在线。outbox 分开 Prepare、Append、PeerPretransfer、MailboxPut、DeliveryReceipt、MailboxAck、ControlRebuild 等任务。每群同一执行器串行推进密码学状态，多个群可以有界并行。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 常驻运行时的附件回收阶段（SYNC-003 收口，2026-10-19，Windows x64）

回收（ATTACHMENTS-008 的两阶段回收）以前只能由调用方手工驱动。本轮把它接进常驻循环，成为一个
**低频**阶段：默认每 10 分钟一次、可通过配置关闭。

代码：[crates/core/src/sync_runtime.rs](../../crates/core/src/sync_runtime.rs)；
测试：[crates/core/tests/sync_runtime_gc.rs](../../crates/core/tests/sync_runtime_gc.rs)（3 个，真库）。

### 配置

```text
SyncRuntimeConfig {
    ...,
    gc_interval_ms: u64,   // 0 = 关闭；默认 DEFAULT_GC_INTERVAL_MS = 10 分钟
    gc_grace_ms: i64,      // 宽限期；默认 DEFAULT_GC_GRACE_MS = 1 小时
}
// builder：config.with_attachment_gc(interval_ms, grace_ms)
```

### 循环里的位置与语义

每轮顺序：**outbox（阶段 0）→ 附件回收（阶段 0.5）→ stop 检查 → 拉取（阶段 1）→ 等待**。

1. **低频**：只有 `now >= gc_next_attempt_at_ms` 才跑；成功后下一次 = now + `gc_interval_ms`。
2. **两阶段**：一次调用 = `plan_attachment_gc`（只读）→ `execute_attachment_gc`（先标记 gc_pending 再删除），
   单轮最多一页（`MAX_ATTACHMENT_GC_BATCH = 256`）。
3. **失败隔离**：回收失败只写 `gc_last_error_code` 并按 `pull_policy.failure_delay_for` 退避
   （被 `policy.clamp` 夹在 [min,max] 轮等待内），**不影响** outbox 与拉取，也不占它们的失败预算。
4. **状态可见**：`SyncStatusView` 新增 `gc_removed_total` / `gc_last_error_code` / `gc_next_attempt_at_ms`。
5. **stop 在任务边界生效**：回收阶段前与拉取阶段前各检查一次停止信号，正在执行的阶段不被中断。

### 证据（ @@cargo test --offline -p chat-core --test sync_runtime_gc@@，3 passed ）

- `expired_unreferenced_attachments_are_reclaimed_and_counted`：真库里两个超期无引用附件（带密钥）被回收、
  `gc_removed_total == 2`、`gc_last_error_code` 为空；被消息引用的与宽限期内的附件都保留。
- `gc_disabled_keeps_the_counter_at_zero`：`interv​al = 0` 时跑过 3 轮以上，`gc_removed_total` 恒为 0，
  附件原样保留。
- `a_failing_gc_never_blocks_the_pull_loop`：非法宽限期（-1）让回收必然失败 → `gc_last_error_code = INVALID_INPUT`、
  计数不增、`rounds` 继续增长（循环没有被回收拖住），附件未被删除。

### 已知限制

1. **只有回收阶段，没有缓存目录清理**：磁盘密文缓存仍由 `collect_unreferenced_attachments` 负责。
2. **回收与拉取共用一个库句柄**：单轮回收会占用该轮的一点时间（两阶段各一次短事务），因此在最坏情况下
   会让该轮拉取晚几毫秒开始；没有单独线程。
3. **宽限期由部署侧给**：给得太小会把“正在上传/投递”的附件误判为孤儿（outbox 引用会被护栏挡住，
   但更复杂的流水线仍建议 ≥ 1 小时）。

## 作者账户：凭据目录映射（SYNC-002 补全，2026-10-19，Windows x64）

同步 apply 与应用消息落库以前用**确定性占位**把作者凭据变成账户 ID，别的设备无法把它对上真实账户。
本轮改走 `crates/storage` 的 0011 凭据目录（见 storage.md）：

- `sync_worker::resolve_author_account(store, credential) -> (账户文本, 是否已知)`：目录命中 → 目录里的
  `account_id`（`known = true`）；未命中 → 确定性占位（`known = false`），调用方必须计数；
- `apply_verified_page`（现在公开，作为拉取管线的 apply 步骤可被诊断/测试直接调用）在落库每条应用消息前
  解析作者：已知 → 真实账户，未知 → 占位并 `ApplyReport::unknown_authors += 1`；
- `ApplyReport` 新增字段 `unknown_authors: u64`（**绝不静默伪造**：未知作者既不丢消息也不假装已知）。

目录的写入点在 core 的群路径：`create_group(_with_welcome)` 与 `group_join_from_welcome` 成功后把本机与
已知成员的凭据（含 `device_id`）写一次（`sync_credential_directory`，幂等 upsert）。

### 证据

- `credential_directory::creating_a_group_writes_the_credential_directory`：建群后目录含 alice/bob 两条、
  ID 都是 32 字节且 account ≠ device；`resolve_author_account("alice")` 返回目录里的账户（known = true），
  未知凭据返回占位（known = false）；`ApplyReport::default().unknown_authors == 0`。
- `credential_directory::joining_writes_the_directory_and_unknown_credentials_count_as_placeholders`：
  加入侧同样写入两条；删掉 alice 的映射后同一凭据只能拿到占位（对应 apply 时的 `unknown_authors` 计数）。

### 已知限制

1. **未知作者的端到端计数未用真节点覆盖**：构造带签名的 `LogEntry` 需要节点回执与候选签名，本轮用
   `resolve_author_account` 的 known/unknown 分支 + `ApplyReport` 字段做代理验证；接节点夹具后可补一条
   端到端用例。
2. **目录只在建群/入群写入**：成员后续增删不刷新目录（凭据不变，故目前够用）；身份层接入后应补。
3. **account 值仍是本机派生**（见 storage.md 限制 1）。

## 重试/信箱/发送调度实现记录（2026-10-18，Windows x64）

本轮实现 sync 的**纯逻辑部分**（不连网、不连库、不 sleep），网络与持久化通过 trait/端口注入：

- [crates/sync/src/error.rs](../../crates/sync/src/error.rs)：`SyncError`（冻结码 + 静态诊断键）与 `to_public_error`。
- [crates/sync/src/clock.rs](../../crates/sync/src/clock.rs)：`Clock` trait、`FixedClock`（测试确定性）、`SystemClock`（生产）。
- [crates/sync/src/ids.rs](../../crates/sync/src/ids.rs)：文本 ID 校验与**确定性 SubmissionId 派生**（PROTOCOL-001 强类型）。
- [crates/sync/src/retry.rs](../../crates/sync/src/retry.rs)：SYNC-011 `schedule_retry` 与 `RetryLedger`。
- [crates/sync/src/mailbox.rs](../../crates/sync/src/mailbox.rs)：SYNC-007 单页消费 + `MailboxPoller` 轮询状态机（`MailboxSource`/`AckSink` 端口）。
- [crates/sync/src/send.rs](../../crates/sync/src/send.rs)：SYNC-003/SYNC-004 的 `plan_submission` 调度决策。
- [crates/sync/src/pull.rs](../../crates/sync/src/pull.rs)：SYNC-002 的拉取循环决策核心 `mailbox_pull_loop`（Pull/Backoff/Resync/Stop）。
- [crates/sync/src/catchup.rs](../../crates/sync/src/catchup.rs)：SYNC-008 的追赶计划核心 `catchup_plan`（分页拉取、逐 epoch 重建、等待控制结果）。
- [crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)：SYNC 编号与上述模块的映射，以及未实现项边界。

依赖只来自根 `[workspace.dependencies]`（`chat-protocol`、`chat-domain`、`sha2`）。本轮同时**移除**了 chat-sync 里未使用的 `chat-storage`/`chat-transport` 依赖：它们会把其他 crate 的 clippy 告警带进本 crate 的验收命令，且本轮的纯逻辑不需要它们；接入 SYNC-002/005/006 时必须重新加入。

### 错误码映射（码表取自 PROTOCOL-014，不新增码）

计划书里的 `JOB_KEY_CONFLICT` / `PERMANENT_FAILURE` / `RETRY_BUDGET_EXHAUSTED` 不是码表条目，按下表表达：

| 场景 | 冻结码 |
|---|---|
| 参数/策略非法（limit、worker、policy、负时钟、重建序号超预算） | `INVALID_TARGET` |
| ID 文本非法（item/group/worker/draft） | `INVALID_ID` |
| 失败序号从 1 起（attempt 0） | `INVALID_SEQUENCE` |
| 页/条目/暂存/待 ACK 超上限 | `CONTENT_TOO_LARGE` |
| 游标回退/无进展/页内位置乱序/落在游标之后的未知条目 | `INVALID_CURSOR` |
| 同一 item id 内容不同（重复但内容冲突） | `INVALID_CANDIDATE` |
| 乱序暂存满（需要先补日志） | `MISSING_LOG_ENTRIES` |
| 投影不一致（尝试属于别的草稿、租约无持有者、草稿已接受但尝试未接受、拉取页条数与范围不一致、失败未计数） | `STATE_CORRUPTED` |
| 本地已验证日志序号/epoch 超过节点 head（节点回退或换节点） | `LOG_CONFLICT` |
| 追赶跨度/步数超过单次计划上限（不截断） | `CONTENT_TOO_LARGE` |
| 跨 epoch 数超过单次计划上限（缺控制日志） | `MISSING_LOG_ENTRIES` |
| 草稿已取消、未知结果被强行重发 | `INVALID_STATE_TRANSITION` / `UNKNOWN_RESULT` |
| 业务键冲突（计划书的 `JOB_KEY_CONFLICT`） | `INVALID_STATE_TRANSITION` |
| 永久失败 / 重试预算耗尽（计划书的结果类别） | `RetryAction::PermanentFailure` / `RetryAction::RetryBudgetExhausted`，并保留根因冻结码 |
| 网络/节点/限流/存储忙 | 原样传播（`NETWORK_UNAVAILABLE` / `NODE_UNAVAILABLE` / `RATE_LIMITED` / `STORAGE_LOCKED`） |

### 重试策略（SYNC-011）

固定参数（`RetryPolicy::DEFAULT`，全部可测）：基础 1 s、倍数 2、自动退避上限 60 s、抖动 ±20%、连续失败预算 8 次。语义：

1. 错误先经 `chat_domain::classify_retry`（DOMAIN-010）分类，本层不发明分类。
2. `ResendSameBytes` → 指数退避 `base × 2^(attempt-1)`（封顶 60 s），再施加由注入 `JitterSource` 抽样的 ±20% 抖动（抽样 0/500/1000 = 下界/中点/上界）。
3. Retry-After 是**下界**：`delay = max(抖动后退避, clamp(retryAfter, 1, 300000))`，上限取 PROTOCOL-014 的 `MAX_RETRY_AFTER_MS`（5 分钟）。
4. `QueryExisting`（结果未知）→ 只按原 SubmissionId 对账，仍给有界轮询延迟；`SyncThenRebuild`/`WaitForUser`/`PermanentFailure` → 无退避时刻。
5. `attempt > max_attempts` → `RetryAction::RetryBudgetExhausted`（终态，保留根因码）。
6. 连续失败计数用 `RetryLedger`：**唯一重置条件是正式成功证据**（`record_success`）；重启用 `RetryLedger::restore` 恢复计数，绝不归零。

### 信箱轮询（SYNC-007）

`MailboxPoller::tick(clock, source, ack)` 顺序：**先补 ACK → 再拉取 → 消费 → 记账 → 计算下一次时间**。

- 游标：页面 `next.position` 只能前进；回退、非空页无进展、页内位置不递增、位置落在游标之后且 ID 未知 → `INVALID_CURSOR`，且状态不变。
- 去重：保留窗口（4096 条）内的 item id 与内容指纹；相同 → 记重复、不重复投递；不同 → `INVALID_CANDIDATE`。
- 乱序：群日志按 `group_seq` 连续推进；跳号先**耐久暂存**（同时 ACK，避免节点重发），缺号到达后按序提升；暂存上限 512，超限 `MISSING_LOG_ENTRIES` 并整页回滚。
- 预算：单页 ≤100 条、单页 ≤1 MiB、单条 ≤64 KiB；跟踪群 ≤256；待 ACK 集合 ≤4096（满则背压）。
- ACK 失败：保留待 ACK 集合、游标不回退、不重复投递；下一轮先重试 ACK（1 s 退避）。
- 空页不空转：空页退避 30 s，非空页之间至少 1 s；时间未到返回 `Idle` 且**不调用数据源**；拉取失败按 1 s 起指数退避（封顶 60 s）。

### 发送调度（SYNC-003 / SYNC-004）

`plan_submission(draft, attempt, worker, now)` 是纯决策函数，输出下一步动作：

- `Prepare`：新尝试；SubmissionId = SHA-256(域分隔 ‖ draft_id ‖ mls_epoch ‖ attempt_index) 的 PROTOCOL-001 强类型 ID，同一输入永远同 ID，因此重启/事务重试/二次调度都不会产生第二个 ID，也不会第二次加密。
- `Resubmit`：已有耐久尝试且**租约已过期/未租用**，同 ID 同字节重发（`was_submitted` 仅诊断）。
- `WaitForLease`：`lease_until_ms > now` 时禁止并发重发（含自己持有的情况），与 STORAGE-014 的租约语义一致。
- `QueryExisting`：`unknown` 只对账——即使租约已过期也不重发、不重新加密。
- `Release`：只有 `Accepted` 才释放；重复调度保持 Release。
- `Rebuild`：`Rejected` 且草稿未取消 → `attempt_index + 1` 得到新 SubmissionId（先同步补齐，本函数不猜 epoch）；重建序号上限 64。
- `Abandoned`：草稿已取消（已接受的尝试仍走 Release，不丢成功事实）。

### 拉取循环（SYNC-002 决策核心）

`mailbox_pull_loop(policy, input) -> Result<PullAction>` 是常驻同步 worker 的循环控制，纯决策、不联网：

- 输入：本地已应用到的位置 `local_tail`、节点签名 head 的 `node_head`、上一次拉取结果 `LastPull`（未拉过/连续页/空页/失败）、连续失败次数、`now_ms`、上次拉取时间、`next_attempt_at_ms`。
- 输出：`Pull{from,to}`（含端点，条数 ≤ `page_items`）、`Backoff{delay_ms,next_attempt_at_ms,reason}`、`Resync{reason}`、`Stop{reason}`。
- **缺口不跳过**：上一页不覆盖 `local_tail+1`（空洞）、整页落在本地游标之前（过期页）、页越过节点 head、或节点声称 head 前进却返回空页，一律 `Resync`；交给 SYNC-008 追赶计划重新对齐。
- **范围有界**：单次 Pull ≤ `page_items`（默认 100，可配 ≤500）；落后超过 `max_incremental_gap`（默认 1000）不一次拉完，而是 `Resync{GapTooLarge}`；本地越过节点 head 是 `Resync{LocalAheadOfNode}`（节点回退/换节点不能靠“继续拉”自愈）。
- **不忙循环**：未到 `next_attempt_at_ms` 返回 `Backoff{NotDue}` 且不调用数据源；非空页之间至少 `page_interval_ms`（默认 1 s）；失败按 `base × 2^(failures-1)`（1/2/4/8/16/32/60 s，封顶 60 s）；Retry-After 是下界并夹到 1..=5 分钟（可超过自动退避上限，但不超过 PROTOCOL-014 的 5 分钟）；超过失败预算（默认 8）返回 `Stop{FailureBudgetExhausted}`。
- **幂等/可重入**：同一输入永远同一动作；应用一次 Pull 后重新决策只前进一页；畸形输入（负时钟、时钟回退、页条数与范围不一致、失败未计数、序号超出可存储范围）显式失败（`INVALID_TARGET` / `INVALID_CURSOR` / `STATE_CORRUPTED` / `CONTENT_TOO_LARGE`），不夹取、不静默修正。

### 追赶计划（SYNC-008 计划核心）

`catchup_plan(local: VerifiedHead, node: NodeLogHead, policy) -> Result<CatchupPlan>`，输入本机已验证 head 与节点签名 head 的公开字段（签名验证属于 TRANSPORT-007/PROTOCOL-007）：

- 步骤序列：`PullLog{from,to}`（从 `local.log_sequence+1` 起按 `page_items` 分页、连续升序、无重叠）→ `RebuildEpoch{at_sequence,from_epoch,to_epoch}`（**一次只推进 1 个 epoch**，不跳级，锚点为本次拉取末端）→ `CommitHead`（把已验证 head 提交为节点签名 head 的值）。
- **缺控制日志不能直接解密新 epoch**：节点 head 声称更新的 epoch/控制头却没有任何新条目承载（`span == 0`）时只返回 `AwaitControl{MissingEpochCommit}`，不猜重建；本地策略已更新但节点还没包含（`local.policy_version > node.policy_version`）时返回 `AwaitControl{PendingControlResult}`，不回退本地策略。
- **冲突/超限显式失败，不截断**：本地日志序号或 epoch 超过节点 head → `LOG_CONFLICT`；跨度超过 `max_entries`（默认 5000）或步数超过 `max_steps`（默认 64）→ `CONTENT_TOO_LARGE`；跨 epoch 数超过 `max_epoch_jumps`（默认 64）→ `MISSING_LOG_ENTRIES`；策略参数越界或序号超出可存储范围 → `INVALID_TARGET`。
- **幂等**：已完全追平（seq/epoch/控制头/策略一致）返回空计划且 `is_caught_up() == true`，重复调用结果相同；每群独立计算，一个群的冲突/等待不影响其它群的计划。

### 复现命令（全程离线，本轮实测）

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-sync    # lib 2 / mailbox 14 / pull 9 / catchup 9 / retry 9 / send 10 = 53 passed
cargo clippy --offline -p chat-sync --all-targets  # 0 warning
```

### 证据（测试名 → 契约点）

- 重试/退避（`tests/retry_backoff.rs`，9 个）：`backoff_is_exponential_and_capped`（1/2/4/8/16/32/60/60 s 序列与上限）、`jitter_window_is_fixed_and_bounded`（0/500/1000 抽样 = 下界/中点/上界）、`retry_after_takes_priority_and_is_capped_at_five_minutes`（优先且 5 分钟上限、0 被夹到 1 ms）、`non_retryable_errors_are_terminal_without_a_delay`（权限/撤销/损坏/凭据/epoch 立即终态且保留根因码）、`unknown_results_only_reconcile_the_existing_submission`、`retry_budget_exhaustion_stops_automatic_retries`、`ledger_counts_failures_and_only_success_resets`（重启恢复计数、只有成功重置）、`malformed_policies_and_inputs_are_rejected`、`seeded_jitter_is_deterministic_and_stays_in_the_window`。
- 信箱（`tests/mailbox_polling.rs`，14 个）：`ordered_items_are_delivered_and_the_cursor_advances`、`duplicate_pages_are_idempotent_and_not_redelivered`、`duplicate_item_id_with_different_content_is_rejected`、`out_of_order_log_entries_are_held_then_promoted_in_order`（先 2/3 后 1 → 交付 1/2/3）、`cursor_regression_and_no_progress_are_rejected`、`ack_failure_keeps_progress_and_is_retried`、`empty_pages_back_off_and_do_not_busy_loop`、`pull_failures_back_off_exponentially_and_keep_the_cursor`、`page_and_item_budgets_are_enforced_without_state_change`、`hold_buffer_is_bounded`、`tracked_group_slots_are_bounded`、`seed_group_aligns_the_expected_sequence`、`unknown_item_kinds_are_delivered_without_interpretation`、`malformed_ids_and_cursors_are_rejected`。
- 拉取循环（`tests/pull_loop.rs`，9 个）：`incremental_pull_is_bounded_and_advances_one_page_at_a_time`（正常增量 1..100→101..200→201..250，尾页不越界，到 head 转 IdleAtHead）、`gap_in_the_last_page_requires_a_resync_instead_of_skipping`（空洞/过期页 → Resync）、`pages_beyond_the_head_or_reversed_are_rejected_explicitly`（越过 head → Resync；页尾<页首 → INVALID_CURSOR；条数与范围不一致 → STATE_CORRUPTED）、`empty_page_with_head_ahead_requires_a_resync`、`oversized_pages_and_gaps_are_refused_instead_of_truncated`（超大一页 → CONTENT_TOO_LARGE；落后超上限 → Resync{GapTooLarge}；本地越过 head → Resync）、`failure_backoff_is_exponential_capped_and_budgeted`（1/2/4/8/16/32/60/60 s、Retry-After 下界与 5 分钟夹取、预算耗尽 → Stop）、`not_due_and_page_interval_avoid_busy_looping`、`planning_is_deterministic_and_replay_is_idempotent`、`malformed_policy_and_clock_inputs_are_rejected`。
- 追赶计划（`tests/catchup_plan.rs`，9 个）：`incremental_catch_up_pulls_the_gap_then_commits_the_head`、`long_gaps_are_paged_and_each_step_stays_within_the_page_limit`（100 条分页、连续递增无洞）、`epoch_lag_is_rebuilt_one_epoch_at_a_time_never_jumping`（epoch 1→4 得 2/3/4 三步，且拉取在重建之前、提交在最后）、`newer_epoch_without_entries_must_wait_for_the_control_result`（缺控制条目 → AwaitControl{MissingEpochCommit}）、`local_policy_ahead_of_the_node_waits_instead_of_rolling_back`、`conflicts_fail_explicitly_and_do_not_block_other_groups`（LOG_CONFLICT 且其它群正常）、`oversized_plans_are_rejected_instead_of_truncated`（跨度/步数 → CONTENT_TOO_LARGE，跨 epoch → MISSING_LOG_ENTRIES，策略/序号 → INVALID_TARGET）、`already_caught_up_is_an_idempotent_empty_plan`、`control_change_with_new_entries_is_explained_by_the_pulled_range`。
- 发送调度（`tests/send_scheduling.rs`，10 个）：`first_scheduling_derives_one_submission_id_and_is_idempotent`、`derived_ids_are_stable_and_domain_separated`、`prepared_attempt_is_resubmitted_with_the_same_id_and_bytes`、`active_lease_blocks_concurrent_resend_until_it_expires`、`unknown_is_never_resent_even_after_lease_expiry`、`only_formal_acceptance_releases_the_attempt`、`rejection_rebuilds_with_a_new_submission_id`、`cancelled_draft_abandons_every_path`、`inconsistent_or_malformed_snapshots_are_rejected`、`the_plan_sequence_encrypts_exactly_once`（按计划推进 6 步：加密 1 次、重发 2 次、unknown 后只对账 2 次，最终 Release）。

### 已知限制（本轮只对纯逻辑负责）

1. **不验证 envelope**：SYNC-007 步骤 1-4（授权、Welcome 加入、日志补齐验证、毒性条目隔离）需要 CRYPTO/STORAGE 正式路径，本轮未接入；因此本层“已处理”只等于“已进入本机顺序/暂存”，不代表消息已验证或已到达用户。
2. **耐久性契约**：ACK 语义要求 core 在同一事务里把条目写入耐久去重表（`inbox_dedup`，STORAGE-）后才允许 ACK；本状态机只保证顺序与记账，进程崩溃后的耐久恢复由 core 负责。
3. **去重窗口有界**（4096 条）：被驱逐的 item id 若被节点重放，需要存储侧 `(conversation, event_id)` 去重兜底（计划书也说明外层 item ID 不作可信消息去重）。
4. **群日志起点默认 1**：已加入设备必须先由 SYNC-008 补齐并把 verified head 通过 `seed_group` 对齐；本层不推断 `joined_seq`，也不主动补拉缺号（那是 SYNC-008）。
5. **退避参数是固定取值**：正式配置来源（用户/系统预算）待 CONFIG/BRIDGE 接入；抖动来源生产必须由桥接层注入系统 RNG，本轮只提供 `FixedJitter`/`SeededJitter`。
6. **未接入真实网络/数据库/MLS/时钟**：SYNC-001/002(outbox 批执行)/005/006/009/010/012/013/014 未实现；因此本页的完成验收只勾选纯逻辑可证的项（SYNC-003/004/007/011 的部分项，以及 SYNC-002/008 的决策核心），整体模块不计完成。
   **补充（SW 包）**：SYNC-002 的拉取循环与 SYNC-008 的追赶计划已经有真节点落地层（[crates/core/src/sync_worker.rs](../../crates/core/src/sync_worker.rs)，见下文“常驻拉取 worker 实现记录”）：联网/验签/落库/游标推进已实装，MLS 内层应用仍未接。
7. **编号对照**：计划书的 SYNC-002 是 `run_outbox_once`（outbox 任务批执行）。本轮交付的 `mailbox_pull_loop` 是常驻同步 worker 的**拉取循环决策**（对应 SYNC-006 步骤 2-3 与 SYNC-008 的内环），不是 outbox 批执行本身，因此 SYNC-002 的验收框保持未勾选。
8. **追赶计划本身不验签、不应用**：`catchup_plan` 假定节点 head 已由 TRANSPORT-007/PROTOCOL-007 用固定节点公钥验证；逐条验签/持久化已由 SW 包的 `run_catchup_once` 接上（`PullLog` 现在真的逐页验签落库），**MLS 应用与 `RebuildEpoch` 仍交回上层**（结果里的 `upper_actions`）。控制条目与 epoch 的对应关系也不由计划猜测（`RebuildEpoch.at_sequence` 只是“不早于本次拉取末端”的水位）。
9. **拉取循环的去重/ACK 仍由 SYNC-007 负责**：`mailbox_pull_loop` 只决定“这一轮该不该拉、拉哪一段、等多久、是否需要重新对齐”，不消费条目、不写去重表、不发 ACK。

## 常驻拉取 worker 实现记录（SW 包：run_pull_once / run_catchup_once，2026-10-19，Windows x64）

代码：[crates/core/src/sync_worker.rs](../../crates/core/src/sync_worker.rs)（新模块，`lib.rs` 加一行 `pub mod sync_worker;`）；
测试：[crates/core/tests/sync_worker.rs](../../crates/core/tests/sync_worker.rs)（8 个用例：真 axum 节点 + 真正式库 + 真设备签名）。

这一层把两个**纯决策核心**接到真实网络上：`mailbox_pull_loop`（SYNC-002 拉取循环）与 `catchup_plan`（SYNC-008 追赶计划）。
决策规则一条都没有在这里重写——本模块只负责“按决策读节点、验签、落库、前进游标”。

### 公开面

```text
pub fn initialize_pull_state(store: &mut Store, group_id: &str, head: VerifiedHead) -> SyncResult<()>
pub fn load_pull_state(store: &Store, group_id: &str) -> SyncResult<Option<PullStateView>>
pub fn load_applied_entries(store: &Store, group_id: &str) -> SyncResult<Vec<LogEntry>>

pub fn run_pull_once(store: &mut Store, client: &NodeClient, group_id: &str) -> SyncResult<PullOutcome>
pub fn run_pull_once_at(store: &mut Store, client: &NodeClient, group_id: &str, now_ms: i64) -> SyncResult<PullOutcome>
pub fn run_catchup_once(store: &mut Store, client: &NodeClient, group_id: &str) -> SyncResult<CatchupOutcome>
pub fn run_catchup_once_at(store: &mut Store, client: &NodeClient, group_id: &str, now_ms: i64) -> SyncResult<CatchupOutcome>

pub fn verify_pull_page(pinned: &[u8; 32], group_id: &str, previous: Digest, from: u64, to: u64,
                        entries: &[LogEntry]) -> SyncResult<Vec<LogEntry>>
```

- `PullOutcome { applied, cursor_after, resync_required, resync_reason, node_head, backoff_ms, stopped }`
- `CatchupOutcome { applied, cursor_after, executed_steps, upper_actions, awaiting, caught_up }`
- `*_at` 变体是测试/诊断入口（显式时钟）；非 `_at` 版本用 `SystemClock`。

### 关键语义

1. **游标只在“验签 + 落库”成功后前进**：一页条目先整体验签，再和游标状态**同一次** storage 快照写入；
   写入失败则游标不动（不存在“游标前进但条目没落盘”）。
2. **未到期不触网**：先用“上次观察到的节点 head”跑一次 `mailbox_pull_loop`；得到 `Backoff{NotDue}` 或 `Stop`
   就直接返回（不取 head、不拉条目），拿到新 head 后再用**同一个纯函数**重新决策一次。
3. **逐条验签**（`verify_pull_page`，也是公开函数）：节点回执签名（固定节点公钥）→ 回执绑定候选（`verify_candidate`）
   → 群绑定（回执与候选的 group 都必须等于本群）→ 序号连续（`from..=to` 逐条）→ 前序哈希连续（第一条必须接上本地游标）
   → 页的 head 不得落后于请求范围、head 公钥必须等于固定公钥。
4. **缺口绝不静默跳过**：决策核心给出的 `Resync` 原样返回（`resync_required=true` + 原因），由调用方转 `run_catchup_once`。
5. **失败 = 记录退避，不 sleep**：连续失败数 + 下次允许时间写进状态（等待时长来自 `PullPolicy::failure_delay_for`），
   然后返回显式错误；下一次调用在窗口内自然得到 `Backoff`（0 应用、不触网），到期后正常重试。
6. **追赶计划按序执行**：`PullLog` 逐页验签落库；`CommitHead` 只有在**日志真的验到该序号**时才把节点 head 的控制状态
   采纳为已验证；`RebuildEpoch` / `AwaitControl` 一律停下并入 `upper_actions` 返回，**不猜 epoch、不伪造推进**。
7. **同步 API**：与 `group_node` 相同的桥（独立线程 + 独立 current-thread 运行时 + `thread::scope`），
   不把 async 传染到 FFI；调用方即使在 tokio 运行时里也不会嵌套 panic。

### 持久化形态（临时但明确的边界）

- 状态与条目用 storage 的密封 KV：storage group id = `syncpull/<sha256(group_id)>`，**epoch 字段就是游标序号**
  （storage 自带的 epoch 单调守卫因此顺带保证“游标不会倒退”）；条目每条一个 provider 记录，键 `log/<8 字节大端 seq>`。
- 之所以这样放：schema 里给这件事准备的 `group_log_entries` / `group_policy_snapshots` 表**还没有 storage API**，
  而 storage 不在本包写作用域。后续 storage 切片应把这两张表接出来，本模块的读写函数整块替换即可（对外签名不变）。
- 代价：storage 的快照写入会整组替换记录，所以每次落库要重写本群已有条目（O(已应用条数)），单群上限 4096 条
  （`MAX_PERSISTED_ENTRIES`）；超限显式 `CONTENT_TOO_LARGE`，不静默丢弃。

### 语义边界（本轮不等于“已应用消息”）

- **作者签名不在本层重复验证**：节点 NODE-006 接收候选时已用作者设备公钥验签；客户端没有 IDENTITY 凭据目录查远端设备公钥。
  因此本层证明的是“节点确实接受并签发了这条条目，且链连续、候选字节与回执绑定”，不重复证明作者身份。
- **没有 MLS 内层应用**：拉到的条目只做“链/回执/绑定”验证与落库；`process_verified_entry`、epoch 重建、消息落库
  （SYNC-009 / CRYPTO-006）仍属上层，`upper_actions` 就是交给它们的接口。
- **控制头水位**：只在 `CommitHead` 时把节点 head 的 control_head/policy_version 采纳为已验证值；不做“控制条目 → 策略应用”的解释。
- **单群串行**：同一群的两次并发调用没有互斥保护（storage 的 epoch 守卫会拒绝倒退的写入）；跨群调度仍属上层。

### 错误码（全部取自 PROTOCOL-014，不新增码）

| 情形 | code |
|---|---|
| 游标未初始化 | `INVALID_CURSOR` |
| 群 ID 非规范 / 固定公钥缺失 / 页属于别的群 | `INVALID_TARGET` |
| 节点不可达 / 超时（含桥线程失败） | `NODE_UNAVAILABLE` |
| 回执签名或候选绑定不成立（含换固定公钥、篡改候选） | `INVALID_SIGNATURE` |
| 序号不连续 / 页不覆盖请求范围 | `MISSING_LOG_ENTRIES` |
| 前序哈希断裂 / 页越过 head / 节点 head 落后 | `LOG_CONFLICT` |
| 持久状态损坏或版本不支持 | `STATE_CORRUPTED` |
| 落库失败 | `STORAGE_FAILURE` |
| 单群条目超上限 | `CONTENT_TOO_LARGE` |

### 复现命令（全程离线）

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test sync_worker   # 8 passed
cargo test --offline -p chat-core                      # 全绿（0 警告）
```

### 证据（测试名 → 覆盖点）

- `sync_worker::pull_applies_new_entries_and_advances_the_cursor`：空群初始化 → 节点追加 2 条 → `run_pull_once_at`
  得到 `applied=2`、`cursor_after=2`、`resync_required=false`、`node_head=2`；条目真的落库（序号/提交 ID/密文与节点一致、
  链尾等于节点回执的 log_hash）；状态里 `applied_entries=2`、`consecutive_failures=0`。
- `sync_worker::repeated_pulls_are_idempotent_and_never_duplicate_entries`：第一轮 `applied=1`；已到 head 的第二轮
  `applied=0` + 返回退避、游标仍 1、条目仍 1 条、`applied_entries` 仍 1（不重复应用）。
- `sync_worker::local_cursor_ahead_of_the_node_requires_resync`：本地声称已到 seq 3 而节点只有 1 条 → `resync_required=true`、
  原因 `LocalAheadOfNode`、游标**不回退**、没有落任何条目。
- `sync_worker::tampered_or_gapped_pages_are_rejected_before_anything_is_stored`：用真页与 5 种坏页直接打 `verify_pull_page`
  （真页通过；换固定公钥 → `INVALID_SIGNATURE`；改前序哈希 → `LOG_CONFLICT`；换候选密文 → `INVALID_SIGNATURE`；
  页不覆盖请求范围 → `MISSING_LOG_ENTRIES`；前序对不上游标 → `LOG_CONFLICT`）。
- `sync_worker::verification_failure_does_not_advance_the_cursor`：固定**错误**节点公钥 → 拉取失败（`INVALID_SIGNATURE` 或
  `NODE_UNAVAILABLE`），游标/条目/`applied_entries` 全都不动；换成真节点后仍能正常拉到（失败不是破坏性的）。
- `sync_worker::unreachable_node_fails_explicitly_and_backs_off_without_sleeping`：不可达端口 → `NODE_UNAVAILABLE`、
  游标不动、失败计数 1、下次允许时间前进；窗口内再调用**不触网**（`applied=0` + `backoff_ms`）；到期后重试仍失败、计数 2。
- `sync_worker::catchup_runs_pull_log_then_commits_the_head`：节点追加 3 条 → `run_catchup_once_at` 得到 `applied=3`、
  `cursor_after=3`、`executed_steps≥2`（PullLog + CommitHead）、`upper_actions` 为空；再跑一次得到空计划（`caught_up=true`、0 应用）。
- `sync_worker::catchup_returns_upper_actions_instead_of_faking_epoch_progress`：本地策略版本 2 > 节点 1 → 计划只给
  `AwaitControl{PendingControlResult}`，结果 `applied=0`、`upper_actions=[AwaitControl]`、`executed_steps=0`，本地游标/策略**未被伪造推进**。

### 已知限制

1. **作者签名不重复验证**（见语义边界）：依赖节点在接收时验签；客户端侧完整的 PROTOCOL-005 需要 IDENTITY 凭据目录。
2. **没有 MLS 应用**：`RebuildEpoch` / 控制策略应用 / 消息落库都交回上层，本模块只保证“链与回执正确、状态耐久”。
3. **持久化用临时载体**：`mls_group_state` / `mls_record` 替代尚未接 API 的 `group_log_entries` / `group_policy_snapshots`；
   每次落库重写整组记录（O(已应用条数)）、单群上限 4096 条。
4. **单群无并发互斥**：同群并发调用靠 storage 的 epoch 守卫兜底（拒绝倒退写入），没有进程内锁；跨群调度未接入。
5. **退避只记录不睡眠**：常驻循环（按 `backoff_ms`/`next_attempt_at_ms` 调度下一次调用）属上层 worker 循环，本轮未提供。
6. **失败只累计不分类**：验签失败也会计入连续失败（最终 `Stop{FailureBudgetExhausted}` 停止自动重试，等人工处置）——
   这是保守选择：宁可停下来人工排查，也不无限重试一条签名不成立的条目。

## 函数导航

- [SYNC-001 enqueue_job](sync.md#sync-001)
- [SYNC-002 run_outbox_once](sync.md#sync-002)
- [SYNC-003 prepare_draft_job](sync.md#sync-003)
- [SYNC-004 submit_attempt_job](sync.md#sync-004)
- [SYNC-005 rebuild_stale_attempt](sync.md#sync-005)
- [SYNC-006 sync_now](sync.md#sync-006)
- [SYNC-007 consume_mailbox_page](sync.md#sync-007)
- [SYNC-008 catch_up_group](sync.md#sync-008)
- [SYNC-009 apply_log_page](sync.md#sync-009)
- [SYNC-010 process_delivery_receipt](sync.md#sync-010)
- [SYNC-011 schedule_retry](sync.md#sync-011)
- [SYNC-012 set_lifecycle](sync.md#sync-012)
- [SYNC-013 set_connectivity](sync.md#sync-013)
- [SYNC-014 configure_node](sync.md#sync-014)

<a id="sync-001"></a>

## SYNC-001 · enqueue_job

~~~text
enqueue_job(tx: CoreTransaction, job: NewOutboxJob) -> Result<JobId>
~~~

**职责与输出**：基于唯一业务键插入/复用持久任务。

**输入与前置条件**：调用方持有统一事务；任务引用已持久 payload 或同事务新记录。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证 job 类型和依赖。
2. 建立 dedup key。
3. 同键同 payload 返回原 job。
4. 不同 payload 拒绝。
5. 插入 pending。

**持久化、并发与副作用**：事务组成步骤；绝不自己 commit 调用方事务。

**失败处理**：JOB_KEY_CONFLICT，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 连点、重启或重复回执入队均幂等。
- [ ] 不存在 payload 不可排队。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="sync-002"></a>

## SYNC-002 · run_outbox_once

~~~text
run_outbox_once(now: Time, worker: WorkerId, budget: WorkBudget) -> Result<WorkSummary>
~~~

**职责与输出**：执行一批到期任务并持久记录结果。

**输入与前置条件**：core 已启动；网络/电量/前后台预算可用。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 短事务租用任务。
2. 按群依赖顺序执行。
3. 网络等待事务外。
4. 分类结果。
5. 短事务保存。
6. 报告计数。

**持久化、并发与副作用**：租约保证恢复；每批有界条数/字节/时间，避免 UI 饥饿。

**失败处理**：NETWORK_UNAVAILABLE，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[STORAGE-014 claim_outbox_batch](storage.md#storage-014)、[STORAGE-015 mark_outbox_result](storage.md#storage-015)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 任务执行后落盘前 crash 允许原字节重复。
- [x] 断网不忙循环。（部分：**拉取循环**决策核心已证有界退避——`pull_loop::failure_backoff_is_exponential_capped_and_budgeted`、`pull_loop::not_due_and_page_interval_avoid_busy_looping`；本函数自身的 outbox 批执行未实现，故不勾选）

**实现状态（OB 包，2026-10-19）**：outbox 批执行已实现。源码 [crates/core/src/outbox_worker.rs](../../crates/core/src/outbox_worker.rs)：

`@text
pub fn run_outbox_once(orchestrator, stop: &AtomicBool) -> SendResult<OutboxBatchResult>
pub fn run_outbox_once_at(orchestrator, stop, now_unix_ms, limit) -> SendResult<OutboxBatchResult>
pub struct OutboxBatchResult { processed, accepted, unknown, rejected, deferred, stopped_reason }
`@

已实现行为（**不新造状态机**：每个任务都交给既有 `SendOrchestrator` 的 `prepare`/`dispatch`，
调度决策仍是 `chat_sync::send::plan_submission`）：

1. **阶段 1（send，不预先领取）**：从 `outbox_jobs` 只读枚举到期的 send 任务（`pending && retry_at<=now`
   或租约已过期），按 `(retry_at, job_id)` 稳定排序、截断到批量上限；逐条调用 `dispatch(draft)`。
   领取与三态结果落库都由 dispatch 内部完成——这样发送计划不会把**本 worker 自己的租约**误判成
   “别人持有”而原地等待。
2. **阶段 2（prepare，需要租约）**：`claim_outbox_batch` 原子领取一批（≤32，含字节预算），
   执行 `prepare(draft)`；成功时 T2 事务已经把准备任务置 complete（再写同结果幂等），
   失败/未知时用租约写结果。被阶段 2 领到的 send 任务（阶段 1 没推进的）只做一次可重试退避，
   交回 pending 让下一轮重试，避免“持有租约却让 dispatch 等待”。
3. **结果分类**：`accepted`（任务完成，含幂等重放）/ `unknown`（只写 6 小时对账窗口，
   **绝不重发、绝不重新加密**）/ `rejected`（明确拒绝或不可重试错误 → 任务 `failed`）/
   `deferred`（可重试失败已按退避重排、等待租约、或被其它 worker 有效持有——后者在本轮**领不到**时
   按 `outbox_jobs` 的 `leased && lease_until > now` 行计数，明确报告“有活但拿不到”）。
4. **一条失败不阻塞其它**：逐任务捕获结果与错误；错误码的 `ErrorCode::retryable()` 决定延后还是拒绝。
5. **幂等**：任务完成后不再被领取，重复执行同一批得到 `processed=0`、零提交、零加密。
6. **不 sleep、可打断**：只跑一圈；`stop` 只在任务边界检查，返回 `stopped_reason = StopRequested`。
7. **租约过期由既有机制接管**：`claim_outbox_batch` 已包含 `leased && lease_until<=now` 的重领，
   本模块不重复实现；到期后同 worker 的下一轮自然接管（测试覆盖）。

验证证据（`cargo test --offline -p chat-core --test outbox_worker`，**真 axum 节点 + 真加密库 + 真 outbox 表**）：

- `three_drafts_are_all_accepted_in_one_batch`：3 条草稿两轮全部 accepted；每条恰好加密一次、恰好提交一次、
  零对账；草稿最终 `accepted`、任务全部 `complete`。
- `rejected_submission_does_not_block_the_others`：先用相异字节在节点占用某草稿的 SubmissionId →
  该条提交被节点 409 拒绝（三态 = Rejected）→ `rejected=1`、`accepted=1`，另一条不受影响。
- `unreachable_node_marks_every_send_unknown_and_never_resends`：提交端点指向不可达地址 → 两条全 `unknown`、
  `accepted=0`；重复执行 `processed=0` 且 `submit_calls` 不增长（**零重发**）。
- `repeating_a_finished_batch_submits_nothing`：完成后连跑两轮 → `processed=0`、零提交、零加密。
- `task_held_by_another_worker_is_deferred`：另一 worker 持有效租约 → `processed=0, deferred=1`；
  租约到期后本 worker 自然接管并 accepted。
- `stop_request_stops_at_a_task_boundary`：`stop=true` → `processed=0`、`stopped_reason=StopRequested`、零加密。

已知限制：

1. **批内顺序**用 `(retry_at, job_id)`：`retry_at` 初始值就是草稿创建时间，因此等价于“先创建先执行”；
   `LeasedJob` / `OutboxJobRecord` 不暴露 `created_at` 列（不新增迁移）。
2. **阶段 1 的错误不写租约**（没有领取就没有租约）：可重试错误计 `deferred`、不可重试错误计
   `rejected`，但任务行仍是 `pending`，下一轮会自然重试（不会永久卡死，也没有指数退避累加）。
   阶段 2 的错误会写进退避（`retry_at` 按 `retry_count` 指数增长）。
3. **阶段 1 的 `"尝试缺失"**（outbox 行指向不存在的 attempt）只计 `rejected`，不写终态：没有租约就不改行，
   留给恢复/对账流程。
4. **不负责节奏**：谁在什么时候调用（常驻循环/定时器/前后台预算）由上层决定；本模块只跑一圈、不 sleep。
5. **依赖注入的提交端口**：真节点用 `NodeClientSubmitter`；本模块不实现网络重试，未知结果只写对账窗口。


**实现状态**：部分实现（仅拉取循环决策核心，**不是** outbox 批执行）。`mailbox_pull_loop(policy, input)` 见 [crates/sync/src/pull.rs](../../crates/sync/src/pull.rs)：输入本地已应用位置/节点 head/上次结果/失败与时间状态，输出 `Pull{from,to}` / `Backoff` / `Resync` / `Stop`；缺口、过期页、越过 head、空页但 head 前进一律 Resync（绝不跳过），单次 Pull ≤ `page_items`，落后超过 `max_incremental_gap` 交给追赶计划，失败指数退避封顶 60 s 且 Retry-After 为下界（≤5 分钟），失败预算耗尽即 Stop。证据见上文“拉取循环（SYNC-002 决策核心）”与 `tests/pull_loop.rs`（9 个用例）。限制：不联网、不消费条目、不发 ACK（那是 SYNC-007 的 `MailboxPoller`）；计划的 `run_outbox_once`（依赖 STORAGE-014/015）仍未实现，因此本函数验收框保持未勾选。

<a id="sync-003"></a>

## SYNC-003 · prepare_draft_job

~~~text
prepare_draft_job(draft: DraftId) -> Result<AttemptId>
~~~

**职责与输出**：把待发意图变为一次耐久加密尝试。

**输入与前置条件**：群串行执行权；附件已上传完成或明确依赖阻塞。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取草稿。
2. 同步必要控制状态。
3. 重新校验发言权限。
4. 建立事务。
5. 加密一次并保存 attempt/outbox。
6. 提交发布。

**持久化、并发与副作用**：T2；已有可用 attempt 直接复用，不重复加密。

**失败处理**：PERMISSION_DENIED，ATTACHMENT_NOT_READY，STALE_EPOCH。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-001 validate_send_permission](domain.md#domain-001)、[CRYPTO-004 prepare_application](crypto.md#crypto-004)、[STORAGE-007 persist_prepared_attempt](storage.md#storage-007)、[STORAGE-009 commit_transaction](storage.md#storage-009)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 创建意图后被禁言应失败。（未验证：权限校验在 DOMAIN-001 + core 的正式状态里，本轮只实现调度决策，不做权限判定）
- [x] 同 job 重跑取得同 attempt：`send_scheduling::first_scheduling_derives_one_submission_id_and_is_idempotent`（同一草稿同一 epoch/序号二次调度得到完全相同的 `Prepare` 与 SubmissionId）、`send_scheduling::the_plan_sequence_encrypts_exactly_once`（按计划推进只加密一次）。

**实现状态**：部分实现（仅调度决策）。`plan_submission(draft, attempt, worker, now)` 见 [send.rs](../../crates/sync/src/send.rs)：新草稿 → `Prepare`（确定性 SubmissionId）、已有耐久尝试且租约过期 → `Resubmit`（同 ID 同字节）、租约有效 → `WaitForLease`、已接受 → `Release`、拒绝 → `Rebuild`（新 ID）、取消 → `Abandoned`。限制：不加密、不写库、不做权限校验；真实 T2 事务（`persist_prepared_attempt`）与 outbox 接入由 core 完成。

<a id="sync-004"></a>

## SYNC-004 · submit_attempt_job

~~~text
submit_attempt_job(attempt: AttemptId) -> Result<AttemptOutcome>
~~~

**职责与输出**：提交原始密文并处理正式接受或冲突。

**输入与前置条件**：attempt 不可变且已耐久；群前序控制任务满足。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 加载原 bytes。
2. 调用 append。
3. 回执成功交验证应用。
4. 明确旧状态转 rebuild。
5. 结果未知只原 ID 重试/查询。

**持久化、并发与副作用**：网络外执行；状态更新短事务；不得自行新建密文。

**失败处理**：ACCEPTANCE_UNKNOWN，STALE_CONTROL_HEAD，STALE_EPOCH。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[TRANSPORT-006 append_candidate](transport.md#transport-006)、[SYNC-009 apply_log_page](sync.md#sync-009)、[DOMAIN-010 classify_retry](domain.md#domain-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 超时实际已成功恢复同 seq。（未验证：需要 TRANSPORT-006 `append_candidate` 与回执验证，本轮不连网）
- [x] 同 ID 同 bytes 多次提交只接受一次：调度侧证据 `send_scheduling::prepared_attempt_is_resubmitted_with_the_same_id_and_bytes`（同 ID 重发不重新加密）、`send_scheduling::unknown_is_never_resent_even_after_lease_expiry`（未知结果只 `QueryExisting`，租约过期也不换新 ID）。节点侧“只接受一次”属于 NODE-003/协议去重，不在本 crate。

**实现状态**：部分实现（决策与租约/未知语义）。`plan_submission` 保证 unknown → `QueryExisting`（不重发、不重新加密）、租约有效 → `WaitForLease`、成功 → `Release`，并提供 `reject_unknown_resend` 供绕过计划的调用方自检（`UNKNOWN_RESULT`）。限制：真实 append 调用、回执验证与 `STORAGE-008` 应用未接入。

<a id="sync-005"></a>

## SYNC-005 · rebuild_stale_attempt

~~~text
rebuild_stale_attempt(draft: DraftId, rejected: AttemptId) -> Result<RebuildResult>
~~~

**职责与输出**：旧尝试明确拒绝后按新 epoch 创建新尝试。

**输入与前置条件**：必须证明旧候选未被接受；草稿未取消；重新授权仍允许。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 同步日志。
2. 清理 pending 分支。
3. 重新查角色/设备。
4. 保留 DraftId。
5. 生成新 SubmissionId/密文。
6. 记录旧新关联。

**持久化、并发与副作用**：T2；不回滚密码学已消费状态；旧 Attempt 不原地覆盖。

**失败处理**：ACCEPTANCE_UNKNOWN，PERMISSION_DENIED，STATE_CORRUPTED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CRYPTO-008 discard_rejected_pending](crypto.md#crypto-008)、[SYNC-008 catch_up_group](sync.md#sync-008)、[SYNC-003 prepare_draft_job](sync.md#sync-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 并发禁言后不重发。
- [ ] 新尝试 ID/摘要与旧尝试不同。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="sync-006"></a>

## SYNC-006 · sync_now

~~~text
sync_now(budget: SyncBudget) -> Result<SyncResult>
~~~

**职责与输出**：合并同步请求，返回已完成/受限/待重试摘要。

**输入与前置条件**：启动完成；请求可以来自用户/前台/WS/恢复。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 合并并发触发。
2. 拉信箱。
3. 按群补日志。
4. 执行 outbox。
5. 发送已耐久 ACK。
6. 重新查本地状态。

**持久化、并发与副作用**：每群各自事务，跨群不假称全局原子；取消保留进度。

**失败处理**：NODE_UNAVAILABLE，SYNC_PARTIAL，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[TRANSPORT-009 pull_mailbox](transport.md#transport-009)、[SYNC-008 catch_up_group](sync.md#sync-008)、[SYNC-002 run_outbox_once](sync.md#sync-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 多次同时触发不创建重复 worker。
- [ ] 断线恢复按游标续拉。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="sync-007"></a>

## SYNC-007 · consume_mailbox_page

~~~text
consume_mailbox_page(page: MailboxPage) -> Result<MailboxConsumeResult>
~~~

**职责与输出**：只 ACK 已安全处理或持久隔离的条目。

**输入与前置条件**：页面 target=当前设备；外层 item ID 不作可信消息去重。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查授权与 envelope。
2. Welcome 走加入路径。
3. 日志走补齐验证。
4. 毒性条目按政策隔离告警。
5. 持久消费记录。
6. 排队 ACK。

**持久化、并发与副作用**：T3 或独立隔离事务；缺失前置日志时先耐久待补，不能丢条目。

**失败处理**：INVALID_CREDENTIAL，MISSING_LOG_ENTRIES，WELCOME_NOT_AUTHORIZED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CRYPTO-009 join_from_welcome](crypto.md#crypto-009)、[SYNC-008 catch_up_group](sync.md#sync-008)、[TRANSPORT-010 ack_mailbox](transport.md#transport-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 先收到 seq 8 必须补1..7。（部分：本轮先持久暂存 8 并在 1..7 到达后按序交付——`mailbox_polling::out_of_order_log_entries_are_held_then_promoted_in_order`；**主动补拉** 1..7 属于 SYNC-008 `catch_up_group`，未实现）
- [ ] 坏条目不阻塞全设备但不能被当真消息。（部分：畸形页整页显式拒绝且不改变状态——`page_and_item_budgets_are_enforced_without_state_change`；未知类别只保留不解释——`unknown_item_kinds_are_delivered_without_interpretation`；逐条隔离/告警依赖 STORAGE-007 隔离事务，未实现）

**实现状态**：部分实现（顺序/去重/预算/ACK 记账）。`MailboxPoller::tick(clock, source, ack)` 见 [mailbox.rs](../../crates/sync/src/mailbox.rs)：游标只前进、同 item id 幂等去重（内容冲突 `INVALID_CANDIDATE`）、乱序暂存（512 上限）并按序提升、单页/单条/总量/群数/待 ACK 五类上限、ACK 失败保留进度并重试、空页 30 s 退避不空转、拉取失败指数退避（证据见上文实现记录）。限制：不做 envelope 授权/MLS 验证与毒性条目隔离（依赖 CRYPTO-*/STORAGE-007，未接入）；耐久去重依赖 core 写 `inbox_dedup`。

<a id="sync-008"></a>

## SYNC-008 · catch_up_group

~~~text
catch_up_group(group: GroupId, target: Option<u64>, budget: SyncBudget) -> Result<VerifiedHead>
~~~

**职责与输出**：补齐连续日志到目标/当前节点头。

**输入与前置条件**：当前 verified head 已持久；节点公钥固定。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 查询签名 head。
2. 从 verified+1 分页。
3. 检查页连续。
4. 逐条验证并应用。
5. 达到预算保存进度。
6. 冲突暂停该群。

**持久化、并发与副作用**：按条或有限连续批次 T3；不直接跳到新 epoch。

**失败处理**：MISSING_LOG_ENTRIES，LOG_CONFLICT，NODE_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[TRANSPORT-007 fetch_group_entries](transport.md#transport-007)、[SYNC-009 apply_log_page](sync.md#sync-009)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 缺控制日志不能直接解密新 epoch。（部分：计划层保证“先拉取控制条目、逐个 epoch 重建、缺条目即等待”——`catchup_plan::epoch_lag_is_rebuilt_one_epoch_at_a_time_never_jumping`、`catchup_plan::newer_epoch_without_entries_must_wait_for_the_control_result`；真正阻止解密的调用点在 CRYPTO-006/010 与 SYNC-009，未接入）
- [ ] 暂停群不阻断其他群。（部分：`catchup_plan` 是每群纯函数，冲突群返回 `LOG_CONFLICT`、等待群返回 `AwaitControl`，都不影响其它群的计划——`catchup_plan::conflicts_fail_explicitly_and_do_not_block_other_groups`；跨群调度隔离在 SYNC-006/core，未接入）

**实现状态**：部分实现（仅计划核心，不联网/不验签/不应用）。`catchup_plan(local: VerifiedHead, node: NodeLogHead, policy)` 见 [crates/sync/src/catchup.rs](../../crates/sync/src/catchup.rs)：输出按序步骤 `PullLog`（从 `local.log_sequence+1` 起按页递增、连续无重叠）→ `RebuildEpoch`（一次只推进 1 个 epoch，不跳级）→ `CommitHead`；节点 head 声称新 epoch/控制头但没有新条目承载时只给 `AwaitControl{MissingEpochCommit}`（绝不猜重建），本地策略已更新而节点未包含时 `AwaitControl{PendingControlResult}`（不回退策略）；本地序号/epoch 超过节点 → `LOG_CONFLICT`，跨度/步数超限 → `CONTENT_TOO_LARGE`，跨 epoch 超限 → `MISSING_LOG_ENTRIES`，均已完全追平时返回幂等空计划。证据见上文“追赶计划（SYNC-008 计划核心）”与 `tests/catchup_plan.rs`（9 个用例）。限制：节点 head 的签名验证（TRANSPORT-007/PROTOCOL-007）、逐条验签/MLS 应用/持久化（SYNC-009 与 core）未接入；控制条目与 epoch 的对应关系不由本函数猜测。

<a id="sync-009"></a>

## SYNC-009 · apply_log_page

~~~text
apply_log_page(group: GroupId, page: UnverifiedLogPage) -> Result<AppliedPage>
~~~

**职责与输出**：依次完成链/权限/MLS 验证并提交。

**输入与前置条件**：页有界；群串行执行；不能信任服务端已标 validated。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证 seq/hash/签名。
2. 业务授权。
3. MLS stage/解密。
4. 写消息/状态/去重。
5. 提交。
6. 发布。
7. 下一条。

**持久化、并发与副作用**：T3，每条或经证明安全的有限批；失败停止当前分支。

**失败处理**：LOG_CONFLICT，INVALID_CONTROL_RESULT，STATE_CORRUPTED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-007 verify_log_entry](protocol.md#protocol-007)、[CRYPTO-006 process_verified_entry](crypto.md#crypto-006)、[STORAGE-008 apply_verified_entry](storage.md#storage-008)、[STORAGE-009 commit_transaction](storage.md#storage-009)、[CRYPTO-010 publish_committed_state](crypto.md#crypto-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 后条失败不撤销已成功前条。
- [ ] 回执循环不产生新回执。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="sync-010"></a>

## SYNC-010 · process_delivery_receipt

~~~text
process_delivery_receipt(receipt: AuthenticatedReceipt) -> Result<DeliverySummary>
~~~

**职责与输出**：将对端已持久化证据归入对应消息。

**输入与前置条件**：receipt 自身经 MLS/设备认证且属于接收集合。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 匹配 EventId/SubmissionId。
2. 核对接收设备权限时点。
3. 幂等保存。
4. 聚合账户数。
5. 更新视图。

**持久化、并发与副作用**：短事务；回执消息类型不再生成送达回执。

**失败处理**：INVALID_RECEIPT_SCOPE，INVALID_CREDENTIAL。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-009 aggregate_delivery](domain.md#domain-009)、[DOMAIN-008 transition_send_state](domain.md#domain-008)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 伪造目标/自己第二台不增加对方计数。
- [ ] 重复回执不重复计数。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="sync-011"></a>

## SYNC-011 · schedule_retry

~~~text
schedule_retry(job: JobId, error: CoreError, now: Time) -> Result<RetrySchedule>
~~~

**职责与输出**：写入有抖动指数退避/服务器 Retry-After。

**输入与前置条件**：retry 计数和截止策略来自配置；可注入时钟/RNG用于验收。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 分类错误。
2. 计算有上限退避。
3. 遵守 retryAfter 下界。
4. 记录下一时刻。
5. 需要用户处理时停止自动重试。

**持久化、并发与副作用**：短事务；不在工作线程长 sleep；恢复后仍使用同计划。

**失败处理**：PERMANENT_FAILURE，RETRY_BUDGET_EXHAUSTED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-010 classify_retry](domain.md#domain-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 断网一小时不忙轮询：`retry_backoff::backoff_is_exponential_and_capped`（退避有 60 s 上限）、`retry_backoff::retry_budget_exhaustion_stops_automatic_retries`（预算耗尽即终态，不会无限重试）、`mailbox_polling::empty_pages_back_off_and_do_not_busy_loop`（时间未到不调用数据源）、`mailbox_polling::pull_failures_back_off_exponentially_and_keep_the_cursor`。
- [x] 重启不把失败计数归零：`retry_backoff::ledger_counts_failures_and_only_success_resets`（`RetryLedger::restore` 保留计数；唯一重置条件是 `record_success`）。

**实现状态**：已实现（纯策略）。`schedule_retry(policy, attempt, input, jitter)` 与 `RetryLedger` 见 [retry.rs](../../crates/sync/src/retry.rs)：基础 1 s / 倍数 2 / 上限 60 s / 抖动 ±20% / 预算 8 次；先经 `chat_domain::classify_retry`（DOMAIN-010）分类；Retry-After 作为下界且夹到 `MAX_RETRY_AFTER_MS`（5 分钟）；不可重试错误立即终态并保留根因码。限制：退避参数固定为代码取值（正式配置来源待 CONFIG/BRIDGE 接入）；生产抖动必须由桥接层注入系统 RNG；本模块不 sleep、不写库（排期由 core 用短事务落盘）。

<a id="sync-012"></a>

## SYNC-012 · set_lifecycle

~~~text
set_lifecycle(state: AppLifecycleState) -> Result<SyncStatus>
~~~

<a id="sync-worker"></a>

<a id="sync-013"></a>

## SYNC-013 · set_connectivity

~~~kotlin
suspend fun setConnectivity(state: ConnectivityState): SyncStatus
~~~

**职责与输出**：把上层观测到的网络/生命周期状态交给同步引擎，返回当前同步状态快照。

**输入与前置条件**：状态取自平台生命周期与网络监听；重复设置同一状态幂等；引擎未启动显式失败。

**处理步骤**：校验枚举 → 相同则直接返回（幂等）→ Offline/Background 停止新的拉取与提交（保留耐久队列与游标）→ 恢复时按既有退避策略唤醒拉取循环。

**持久化、并发与副作用**：不修改消息/草稿状态，不伪造投递成功，只影响调度。

**失败处理**：非法枚举 INVALID_INPUT；引擎未启动 INVALID_STATE_TRANSITION。

**实现状态**：**未实现**（常驻运行时见 `crates/core/src/sync_runtime.rs`；连接态调度尚未接入）。

<a id="sync-014"></a>

## SYNC-014 · configure_node

~~~kotlin
suspend fun configureNode(node: NodeEndpoint): SyncStatus
~~~

**职责与输出**：设置同步使用的节点端点（地址 + 节点 ID + 可选固定公钥），返回当前同步状态。

**输入与前置条件**：端点与节点 ID 校验通过；固定公钥仅在明确配置时使用。

**处理步骤**：校验格式 → 保存配置（不联网探测）→ 已运行的拉取循环下一轮使用新端点 → 返回状态快照。

**持久化、并发与副作用**：只改配置，不发送请求、不改游标、不改投递状态。

**失败处理**：格式非法 INVALID_INPUT；不可达在真正请求时以 NODE_UNAVAILABLE 显式失败（配置阶段不假装可用）。

**实现状态**：**未实现**（发送侧 `configure_send_node` 与群治理 `submit_control` 已各自接受端点参数，统一配置层待接入）。

<a id="sync-worker"></a>

## 实现记录：常驻拉取与同步运行时（R-67 / R-70）

- 拉取 worker（`crates/core/src/sync_worker.rs`）：`run_pull_once` 读游标与节点 head → `pull::mailbox_pull_loop` 决策 → 分页拉取并逐条验签（回执签名、绑定候选、群绑定、序号连续、前序哈希接上游标、页 head 与固定公钥），与游标同一快照落库后再前进；`run_catchup_once` 执行 `catchup_plan` 的 `PullLog`，只有当日志真验到该序号才 `CommitHead`，`RebuildEpoch`/`AwaitControl` 交回上层。
- 常驻运行时（`crates/core/src/sync_runtime.rs`）：独立线程 + current-thread 运行时；每轮一次拉取，成功按间隔等待、Resync 立刻追赶、失败退避、预算耗尽或不可重试错误终止；`status()` 暴露状态快照；`stop()` join 并交还库句柄。
- 证据：`cargo test -p chat-core` 的 `sync_worker`(8) 与 `sync_runtime`(4) 用例（自动应用、stop 生效、断开退避后可恢复、缺口不跳过、5 种坏页被拒）。
- 限制：作者签名不重复验证；无 MLS 内层应用；持久化复用 `mls_group_state`/`mls_record`（单群 4096）；同群多运行时无互斥；无事件回调。

## 实现记录：outbox 批执行接进常驻运行时（SR2 包，2026-10-19，Windows x64）

源码 [crates/core/src/sync_runtime.rs](../../crates/core/src/sync_runtime.rs)；测试 [crates/core/tests/sync_runtime.rs](../../crates/core/tests/sync_runtime.rs)。

已实现行为：

1. **每轮顺序：先 outbox、后 pull**。阶段 0 用上一轮的 `run_outbox_once_at`（有界 ≤32）推进待办草稿；
   阶段 1 仍是既有的单轮拉取 + 追赶。outbox 失败**不阻塞**拉取：outbox 的错误只写自己的状态字段，
   拉取照常决策/执行。
2. **独立退避**：outbox 有自己的失败计数与 `outbox_next_attempt_at_ms`（失败按
   `PullPolicy::failure_delay_for` 指数退避并 `policy.clamp`；成功则 `now + round_wait_min`）。
   未到 `outbox_next_attempt_at_ms` 时本轮跳过 outbox，**不影响**拉取的 `next_attempt_at_ms`。
3. **状态快照新增字段**（只读、无密钥/正文）：`outbox_accepted_total`、`outbox_unknown_total`、
   `outbox_last_error_code`（PROTOCOL-014 文本；未注入端口时为本地标记 `OUTBOX_NOT_CONFIGURED`）、
   `outbox_next_attempt_at_ms`。
4. **配置开关**：`SyncRuntimeConfig::enabled_outbox` 默认 **true**；端口通过
   `with_outbox(engine, submitter)` 注入（加密端口 `Box<dyn CryptoEngine + Send>`、提交端口
   `Box<dyn NodeSubmitter + Send>`），`without_outbox()` 可关闭做对照。
   为不改动其它 crate（FFI 仍在用 `SyncRuntimeConfig::new`），**未注入端口时启动不失败**：outbox 显式
   不执行并把 `outbox_last_error_code = OUTBOX_NOT_CONFIGURED` 写进状态（不是静默跳过，也不是假成功）。
5. **停止与不 sleep**：`stop` 在**任务边界**生效（每轮开始前与 outbox 之后各检查一次），循环仍用条件变量
   等待（`wait_round`），没有忙等/自旋；停止原因 `stop_requested`。
6. **后端持有方式**：开启 outbox 时库句柄由 `SendOrchestrator` 独占（拉取通过 `store_mut()` 使用同一句柄），
   关闭时退回裸 `Store`；`stop()` 仍返回库句柄给调用方。

验证证据（`cargo test --offline -p chat-core --test sync_runtime`，真 axum 节点 + 正式库）：

- `outbox_runtime_sends_drafts_without_manual_dispatch`：起运行时后**不需要任何手动调度**，
  两张草稿共 4 个任务（2 prepare + 2 send）全部 accepted，草稿落 `accepted`、恰好 2 条加密尝试。
- `unreachable_submitter_backs_off_outbox_but_keeps_pulling`：提交端口返回 Unknown（模拟节点不可达）→
  `outbox_accepted_total = 1`（prepare 成功）、`outbox_unknown_total = 1`（send 未知，绝不重发）；
  拉取循环继续按轮推进、`last_error_code` 保持 `None`、运行时**没有**因 outbox 失败停止。
- `disabled_outbox_leaves_drafts_pending`：`without_outbox()` → 多轮之后草稿仍是 `queued`、
  零加密尝试、`outbox_accepted_total = 0`、`outbox_last_error_code` 为 `None`（关闭不是错误）。
- `stop_halts_outbox_at_a_task_boundary`：停前只提交过 1 次（未知结果不重发），`stop()` 后状态
  `Stopped/stop_requested`，再等一段时间**没有任何新提交**。

已知限制：

1. 端口必须由上层注入（core 不持有 MLS 设备与设备签名钥）；未注入时 outbox 显式不执行并标记
   `OUTBOX_NOT_CONFIGURED`（FFI 现有调用不受影响）。
2. outbox 的失败预算**没有**与拉取的 `max_failures` 合并成“停进程”：outbox 连续失败只延长自己的退避
   （任务本身还会按 outbox 的 `retry_at` 排队），避免因为发送通道抖动把同步也停掉。
3. 一轮的 outbox 与 pull 共享同一个库句柄，因此 outbox 执行期间拉取顺延到同一轮之后（不是并行）；
   两者都不在事务里跨网络等待（事务都是短的）。
4. 本轮的 outbox 只推进“已存在的草稿”；`begin_send` 仍由发送 API（SendRuntime/编排器）写入。
5. 拉取侧“运行期间新追加条目”的即时性受既有 `PullPolicy.page_interval_ms` 与持久 `next_attempt_at_ms`
   约束（本轮未改动 `sync_worker` 的决策语义）。

## 实现记录：常驻同步接 FFI（SyncFfi，R-71，2026-10-18，Windows x64）

目标：界面能看到"常驻同步"的生老病死——打开、启动、状态轮询、停止、关闭，且状态全部是计数/稳定码。

代码：[crates/ffi/src/sync_ffi.rs](../../crates/ffi/src/sync_ffi.rs)（新）；测试：
[crates/ffi/tests/sync_ffi.rs](../../crates/ffi/tests/sync_ffi.rs)（3 个用例，**真 axum 节点 + 真临时加密库**，全程经 FFI）。
契约细节见 [bridge.md 的“常驻同步 FFI”](bridge.md)。

### 公开面（Kotlin 可见）

~~~text
SyncFfi.open(database_path, master_key_hex, endpoint_url, node_id, node_public_key_hex, group_id)
    -> Arc<SyncFfi>            // 只装配 + 校验，不启动
start() -> SyncStatusFfi       // 已在运行 → INVALID_STATE_TRANSITION
stop() -> SyncStatusFfi        // 协作式停止；未运行幂等返回当前状态
status() -> SyncStatusFfi      // 未启动 = idle；已停止 = 最后状态
is_open() -> bool              // group_id() -> String
shutdown()                     // 幂等；之后所有调用 CORE_CLOSED

SyncStatusFfi { state, applied_total, last_error_code, next_attempt_at_ms,
                resync_required, rounds, stopped_reason }
~~~

`state` 是稳定文本：`idle` / `pulling` / `backoff` / `stopped`。

### 关键语义

1. **open 只装配**：打开正式加密库读持久身份种子 → 派生设备签名钥 → 固定端点/固定节点公钥构造
   NodeClient → **确认该群拉取游标已初始化**；任何一步失败都不留下半打开对象，且**不启动**循环。
2. **游标必须来自可信起点**：未初始化 → `INVALID_STATE_TRANSITION`（拉取起点来自建群/加入回执，
   不是"问节点要一个 head 然后相信它"）。
3. **start 重建配置**：NodeClient 被运行时消费，因此每次 start 都重新派生客户端与配置；库句柄在
   装配后立刻释放（运行时自己开/关库），所以 open 之后别的对象仍能短暂读库。
4. **stop 幂等且保留最后状态**：停止是协作式的（等待中的条件变量被立刻唤醒），返回的状态带
   `stopped_reason = "stop_requested"`；未运行时 stop 只返回当前状态。
5. **shutdown 幂等**：先停循环再标记关闭；之后 start/stop/status 一律 `CORE_CLOSED`，库锁被释放。

### 复现命令（全程离线；测试内起真实 loopback 节点）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi --test sync_ffi   # 3 passed
cargo test --offline -p chat-ffi                   # 42 passed（既有 39 继续通过）
~~~

### 证据

- `sync_ffi::open_is_idle_until_start_and_status_tracks_applied_entries`：open 后 status = idle /
  applied_total 0（**open 不自动启动**）→ 节点追加 2 条 → start 后轮询到 applied_total >= 2、
  last_error_code 为空、rounds >= 1 → stop 后 state = stopped、stopped_reason = stop_requested →
  停止后再追加一条并等待 400 ms，applied_total **不再增长**、state 仍是 stopped。
- `sync_ffi::uninitialized_cursor_fails_explicitly_on_open`：另一个已在节点登记但**没有本地游标**的群 →
  open 直接 `INVALID_STATE_TRANSITION`（不构造运行时、不触网）。
- `sync_ffi::shutdown_is_idempotent_and_closes_the_object`：运行中 shutdown → is_open = false；
  再 shutdown 一次无副作用；start/stop/status 全部 `CORE_CLOSED`；随后另一个写者能重新打开该库
  （说明库锁确实释放了）。

### 已知限制

1. **单群单运行时**：对象绑定一个群；同群多运行时没有互斥（core 侧限制不变）。
2. **状态是轮询式**：没有事件回调，界面需要按需 `status()`。
3. **core 侧既有语义不变**：作者签名不重复验证、无 MLS 内层应用、持久化复用 `mls_group_state`/`mls_record`。
4. **同步阻塞**：start/stop/status 都在 FFI 的 `Mutex` 内串行；stop 会等当前那一轮网络请求结束。
5. **一次只能有一个 SyncFfi 持有该库的运行时**：启动期间库被运行时独占，其它 FFI 对象要等 stop/shutdown。
6. **`node_public_key_hex` 必填**：运行时拒绝未固定节点公钥的配置（core 的 `SyncRuntimeConfig::validate` 语义）。
## 拉取条目应用进 MLS 并落库（SYNC-002 补全，2026-10-18，Windows x64）

背景：`run_pull_once` 之前只做“验签 + 落库 + 前移游标”，拉到的条目并不 apply 进 MLS，
自动收消息缺最后一环。本轮补上 **apply 阶段**。

代码：[crates/core/src/sync_worker.rs](../../crates/core/src/sync_worker.rs)（本包内定点改动，**未改 groups.rs**）；
测试：[crates/core/tests/sync_apply.rs](../../crates/core/tests/sync_apply.rs)（4 个，真节点 + 真双设备两库）。

### 新增公开面

~~~text
run_pull_once_reporting_at(store, client, group_id, now_ms) -> SyncResult<(PullOutcome, ApplyReport)>
applied_message_id(group_id, seq) -> String            // 确定性消息 ID（幂等键）
ApplyReport { applied, stale, unsupported, needs_catchup, unknown_authors, added_credentials, removed_credentials, pending_controls }   // skipped() = stale + unsupported
~~~

`run_pull_once` / `run_pull_once_at` 行为不变（内部转调同一实现，报告被丢弃）。

### 应用与分类语义

1. **应用**：对本页每条已通过 `verify_pull_page` 的条目，用节点日志里的候选构造
   `PeerEntryInput`（群、epoch、`author_device_id` 作为已认证作者、密文载荷，以及候选
   control metadata 里的 `added_credentials` / `removed_credentials`）并调用
   `crate::groups::group_process_peer_entry` ——它内部走 CRYPTO-006 的 `process_verified_entry`，
   Commit 合并后 epoch **恰好 +1**、成员增删按声明的凭据写回快照，并把 MLS 状态 `export_to_store` + 快照落库。
2. **应用消息落库**：解密出的正文写成正式消息行（STORAGE-008 `apply_verified_entry`），
   message id 由 `(group_id, 日志序号)` 确定性派生（`applied_message_id`），因此**重放幂等**；
   正文不是合法 UTF-8 → 记 skipped（不写半条）。
3. **失败分类**（绝不静默丢数据）：
   - `STALE_EPOCH` → 跳过并计 `stale`，游标照常前进（否则会被一条旧条目永久卡住）；
   - `MISSING_LOG_ENTRIES` → 置 `needs_catchup`，**不推进游标**，返回
     `resync_required=true`（原因 `GapTooLarge`，交给追赶流程的 `RebuildEpoch`）；
   - `UNSUPPORTED_ENTRY` / 载荷不是 MLS 消息 / 本机不在该群 / 正文非 UTF-8 → 跳过并计 `unsupported`；
   - 其它（如 `STATE_CORRUPTED`）→ **向上抛错**，游标不前进。
4. **游标只在 apply 之后前进**：条目落库与游标是同一次 storage 快照写入。

### 控制提交成员凭据（task-91）

日志候选的 control metadata 现在可携带 PROTOCOL-004 字段 59/60（新增/移除成员凭据）：

- 提交侧（`group_node::resolve_control_metadata`）：`invite` 从本地快照的 `pending_members` 取凭据、
  `remove` 从 `pending_removed` 取凭据；只有数量与 pending 计数完全一致才填，否则留空（保留旧的“无法证明”语义）。
  结果通过 node-services 的 `CandidateControlMetadata.added_credentials/removed_credentials`（serde 默认空）写进候选，
  签名原文覆盖这两个字段。
- 应用侧（`sync_worker::apply_verified_page`）：把候选里的两个列表填进 `PeerEntryInput.added_members/removed_members`；
  `group_process_peer_entry` 会核对“声明数量 == 真实 MLS 增删数量”，因此只有真正的 invite/remove 提交才会写回成员集合，
  声明与提交不符仍显式失败（不会被“宽容”应用）。
- 报告：`ApplyReport` 新增 `added_credentials` / `removed_credentials`（真正应用或显式挂起的控制提交携带的凭据）与
  `pending_controls`（本机 pending 回显、或本机落后/epoch 过期而只能挂起的控制提交数）。旧候选没有列表时行为不变
  （仍是 `unsupported` 计数，绝不伪造身份）。

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test sync_apply      # 4 passed
cargo test --offline -p chat-core --test sync_membership # 1 passed（task-91 新增）
cargo test --offline -p chat-core                        # 226 passed（既有 225 继续通过，0 failed）
cargo test --offline -p chat-protocol                    # 88 passed（lib 74 + vectors 14）
~~~

### 证据

- `sync_apply::pull_applies_the_application_message_into_mls_and_the_store`：alice 用真 MLS 加密
  一条消息提交到真节点 → bob 的同步 worker 拉取 → `report.applied == 1`、游标 1、
  `message_exists(applied_message_id)` 为真、会话摘要 `preview == 解密后的正文`。
- `sync_apply::repeated_pulls_do_not_apply_the_same_entry_twice`：同一页再拉一次 → applied 0、
  游标不变、消息仍只有一条（幂等）。
- `sync_apply::a_page_for_a_group_the_local_store_is_not_in_is_skipped_but_advances`：本机不在该群
  （无群快照）→ 两条条目 `unsupported == 2`、applied 0，但游标前进到 2。
- `sync_apply::applying_a_control_commit_advances_the_epoch`：alice 提交真 SelfUpdate 控制提交 →
  bob 拉取应用 → 快照 `epoch == before + 1`（控制提交真的合进了 MLS）。
- `sync_membership::invite_and_remove_commits_with_credentials_change_membership`（新）：alice 的真实 invite 提交
  （携带 carol 凭据）→ bob 侧 apply → `applied == 1`、`unsupported == 0`、`added_credentials == [carol]`、成员集合出现 carol；
  随后真实 remove 提交（携带 carol 凭据）→ `removed_credentials == [carol]`、成员集合移除 carol、epoch 依次 1→2→3。

### 已知限制

1. **作者身份是外层设备 ID**：应用消息的作者取日志候选的 `author_device_id`，写消息行时
   `author_account` 用**确定性占位**（`placeholder_author_account`）——真实账户映射要等 IDENTITY
   凭据目录接入；因此群消息在会话列表里的“对端账户”暂时不是真实账户 ID。
2. **策略语义不经拉取同步**：`set_role` / `transfer_ownership` 等角色变更需要外层声明的目标成员，
   日志里只有 `domain_target_digest`，故 apply 阶段**不猜测策略**（角色按本地保持不变）；
   角色同步仍走显式 `group_process_peer_entry(policy_intent)` 路径。
3. **成员增删提交已可应用（task-91）**：候选携带 PROTOCOL-004 字段 59/60 时按声明的凭据写回成员集合；旧候选
   （无列表）或声明数量与真实 Commit 不符时仍按 `INVALID_CONTROL_RESULT`/unsupported 显式跳过；角色策略语义仍未从
   拉取路径猜测（见上一条）。
4. `MISSING_LOG_ENTRIES` 复用 `ResyncReason::GapTooLarge` 表达“本机 MLS 落后”（`ResyncReason`
   属于 chat-sync 的日志对齐语义，本包不改它的枚举）。
5. 应用阶段每条消息一次 storage 事务；大批量页（上百条）会变慢，尚未批处理。



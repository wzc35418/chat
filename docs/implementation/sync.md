# 可靠发送、信箱、日志补齐与生命周期

阶段：P1 → P3。状态：函数级设计；SYNC-003/004（发送调度决策）、SYNC-007（信箱顺序/去重/预算/ACK 记账）、SYNC-011（重试/退避策略）已实现并被自动化用例覆盖，其余函数未开始。目标源码：[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)。

可靠性来自耐久队列，不依赖 Flow 或 WebSocket 是否在线。outbox 分开 Prepare、Append、PeerPretransfer、MailboxPut、DeliveryReceipt、MailboxAck、ControlRebuild 等任务。每群同一执行器串行推进密码学状态，多个群可以有界并行。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 重试/信箱/发送调度实现记录（2026-10-18，Windows x64）

本轮实现 sync 的**纯逻辑部分**（不连网、不连库、不 sleep），网络与持久化通过 trait/端口注入：

- [crates/sync/src/error.rs](../../crates/sync/src/error.rs)：`SyncError`（冻结码 + 静态诊断键）与 `to_public_error`。
- [crates/sync/src/clock.rs](../../crates/sync/src/clock.rs)：`Clock` trait、`FixedClock`（测试确定性）、`SystemClock`（生产）。
- [crates/sync/src/ids.rs](../../crates/sync/src/ids.rs)：文本 ID 校验与**确定性 SubmissionId 派生**（PROTOCOL-001 强类型）。
- [crates/sync/src/retry.rs](../../crates/sync/src/retry.rs)：SYNC-011 `schedule_retry` 与 `RetryLedger`。
- [crates/sync/src/mailbox.rs](../../crates/sync/src/mailbox.rs)：SYNC-007 单页消费 + `MailboxPoller` 轮询状态机（`MailboxSource`/`AckSink` 端口）。
- [crates/sync/src/send.rs](../../crates/sync/src/send.rs)：SYNC-003/SYNC-004 的 `plan_submission` 调度决策。
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
| 投影不一致（尝试属于别的草稿、租约无持有者、草稿已接受但尝试未接受等） | `STATE_CORRUPTED` |
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

### 复现命令（全程离线，本轮实测）

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-sync    # 14 mailbox = ok / 9 retry = ok / 10 send = ok；lib 0 个
cargo clippy --offline -p chat-sync  # 0 warning（lib + --all-targets 均无告警）
```

### 证据（测试名 → 契约点）

- 重试/退避（`tests/retry_backoff.rs`，9 个）：`backoff_is_exponential_and_capped`（1/2/4/8/16/32/60/60 s 序列与上限）、`jitter_window_is_fixed_and_bounded`（0/500/1000 抽样 = 下界/中点/上界）、`retry_after_takes_priority_and_is_capped_at_five_minutes`（优先且 5 分钟上限、0 被夹到 1 ms）、`non_retryable_errors_are_terminal_without_a_delay`（权限/撤销/损坏/凭据/epoch 立即终态且保留根因码）、`unknown_results_only_reconcile_the_existing_submission`、`retry_budget_exhaustion_stops_automatic_retries`、`ledger_counts_failures_and_only_success_resets`（重启恢复计数、只有成功重置）、`malformed_policies_and_inputs_are_rejected`、`seeded_jitter_is_deterministic_and_stays_in_the_window`。
- 信箱（`tests/mailbox_polling.rs`，14 个）：`ordered_items_are_delivered_and_the_cursor_advances`、`duplicate_pages_are_idempotent_and_not_redelivered`、`duplicate_item_id_with_different_content_is_rejected`、`out_of_order_log_entries_are_held_then_promoted_in_order`（先 2/3 后 1 → 交付 1/2/3）、`cursor_regression_and_no_progress_are_rejected`、`ack_failure_keeps_progress_and_is_retried`、`empty_pages_back_off_and_do_not_busy_loop`、`pull_failures_back_off_exponentially_and_keep_the_cursor`、`page_and_item_budgets_are_enforced_without_state_change`、`hold_buffer_is_bounded`、`tracked_group_slots_are_bounded`、`seed_group_aligns_the_expected_sequence`、`unknown_item_kinds_are_delivered_without_interpretation`、`malformed_ids_and_cursors_are_rejected`。
- 发送调度（`tests/send_scheduling.rs`，10 个）：`first_scheduling_derives_one_submission_id_and_is_idempotent`、`derived_ids_are_stable_and_domain_separated`、`prepared_attempt_is_resubmitted_with_the_same_id_and_bytes`、`active_lease_blocks_concurrent_resend_until_it_expires`、`unknown_is_never_resent_even_after_lease_expiry`、`only_formal_acceptance_releases_the_attempt`、`rejection_rebuilds_with_a_new_submission_id`、`cancelled_draft_abandons_every_path`、`inconsistent_or_malformed_snapshots_are_rejected`、`the_plan_sequence_encrypts_exactly_once`（按计划推进 6 步：加密 1 次、重发 2 次、unknown 后只对账 2 次，最终 Release）。

### 已知限制（本轮只对纯逻辑负责）

1. **不验证 envelope**：SYNC-007 步骤 1-4（授权、Welcome 加入、日志补齐验证、毒性条目隔离）需要 CRYPTO/STORAGE 正式路径，本轮未接入；因此本层“已处理”只等于“已进入本机顺序/暂存”，不代表消息已验证或已到达用户。
2. **耐久性契约**：ACK 语义要求 core 在同一事务里把条目写入耐久去重表（`inbox_dedup`，STORAGE-）后才允许 ACK；本状态机只保证顺序与记账，进程崩溃后的耐久恢复由 core 负责。
3. **去重窗口有界**（4096 条）：被驱逐的 item id 若被节点重放，需要存储侧 `(conversation, event_id)` 去重兜底（计划书也说明外层 item ID 不作可信消息去重）。
4. **群日志起点默认 1**：已加入设备必须先由 SYNC-008 补齐并把 verified head 通过 `seed_group` 对齐；本层不推断 `joined_seq`，也不主动补拉缺号（那是 SYNC-008）。
5. **退避参数是固定取值**：正式配置来源（用户/系统预算）待 CONFIG/BRIDGE 接入；抖动来源生产必须由桥接层注入系统 RNG，本轮只提供 `FixedJitter`/`SeededJitter`。
6. **未接入真实网络/数据库/MLS/时钟**：SYNC-001/002/005/006/008/009/010/012/013/014 未实现；因此本页的完成验收只勾选纯逻辑可证的两项（SYNC-003/004/007/011 的部分项），整体模块不计完成。

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

- [ ] 任务执行后落盘前 crash 允许原字节重复。
- [ ] 断网不忙循环。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 缺控制日志不能直接解密新 epoch。
- [ ] 暂停群不阻断其他群。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

**职责与输出**：前台恢复补齐，后台保存进度并收缩连接。

**输入与前置条件**：Android/桌面通过平台事件调用，状态可重复。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 原子更新生命周期。
2. 后台暂停非必要任务。
3. 耐久保存检查点。
4. 前台重建连接并触发同步。
5. 发布状态。

**持久化、并发与副作用**：不因进入后台删除 outbox；系统可直接杀进程。

**失败处理**：STORAGE_FAILURE，BACKGROUND_BUDGET_EXCEEDED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[SYNC-006 sync_now](sync.md#sync-006)、[TRANSPORT-012 shutdown_transport](transport.md#transport-012)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] Android kill 后重启恢复。
- [ ] 前后台频繁切换不漏消息。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="sync-013"></a>

## SYNC-013 · set_connectivity

~~~text
set_connectivity(state: ConnectivityState) -> Result<SyncStatus>
~~~

**职责与输出**：断网暂停网络工作，恢复后合并触发重连。

**输入与前置条件**：OS 网络提示不保证节点可达。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 记录连接 hint。
2. 失网取消有限网络任务。
3. 在线使到期 job 可执行。
4. 节点实际失败仍退避。

**持久化、并发与副作用**：只改调度状态，不改消息已接受事实。

**失败处理**：NETWORK_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[SYNC-011 schedule_retry](sync.md#sync-011)、[SYNC-006 sync_now](sync.md#sync-006)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 有 Wi-Fi 无互联网保持待发。
- [ ] 旧连接 ACK 不覆盖新状态。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="sync-014"></a>

## SYNC-014 · configure_node

~~~text
configure_node(profile: NodeProfile) -> Result<NodeConfigurationStatus>
~~~

**职责与输出**：验证并保存节点设置，不自动迁移已有会话协调者。

**输入与前置条件**：用户显式输入地址和信任确认；禁止静默公共回退。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验 HTTPS/主机策略。
2. 抓能力。
3. 核对公钥信任。
4. 保存 profile。
5. 新会话使用新设置。
6. 旧会话保留固定协调关系。

**持久化、并发与副作用**：配置短事务；网络检查事务外；错误保留旧可用配置。

**失败处理**：INVALID_NODE_PROFILE，NODE_KEY_CHANGED，PROTOCOL_UNSUPPORTED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-011 negotiate_capabilities](protocol.md#protocol-011)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 换节点不改变账户 ID。
- [ ] 旧群不被误指到新节点。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。


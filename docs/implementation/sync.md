# 可靠发送、信箱、日志补齐与生命周期

阶段：P1 → P3。状态：函数级设计，尚未实现。目标源码：[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)。

可靠性来自耐久队列，不依赖 Flow 或 WebSocket 是否在线。outbox 分开 Prepare、Append、PeerPretransfer、MailboxPut、DeliveryReceipt、MailboxAck、ControlRebuild 等任务。每群同一执行器串行推进密码学状态，多个群可以有界并行。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

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

- [ ] 创建意图后被禁言应失败。
- [ ] 同 job 重跑取得同 attempt。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 超时实际已成功恢复同 seq。
- [ ] 同 ID 同 bytes 多次提交只接受一次。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 先收到 seq 8 必须补1..7。
- [ ] 坏条目不阻塞全设备但不能被当真消息。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 断网一小时不忙轮询。
- [ ] 重启不把失败计数归零。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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


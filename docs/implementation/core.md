# 客户端内核与 Kotlin 业务服务映射

阶段：P1 → P2。状态：函数级设计，尚未实现。目标源码：[crates/core/src/service.rs](../../crates/core/src/service.rs)。

本层编排，不把各子模块的事务拆成独立提交。函数名与 Kotlin 的 camelCase 一一对应；底层 TaskId/AttemptId 不必全暴露 UI。start/close 是幂等生命周期，所有查询都来自 Rust 持久真相。身份与群、附件服务分别委托对应章节。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [CORE-001 start_core](core.md#core-001)
- [CORE-002 close_core](core.md#core-002)
- [CORE-003 observe_events](core.md#core-003)
- [CORE-004 open_direct](core.md#core-004)
- [CORE-005 list_conversations](core.md#core-005)
- [CORE-006 list_messages](core.md#core-006)
- [CORE-007 send_message](core.md#core-007)
- [CORE-008 retry_draft](core.md#core-008)
- [CORE-009 cancel_queued](core.md#core-009)
- [CORE-010 get_draft_status](core.md#core-010)
- [CORE-011 get_delivery](core.md#core-011)
- [CORE-012 handle_peer_pretransfer](core.md#core-012)

<a id="core-001"></a>

## CORE-001 · start_core

~~~text
start_core(options: CoreStartOptions, platform: PlatformCapabilities) -> Result<CoreHandle>
~~~

**职责与输出**：返回已恢复可查询的内核，锁定时返回明确锁状态。

**输入与前置条件**：一个数据目录只允许一个活动 core；平台密钥/文件桥已注入。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证配置。
2. 解包存储主密钥。
3. 打开库并恢复。
4. 装载身份和群执行器。
5. 注册有界事件桥。
6. 启动网络和调度。

**持久化、并发与副作用**：启动逆序清理；恢复完成前不发送；无网络可本地启动。

**失败处理**：STORAGE_LOCKED，STATE_CORRUPTED，CORE_ALREADY_RUNNING。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[STORAGE-001 open_store](storage.md#storage-001)、[PLATFORM-002 unwrap_storage_key](platform.md#platform-002)、[STORAGE-011 recover_incomplete_writes](storage.md#storage-011)、[TRANSPORT-001 start_endpoint](transport.md#transport-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 启动中取消无遗留进程。
- [ ] 离线可查本地。
- [ ] 损坏库不继续发送。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="core-002"></a>

## CORE-002 · close_core

~~~text
close_core(handle: CoreHandle) -> Result<CloseReport>
~~~

**职责与输出**：停止接收新操作、持久保存并释放原生句柄。

**输入与前置条件**：允许重复 close；存在进行中操作时使用明确截止时间。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 转 draining。
2. 停止调度。
3. 等待当前本地事务。
4. 关闭传输。
5. 关闭事件桥。
6. 释放数据库/敏感引用。

**持久化、并发与副作用**：已提交任务保留下次恢复；不得硬中断一半提交。

**失败处理**：SHUTDOWN_TIMEOUT，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[TRANSPORT-012 shutdown_transport](transport.md#transport-012)。core关闭事件源，FFI拥有者负责外部回调释放，禁止反向调用ffi。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [ ] close 后业务调用返回 CLOSED。
- [ ] 重复 close 不 double free。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="core-003"></a>

## CORE-003 · observe_events

~~~text
observe_events(listener: EventListener, capacity: u32) -> Result<SubscriptionHandle>
~~~

**职责与输出**：返回可释放、有界、只通知变化的订阅。

**输入与前置条件**：容量有上下限；listener 不能访问原生内部状态。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 登记 listener。
2. 分配有界队列。
3. 串行转发事件。
4. 溢出合并为 ResyncRequired。
5. 释放后停止回调。

**持久化、并发与副作用**：事务提交后发布；通知不是可靠消息日志。

**失败处理**：CORE_CLOSED，INVALID_SUBSCRIPTION。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CRYPTO-010 publish_committed_state](crypto.md#crypto-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 消费者慢导致 resync 仍可查询完整状态。
- [ ] release 后无回调。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="core-004"></a>

## CORE-004 · open_direct

~~~text
open_direct(contact: AccountId, client_operation: OperationId) -> Result<ConversationId>
~~~

**职责与输出**：复用或建立与已接受联系人设备组成的协调私聊。

**输入与前置条件**：联系人双向接受；用户选择节点；设备凭据有效。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 查现有 direct 索引。
2. 检查联系人状态。
3. 建立幂等私聊创建任务。
4. 收集 KeyPackage。
5. 创建 MLS/协调群。
6. 等待加入握手。

**持久化、并发与副作用**：本地意图先 T1；建立 MLS 用 T2；远端创建未完成显示 pending。

**失败处理**：CONTACT_NOT_ACCEPTED，INVALID_CREDENTIAL，NODE_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[GROUPS-001 create_group](groups.md#groups-001)、[CRYPTO-003 validate_key_package](crypto.md#crypto-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重复点击只一会话。
- [ ] 联系人拒绝不能绕过。
- [ ] 双方并发打开按明确握手去重。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="core-005"></a>

## CORE-005 · list_conversations

~~~text
list_conversations(cursor: Option<Cursor>, limit: u32) -> Result<Page<ConversationView>>
~~~

**职责与输出**：向 UI 提供会话摘要/待发/同步状态。

**输入与前置条件**：core started；limit 受控。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验账户锁定状态。
2. 委托分页。
3. 加入 group paused/identity warning 状态。
4. 映射 DTO。

**持久化、并发与副作用**：只读；无需等待网络。

**失败处理**：STORAGE_LOCKED，INVALID_CURSOR。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[STORAGE-013 query_conversation_page](storage.md#storage-013)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 断网仍可列会话。
- [ ] 预传未接受不算正式最后消息。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="core-006"></a>

## CORE-006 · list_messages

~~~text
list_messages(conversation: ConversationId, before: Option<Cursor>, limit: u32) -> Result<Page<MessageView>>
~~~

**职责与输出**：提供消息历史页与本机草稿状态。

**输入与前置条件**：当前账户可访问 conversation；不自动导入旧历史。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证会话访问。
2. 委托 storage 分页。
3. 映射内容回退/附件状态。
4. 返回 nextCursor。

**持久化、并发与副作用**：只读；DTO 不包含 crypto/provider 对象。

**失败处理**：PERMISSION_DENIED，INVALID_CURSOR。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[STORAGE-012 query_message_page](storage.md#storage-012)、[DOMAIN-012 enforce_join_visibility](domain.md#domain-012)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 未安装插件显示 fallback。
- [ ] 加入前正文不可查询。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="core-007"></a>

## CORE-007 · send_message

~~~text
send_message(request: SendRequest) -> Result<SendTicket>
~~~

**职责与输出**：持久化发送意图后立即返回 QUEUED，正式接受异步完成。

**输入与前置条件**：UI 或已确认插件草稿；稳定 clientOperationId；正文有界。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 解析 IDs/内容。
2. 校验当前权限与回复。
3. 查操作 ID。
4. 持久化意图/任务。
5. 提交。
6. 发 DraftChanged。
7. 唤醒 worker。

**持久化、并发与副作用**：T1：成功返回意味着草稿耐久；网络/MLS 由后续 T2/T3 执行。

**失败处理**：PERMISSION_DENIED，CONTENT_TOO_LARGE，OPERATION_ID_CONFLICT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-001 validate_send_permission](domain.md#domain-001)、[DOMAIN-011 validate_reply_target](domain.md#domain-011)、[STORAGE-006 persist_send_intent](storage.md#storage-006)、[STORAGE-009 commit_transaction](storage.md#storage-009)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 双击/跨 FFI 重试只同 Draft。
- [ ] 节点离线仍 QUEUED。
- [ ] 取消回调后查询可恢复。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="core-008"></a>

## CORE-008 · retry_draft

~~~text
retry_draft(draft: DraftId) -> Result<SendTicket>
~~~

**职责与输出**：按当前失败原因恢复原意图，不能伪造新消息或跳权限。

**输入与前置条件**：草稿属于本账户；未已接受/取消；原因允许重试。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读 draft/attempt。
2. 查询未知接受结果。
3. 重新验证权限。
4. 复用已有原始尝试或排队重建。
5. 返回持久状态。

**持久化、并发与副作用**：短事务更新任务；明确未接受才能生成新尝试。

**失败处理**：DRAFT_ALREADY_ACCEPTED，PERMISSION_DENIED，RETRY_NOT_ALLOWED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-010 classify_retry](domain.md#domain-010)、[SYNC-005 rebuild_stale_attempt](sync.md#sync-005)、[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 超时但已接受显示 ACCEPTED。
- [ ] 禁言后重试拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="core-009"></a>

## CORE-009 · cancel_queued

~~~text
cancel_queued(draft: DraftId) -> Result<CancelResult>
~~~

**职责与输出**：区分 Cancelled、AlreadyAccepted、AcceptanceUnknown。

**输入与前置条件**：只针对本机未正式接受意图；不承诺网络撤回。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 锁 draft/任务。
2. 已接受直接返回。
3. 已发在途先标 cancel_requested。
4. 未提交任务取消。
5. 释放未引用附件。
6. 发布状态。

**持久化、并发与副作用**：短事务；在途不删除原 attempt，必须对账接受结果。

**失败处理**：DRAFT_NOT_FOUND，ACCEPTANCE_UNKNOWN。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-008 transition_send_state](domain.md#domain-008)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 取消与节点提交竞争最终显示真实接受。
- [ ] 不误删已接受记录。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="core-010"></a>

## CORE-010 · get_draft_status

~~~text
get_draft_status(draft: DraftId) -> Result<DraftStatus>
~~~

**职责与输出**：返回本地意图、最新尝试、失败原因和可操作动作。

**输入与前置条件**：草稿属于当前账户。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 查询 draft/attempt/job。
2. 归类 waiting network/attachment/conflict。
3. 映射稳定错误。
4. 输出状态。

**持久化、并发与副作用**：只读快照；不暴露密文字节或密钥。

**失败处理**：DRAFT_NOT_FOUND，STORAGE_LOCKED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-014 map_error](protocol.md#protocol-014)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 节点存信箱状态独立展示。
- [ ] 同意图多尝试可追踪。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="core-011"></a>

## CORE-011 · get_delivery

~~~text
get_delivery(message: MessageId) -> Result<DeliverySummary>
~~~

**职责与输出**：返回每账户/设备送达摘要。

**输入与前置条件**：message 已正式接受且属于可查询会话。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取消息当时收件集合。
2. 读取已认证回执。
3. 排除本人。
4. 聚合 DTO。

**持久化、并发与副作用**：只读；传输 ACK 与 mailbox ACK 不参与。

**失败处理**：MESSAGE_NOT_FOUND，PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-009 aggregate_delivery](domain.md#domain-009)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 二设备同账户仅1计数。
- [ ] 退出后的状态按消息接收窗口。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="core-012"></a>

## CORE-012 · handle_peer_pretransfer

~~~text
handle_peer_pretransfer(frame: UnverifiedFrame) -> Result<PretransferResult>
~~~

**职责与输出**：保存有界待确认密文或已确认日志提示。

**输入与前置条件**：入站连接已验证端点；预传仍不能证明正式接受。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 有界解析/验外层。
2. 已确认 entry 交日志验证。
3. 未确认候选暂存受配额缓存。
4. 触发节点补齐。
5. 不触发正式消息插件。

**持久化、并发与副作用**：预传缓存独立 TTL/配额；不推进正式 MLS 或 verified cursor。

**失败处理**：INVALID_SIGNATURE，PRETRANSFER_QUOTA_EXCEEDED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-005 verify_candidate](protocol.md#protocol-005)、[SYNC-008 catch_up_group](sync.md#sync-008)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 恶意发送大量未接受候选被限额。
- [ ] 禁言前后以正式 seq 为准。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。


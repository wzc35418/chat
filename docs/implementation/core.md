# 客户端内核与 Kotlin 业务服务映射

阶段：P1 → P2。状态：函数级设计；竖直切片（`ConversationCore` 的 open / create_draft / draft_status / apply_verified_entry / list_conversations / list_messages / close）与 P1 身份持久化增量（save_identity_account / load_identity_account / upsert_contact / list_contacts / consume_invite）已在 Windows x64 实测通过；CORE-007 的发送编排（[crates/core/src/send_orchestrator.rs](../../crates/core/src/send_orchestrator.rs)）已把「意图 → 一次真实 MLS 加密 → 提交 → Accepted/Unknown/Rejected 落库」接通（节点端口可注入，真实节点链路见 `node_submitter` 记录），CORE-008 重试 / CORE-009 取消 / outbox 结果对账 `reconcile` 也已实现（同一文件，只加方法）；CORE-010 草稿状态视图（`DraftStatusView` + 共用投影 `draft_status_view`）与 CORE-003 的拉取式事件流（`crates/core/src/core_events.rs`：有界缓冲 + 单调序号 + 溢出 resync）同样已实测通过；`ConversationCore` 层的 CORE-009 `cancel_draft` 与 CORE-003 `poll_events`（含 FFI 未接线时的 `StubSubmitter`/`StubCrypto` 显式失败）也已实测通过；附件上传/下载（ATTACHMENTS-003/005）已在 core 层通过可注入的块传输端口接通编排与状态机（见 [attachments.md](attachments.md)）；其余 CORE 函数未开始。目标源码：[crates/core/src/service.rs](../../crates/core/src/service.rs) 与 [crates/core/src/send_orchestrator.rs](../../crates/core/src/send_orchestrator.rs)。

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

## 身份持久化 API（core 服务层增量，无计划书编号）

计划书的 CORE-001..012 只覆盖启动/会话/消息/送达；本轮把 IDENTITY-001/002/006 与
邀请一次性消费接到正式加密存储，在 `ConversationCore` 上**只新增方法**（未改动既有
签名），实现在 [crates/core/src/service.rs](../../crates/core/src/service.rs)：

~~~text
save_identity_account(input: IdentityAccountInput) -> Result<IdentityAccountView>
load_identity_account() -> Result<Option<IdentityAccountView>>
upsert_contact(request: ContactUpsertRequest) -> Result<ContactView>
list_contacts() -> Result<Vec<ContactView>>
consume_invite(invite_id: &str) -> Result<ConsumedInviteView>
invite_id_for_token(invite_token: &str) -> Result<String>   // 关联辅助函数
~~~

**职责与输出**：

- `save_identity_account`：把本机账户（单账户/单设备）落到正式库。输入只有显示名、
  账户种子十六进制、设备种子十六进制与创建时间（0 表示取当前时钟）；账户/设备 ID、
  公钥与指纹**由 core 用 `chat-identity` 从种子派生**，不信任调用方传入的 ID。
  显示名与时间由 `LocalAccount::from_seed` 校验（1..=64 字节、非空白、无控制字符、
  时间 > 0）。
- `load_identity_account`：返回脱敏投影（账户/设备 ID、显示名、**分组展示的核验指纹**、
  创建时间），未保存时返回 `None`；不含任何种子或私钥。
- `upsert_contact`：`account_id` 必须是 PROTOCOL-001 `acct:1:<64 hex>`；`fingerprint`
  接受分组或连续十六进制（identity `Fingerprint::parse`）；`state` 只用 identity 的
  五个冻结状态。记录按 storage 派生的 `contact_id` 幂等；**已存在记录必须先通过
  identity 的冻结迁移表**（`transition_allowed`），非法迁移返回
  `INVALID_STATE_TRANSITION`，绝不静默归一。
- `list_contacts`：按事件时间降序、联系人键升序返回脱敏投影。
- `consume_invite`：在单笔身份作用域事务内记录一次性消费。**第二次（或同 ID 不同
  摘要）返回 `INVALID_STATE_TRANSITION`**；失败整体回滚，不存在半消费。
- `invite_id_for_token`：只校验 `inv1` 前缀与长度上限，然后派生稳定的
  `invite-<32 hex>` 一次性 ID（同一口令永远同 ID）。**不验证签名/期限/信任锚**。

**失败处理**：`INVALID_INPUT`（种子十六进制、显示名长度/控制字符、状态白名单、
指纹/账户 ID 文本、超长邀请 ID）、`INVALID_TARGET`（identity 的显示名错误映射）、
`IDENTITY_ACCOUNT_CONFLICT`（已绑定另一账户/另一份密钥材料）、
`INVALID_STATE_TRANSITION`（非法联系人迁移或邀请已消费）、
`STATE_CORRUPTED`（密封记录被篡改/错误主密钥）、`CORE_CLOSED`（close 之后）。
`error_code` 现在也透传 `IdentityError` 的 PROTOCOL-014 冻结码；既有 `StorageError` /
`CoreError` 映射不变。

**持久化、并发与副作用**：全部写入走 storage 的 `WriteScope::identity` 单笔事务；
种子只在内存中解码，交给 storage 的副本由 storage 的 `Zeroizing` 缓冲持有并在返回前
清零；core 不扩散、不记录、不返回种子。core 用 `Mutex` 串行化单写者，跨进程并发由
`open_store` 的独占锁拒绝。

**完成验收**（本轮已覆盖）：

- [x] 写入后重开（同主密钥）能读回账户与联系人：`identity::identity_account_round_trips_across_reopen`、`identity::contacts_upsert_list_and_enforce_the_identity_state_machine`。
- [x] 错误主密钥拒绝且不覆盖数据：`identity::identity_duplicate_and_conflict_are_explicit_and_wrong_key_is_rejected`（`STATE_CORRUPTED`，原密钥仍读回同一账户）。
- [x] 重复保存同内容幂等、不同账户显式冲突：同上（`already_existed` 为真；另一账户 → `IDENTITY_ACCOUNT_CONFLICT`）。
- [x] 邀请并发/重复消费只成功一次：`identity::invite_consumption_is_one_shot_even_under_concurrent_calls`（4 线程并发只 1 个成功，重开仍为已消费）。
- [x] 畸形输入与关闭后显式失败：`identity::malformed_identity_inputs_and_closed_core_fail_explicitly`。
- [ ] Android kill 后重启恢复身份会话（未验证：core 尚不重建 `LocalAccount`，见限制 1）。

**证据**：`cargo test --offline -p chat-core`（本轮实测 lib 9 + identity 5 + p0 2 + service 5 = 21 passed，0 失败；新增的 5 个用例都在 `tests/identity.rs`，其余既有用例未改动）；`cargo test --offline -p chat-storage -p chat-core` 与 `cargo build --locked --offline` 全绿。存储层语义、明文扫描与损坏记录证据见 [storage.md](storage.md) 的“身份/联系人持久化与邀请一次性消费实现记录”段。

**已知限制**：

1. **种子不外流**：本轮不提供把种子读回内存重建 `LocalAccount` 的 trusted-path 方法，
   因此核心的会话签名/邀请签发路径还没有接到已持久化的身份上（需要后续 trusted-path
   接口，禁止把种子放进普通 DTO）。`load_identity_account` 只返回脱敏投影。
2. 设备凭据/撤销集合（IDENTITY-003）未持久化；本轮只保存单设备种子与公钥，多设备与
   撤销版本留给后续任务。
3. `consume_invite` 只保证“同一 ID 只成功一次”，**不校验邀请本身**；调用方必须先用
   `chat_identity::verify_invite`（签名/期限/信任锚/作用域）校验并通过
   `invite_id_for_token` 派生 ID，否则可以消费任意合法 ID。
4. core 的并发模型是单写者 `Mutex` + 独占库锁；跨进程同时打开仍返回 `STORAGE_LOCKED`。

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

**实现状态**：切片实现。`ConversationCore::open(database_path, master_key_hex)` 解析 64 位十六进制主密钥（解码缓冲用后清零）→ `open_store`（独占锁 + 密钥校验 + 迁移），错误密钥/损坏库/不存在的数据目录显式失败（`STATE_CORRUPTED` / `INVALID_INPUT`），不重建同名空库，也不做“启动即恢复”（`recover_incomplete_writes` 仍须显式调用，属 STORAGE-011 的后续接线）。平台密钥解包（PLATFORM-002）、身份/群执行器装载、事件桥与网络启动均未开始。

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

- [x] 消费者慢导致 resync 仍可查询完整状态：`draft_status_events::overflow_returns_resync_required_instead_of_dropping_silently`（慢消费者游标落在缓冲起点之前时返回 `resync_required = true` 与一条 `ResyncRequired`，调用方据此整体重查；仍在窗口内的游标可继续增量读）。
- [ ] release 后无回调：本轮是**拉取式** `poll_events`（没有 `EventListener` 回调注册），因此“释放订阅后不再回调”暂不适用；`shutdown` 后只追加一条 `CoreClosed` 且不再发布任何事件。

**实现状态**：部分实现（拉取式最小切片，Windows x64 实测）。`crates/core/src/core_events.rs` 提供 `CoreEvent`（`DraftChanged`/`ConversationChanged`/`ResyncRequired`/`CoreClosed`，只含 ID 与状态文本）、`EventBus`（有界环形缓冲，容量 `1..=MAX_EVENT_CAPACITY = 4096`，默认 256；序号从 1 单调递增；单批上限 `MAX_EVENT_BATCH = 256`）与 `SendService::poll_events(since_seq, limit)`。与契约的差异：契约是“登记 listener + 串行转发 + 释放订阅”，本轮改为“事件进入有界缓冲，调用方按序号增量拉取”，因此没有 `SubscriptionHandle`/回调释放，取而代之的是 `shutdown` + `CoreClosed`；这部分留给 BRIDGE-003/004 的事件桥接任务。证据：`draft_status_events::events_are_monotonic_and_repeat_polling_is_idempotent`、`draft_status_events::overflow_returns_resync_required_instead_of_dropping_silently`、`draft_status_events::events_never_carry_message_content_or_keys`、`draft_status_events::shutdown_publishes_core_closed_and_rejects_later_calls`。**ConversationCore 层接线（task-21）**：`ConversationCore` 现在自带 `Mutex<EventBus>` 并提供 `poll_events(since_seq, limit)` 与 `close()` 的 `CoreClosed` 发布，证据见下文“ConversationCore 层取消与事件接线”。

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

- [x] 断网仍可列会话：`service::end_to_end_draft_then_verified_entry_then_pages`（全离线只读本地库即可返回标题/预览/活动时间，未读显式 `None`）。
- [x] 预传未接受不算正式最后消息：`formal_query::conversation_page_orders_by_activity_with_stable_tiebreak` 与 STORAGE-012 的待发/正式分段（本层直接委托，不自行合并）。
- [ ] 群 paused / identity warning 状态注入：未开始（需要群与身份模块的会话状态）。

**实现状态**：切片实现。`ConversationCore::list_conversations(cursor, limit)` 委托 STORAGE-013 分页，逐条读取 `Store::session_summary` 组装 `ConversationView { conversation_id, title, preview, unread_count, updated_at_ms }`；游标原样透传（内部 `spg1` 令牌），`unread_count` 恒为 `None`。证据：`service::end_to_end_draft_then_verified_entry_then_pages`、`service::message_pages_are_keyset_paginated_and_bound_cursors`（伪造游标 `INVALID_CURSOR`、limit 0 `INVALID_INPUT`）。

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

- [x] 加入前正文不可查询：`formal_query::message_page_enforces_limits_and_rejects_bad_cursors`（不存在或已 left/dissolved 的会话 `PERMISSION_DENIED`，不返回正文）。
- [ ] 未安装插件显示 fallback：未开始（附件/插件投影不在本切片）。
- [ ] 加入窗口（DOMAIN-012）判定：未开始，当前只依赖存储的会话可见性。

**实现状态**：切片实现。`ConversationCore::list_messages(conversation_id, cursor, limit)` 校验会话 ID 的 PROTOCOL-001 文本后委托 STORAGE-012，映射为 `MessageRowView { message_id, conversation_id, author, body, mine, created_at_ms }`；本机待发意图（`local_pending`）判定 `mine = true`、作者取摘要里的本机身份，正式消息按摘要记录的本机账户/设备判定 `mine`。证据：`service::end_to_end_draft_then_verified_entry_then_pages`（待发在前、正式在后、不重复）、`service::message_pages_are_keyset_paginated_and_bound_cursors`（2/2/1 三页、游标 `spg1` 前缀、无重复无跳页）。

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

- [x] 双击/跨 FFI 重试只同 Draft：`service::end_to_end_draft_then_verified_entry_then_pages`（同一 operation ID 两次 create_draft 返回同一 `draft_id`）、`send_orchestrator::begin_send_is_idempotent_and_prepare_encrypts_exactly_once`（同一 operation ID 两次 `begin_send` 返回同一 draft_id）。
- [x] 节点离线仍 QUEUED：`service::end_to_end_draft_then_verified_entry_then_pages`（完全离线、无网络调用即返回 `queued` 草稿）、`send_orchestrator::accepted_result_persists_draft_and_outbox_then_replays_idempotently`（提交前草稿恒为 `queued`，接受后才 `accepted`）。
- [x] 取消回调后查询可恢复：`service::end_to_end_draft_then_verified_entry_then_pages`（`draft_status` 读回同一草稿；未知草稿 `None`、畸形 ID `INVALID_INPUT`）。
- [ ] 发送 worker 唤醒与网络提交：只完成到可注入节点端口（`NodeSubmitter`）与 `dispatch`；真实 TRANSPORT-006 `append_candidate` / SYNC-002 `run_outbox_once` 常驻 worker 未接线。

**实现状态**：切片实现，分两层：
① `ConversationCore::create_draft(conversation_id, operation_id, body)`（[service.rs](../../crates/core/src/service.rs)）：校验 PROTOCOL-001 会话/操作 ID 与线协议正文上限后，在单笔 T1 事务内 `materialize_session` + STORAGE-006 `persist_send_intent`，返回 `DraftView { draft_id, operation_id, conversation_id, body, state, submission_id }`；失败整体回滚。
② `SendOrchestrator::begin_send / prepare / dispatch`（[send_orchestrator.rs](../../crates/core/src/send_orchestrator.rs)）：把意图推进为一次真实 MLS 加密尝试并提交，见下文「发送编排实现记录（CORE-007 / SYNC-003/004）」。证据：`service::end_to_end_draft_then_verified_entry_then_pages`、`service::identifiers_are_validated_against_the_protocol_contract`、`send_orchestrator::*`（9 个用例）。

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

- [x] 超时但已接受显示 ACCEPTED：`send_orchestrator::retry_draft_reports_accepted_and_unknown_without_resending`（unknown 时只报告 `AwaitingReconciliation`；对账获得接受后 `release` 路径返回 `AlreadyAccepted`，重复调用仍为 `AlreadyAccepted`，零重发零重加密）。
- [x] 禁言后重试拒绝：`send_orchestrator::retry_draft_rejects_cancelled_permission_and_corrupted_drafts`（会话 `left` 时按 DOMAIN-010 `classify_retry` 判为 `WaitForUser` → `PERMISSION_DENIED`；已取消的草稿 `INVALID_STATE_TRANSITION`；被篡改的草稿 `STATE_CORRUPTED`；有效租约属于别的 worker 时 `WaitingForLease` 且零加密零提交）。

**实现状态**：已实现（切片，Windows x64 实测）。`SendOrchestrator::retry_draft(draft_id) -> SendResult<PrepareOutcome>`（[send_orchestrator.rs](../../crates/core/src/send_orchestrator.rs)）：复用 `chat_sync::send::plan_submission` 决策——已接受 → `AlreadyAccepted`；结果未知 → `AwaitingReconciliation`（不加密、不提交、不改状态）；已取消/终止 → 显式 `INVALID_STATE_TRANSITION`；租约有效 → `WaitingForLease`；其余（新草稿、可重试失败、租约过期、被拒绝）→ 复用 `prepare` 路径：`Resubmit` 复用同 SubmissionId 同密文、`Rebuild` 用新 SubmissionId 重建（受 `MAX_ATTEMPT_INDEX = 64` 限制）、`Prepare` 首次加密。不可重试分类复用 DOMAIN-010 `classify_retry`（本轮用它判权限类为 `WaitForUser`）。证据：`send_orchestrator::retry_draft_reports_accepted_and_unknown_without_resending`、`send_orchestrator::retry_draft_rebuilds_a_rejected_attempt_and_stays_idempotent`、`send_orchestrator::retry_draft_rejects_cancelled_permission_and_corrupted_drafts`。

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

- [x] 取消与节点提交竞争最终显示真实接受：`send_orchestrator::cancel_draft_returns_three_states_and_blocks_dispatch`（结果未知时 `CancelUnknown` 不写 `cancel_requested`，对账得到接受后草稿以接受为准落 `accepted`）、`send_orchestrator::reconcile_repairs_the_two_transaction_window_and_stays_idempotent`（取消与接受竞态下 `reconcile` 把草稿收敛回 `accepted`）。
- [x] 不误删已接受记录：`send_orchestrator::cancel_draft_returns_three_states_and_blocks_dispatch`（已接受 → `CancelTooLate`，草稿 `accepted`、attempt `accepted` 均不变，`cancel_requested` 保持 0）。

**实现状态**：已实现（切片，Windows x64 实测）。`SendOrchestrator::cancel_draft(draft_id) -> SendResult<CancelOutcome>`，三态 `CancelRequested { draft_id, submission_id }` / `CancelTooLate { draft_id, submission_id }` / `CancelUnknown { draft_id, submission_id }`：已接受 → `CancelTooLate`（不撤回、不删除消息）；结果未知 → `CancelUnknown`（只把 `drafts.state` 置 `cancelled` 表达用户意图，**不写** `cancel_requested`，否则对账窗口结束后该尝试永远无法再被领取对账）；其余 → `CancelRequested`（草稿字节与 attempt 行保留、`drafts.state = 'cancelled'`、prepare/send 任务标 `cancel_requested = 1`，之后 `dispatch` 一律 `DispatchOutcome::Abandoned` 且零提交）。附件释放（ATTACHMENT）未接入。证据：`send_orchestrator::cancel_draft_returns_three_states_and_blocks_dispatch`。**ConversationCore 层（task-21）**：`ConversationCore::cancel_draft(&mut self, draft_id) -> anyhow::Result<CancelOutcome>` 把同一个 `Store` 临时交给 `SendService`（桩端口 + 系统时钟）执行三态取消并把产生的事件并入内核事件总线，无论成败都把句柄放回；证据见 `cancel_events::cancel_queued_draft_requests_and_publishes_events`、`cancel_events::cancel_accepted_draft_is_too_late`、`cancel_events::cancel_unknown_draft_reports_unknown`、`cancel_events::cancel_failure_keeps_the_store_usable`。

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

- [x] 同意图多尝试可追踪：`draft_status_events::draft_status_tracks_every_lifecycle_step`（`DraftStatusView.attempt_index` / `attempts` / `submission_id` 直接来自按序枚举的 `message_attempts`：拒绝后重建得到 `attempt_index = 1`、`attempts = 2` 与新 SubmissionId；状态查询还给 `ConversationCore::draft_status` 的正文投影复用同一实现）。
- [ ] 节点存信箱状态独立展示：未开始（需要节点信箱与 outbox 结果投影；本轮 `updated_at_ms` 只取草稿创建时间与发送任务的 `retry_at`/`lease_until`）。

**实现状态**：已实现（切片，Windows x64 实测）。共用投影 `service::draft_status_view(store, draft_id)`，两个入口：`ConversationCore::get_draft_status(draft_id)`（持有 `Store` 的服务对象）与 `SendService::get_draft_status(draft_id)`（包装编排器的发送服务）。返回 `DraftStatusView { draft_id, conversation_id, operation_id, status, submission_id, attempt_index, attempts, updated_at_ms, can_retry, can_cancel }`，**不含正文**（不解密 `drafts.protected_content`，正文损坏只在 `draft_status` 的 body 投影与发送路径上显式 `STATE_CORRUPTED`）；未知草稿返回 `None`，畸形草稿 ID 返回 `INVALID_INPUT`。

有效状态推导（草稿状态优先，其余取最新 attempt）：`drafts.state` 为 `accepted`/`delivered` → `accepted`；`cancelled` → `cancelled`；`failed` → `failed`；否则最新 attempt 的 `prepared`/`submitted`/`accepted`/`rejected`/`unknown` 原样作为状态，没有 attempt → `queued`（acceptance 是权威事实：`attempt accepted` 即使草稿还没落 accepted 也显示 `accepted`）。

可执行动作推导（由状态与冻结规则推导，不臆造）：

| 有效状态 | can_retry | can_cancel |
|---|---|---|
| queued / prepared / submitted（无有效租约） | true | true |
| queued / prepared / submitted（发送任务有效租约被占） | false（等租约） | true |
| rejected（`attempt_index < MAX_ATTEMPT_INDEX = 64`） | true（新 SubmissionId 重建） | true |
| rejected（预算用尽） | false | true |
| unknown | false（只能对账） | true（`CancelUnknown` 只记意图） |
| accepted | false（`AlreadyAccepted`） | false（`CancelTooLate`） |
| cancelled / failed | false | false |

证据：`draft_status_events::draft_status_tracks_every_lifecycle_step`（queued → prepared → accepted、unknown、rejected → 重建、cancelled、未知/畸形草稿、以及 `ConversationCore` 与 `SendService` 同一投影）。

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

## 竖直切片实现记录（CORE-001/005/006/007/010，2026-10-18，Windows x64）

实现位置：[crates/core/src/service.rs](../../crates/core/src/service.rs)（`ConversationCore`），测试：[crates/core/tests/service.rs](../../crates/core/tests/service.rs)。本层只编排既有 storage 函数，不自行加密、不自行判定投递成功。

公开面（Kotlin 侧经 `LocalStoreFfi` 调用，形状一致）：

~~~text
ConversationCore::open(database_path: &str, master_key_hex: &str) -> anyhow::Result<Self>
ConversationCore::create_draft(&mut self, conversation_id: &str, operation_id: &str, body: &str) -> Result<DraftView>
ConversationCore::draft_status(&self, draft_id: &str) -> Result<Option<DraftView>>
ConversationCore::apply_verified_entry(&mut self, input: VerifiedEntryInput) -> Result<MessageRowView>
ConversationCore::list_conversations(&self, cursor: Option<&str>, limit: u32) -> Result<ConversationPageView>
ConversationCore::list_messages(&self, conversation_id: &str, cursor: Option<&str>, limit: u32) -> Result<MessagePageView>
ConversationCore::close(&self)                 // 幂等；释放 Store 与独占写锁
ConversationCore::is_open(&self) -> bool
~~~

- `DraftView`：`draft_id, operation_id, conversation_id, body, state("queued"|"prepared"), submission_id: Option<String>`。
- `ConversationView`：`conversation_id, title, preview, unread_count: Option<u32>, updated_at_ms: u64`。
- `MessageRowView`：`message_id, conversation_id, author, body, mine, created_at_ms: u64`。
- `ConversationPageView` / `MessagePageView`：`items` + `next_cursor: Option<String>`。

输入契约：

- ID：`conversation_id` 必须是 PROTOCOL-001 `conv:1:<64 位小写 hex>`，`message_id` 是 `msg:1:<…>`，`operation_id` 是 `op:1:<…>`，作者账户/设备是 `acct:1:<…>` / `dev:1:<…>`，否则 `INVALID_INPUT`；`draft_id` 是 storage 派生的 `draft-<32 位小写 hex>` 本地标识，不是 PROTOCOL-001 文本，只做有界结构校验。
- 正文：沿用线协议 `validate_text`（非空、≤ 8 KiB）；标题 ≤ 512 字节；时间戳必须非负。
- 主密钥：64 位十六进制（32 字节），解码缓冲用后清零；字符串本身的生命周期由调用方负责（未做内存清零，见限制）。
- 关闭后所有方法返回 `CORE_CLOSED`（错误码由 `service::error_code` 投影，FFI 直接透传）。

复现命令（全程离线，本轮实测）：

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core   # 2 p0 + 5 service = 7 passed（本切片新增 5 个 service 用例）
~~~

证据（测试名 → 覆盖的契约点）：

- `service::end_to_end_draft_then_verified_entry_then_pages`：临时目录建库 → 建草稿（幂等）→ `draft_status` 读回 → 写一条已验证消息 → 消息页（待发在前）与会话页（标题/预览/活动时间/未读 None）全部读回。
- `service::message_pages_are_keyset_paginated_and_bound_cursors`：5 条消息按 limit 2 翻三页、无重复无跳页、顺序为序号降序、游标是 `spg1` 内部令牌；伪造游标 `INVALID_CURSOR`，limit 0 `INVALID_INPUT`。
- `service::verified_entry_is_idempotent_and_conflict_is_explicit`：同内容重放返回同一投影且不新增行；同 `message_id` 换正文 `MESSAGE_ID_CONFLICT`。
- `service::identifiers_are_validated_against_the_protocol_contract`：非规范会话/操作/消息 ID 与超长正文都是 `INVALID_INPUT` 且不写库；空库会话页为空而不是报错。
- `service::close_releases_the_writer_and_open_rejects_bad_keys_or_directories`：第二个句柄 `STORAGE_LOCKED`；`close` 后 `CORE_CLOSED` 且独占锁释放（同密钥可重开、数据仍在）；错误密钥 `STATE_CORRUPTED`、畸形密钥与不存在目录 `INVALID_INPUT`。

已知限制（本切片不覆盖）：

1. **`mine` 的判定来源**：`messages` 没有 `mine` 列，`mine` 由会话摘要记录的“本机作者（账户优先，缺账户时用设备）”与消息作者比对；尚未接入身份模块，因此没有写过 `mine = true` 条目的会话里远端消息一律 `false`（不猜测），本机身份的准确性依赖该记录。
2. **`unread_count` 恒为 `None`**：未读需要读位置（read cursor）存储，schema 无该表，本轮不新增迁移、不返回伪造的 0。
3. **游标是内部令牌**：`list_*` 返回 storage 的内部 `spg1` 令牌（10 分钟 TTL），不是线协议 `cur1`；跨层/上网都不可用。
4. **待发与正式未关联**：本切片输入不带 `draft_id`，本机消息可能短暂同时出现“待发投影 + 正式消息”，等 outbox 结果（STORAGE-014/015）把草稿置为 `accepted` 后消失。
5. **无发送/同步/事件**：`create_draft` 只做 T1 持久化，不唤醒 worker、不发网络、不产生附件或回执；`observe_events`、`open_direct`、`send_message` 的正式接受路径、`get_delivery`、`handle_peer_pretransfer` 都未开始。
6. **启动不做恢复**：`open` 只打开库与迁移，`recover_incomplete_writes`（STORAGE-011）仍需显式调用，未接进 `open`。
7. **主密钥字符串未清零**：`master_key_hex` 由调用方持有，本轮只清零解码后的字节缓冲；`chat-core` 未引入 `zeroize` 依赖。

## 发送编排实现记录（CORE-007 / SYNC-003/004，2026-10-18，Windows x64）

实现位置：[crates/core/src/send_orchestrator.rs](../../crates/core/src/send_orchestrator.rs)（新增，`lib.rs` 加一行 `pub mod send_orchestrator;`），测试：[crates/core/tests/send_orchestrator.rs](../../crates/core/tests/send_orchestrator.rs)。本轮**没有另造状态机**：所有调度决策仍由 `chat_sync::send::plan_submission`（SYNC-003/004）给出，本层只负责「按计划加密一次 → 持久化 → 调用节点端口 → 落库」。

公开面：

~~~text
pub trait CryptoEngine {
    fn epoch(&self, conversation_id: &str) -> SendResult<u64>;
    fn encrypt(&mut self, conversation_id: &str, draft_id: &str, body: &str) -> SendResult<EncryptedPayload>;
}
pub trait NodeSubmitter {
    fn submit(&self, attempt: &SubmitAttempt) -> SendResult<SubmitOutcome>;
    fn query(&self, submission_id: &str) -> SendResult<SubmitOutcome>;
}
pub enum SubmitOutcome { Accepted { detail }, Unknown { detail }, Rejected { detail } }

pub struct SendOrchestrator<S: NodeSubmitter, C: CryptoEngine, K: Clock> { .. }
impl .. {
    pub fn new(store: Store, submitter: S, engine: C, clock: K, worker: &str) -> SendResult<Self>;
    pub fn begin_send(&mut self, conversation_id: &str, operation_id: &str, body: &str) -> SendResult<String>; // draft_id
    pub fn prepare(&mut self, draft_id: &str) -> SendResult<PrepareOutcome>;
    pub fn dispatch(&mut self, draft_id: &str) -> SendResult<DispatchOutcome>;
    pub fn store(&self) -> &Store;         pub fn store_mut(&mut self) -> &mut Store;
    pub fn clock_mut(&mut self) -> &mut K;  pub fn worker(&self) -> &str;
}
~~~

三段语义：

1. **`begin_send`（T1）**：校验 PROTOCOL-001 会话/操作 ID 与线协议正文上限 → 单笔 intent 事务内 `materialize_session` + STORAGE-006 `persist_send_intent`（同一 operation ID 幂等）→ 返回 `draft_id`。
2. **`prepare`（T2，SYNC-003 执行侧）**：读草稿与 attempt 投影（attempt 序号按“既有 attempt 行数 - 1”推导）→ `plan_submission` → 只有 `Prepare`/`Rebuild` 才调用 `CryptoEngine::encrypt` **一次**，把 `(epoch, ciphertext)` 通过 STORAGE-007 `persist_prepared_attempt` 写入（同事务写 send outbox 任务、把 prepare 任务置 complete）。`Resubmit`（已有耐久尝试）直接复用既有字节、不重新加密；`QueryExisting`（未知）只提示对账；`WaitForLease` 等待；`Release` 报告已接受；`Abandoned` 放弃。同 SubmissionId 已存在时再次 `prepare` 返回 `already_prepared = true` 且不加密。
3. **`dispatch`（SYNC-004 执行侧）**：按计划分支——
   - `Resubmit`：读**耐久密文**（`Store::read_attempt`）→ `NodeSubmitter::submit` → 三态落库；
   - `QueryExisting`（unknown）：只调用 `NodeSubmitter::query`，**绝不重发、绝不重新加密**；
   - `Release`：幂等返回 `Accepted { idempotent: true }`，并顺手补齐草稿状态（自愈）；
   - `WaitForLease` / `Abandoned`：不提交；`Prepare`/`Rebuild`：显式 `INVALID_STATE_TRANSITION`（必须先 `prepare`）。
   落库走 STORAGE-015 语义：`Accepted → JobResult::FormallyAccepted`（任务 complete + attempt accepted + 草稿 accepted）、`Unknown → JobResult::Unknown`（租约延长到 6 小时对账窗口、attempt unknown）、`Rejected → JobResult::Failed{retryable:false}`（任务 failed + attempt rejected）。节点端口返回的错误（如 `STALE_EPOCH` / `INVALID_SIGNATURE`）原样上抛，状态不变，草稿保留。

并发与租约：

- 调度决策里的租约门来自 `AttemptSnapshot.lease_owner/lease_until/lease_version`，由发送任务行（`outbox_jobs.dedup_key = 'send:<SubmissionId>'`）投影；有效租约被其它 worker 持有时 `dispatch`/`prepare` 都返回 `WaitingForLease`，不提交也不加密。
- storage 只提供批量领取（`claim_outbox_batch`），本层在 `WriteScope::outbox` 事务内用**同一 CAS 规则**（`lease_version` + `state/lease_until` 条件）定向领取该 SubmissionId 的任务；CAS 失手时返回 `WaitingForLease`，绝不覆盖别人的租约。unknown 对账窗口内沿用自己尚未到期的租约（`lease_for_result`），因为此时不能再“领取”。
- 重复 `dispatch` 幂等：已接受 → `Release` 分支不再次提交；未知 → 只查询。

复现命令（全程离线，本轮实测）：

~~~powershell
. .	oolsactivate.ps1
cargo test --offline -p chat-core   # 9 lib + 5 identity + 2 p0 + 9 send_orchestrator + 5 service = 30 passed（既有 21 个继续通过）
~~~

证据（测试名 → 覆盖的契约点）：

- `send_orchestrator::begin_send_is_idempotent_and_prepare_encrypts_exactly_once`：同 operation ID 两次 `begin_send` 同一 draft；`prepare` 两次只加密 1 次、attempt 行 1 条、prepare 任务 complete、send 任务 pending；**存库密文用对端真实 OpenMLS 设备解密得到原文**。
- `send_orchestrator::unknown_result_is_never_resent_and_reconciles_by_submission_id`：提交返回 Unknown 后 `submit` 次数恒为 1、加密次数恒为 1；后续 `dispatch` 只走 `query`；对账得到 Accepted 后 attempt accepted、send 任务 complete、草稿 accepted。
- `send_orchestrator::rejected_attempt_rebuilds_with_a_new_submission_id`：拒绝后 attempt rejected、草稿保留 queued、直接再 `dispatch` 被 `INVALID_STATE_TRANSITION` 拒绝；`prepare` 用 `attempt_index + 1` 得到**新 SubmissionId** 并再加密一次（新密文同样可被对端解密）；再 `dispatch` 接受。
- `send_orchestrator::accepted_result_persists_draft_and_outbox_then_replays_idempotently`：接受后草稿 `accepted`、attempt `accepted`、send 任务 `complete`；重复 `dispatch` 返回 `idempotent = true` 且不再提交；再 `prepare` 直接 `AlreadyAccepted`。
- `send_orchestrator::live_lease_from_another_worker_blocks_encryption_and_submission`：真实 `claim_outbox_batch`（worker-b）持租约期间 `dispatch`/`prepare` 都等待、零提交；租约到期后接管并提交**同一密文**。
- `send_orchestrator::transport_error_keeps_the_draft_and_retries_after_lease_expiry`：端口返回 `STALE_EPOCH` 时草稿仍 `queued`、attempt 仍 `prepared`、不重新加密；租约到期后用同一 SubmissionId 同字节重试成功。
- `send_orchestrator::protocol_errors_and_state_preconditions_are_classified`：非规范 ID `INVALID_ID`、空正文 `INVALID_TARGET`、未准备的草稿 `INVALID_STATE_TRANSITION`、未知草稿 `INVALID_TARGET`、端口协议错误原码上抛且状态不变。
- `send_orchestrator::rebuild_budget_is_bounded`：铺满 `MAX_ATTEMPT_INDEX = 64` 后 `prepare` 返回 `INVALID_TARGET` 且零加密（重建序号有界）。
- `send_orchestrator::orchestrator_rejects_malformed_worker_id`：畸形 worker 标识 `INVALID_ID`；时钟非正值 `INVALID_TARGET`（时钟故障不静默继续）。

已知限制（本切片不覆盖）：

1. **真实网络未接线**：`NodeSubmitter` 是可注入端口，本层不实现 TRANSPORT-006 `append_candidate`、回执验证（SYNC-009）或节点信箱；常驻 worker/SYNC-002 `run_outbox_once` 也未接。生产接法：把 `submit` 实现为 append_candidate + 正式回执校验，把 `query` 实现为节点按 SubmissionId 查询。
2. **storage 缺三个正式入口**：按草稿枚举 attempt、按 SubmissionId 定向领取租约、把草稿置 `accepted`。本层用只读连接做投影查询、用 STORAGE-014 的同一 CAS 规则定向领取、在 intent 事务内更新 `drafts.state`；storage 补正式 API 后应替换并删除对应限制。
3. **接受的两笔事务窗口**：`Accepted` 先落 outbox 结果再置草稿 accepted，第二笔失败时会出现 attempt accepted 而草稿仍 queued；下次 `dispatch` 走 `Release` 分支会补齐（自愈），但本轮没有专门的恢复任务。
4. **attempt 序号不落库**：P0 schema 没有 `attempt_index` 列，本层用“既有 attempt 行数 - 1”推导，重建依赖“每个被拒绝的 attempt 行都保留”（storage 目前确实不删除）。
5. **MLS provider 记录未写入**：`crypto_changes` 传空，加密引擎是会话级 OpenMLS 设备而非 storage 支撑的 provider；CRYPTO-004/005 的 provider 持久化接入后应改为同事务写 `mls_state_records`（并同步提升 epoch）。
6. **无权限/附件/同步补齐**：DOMAIN-001 权限校验、DOMAIN-011 回复目标、附件依赖（ATTACHMENT_NOT_READY）、SYNC-008 补齐都未接入；本层也不产生 DraftChanged 事件（CORE-003 未实现）。
7. **单写者**：`SendOrchestrator` 拥有独占的 `Store`，与 `ConversationCore` 不能同时打开同一库；两者合并到同一个 core 句柄的方式（让 `ConversationCore` 暴露 `&mut Store` 或把编排器作为其内部字段）留待 FFI 接线时确定。

## 节点提交端口实现记录（NodeClientSubmitter，O 包，2026-10-18，Windows x64）

实现位置：[crates/core/src/node_submitter.rs](../../crates/core/src/node_submitter.rs)（lib.rs 由 Lead 加一行 `pub mod node_submitter;`），测试：[crates/core/tests/node_submitter.rs](../../crates/core/tests/node_submitter.rs)。本轮把“发送编排”的可注入端口 `NodeSubmitter` 接到**真实节点**：客户端用 [crates/node-client](../../crates/node-client)（reqwest + Ed25519 + 一次性挑战），服务端是真实 [crates/node-api](../../crates/node-api) + [crates/node-services](../../crates/node-services)。编排状态机没有改动：调度决策仍由 `chat_sync::send::plan_submission` 给出。

公开面：

~~~text
pub struct NodeClientSubmitter { .. }   // 一个实例绑定一个群 + 一台设备（签名钥在 NodeClient 里）
impl NodeClientSubmitter {
    pub fn new(client: NodeClient, group_id: impl Into<String>) -> SendResult<Self>;
    pub fn with_page_limit(client: NodeClient, group_id: impl Into<String>, page_limit: u32) -> SendResult<Self>;
    pub fn group_id(&self) -> &str;
    pub fn metrics(&self) -> SubmitterMetrics;   // 可克隆计数句柄：在编排器之外断言“没有重发”
    pub fn submit_calls(&self) -> u64;
    pub fn query_calls(&self) -> u64;
    pub fn last_receipt(&self) -> Option<AppendResult>;
}
impl NodeSubmitter for NodeClientSubmitter {
    fn submit(&self, attempt: &SubmitAttempt) -> SendResult<SubmitOutcome>;
    fn query(&self, submission_id: &str) -> SendResult<SubmitOutcome>;
}
pub struct SubmitterMetrics { .. }      // submit_calls / query_calls / last_receipt
~~~

`submit` 的真实链路：

1. 校验 SubmissionId 是 PROTOCOL-001 文本、密文非空；
2. `GET /v1/groups/{id}/head` 取节点**当前公开前置状态**（`control_head / policy_version / mls_epoch`），失败按三态映射；
3. 用节点给出的这三个值 + `(SubmissionId, hex(密文))` 构造 PROTOCOL-004 候选（`kind = application`，作者=本设备），由 NodeClient 用设备私钥签名；
4. `POST /v1/groups/{id}/entries` 追加；节点按 `(group, submission)` 去重，因此同 ID 同字节重试只返回原回执（不会产生第二条日志）；
5. 回执必须绑定本次 `group/submission`，绑定失败按 `INVALID_RECEIPT_SCOPE` 显式失败；成功返回 `Accepted { detail: "node accepted seq=N" }`。

`query` 的真实链路：读 head → 若 `seq == 0` 直接 `Unknown` → 否则按页扫描 `GET /v1/groups/{id}/log`，命中回执即 `Accepted`；扫描完仍未命中返回 **`Unknown`（绝不返回 `Rejected`）**——节点只保存“已接受”的条目，找不到不能推断为拒绝。分页不前进时 `STATE_CORRUPTED` 显式失败，不无限循环。

三态映射（`classify`，节点错误码 → 结果）：

|节点结果|映射|理由|
|---|---|---|
|成功（已分配 seq）|`Accepted`|回执绑定本次候选、节点签名（固定公钥时）已复核|
|409 冲突（相异密文 / 群 ID 冲突 / stale head / stale epoch）|`Rejected`|同一次尝试永远不会被接受，必须由下一次 `prepare` 用新 SubmissionId 重建|
|401 鉴权（INVALID_SIGNATURE / INVALID_CREDENTIAL / AUTH_* / DEVICE_REVOKED）|`Rejected`|换挑战或重试都不会变好|
|403 权限（PERMISSION_DENIED / HISTORY_NOT_AUTHORIZED / INVALID_CAPABILITY）|`Rejected`|非成员/能力不足是永久失败|
|404（GROUP_NOT_FOUND）|`Rejected`|目标不存在|
|400（INVALID_REQUEST 等）与内容上限（PAYLOAD_TOO_LARGE）|`Rejected`|这份请求/密文本身不会被接受|
|网络 / 超时 / 限流 / 节点 5xx / 无法解释的响应|`Unknown`|节点可能已写入：只允许按 ID 对账，绝不重发|
|本地配置错误（NodeClientError::Config）或桥不可用|Err|本地故障，状态不变|

同步/异步边界：`NodeSubmitter` 的方法是同步的，`NodeClient` 是异步的。`SubmitBridge` 用**一条独立线程 + current-thread 运行时**把同步调用桥到异步请求，因此：

- 可以在任意上下文（包括已经是 tokio 运行时的调用方）里安全调用，不会触发“在运行时内阻塞”的 panic；
- 桥线程在 Drop 时 join，避免残留线程继续跑网络请求；桥不可用时返回 `INTERNAL_FAILURE` 而不是把调用挂死。

复现命令（全程离线，本轮实测）：

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core   # 14 lib + 6 identity + 5 node_submitter + 2 p0 + 9 send_orchestrator + 5 service = 41 passed
~~~

既有 30 个测试继续通过；本轮新增 10 个（5 个 lib 单元测试 + 5 个端到端）。`tests/identity.rs` 有 2 个既存的 `unused mut` 警告（不在本包写入范围，未改）。

证据（测试名 → 覆盖的契约点）：

- `node_submitter::real_node_accepts_the_orchestrated_send_and_persists_every_layer`：真节点链路 `begin_send → prepare → dispatch → Accepted`；草稿 accepted、attempt accepted、发送任务 complete、提交次数 1、加密次数 1；节点回执 `seq = 1` 且由节点签名；**节点日志里存的密文与本地耐久密文逐字节相同，且对端真实 OpenMLS 设备能解出原文**；节点 head 前进到 seq 1 并与回执 `log_hash` 一致。
- `node_submitter::repeated_dispatch_after_acceptance_never_resubmits`：接受后重复 `dispatch` 返回 `idempotent = true`，提交次数恒为 1、零对账、零重新加密，节点日志仍只有一条。
- `node_submitter::node_conflict_on_divergent_bytes_maps_to_rejected`：先用同一 SubmissionId 的相异字节让节点接受一条，再用真实密文提交同 ID → 节点 409 → `Rejected`；attempt rejected、发送任务 failed、草稿保留 queued、提交次数 1；节点日志仍只有相异那条；随后 `prepare` 用新 SubmissionId 重建并真实再加密一次。
- `node_submitter::unreachable_node_maps_to_unknown_and_then_only_reconciles`：提交端点连不上 → `Unknown`（attempt unknown、草稿 queued）；再次调度只走 `query`，对账同样连不上则显式 `NETWORK_UNAVAILABLE` 上抛，提交次数恒为 1、加密次数恒为 1。
- `node_submitter::query_reconciles_accepted_and_reports_unknown_for_missing_submission`：把本地投影置为“提交后崩溃”的 unknown（真实节点已接受）→ `dispatch` 只查询并按 SubmissionId 对账为 `Accepted`，提交次数不变；另一个节点从未收到的 SubmissionId 则始终返回 `Unknown`（不是 `Rejected`），两次调度都只查询、提交次数不变。
- lib 单元测试：`conflicting_and_unauthorized_answers_are_rejected`、`transport_rate_limit_and_5xx_answers_are_unknown`、`local_configuration_failures_are_errors_not_outcomes`（三态映射表逐项断言）、`constructor_rejects_non_protocol_ids_and_bad_limits`（群 ID/分页上限有界拒绝）、`debug_output_omits_keys_and_client_internals`（日志不含签名钥/客户端内部）。

已知限制（本切片不覆盖）：

1. **一个实例绑定一个群**：`conversation → group` 的映射由上层在构造时确定（当前切片一会话一群）；多群需要多个实例或按群路由。
2. **每次提交多一次往返**：候选的 CAS 字段只能来自节点 head（不本地推断），因此 `submit` 先 `get_head` 再 append。若两个写者之间的 head 发生变化（并发控制条目），本次会被映射为 `Rejected` 并由上层重建；更聪明的客户端应区分“stale head 可重读后重试”与“永久拒绝”。
3. **MLS epoch 与节点声明不做交叉校验**：节点侧把 `mls_epoch/policy_version` 当 CAS 常量（见 node.md 的 NODE-006 限制），本层上传的是节点 head 的值；真实 MLS epoch 仍需“客户端验证过的控制应用路径”落地后才会与节点状态对齐。
4. **`Unknown` 是保守取舍**：网络/超时/5xx 一律 `Unknown`（绝不重发）。若节点从未收到，对账只会一直返回 `Unknown`，草稿会停在对账窗口，需要上层（CORE-008 retry / CORE-009 cancel）介入；节点没有“从未收到”的正式回答。
5. **`query` 是线性扫描**：节点没有按 SubmissionId 的查询端点，只能按页扫日志（`DEFAULT_QUERY_PAGE_LIMIT = 50`，上限 500）；大群 O(n) 往返。
6. **回执不落库**：`Accepted` 的 seq/log_hash 只出现在 `SubmitOutcome.detail` 与诊断用的 `last_receipt()` 里，orchestrator 目前只落接受状态，不保存正式回执/序号；VerifiedAcceptedReceipt 的持久化留待 SYNC-009 接线。
7. **无速率限制/退避**：API-005 未实现，限流映射为 `Unknown`，重试节奏完全由上层 worker 决定。
8. **线程模型**：每个实例一条桥线程 + 一个 current-thread 运行时；同进程多群多实例时需要上层复用（线程池化）而不是无界创建。
9. **节点信任**：只有 `NodeEndpoint::with_node_public_key` 固定公钥时才会验证挑战与回执签名；本测试按固定公钥运行，未固定的调用方只能依赖 node_id 绑定。

## 发送控制实现记录（CORE-008 / CORE-009 + outbox 对账，2026-10-18，Windows x64）

实现位置：[crates/core/src/send_orchestrator.rs](../../crates/core/src/send_orchestrator.rs)（在既有 `begin_send`/`prepare`/`dispatch` 上**只加方法**，不改既有公开签名），测试：[crates/core/tests/send_orchestrator.rs](../../crates/core/tests/send_orchestrator.rs)（新增 5 个用例，文件内共 14 个）。决策仍然只来自 `chat_sync::send::plan_submission`（SYNC-003/004）与 `chat_domain::classify_retry`（DOMAIN-010），本层不新增状态机。

新增公开面：

~~~text
pub enum CancelOutcome {
    CancelRequested { draft_id: String, submission_id: Option<String> },
    CancelTooLate   { draft_id: String, submission_id: String },
    CancelUnknown   { draft_id: String, submission_id: String },
}
pub struct ReconcileReport {
    pub submission_id: Option<String>,
    pub draft_repaired: bool,
    pub awaiting_reconciliation: bool,
}
impl ReconcileReport { pub fn is_consistent(&self) -> bool; }

impl SendOrchestrator {
    pub fn retry_draft(&mut self, draft_id: &str)  -> SendResult<PrepareOutcome>;   // CORE-008
    pub fn cancel_draft(&mut self, draft_id: &str) -> SendResult<CancelOutcome>;    // CORE-009
    pub fn reconcile(&mut self, draft_id: &str)    -> SendResult<ReconcileReport>;  // outbox 结果对账
}
~~~

CORE-008 `retry_draft` 语义（复用 `prepare` 的加密/持久化路径）：

- 已正式接受 → `PrepareOutcome::AlreadyAccepted`（幂等；不再提交、不再加密）；
- 结果未知 → `AwaitingReconciliation`（**只报告必须对账**：不加密、不提交、不改状态；真正的查询由 `dispatch`/`reconcile` 执行）；
- 已取消/终止 → 显式 `INVALID_STATE_TRANSITION`（`RETRY_NOT_ALLOWED` 的冻结码投影）；
- 不可重试的权限类失败：会话 `left/dissolved/paused` 时用 DOMAIN-010 `classify_retry(PERMISSION_DENIED, Prepared)` 判为 `WaitForUser` → 返回 `PERMISSION_DENIED`；被篡改的草稿正文由存储解密直接 `STATE_CORRUPTED`；
- 其余（新草稿 / 可重试失败 / 租约过期 / 被拒绝）→ 重新走 `prepare`：`Resubmit` 复用同 SubmissionId 同密文（不重新加密）、`Rebuild` 用 `attempt_index + 1` 得到新 SubmissionId 并真实再加密一次（受 `MAX_ATTEMPT_INDEX = 64` 上限）、`Prepare` 首次加密、租约有效则 `WaitingForLease`。

CORE-009 `cancel_draft` 三态：

- 已接受 → `CancelTooLate`：不撤回、不删除消息，草稿与 attempt 保持 `accepted`，`cancel_requested` 不置位；
- 结果未知 → `CancelUnknown`：只把 `drafts.state` 置 `cancelled`（保留用户意图），**不写** `cancel_requested`——否则 6 小时对账窗口结束后该尝试永远无法再被领取对账，等于用取消把未知结果永久搁置；
- 其余（排队 / 已加密待发 / 租约内 / 已拒绝）→ `CancelRequested`：草稿字节与 attempt 行全部保留、`drafts.state = 'cancelled'`、prepare 与 send 任务标 `cancel_requested = 1`（STORAGE-014 的领取会跳过），之后 `dispatch` 走 `plan_submission` 的 `Abandoned` 分支，返回 `DispatchOutcome::Abandoned` 且**零提交**。

`reconcile` 语义：修复“outbox 结果已落（attempt accepted）但草稿仍 queued”的两笔事务窗口——接受是权威事实，`mark_draft_accepted` 改成幂等且可覆盖 `cancelled/failed` 的草稿状态并返回“本次是否真的补写”；结果未知只报告 `awaiting_reconciliation` 且不做任何写操作；其余状态报告一致。该路径同时是“取消与接受竞态”的收敛点。

复现命令（全程离线，本轮实测）：

~~~powershell
. .	oolsactivate.ps1
cargo test --offline -p chat-core   # 14 lib + 6 identity + 5 node_submitter + 2 p0 + 14 send_orchestrator + 5 service = 46 passed（既有 41 个继续通过）
~~~

证据（测试名 → 覆盖的契约点）：

- `send_orchestrator::retry_draft_reports_accepted_and_unknown_without_resending`：已接受两次重试都是 `AlreadyAccepted` 且提交/加密次数恒为 1；未知结果两次重试都是 `AwaitingReconciliation`，提交 1 次、查询 0 次、加密 1 次、attempt 保持 `unknown`。
- `send_orchestrator::retry_draft_rebuilds_a_rejected_attempt_and_stays_idempotent`：被拒绝后重试得到 `attempt_index = 1` 的新 SubmissionId 并再加密一次；再次重试复用同一 ID（`already_prepared = true`）不再加密；重建后的尝试可提交并被接受。
- `send_orchestrator::retry_draft_rejects_cancelled_permission_and_corrupted_drafts`：已取消 `INVALID_STATE_TRANSITION`、会话 `left` `PERMISSION_DENIED`、篡改草稿 `STATE_CORRUPTED`、未知草稿 `INVALID_TARGET`、别家租约 `WaitingForLease` 且零加密零提交。
- `send_orchestrator::cancel_draft_returns_three_states_and_blocks_dispatch`：四段场景——排队取消（`CancelRequested { submission_id: None }`、prepare 任务 `cancel_requested = 1`、prepare/dispatch 均 `Abandoned`、加密 0）；已加密待发（保留密文与 attempt、send 任务 `cancel_requested = 1`、dispatch 零提交）；已接受（`CancelTooLate`、状态不回退、`cancel_requested = 0`）；结果未知（`CancelUnknown`、不置 `cancel_requested`、随后对账为 `Accepted` 且草稿 accepted）。
- `send_orchestrator::reconcile_repairs_the_two_transaction_window_and_stays_idempotent`：一致状态报告 `is_consistent()`；把草稿强制回 `queued` 后 `reconcile` 报 `draft_repaired = true` 并补成 accepted，再次对账不再写；取消后接受竞态同样收敛为 accepted；未知结果只报 `awaiting_reconciliation` 且不改 attempt。

已知限制（本切片不覆盖）：

1. **撤销/凭据类不可重试无法从持久状态判定**：草稿不记录作者设备，本切片也没有设备撤销投影，因此 `DEVICE_REVOKED`/`INVALID_CREDENTIAL` 只能由上游端口原码上抛（`dispatch` 不吞错、不落库）；本层的“不可重试”判定只覆盖会话可见性、草稿/attempt 状态与存储校验（损坏）。
2. **重建不先同步**：`Rebuild` 直接用本地引擎当前 epoch 重新加密，未接 SYNC-008 `catch_up_group`；若群状态已过期，节点会再次拒绝（由上层继续重试，受 `MAX_ATTEMPT_INDEX = 64` 限制）。
3. **取消不释放附件**：ATTACHMENT 模块未接线，本轮只保留草稿与 attempt 字节；已接受消息永不删除。
4. **`retry_draft` 的未知分支不主动查询**：它只报告 `AwaitingReconciliation`，避免在“重试”语义里偷偷产生网络调用；对账仍由 `dispatch`/`reconcile` 完成（`node_submitter` 记录的限制 4 即此入口）。
5. **`cancel_requested` 仍是原始 SQL 投影**：storage 没有“标记取消”的正式入口（与 STORAGE-014 的 `cancel_requested` 语义一致），本层在 outbox 事务内直接更新；storage 补 API 后应替换。
6. **两笔事务窗口只能事后收敛**：`Accepted` 仍是“outbox 结果 + 草稿状态”两笔事务，崩溃窗口内的不一致必须由 `reconcile` 或下一次 `dispatch` 的 `Release` 分支修复，本轮无后台恢复任务。

## 草稿状态与事件流实现记录（CORE-010 / CORE-003，2026-10-18，Windows x64）

实现位置：[crates/core/src/core_events.rs](../../crates/core/src/core_events.rs)（新增，`lib.rs` 加一行 `pub mod core_events;`）+ [crates/core/src/service.rs](../../crates/core/src/service.rs)（`DraftStatusView` 与共用投影 `draft_status_view`、`ConversationCore::get_draft_status`），测试：[crates/core/tests/draft_status_events.rs](../../crates/core/tests/draft_status_events.rs)（5 个用例）。**没有改** `send_orchestrator.rs` 的编排逻辑：事件在 service 层包装（[`SendService`]）里发布。

公开面：

~~~text
// CORE-010（service.rs）
pub struct DraftStatusView {
    pub draft_id: String, pub conversation_id: String, pub operation_id: String,
    pub status: String,                 // queued/prepared/submitted/rejected/unknown/accepted/cancelled/failed
    pub submission_id: Option<String>,  // 最新尝试
    pub attempt_index: Option<u32>,     // 0 = 首次尝试；无 attempt 为 None
    pub attempts: u32,                  // 已尝试次数（含被拒绝的重建）
    pub updated_at_ms: u64,             // 可观测最后活动时间（非伪造写入时间）
    pub can_retry: bool, pub can_cancel: bool,
}
pub fn draft_status_view(store: &Store, draft_id: &str) -> Result<Option<DraftStatusView>, StorageError>;
impl ConversationCore { pub fn get_draft_status(&self, draft_id: &str) -> anyhow::Result<Option<DraftStatusView>>; }

// CORE-003（core_events.rs）
pub enum CoreEvent {
    DraftChanged { draft_id, conversation_id, status, submission_id: Option<String> },
    ConversationChanged { conversation_id },
    ResyncRequired { reason: String },
    CoreClosed,
}
pub struct EventBatch { pub events: Vec<CoreEvent>, pub next_seq: u64, pub resync_required: bool }
pub struct EventBus { .. }  // new(capacity 1..=4096) / push / poll(since_seq, limit 1..=256) / latest_seq / len
pub struct SendService<S: NodeSubmitter, C: CryptoEngine, K: Clock> { .. }
impl SendService {
    pub fn new(store, submitter, engine, clock, worker) -> anyhow::Result<Self>;
    pub fn with_event_capacity(.., event_capacity) -> anyhow::Result<Self>;
    pub fn begin_send/prepare/dispatch/retry_draft/cancel_draft/reconcile(..)  // 转发编排器并发布事件
    pub fn get_draft_status(&self, draft_id: &str) -> anyhow::Result<Option<DraftStatusView>>;
    pub fn poll_events(&self, since_seq: u64, limit: u32) -> anyhow::Result<EventBatch>;
    pub fn shutdown(&mut self)   // 释放存储句柄并发布 CoreClosed（重复调用幂等）
}
~~~

事件语义（CORE-003 的最小实现，拉取式）：

- **只通知变化**：每次写调用（`begin_send`/`prepare`/`dispatch`/`retry_draft`/`cancel_draft`/`reconcile`）成功后才查询真实状态，只有**有效状态变化**才发布 `DraftChanged` + `ConversationChanged`；幂等重放（状态没变）不产生事件。
- **单调序号**：序号从 1 单调递增，`poll_events(since_seq, limit)` 返回 `since_seq` 之后的增量，`next_seq` 是本批最后一条的序号；`since_seq = 0` 表示从头。
- **重复 poll 幂等**：poll 只读，同参数重复调用返回同一批；连续两批序号严格递增、无重复无空档。
- **溢出显式 resync**：有界环形缓冲（默认 256，上限 4096）；游标早于缓冲起点（事件已丢）或超前于本实例时返回 `resync_required = true` 与一条 `ResyncRequired`，绝不静默丢事件；仍在窗口内的游标可以继续增量读。
- **不含敏感材料**：`CoreEvent` 只有 ID 与稳定状态文本，没有正文/密钥/指纹；测试用正文标记反证 `Debug` 输出里不出现正文。
- **关闭**：`shutdown` 丢弃编排器（释放独占存储句柄）后追加一条 `CoreClosed`，之后所有方法显式 `CORE_CLOSED`；重复 `shutdown` 不重复发事件。

错误码：`service::error_code` 新增两条投影——`SyncError`（发送编排/调度决策的冻结码）与 `core_events::EventError`（`INVALID_EVENT_CAPACITY`/`INVALID_EVENT_LIMIT`），因此 `SendService` 的错误在 FFI 层能保持精确码而不是退化成 `CORE_FAILURE`。

**跨 crate 联动（例外改动，已获 Lead 批准）**：`crates/storage/src/formal/error.rs` 在本轮期间由 storage 侧（R-23 MLS epoch CAS）新增了 `StorageError::StaleEpoch { stored, offered }`，导致 `send_orchestrator.rs` 的 `storage_failure` 穷尽 match 断裂。为恢复编译，该文件只增加一行 `StorageError::StaleEpoch { .. } => ErrorCode::StaleEpoch`（PROTOCOL-014 有 `STALE_EPOCH`，DOMAIN-010 归类为“先同步再重建”），其余逻辑未动；按 Lead 裁决保持穷尽 match（不改 catch-all），未来由 storage 侧集中提供 `StorageError::code()` 映射解决。

复现命令（全程离线，本轮实测）：

~~~powershell
. .	oolsactivate.ps1
cargo test --offline -p chat-core   # 14 lib + 5 draft_status_events + 6 identity + 5 node_submitter + 2 p0 + 14 send_orchestrator + 5 service = 51 passed（既有 46 个继续通过）
cargo build --locked --offline -p chat-core   # 成功，Cargo.lock 未变化
~~~

证据（测试名 → 覆盖的契约点）：

- `draft_status_events::draft_status_tracks_every_lifecycle_step`：queued（无 attempt、可重试可取消）→ prepared（`attempt_index = 0`、新 SubmissionId）→ accepted（不可重试不可取消、提交 1 次）；unknown（不可重试、可发起取消）；rejected（可重试、重建后 `attempt_index = 1`/`attempts = 2` 且新 SubmissionId）；cancelled（都不可）；未知草稿 `None`、畸形 ID `INVALID_INPUT`；`ConversationCore` 与 `SendService` 用同一投影（同库第二个句柄返回 `STORAGE_LOCKED`，不静默成功）。
- `draft_status_events::events_are_monotonic_and_repeat_polling_is_idempotent`：空 poll（`next_seq = 0`）→ begin_send 2 条事件（`DraftChanged` + `ConversationChanged`）→ 同参数重复 poll 完全一致 → prepare/dispatch 各 2 条且状态正确 → 幂等重放不再产生事件 → 分页拼接得到序号 `1..=6` 严格单调。
- `draft_status_events::overflow_returns_resync_required_instead_of_dropping_silently`：容量 4 时第 5/6 条事件挤掉最早的 1/2 条 → 游标 0 返回 `resync_required = true` 与仅一条 `ResyncRequired`（`next_seq = 6`）；游标 4 仍在窗口内可增量读到 accepted 事件；超前游标 99 同样要求 resync；`limit = 0`/超上限 → `INVALID_EVENT_LIMIT`，容量越界 → `INVALID_EVENT_CAPACITY`。
- `draft_status_events::events_never_carry_message_content_or_keys`：事件 `Debug` 输出不含正文标记，且 `DraftChanged` 只有 ID/状态字段。
- `draft_status_events::shutdown_publishes_core_closed_and_rejects_later_calls`：shutdown 后追加一条 `CoreClosed`，`get_draft_status`/`begin_send` 返回 `CORE_CLOSED`，重复 shutdown 不再发事件。

已知限制（本切片不覆盖）：

1. **拉取式而非回调式**：没有 `EventListener`/`SubscriptionHandle`（BRIDGE-003/004 的事件桥接任务），因此“release 后无回调”的验收未勾选；UI 侧用 `poll_events` + `ResyncRequired` 全局重查（PRESENTATION-008）即可。
2. **单缓冲，无每订阅队列**：所有消费者共享一个环形缓冲；一个慢消费者不会拖慢别人，但它自己会收到 `ResyncRequired`。多订阅隔离要等桥接层。
3. **事件不是可靠日志**：进程重启后缓冲从 0 开始（`next_seq` 归零），跨进程游标会被判为“超前”并触发 resync；调用方不得把事件当消息数据。
4. **事件覆盖范围**：本轮只有发送草稿路径（`SendService`）发布事件；`apply_verified_entry`/群/身份路径尚未发 `ConversationChanged`（需要把它们也搬到 service 包装层）。
5. **`updated_at_ms` 不是写入时间**：schema 没有 `drafts.updated_at`，取 `drafts.created_at` 与发送任务 `retry_at`/`lease_until` 的最大值；attempt 行没有时间戳。
6. **状态查询不解密正文**：`drafts.protected_content` 被篡改时状态查询仍返回状态（只看 `state` 列），正文损坏只在 `draft_status`（body 投影）与发送路径上显式 `STATE_CORRUPTED`。

## ConversationCore 层取消与事件接线（task-21，2026-10-18，Windows x64）

目标：把 CORE-009 取消与 CORE-003 事件流接到 **`ConversationCore` 本身**，让 FFI 只做薄封装；
FFI 未接真实节点/MLS 时用桩端口**显式失败**，绝不伪造成功。

实现位置：[crates/core/src/service.rs](../../crates/core/src/service.rs)（`ConversationCore` 字段/方法 +
`StubSubmitter`/`StubCrypto`）；两个**纯 getter**（不改编排逻辑）加在
[core_events.rs](../../crates/core/src/core_events.rs) 与 [send_orchestrator.rs](../../crates/core/src/send_orchestrator.rs)：
`SendService::into_store(self) -> Option<Store>`、`SendOrchestrator::into_store(self) -> Store`。

公开面（增量，既有方法签名不变）：

~~~text
pub struct ConversationCore { database_path, store: Mutex<Option<Store>>, events: Mutex<EventBus> }

impl ConversationCore {
    pub fn cancel_draft(&mut self, draft_id: &str) -> anyhow::Result<CancelOutcome>;
    pub fn poll_events(&self, since_seq: u64, limit: u32) -> anyhow::Result<EventBatch>;
    pub fn close(&self);  // 仍在打开 → 关闭时追加一条 CoreClosed（幂等）
}

pub struct StubSubmitter;  // submit/query -> NETWORK_UNAVAILABLE
pub struct StubCrypto;     // epoch/encrypt -> PROTOCOL_UNSUPPORTED
~~~

语义：

- **同一个 Store，不重开库**：`cancel_draft` 在锁内把 `Store` 从 `Mutex<Option<Store>>` 取出，
  交给 `SendService::new(store, StubSubmitter, StubCrypto, SystemClock, "ffi-cancel-worker")`；
  执行 `SendService::cancel_draft` 后通过 `into_store()` **无论成败都把句柄放回**；随后把
  `SendService` 本次产生的事件按序并入 `self.events`。构造 `SendService` 失败是唯一无法
  放回句柄的路径（句柄已被消费），此时对象表现为关闭并原样返回错误。
- **三态不重写**：`CancelRequested`/`CancelTooLate`/`CancelUnknown` 完全由 `SendService` 决定；
  `ConversationCore` 不猜测、不做乐观删除。
- **事件**：`poll_events` 委托 `EventBus`（有界、序号从 1 单调、重复 poll 幂等、溢出/超前
  游标返回 `resync_required = true` 与一条 `resync_required` 事件）；事件只含 ID 与稳定
  状态文本，不含正文/密钥。`close()` 只在“真正从打开变关闭”的那一次追加 `CoreClosed`，
  关闭后事件仍可读（事件总线不依赖存储句柄）。
- **桩端口不伪造成功**：FFI 未接真实节点与 MLS 群时，`StubSubmitter` 的 submit/query 返回
  `NETWORK_UNAVAILABLE`，`StubCrypto` 的 epoch/encrypt 返回 `PROTOCOL_UNSUPPORTED`；
  因此 retry/dispatch 在未接线时必然显式失败（bridge.md 同步说明）。

复现命令（全程离线，本轮实测）：

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test cancel_events   # 7 passed
cargo test --offline -p chat-core                        # lib 15（含 stub_ports）+ 既有 51 个用例全绿（合计 59）
~~~

证据（测试名 → 契约点）：

- `cancel_events::cancel_queued_draft_requests_and_publishes_events`：queued → `CancelRequested`（无 SubmissionId）、状态 `cancelled`、`can_retry/can_cancel = false`；事件为 `draft_changed(cancelled)` + `conversation_changed`、序号 2、重复 poll 幂等、游标之后为空、事件不含正文标记。
- `cancel_events::cancel_accepted_draft_is_too_late`：attempt accepted → `CancelTooLate` 且状态仍 `accepted`。
- `cancel_events::cancel_unknown_draft_reports_unknown`：attempt unknown → `CancelUnknown` 并带 SubmissionId。
- `cancel_events::cancel_failure_keeps_the_store_usable`：未知草稿与畸形 ID 显式失败后，同一个 Store 仍可 `create_draft`/查询/取消。
- `cancel_events::event_bus_overflow_requires_resync`：129 次取消（258 条事件 > 容量 256）后游标 0 返回 resync，最新游标可继续增量读，超前游标同样 resync。
- `cancel_events::poll_events_rejects_invalid_limit`：limit 0 与 `MAX_EVENT_BATCH + 1` → `INVALID_EVENT_LIMIT`。
- `cancel_events::close_publishes_core_closed_exactly_once`：close 追加一条 `CoreClosed`，重复 close 不重复。
- `service::stub_ports::stub_ports_fail_explicitly_never_fake_success`：桩 submit/query → `NETWORK_UNAVAILABLE`，桩 epoch/encrypt → `PROTOCOL_UNSUPPORTED`。

已知限制：

1. **事件覆盖范围**：`ConversationCore` 自身只有取消路径发布 `DraftChanged`/`ConversationChanged`；`create_draft`/`apply_verified_entry` 仍不发布事件（要与 `SendService` 的事件合并，需要把写入路径整体搬到事件包装层，留给后续任务）。
2. **没有 FFI 的 dispatch/retry**：本轮只加 `cancel_draft`/`poll_events`；桩端口用于`ConversationCore` 内部取回 Store 时的包装层，FFI 不暴露 dispatch/retry，因此“取消后 dispatch”以“状态 cancelled + can_retry/can_cancel=false + 桩端口显式失败”证明不可发送。
3. `cancel_draft` 的 Store 交接要求 `&mut self`（与 `create_draft` 一致）；FFI 的 `Mutex` 串行化保证不会并发交接。
4. 事件缓冲是内存态：进程重启后序号从 0 开始（跨进程游标会被判“超前”而 resync）；事件不是可靠日志。

## 附件上传/下载在 core 层的接线（ATTACHMENTS-003/005，2026-10-18，Windows x64）

附件编排在 [crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)，详细记录见 [attachments.md 的上传/下载接块传输端口实现记录](attachments.md)。core 层只做两件事，不引入节点依赖：

- `PersistedAttachmentStore::upload_attachment(attachment_id, &mut transport)`：按 storage 的 `mark_attachment_state` 白名单迁移 `imported → uploading → available`，从本地加密缓存按序上传分块**密文**，节点 complete 后才标 available；已 available 时幂等无操作；中断保留缓存与进程内进度可重试。
- `PersistedAttachmentStore::download_attachment(blob_id, expected_manifest, destination, &mut transport)`：先用 manifest 与本地记录比对（不一致在任何网络读取前 `ATTACHMENT_CORRUPTED`），逐块 `read_chunk` → CRYPTO-012 认证解密 → 临时文件 → 校验密文摘要 + 明文总长 + 明文 SHA-256 后 rename；失败删临时文件、状态留在 `downloading` 可重试；没有进程内 FileKey 时 `ATTACHMENT_KEY_UNAVAILABLE` 且零读取。
- 节点交互抽象为 `BlobTransport` 端口（begin_upload / put_chunk / complete / read_chunk），真实节点适配是 [crates/core/src/attachment_transport.rs](../../crates/core/src/attachment_transport.rs) 的 `NodeBlobTransport`（NODE-012~015，真 axum 节点实测）；端口失败用 `AttachmentError::Transport { code, detail }` 保留冻结码（节点码优先，其次 PROTOCOL-014 码，如 `NODE_UNAVAILABLE` / `NETWORK_UNAVAILABLE` / `QUOTA_EXCEEDED` / `MANIFEST_MISMATCH`）。

`NodeBlobTransport` 的三条关键约定（详见 [attachments.md 的节点块传输适配实现记录](attachments.md)）：

- 标识映射：本地附件 ID → 节点 blob `obj:1:sha256(域 || attachment_id)`；上传 operation → `op:1:sha256(域 || attachment_id || ciphertext_digest)`（重试复用同一会话，节点按 (设备, operation) 幂等）。
- 声明块大小：`chunk_count > 1` → `chunk_bytes = chunk_size + 29`（封装开销）；单块 → `chunk_bytes = ciphertext_bytes`；发送前先校验 `ciphertext_bytes == plain_size + chunk_count*29`。
- 错误保真：节点/网络错误一律 `AttachmentError::Transport{code, detail}`，细节只含静态说明，不含 URL/正文/密钥。

证据：`cargo test --offline -p chat-core` → 76 passed（26 lib + 7 cancel_events + 5 draft_status_events + 6 identity + 5 node_submitter + 2 p0 + 6 attachment_transport + 14 send_orchestrator + 5 service；既有 70 个继续通过）；`--test attachment_transport` 6 passed（真节点上传/下载往返、断链可重试、节点重启后仍可下载、配额/鉴权码保真、多块被节点策略拒绝）。



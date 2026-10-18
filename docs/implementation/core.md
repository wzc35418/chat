# 客户端内核与 Kotlin 业务服务映射

阶段：P1 → P2。状态：函数级设计；竖直切片（`ConversationCore` 的 open / create_draft / draft_status / apply_verified_entry / list_conversations / list_messages / close）与 P1 身份持久化增量（save_identity_account / load_identity_account / upsert_contact / list_contacts / consume_invite）已在 Windows x64 实测通过；CORE-007 的发送编排（[crates/core/src/send_orchestrator.rs](../../crates/core/src/send_orchestrator.rs)）已把「意图 → 一次真实 MLS 加密 → 提交 → Accepted/Unknown/Rejected 落库」接通（节点端口可注入，未连真实网络），其余 CORE 函数未开始。目标源码：[crates/core/src/service.rs](../../crates/core/src/service.rs) 与 [crates/core/src/send_orchestrator.rs](../../crates/core/src/send_orchestrator.rs)。

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

- [x] 同意图多尝试可追踪：`service::end_to_end_draft_then_verified_entry_then_pages`（`DraftView.submission_id` 取该草稿最近的 attempt，没有 attempt 时为 `None`；本切片未产生 attempt，接线后由 STORAGE-007 写入）。
- [ ] 节点存信箱状态独立展示：未开始（需要节点信箱与 outbox 结果投影）。

**实现状态**：切片实现。`ConversationCore::draft_status(draft_id)` 读取 STORAGE-006 的草稿投影并解密正文，附带 `submission_id`；未知草稿返回 `None`（不报错、不伪造），草稿 ID 只做本地有界结构校验（storage 派生的 `draft-<32 hex>` 不是 PROTOCOL-001 文本）。证据：`service::end_to_end_draft_then_verified_entry_then_pages`。

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


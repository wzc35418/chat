# FFI、回调、Flow 包装与资源生命周期

阶段：P0 → P2。状态：函数级设计；BRIDGE-001/002/003/004/005 的通用编排尚未实现，两个竖直切片的 UniFFI 对象已在 Windows x64 编译并实测：本地存储的 `LocalStoreFfi`（见“本地存储竖直切片（LocalStoreFfi）”）与身份/口令加好友的 `IdentityFfi`（见“身份与口令加好友竖直切片（IdentityFfi）”，含持久身份 `open_persisted`）。目标源码：[crates/ffi/src/bridge.rs](../../crates/ffi/src/bridge.rs)（P0 临时实验对象）、[crates/ffi/src/local_store.rs](../../crates/ffi/src/local_store.rs)（正式存储对象）与 [crates/ffi/src/identity_ffi.rs](../../crates/ffi/src/identity_ffi.rs)（身份/联系人对象）。

真实生成绑定必须来自选定框架，P0 前不伪造 ABI。Kotlin 包装代码目标位于 shared/core-bridge/src/jvmMain 及 Android 对应 source set；Flow 只负责 UI 通知。Rust 原生方法不导出 Kotlin coroutine 或 Flow；秘密不作为普通 UI DTO。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [BRIDGE-001 ffi_create_core](bridge.md#bridge-001)
- [BRIDGE-002 ffi_dispatch](bridge.md#bridge-002)
- [BRIDGE-003 ffi_subscribe](bridge.md#bridge-003)
- [BRIDGE-004 release_ffi_subscription](bridge.md#bridge-004)
- [BRIDGE-005 stop_event_bridge](bridge.md#bridge-005)
- [BRIDGE-006 bridge_event_flow](bridge.md#bridge-006)
- [BRIDGE-007 bridge_suspend_call](bridge.md#bridge-007)

<a id="bridge-001"></a>

## BRIDGE-001 · ffi_create_core

~~~text
ffi_create_core(options: FfiOptions, platform: PlatformBridge) -> Result<FfiCoreHandle>
~~~

**职责与输出**：建立有生命周期计数的原生 core 对象。

**输入与前置条件**：绑定框架和 panic 规则已 P0 验证；平台回调注册完整。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验版本/ABI。
2. 映射 options。
3. 调用 start。
4. 登记受控句柄。
5. 返回简单 DTO 和初始化状态。

**持久化、并发与副作用**：句柄引用计数；创建失败释放 callback；不返回裸数据库指针。

**失败处理**：ABI_MISMATCH，STORAGE_LOCKED，CORE_START_FAILED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-001 start_core](core.md#core-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] Windows/Linux/Android 真机创建/释放。
- [ ] 错误回调不泄漏句柄。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="bridge-002"></a>

## BRIDGE-002 · ffi_dispatch

~~~text
ffi_dispatch(handle: FfiCoreHandle, request: FfiRequest) -> Result<FfiResponse>
~~~

**职责与输出**：将有界业务请求映射到内核，不暴露协议内部对象。

**输入与前置条件**：handle 有效，类型/字节/超时受限。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证句柄。
2. 解析生成 DTO。
3. 进入异步任务。
4. 调用对应 core 服务。
5. 脱敏映射错误。
6. 返回稳定 DTO。

**持久化、并发与副作用**：不能持 FFI 锁等待 UI 回调；操作成功以持久化为准。

**失败处理**：CORE_CLOSED，INVALID_FFI_ARGUMENT，OPERATION_CANCELLED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-007 send_message](core.md#core-007)、[PROTOCOL-014 map_error](protocol.md#protocol-014)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重复 send 保持 clientOperationId。
- [ ] UI 取消后已提交意图可查。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="bridge-003"></a>

## BRIDGE-003 · ffi_subscribe

~~~text
ffi_subscribe(handle: FfiCoreHandle, callback: EventCallback) -> Result<FfiSubscription>
~~~

**职责与输出**：建立可释放事件通道。

**输入与前置条件**：回调跨线程规则已核实，UI 主线程由 Kotlin 自行切换。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 注册有界订阅。
2. 原生线程只投递 DTO。
3. 检测回调失败。
4. 溢出标 resync。
5. 保存订阅生命周期。

**持久化、并发与副作用**：引用与 core 关联；关闭后禁止回调已释放对象。

**失败处理**：CALLBACK_FAILED，CORE_CLOSED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-003 observe_events](core.md#core-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 快速订阅/释放竞态无 use-after-free。
- [ ] 慢消费者有 resync。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="bridge-004"></a>

## BRIDGE-004 · release_ffi_subscription

~~~text
release_ffi_subscription(subscription: FfiSubscription) -> Result<()>
~~~

**职责与输出**：幂等解除回调与待发送队列。

**输入与前置条件**：允许重复释放，不能从 callback 内死锁等待自己。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 标 closed。
2. 停止新回调。
3. 处理在途回调计数。
4. 丢弃临时通知。
5. 释放引用。

**持久化、并发与副作用**：不删除持久消息；从 callback 重入时采用延迟清理。

**失败处理**：INVALID_HANDLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] Compose 页面反复打开/关闭无回调增长。
- [ ] release 后无新事件。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="bridge-005"></a>

## BRIDGE-005 · stop_event_bridge

~~~text
stop_event_bridge(core: FfiCoreHandle) -> Result<()>
~~~

**职责与输出**：关闭 core 的全部订阅并等待有界完成。

**输入与前置条件**：core 已 draining；不能无限等恶意/挂起回调。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 快照订阅集合。
2. 逐个取消。
3. 截止时间内 drain。
4. 作废回调引用。
5. 报告未配合回调。

**持久化、并发与副作用**：不回滚 core 已提交事务；平台线程不能被原生 join 死锁。

**失败处理**：CALLBACK_SHUTDOWN_TIMEOUT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[BRIDGE-004 release_ffi_subscription](bridge.md#bridge-004)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 关闭时 callback 触发查询得到 CLOSED 而非死锁。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="bridge-006"></a>

## BRIDGE-006 · bridge_event_flow

~~~text
bridge_event_flow(native: NativeCore) -> Flow<CoreEvent>
~~~

**职责与输出**：Kotlin callbackFlow 包装与 awaitClose 释放。

**输入与前置条件**：选定 coroutine 版本已锁定；调用方遵守页面 scope。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 注册 FFI 回调。
2. 安全映射 DTO。
3. 投递有界 channel。
4. 溢出发 resync。
5. awaitClose 释放订阅。

**持久化、并发与副作用**：Kotlin 仅缓存 UI 状态；回调不在原生线程操作 Compose。

**失败处理**：CORE_CLOSED，INVALID_NATIVE_EVENT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[BRIDGE-003 ffi_subscribe](bridge.md#bridge-003)、[BRIDGE-004 release_ffi_subscription](bridge.md#bridge-004)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] collect 取消释放一次。
- [ ] 重收集查询持久状态补丢失通知。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="bridge-007"></a>

## BRIDGE-007 · bridge_suspend_call

~~~text
bridge_suspend_call(operation: BusinessRequest) -> BusinessResult
~~~

**职责与输出**：将 Rust async 业务调用包装为 Kotlin suspend。

**输入与前置条件**：每个变更请求保留稳定 operationId；回调/任务取消语义明确。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 切换 IO dispatcher。
2. 映射 DTO 防御复制。
3. 调用生成绑定。
4. 取消时只发取消 hint。
5. 映射稳定错误。
6. 必要时查询状态。

**持久化、并发与副作用**：取消 coroutine 不代表撤回已提交消息；原生任务有自己的所有权。

**失败处理**：OPERATION_CANCELLED，CORE_CLOSED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[BRIDGE-002 ffi_dispatch](bridge.md#bridge-002)、[CORE-010 get_draft_status](core.md#core-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 暂停页面导致取消后重开仍看到已排队草稿。
- [ ] 无 UI 阻塞。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

## Kotlin 侧接线（2026-10-18，R-10）

- `shared/core-api/.../LocalStore.kt`：`LocalStore` 接口与 `StoredDraft/StoredConversation/StoredMessage/StoredPage/VerifiedMessageInput`；`LocalStoreException(code, message)` 承载稳定错误码。
- `shared/core-bridge/.../LocalStoreFactory.kt`（commonMain expect）+ `NativeLocalStore.kt`（jvmMain actual，桌面与 Android 共用）：原生调用切到 IO 调度、互斥串行化、`FfiException.Store` → `LocalStoreException`、close 后拒绝调用并释放句柄、u64/u32 与 Kotlin Long/Int 的显式转换。
- 端到端验证：`:apps:desktop:localStoreSmoke`（Kotlin → UniFFI → core → formal store → 展示投影），真实临时库建草稿、写已验证消息、分页读回、明文不落盘、错误密钥拒绝、待发草稿投影可见。运行：`.\gradlew.bat :apps:desktop:localStoreSmoke --offline`。

## 本地存储竖直切片（LocalStoreFfi，2026-10-18，Windows x64）

实现位置：[crates/ffi/src/local_store.rs](../../crates/ffi/src/local_store.rs)（`uniffi::Object`）；P0 临时实验对象 `P0Core`（bridge.rs）保持不变，两者互不复用类型或状态。

生成绑定的公开面（UniFFI 0.32，crate `chat-ffi`）：

~~~text
#[derive(uniffi::Object)] LocalStoreFfi

#[uniffi::constructor]
LocalStoreFfi::open(database_path: String, master_key_hex: String) -> Result<Arc<LocalStoreFfi>, FfiError>

LocalStoreFfi::create_draft(conversation_id: String, operation_id: String, body: String) -> Result<DraftView, FfiError>
LocalStoreFfi::draft_status(draft_id: String) -> Result<Option<DraftView>, FfiError>
LocalStoreFfi::apply_verified_entry(
    conversation_id: String,
    title: String,
    message_id: String,
    author_account: String,
    author_device: String,
    body: String,
    mine: Boolean,
    created_at_ms: u64,
) -> Result<MessageRowView, FfiError>
LocalStoreFfi::list_conversations(cursor: Option<String>, limit: u32) -> Result<ConversationPageView, FfiError>
LocalStoreFfi::list_messages(conversation_id: String, cursor: Option<String>, limit: u32) -> Result<MessagePageView, FfiError>
LocalStoreFfi::cancel_draft(draft_id: String) -> Result<CancelViewFfi, FfiError>
LocalStoreFfi::poll_events(since_seq: u64, limit: u32) -> Result<EventBatchFfi, FfiError>
LocalStoreFfi::shutdown()         // 幂等，释放独占写锁并发布一条 core_closed 事件
// 方法名必须叫 shutdown：UniFFI 为每个对象生成 AutoCloseable.close()，
// 原生 close() 会与生成代码产生 JVM 签名冲突（与 P0Core.shutdown 的既有做法一致）。
LocalStoreFfi::is_open() -> Boolean
~~~

记录（字段名与 core 层一一对应）：

- `DraftView { draft_id, operation_id, conversation_id, body, state, submission_id: Option<String> }`。
- `ConversationView { conversation_id, title, preview, unread_count: Option<u32>, updated_at_ms: u64 }`。
- `MessageRowView { message_id, conversation_id, author, body, mine: Boolean, created_at_ms: u64 }`。
- `ConversationPageView { items: Vec<ConversationView>, next_cursor: Option<String> }`。
- `MessagePageView { items: Vec<MessageRowView>, next_cursor: Option<String> }`。
- `CancelViewFfi { draft_id: String, outcome: String, submission_id: Option<String> }`：`outcome` 只有三态 `cancel_requested` / `cancel_too_late` / `cancel_unknown`（语义由 core 的 `SendService::cancel_draft` 决定，FFI 不重写）。
- `CoreEventFfi { kind: String, draft_id: Option<String>, conversation_id: Option<String>, status: Option<String>, submission_id: Option<String>, reason: Option<String> }`：`kind` 只有 `draft_changed` / `conversation_changed` / `resync_required` / `core_closed`；**只含 ID 与稳定状态文本，绝不含正文/密钥**。
- `EventBatchFfi { events: Vec<CoreEventFfi>, next_seq: u64, resync_required: Boolean }`。

错误（Kotlin 侧 `catch (e: FfiException.Store)`）：

~~~text
#[derive(Debug, thiserror::Error, uniffi::Error)]
enum FfiError {
    #[error("{code}: {reason}")]
    Store { code: String, reason: String },
}
~~~

- `code` 是稳定错误码：`INVALID_INPUT`、`INVALID_CURSOR`、`PERMISSION_DENIED`、`MESSAGE_ID_CONFLICT`、`STATE_CORRUPTED`、`STORAGE_LOCKED`、`STORAGE_FAILURE`、`CORE_CLOSED`、`CORE_FAILED` 等；UI 只依赖它，不解析 `reason`。
- `reason` 由 core 截断到 300 字符并压平换行，不含正文、密钥或 SQL 细节。

调用约定（Kotlin 侧必须遵守）：

1. 所有 ID 参数是 PROTOCOL-001 规范文本：会话 `conv:1:<64 位小写 hex>`、消息 `msg:1:<…>`、操作 `op:1:<…>`、账户 `acct:1:<…>`、设备 `dev:1:<…>`；`draft_id` 是 `create_draft` 返回的 `draft-<32 位小写 hex>` 本地标识，必须原样回传。
2. 时间戳在 FFI 边界是 `u64`（UniFFI 生成的 Kotlin 类型是 `ULong`）；需要 `Long` 时用 `.toLong()`；越界值显式报 `INVALID_INPUT`，不静默截断。
3. `next_cursor` 原样回传；它是 storage 内部 `spg1` 令牌（10 分钟 TTL），不是线协议 `cur1`，不要解释、不要跨设备复用；过期/伪造返回 `INVALID_CURSOR`。
4. `close` 幂等且立即释放独占写锁（同进程内第二次 `open` 在关闭前返回 `STORAGE_LOCKED`）；关闭后所有方法返回 `CORE_CLOSED`。
5. 记录 `Debug` 不含正文之外的密钥材料，正文只出现在返回值里；日志不得打印 `body`/`preview`。
6. **取消**（`cancel_draft`）只做薄封装：三态、落库与事件全部由 core 的 `ConversationCore::cancel_draft` 决定（同一个 `Store` 句柄，不重开库）。FFI 未接真实节点与 MLS 群时**不伪造成功**：取消只记录用户意图；后续 retry/dispatch 在桩端口上显式失败（`NETWORK_UNAVAILABLE` / `PROTOCOL_UNSUPPORTED`）。FFI 目前**不暴露** dispatch/retry，因此“取消后还能不能发”以 `draft_status_view` 的 `status = "cancelled"`、`can_retry = false`、`can_cancel = false` 判定。
7. **事件**（`poll_events`）来自 `ConversationCore` 自有的有界 `EventBus`（`Mutex<EventBus>`，随 `open` 初始化）：`since_seq = 0` 表示从头；`next_seq` 原样回传作为下一次游标；重复调用同参数**幂等**；游标早于缓冲起点或超前于本实例时返回 `resync_required = true` 与一条 `resync_required` 事件，调用方必须整体重查；`limit` 必须 `1..=256`（`INVALID_EVENT_LIMIT`）。事件不是可靠消息日志，收到后必须重查真实状态。
8. `shutdown` 在真正从打开变关闭时追加一条 `core_closed` 事件（重复 shutdown 不重复）；关闭后事件仍可由 `poll_events` 读取，但业务方法返回 `CORE_CLOSED`。

复现命令（全程离线，本轮实测）：

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi      # 20 passed（LocalStoreFfi 5 个：端到端、错误码映射、取消+事件、失败后可用+limit 边界的稳定码、shutdown 发布 core_closed；其余 15 个 IdentityFfi 不变）
cargo test --offline -p chat-core --test cancel_events   # 7 passed（取消三态/事件/溢出/关闭，core 层）
cargo build --locked --offline -p chat-ffi   # 成功，Cargo.lock 未变化
~~~

证据：

- `local_store::tests::open_write_and_read_through_the_ffi_shape`：构造函数建库 → create_draft → draft_status → apply_verified_entry → list_messages（待发 + 正式）→ list_conversations（标题/预览/未读 None）→ close 后 `CORE_CLOSED`。
- `local_store::tests::failures_are_typed_with_stable_codes`：非规范会话 ID 与 `u64::MAX` 时间戳都返回 `FfiError::Store { code = "INVALID_INPUT", .. }`。
- `local_store::tests::cancel_draft_and_poll_events_through_the_ffi_shape`（task-21）：`cancel_draft` → `cancel_requested`；状态 `cancelled` 且 `can_retry/can_cancel = false`；`poll_events` 返回 `draft_changed(cancelled)` + `conversation_changed`（`next_seq = 2`），重复 poll 幂等、游标之后为空、事件里不出现正文标记。
- `local_store::tests::failed_cancel_and_invalid_limit_surface_stable_codes`：未知/畸形草稿与 `limit = 0 / MAX_EVENT_BATCH + 1` 分别返回稳定码（`INVALID_INPUT` / `INVALID_EVENT_LIMIT`），失败后同一个 Store 仍可正常写入与取消。
- `local_store::tests::shutdown_publishes_core_closed_and_events_stay_readable`：shutdown 追加一条 `core_closed`（重复 shutdown 不重复），关闭后 `poll_events` 仍可读，业务方法返回 `CORE_CLOSED`。

已知限制：本对象只覆盖竖直切片，**事件是拉取式 `poll_events`（没有 `EventListener`/`SubscriptionHandle`，即 BRIDGE-003/004/006 的回调式事件桥接仍未实现）**、没有异步/suspend 包装（BRIDGE-007）、没有平台回调（BRIDGE-001 的完整形态）；事件仍只覆盖“取消”产生的 `draft_changed/conversation_changed`、`shutdown` 的 `core_closed` 与溢出 `resync_required`，`create_draft`/`apply_verified_entry` 尚不发事件（core.md 有记录）。FFI **不暴露** dispatch/retry：桩端口（`StubSubmitter`/`StubCrypto`）只在 core 内部取回 Store 的取消包装层使用，直接调用会返回 `NETWORK_UNAVAILABLE`/`PROTOCOL_UNSUPPORTED`，未接真实节点与 MLS 群时不存在“假成功”路径。`#[uniffi::Object]` 内部的 `std::sync::Mutex` 会串行化所有调用，调用方不要在 UI 线程做长分页循环。Kotlin/Android 真机尚未实测（本轮只验证 Rust 侧编译与单元测试）。

## 身份与口令加好友竖直切片（IdentityFfi，Windows x64）

实现位置：[crates/ffi/src/identity_ffi.rs](../../crates/ffi/src/identity_ffi.rs)（`uniffi::Object`）。
两种构造共用同一公开面：进程内身份 `create`（不写库）与持久身份 `open_persisted`
（打开 [chat-core](../../crates/core/src/service.rs) 的 `ConversationCore`，与 `LocalStoreFfi` 同一个库）。
与 `P0Core`、`LocalStoreFfi` 互不共享状态，但**复用同一个** `FfiError`（定义在
[local_store.rs](../../crates/ffi/src/local_store.rs)，UniFFI 允许同一 error 类型服务多个对象），
因此 Kotlin 侧仍然只 `catch (e: FfiException.Store)` 一处。方法名用 `shutdown`（不是 `close`），
避免与生成代码的 `AutoCloseable.close()` 冲突。

生成绑定的公开面（UniFFI 0.32，crate `chat-ffi`；与 Kotlin 侧逐字对接的形状）：

~~~text
#[derive(uniffi::Object)] IdentityFfi

#[uniffi::constructor]
IdentityFfi::create(display_name: String, node: String) -> Result<Arc<IdentityFfi>, FfiError>

#[uniffi::constructor]
IdentityFfi::open_persisted(
    database_path: String,
    master_key_hex: String,
    node: String,
) -> Result<Arc<IdentityFfi>, FfiError>

IdentityFfi::account() -> AccountView
IdentityFfi::create_invite(ttl_ms: u64) -> Result<InviteView, FfiError>
IdentityFfi::preview_invite(token: String) -> Result<InviteSummary, FfiError>
IdentityFfi::accept_invite(token: String, display_name: String) -> Result<ContactView, FfiError>
IdentityFfi::contacts() -> Vec<ContactView>
IdentityFfi::confirm_identity_change(contact_id: String) -> Result<ContactView, FfiError>
IdentityFfi::shutdown()
~~~

记录：

- `AccountView { account_id, device_id, display_name, fingerprint }`。
- `InviteView { token, invite_id, expires_at_ms: u64, account_id, device_id, fingerprint }`。
- `InviteSummary { invite_id, account_id, device_id, fingerprint, node, expires_at_ms: u64 }`。
- `ContactView { contact_id, account_id, display_name, fingerprint, state, can_send }`，
  其中 `state` 是 identity 状态机的稳定名 `requested` | `accepted` | `declined` | `blocked` | `identity_changed`。

字段语义（Kotlin 侧不要自行重新解释）：

1. `node` 参数是 PROTOCOL-001 规范文本 `node:1:<64 位小写 hex>`（identity 的邀请正文绑定
   `CoordinatorId`，因此不接受昵称式字符串；非法文本返回 `INVALID_ID`）。**同一对用户必须使用
   同一个 node 字符串**：`accept_invite` 要求口令绑定的节点与本对象创建时传入的节点一致。
2. `fingerprint` 是 IDENTITY-007 的分组展示形态：16 组 4 位大写 hex、单空格分隔（79 字符）。
   只用于人工核对，不参与任何密码学判定。
3. `invite_id` 是口令正文中 16 字节 nonce 的小写 hex（32 字符），本地去重/展示用；
   `expires_at_ms` 与 `invite_id` 都来自**刚生成口令的验签回读**，不是本地推算值。
4. `contact_id` 当前等于对端 `account_id`（v1 口令不携带独立请求 ID），必须原样回传给
   `confirm_identity_change`。
5. `display_name` 在 `ContactView` 里是**本地备注**：`inv1` 口令正文不携带对方昵称
   （identity 的冻结布局只有账户/设备/节点/作用域/时间/nonce），所以对端昵称只能由 UI
   在调用 `accept_invite` 时给出；传空串表示「未知」，UI 自行回退展示账户 ID 缩写，且不会
   清空已有备注。非空值按显示名规则限制 1..=64 字节、无控制字符（超界 `INVALID_TARGET`）。
6. `can_send` 只有在 `accepted` 且观测指纹等于固定信任锚时为 true；接受口令后只产生
   `requested`（`can_send == false`），与 [界面契约](../design/CONTACT_INVITE_DESKTOP.md) 一致：
   输入区只在 `accepted` 时启用。

### 持久身份（open_persisted）与联系人落库

设计（IDENTITY-001/002/006 的持久化部分，复用 core 的既有服务）：

1. `open_persisted(database_path, master_key_hex, node)` 内部调用
   `ConversationCore::open`（单写者；数据目录必须已存在）。
2. `open_persisted_identity()` 有记录 → 用从**受保护种子**重建的 `LocalAccount` /
   `DeviceKeys` / `device_id`（不重新生成账户）。
3. 没有记录 → `rand` 生成 32 字节账户种子与设备种子，经
   `save_identity_account` 落库（种子进入受主密钥保护的存储）；种子字节用完即清零。
4. 回读时以**库为准**：`open_persisted_identity()` + `load_identity_account()` 的投影
   必须与种子派生的 ID/设备/指纹/显示名完全一致，否则 `STATE_CORRUPTED`
   （绝不降级成「新身份」、绝不重建空库）。
5. `list_contacts()` 载入联系人表，逐行恢复 identity 的冻结状态机
   （`requested` / `accepted` / `declined` / `blocked` / `identity_changed`）。
6. `accept_invite` / `confirm_identity_change` 先 `upsert_contact` 落库、成功后才提交
   内存副本（以持久化为准，写失败不会留下「内存成功、库里没有」的状态）。
7. `shutdown` 释放密钥并 `core.close()`（幂等），单写者锁随之释放——同一目录可以立刻
   被下一个句柄打开（这也正是「core 真的关闭了」的运行时证据）。

昵称与显示名的边界：

- `open_persisted` 没有昵称入参，首次落库的账户显示名是**账户 ID 缩写**
  `acct:1:<前 8 位 hex>`（14 字节，符合显示名规则）。这是「还没有昵称」的诚实占位，
  不是伪造的昵称；改名接口（core 的 `save_identity_account` 支持同账户改显示名）本切片未暴露。
- 接受口令时若本地备注为空（口令不携带对方昵称），落库显示名同样使用**对端**账户 ID
  缩写；重新打开时该值被识别为「未知」并还原成空串，因此 `ContactView.display_name == ""`
  的契约在重开前后保持一致。

持久路径专属错误码（其余同下表）：

| 场景 | code |
|---|---|
| 错误主密钥（AEAD 认证失败） | `STATE_CORRUPTED` |
| 畸形主密钥、数据目录不存在 | `INVALID_INPUT` |
| 同一目录已有句柄（单写者） | `STORAGE_LOCKED` |
| 库中身份/联系人记录与种子或状态机不自洽 | `STATE_CORRUPTED` |

错误（复用 `FfiError::Store { code, reason }`；`code` 全部来自 identity 的 PROTOCOL-014 映射表，
不新增码，见 [identity.md 的错误码映射](identity.md)）：

| 场景 | code |
|---|---|
| 显示名非法、邀请有效期越界（0 / <60 s / >7 d / 溢出）、本地备注越界、自己的口令、未知 `contact_id`、跨节点接受 | `INVALID_TARGET` |
| 口令签名被篡改（正文或签名） | `INVALID_SIGNATURE` |
| 口令过期 | `CURSOR_EXPIRED` |
| 联系人迁移非法（例如对 `requested` 调用 `confirm_identity_change`） | `INVALID_STATE_TRANSITION` |
| 口令文本超长（>1024 字节） | `CONTENT_TOO_LARGE` |
| 口令结构畸形 | `MALFORMED_FRAME` |
| 口令版本前缀不是 `inv1` | `PROTOCOL_UNSUPPORTED` |
| `node` 不是规范 ID 文本 | `INVALID_ID` |
| 已 `shutdown` 后调用需要密钥的方法 | `CORE_CLOSED` |
| 互斥锁被前一次 panic 中毒 | `CORE_PANICKED` |

调用约定（Kotlin 侧必须遵守）：

1. `create` 在**进程内**生成账户根密钥与设备密钥：不写库、不做系统密钥保护，进程退出即丢失
   （等 PLATFORM-001/storage 集成）。因此 UI 不应把 `create` 当成「已持久化账户」。
2. `account()` 与 `contacts()` 的签名没有 `Result`：它们返回**公开投影**，`shutdown` 后仍返回
   关闭前快照（不伪造空值）。所有需要密钥的方法在 `shutdown` 后返回 `CORE_CLOSED`。
3. `preview_invite` 只校验口令本身（验签 + 期限 + 结构），把 `node` 当**提示**展示；
   真正阻断跨节点口令的是 `accept_invite`。两个入口共用 identity 的同一条校验链，
   不存在「先解析、后验签」的旁路。
4. 时间戳在 FFI 边界是 `u64`（Kotlin `ULong`）；内部时钟为系统 UTC 毫秒，测试注入固定时钟。
5. 口令原文 `token` 只在返回值里出现：不得写日志、分析事件或错误详情（上表中的 `reason`
   只含静态描述，不含口令内容）。

复现命令（全程离线，本轮实测）：

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi            # 17 passed（15 个 IdentityFfi + 2 个既有 LocalStoreFfi）
cargo build --locked --offline -p chat-ffi  # 成功
cargo clippy --offline -p chat-ffi --all-targets  # chat-ffi 0 warnings
~~~

证据（`identity_ffi::tests::*`）：

- `account_and_invite_round_trip_through_the_ffi_shape`：`create` → `account()`（acct/dev 前缀、
  79 字符 / 16 组指纹）→ `create_invite(24 h)`（`inv1.` 前缀、32 位 hex `invite_id`、
  `expires_at_ms == now + ttl`、账户/设备/指纹与账户视图一致）→ 对端 `preview_invite` 摘要逐字段一致，
  且 preview 不产生联系人。
- `projection_matches_the_frozen_identity_vectors`：账户种子 0x01 / 设备种子 0x11 的冻结向量
  （`acct:1:804d17d8…`、`dev:1:a4ec09ed…`、指纹 `7EC8 C5E1 … 4742 8EB4`）与 identity 用
  OpenSSL/Node 独立算出的结果完全一致，证明 FFI 只做投影、没有另造派生。
- `production_constructor_uses_the_system_clock`：真实构造路径（系统时钟）→ 口令 → 对端 preview →
  accept，端到端跑通。
- `tampered_tokens_are_rejected_with_invalid_signature`：改签名末位 hex → `INVALID_SIGNATURE`
  （preview 与 accept 都拒绝，且不产生联系人）。
- `expired_invites_are_rejected_with_cursor_expired`：时钟推进到 `expires_at_ms` →
  `CURSOR_EXPIRED`。
- `self_invites_and_foreign_nodes_are_rejected_with_invalid_target`：自己的口令 → `INVALID_TARGET`；
  跨节点口令 preview 可看（`node` 原样展示）、accept → `INVALID_TARGET`。
- `accept_invite_creates_a_requested_contact_only`：accept 后联系人 `state == "requested"`、
  `can_send == false`、`contact_id == 对端 account_id`、本地备注生效；同一口令重复粘贴**不产生重复申请**
  且不丢备注。
- `confirm_identity_change_rejects_illegal_calls`：`requested` → `INVALID_STATE_TRANSITION`；
  未知账户与畸形句柄 → `INVALID_TARGET`。
- `display_name_node_and_lifetime_bounds_are_enforced`：空/超长/含控制字符的显示名 → `INVALID_TARGET`；
  空/非规范的 `node` → `INVALID_ID`；`ttl_ms` 0/1/59 999/7 d+1/`u64::MAX` → `INVALID_TARGET`，
  60 000 与 7 d 通过；2048 字节口令 → `CONTENT_TOO_LARGE`、`inv2.` → `PROTOCOL_UNSUPPORTED`、
  畸形结构 → `MALFORMED_FRAME`。
- `shutdown_releases_keys_and_rejects_further_key_work`：`shutdown` 幂等；之后
  `create_invite`/`preview_invite`/`accept_invite`/`confirm_identity_change` 全部 `CORE_CLOSED`；
  `account()` 仍返回关闭前的公开快照。

持久身份证据（`identity_ffi::tests::*`，真 SQLite 库 + 真主密钥）：

- `persisted_identity_survives_reopen_with_the_same_account`：首次 `open_persisted` 生成并落库账户，
  显示名是账户 ID 缩写；同一目录 + 同一主密钥重开后 `AccountView`（`account_id`/`device_id`/
  `fingerprint`/`display_name`）完全一致；再用 `ConversationCore` 直接读回投影与受保护种子，
  证明身份真的在库里而不是内存假象。
- `persisted_contacts_survive_reopen_and_keep_their_state`：接受口令后 `shutdown` 重开，联系人仍在
  （`state == "requested"`、`can_send == false`、指纹与本地备注不变）；同一口令重复接受仍只有一条；
  空本地备注落库为对端账户 ID 缩写，重开后还原成 `""`。
- `wrong_master_key_is_rejected_with_a_stable_code`：错误主密钥 → `STATE_CORRUPTED`，且不重建空库
  （正确密钥仍读回同一账户）。
- `persisted_shutdown_closes_the_core_and_rejects_key_work`：`shutdown` 后同一目录立刻可以再开
  （单写者锁已释放，证明 core 真的关闭）；旧句柄所有需要密钥的调用返回 `CORE_CLOSED`。
- `restored_identity_change_requires_explicit_reconfirmation`：库里 `identity_changed` 的联系人
  恢复后 `can_send == false`（阻断发送）；`confirm_identity_change` 重新固定观测指纹 →
  `accepted` 且 `can_send == true`，重开后仍是 `accepted`。

已知限制：

- **进程内身份不持久化**：`create` 构造的对象只活在本次进程里（账户/设备密钥不落库）。
  需要重启后仍在的身份请用 `open_persisted`。
- **持久身份仍缺三块**：① 没有改名接口（首次落库的显示名是账户 ID 缩写，UI 需要昵称时得等
  core 的 `save_identity_account` 改名路径暴露）；② 没有「我的口令」列表、撤销与一次性消费
  （`consume_invite` 未接到 FFI）；③ 口令 `preview_invite` 不碰数据库，可在任意对象上调用。
- **持久身份的主密钥来自调用方**：`open_persisted` 只接受 64 位十六进制主密钥字符串，
  系统级密钥保护（PLATFORM-001）与密钥派生仍由上层负责；种子本身在库里受主密钥保护。
- **联系人状态恢复的边界**：`requested`/`accepted`/`declined`/`blocked` 由 identity 的
  冻结构造器/迁移重建；`identity_changed` 无法用 `Contact` 直接构造，恢复时以 `requested`
  承载并保留状态用于展示与显式重新确认（两种情况都不能发送，因此没有放宽发送边界）。
  库中出现未知状态或与种子不自洽的记录一律 `STATE_CORRUPTED`，不静默归一。
- **节点不接受昵称式字符串**：`node` 必须是 `node:1:<64 位小写 hex>`；两侧必须使用同一字符串，
  否则 `accept_invite` 返回 `INVALID_TARGET`。跨节点接受（设计稿把 node 当「提示」）留给后续
  多节点路由任务。
- **没有入站身份观测入口**：本切片只能通过口令建立 `requested` 联系人，没有任何入口把联系人推到
  `accepted`（除上面「从库中恢复的 identity_changed + 显式确认」外）；`accepted`/`identity_changed`
  的正式来源是 core 的握手/凭据事件。
- 没有事件订阅（BRIDGE-003/004/006）、没有 suspend 包装（BRIDGE-007）；`Mutex` 串行化所有调用，
  不要在 UI 线程做长循环。
- **Cargo.lock 变化**：`chat-ffi` 的依赖列表新增 `chat-identity` 与 `rand`（均为工作区 path /
  既有版本 `rand 0.10.3`，无新增或改版本的外部包）；`cargo build --locked` 通过。
- Kotlin/Android 真机尚未实测（本轮只验证 Rust 侧编译与单元测试）。

## 发送链路 FFI 契约（LocalStoreFfi.send_*，task-33，2026-10-18，Windows x64）

代码：[crates/ffi/src/local_store.rs](../../crates/ffi/src/local_store.rs)（薄封装）+ [crates/core/src/send_runtime.rs](../../crates/core/src/send_runtime.rs)（装配与状态机入口）；
测试：[crates/ffi/tests/send_contract.rs](../../crates/ffi/tests/send_contract.rs)（5 个，纯 FFI 契约，不需要真节点）。

### 方法（Kotlin 侧按此命名，均已 `#[uniffi::export]`）

~~~text
// 只保存节点配置，不发任何请求；端点必须是可解析的 http(s) 地址
configure_send_node(endpoint_url: String, node_id: String, node_public_key_hex: String?) -> Result<(), FfiError>
// 可选：显式指定发送所用的 MLS 群键（不设置时自动绑定本库唯一的持久 MLS 群）
configure_send_group(mls_group_key: String) -> Result<(), FfiError>

send_prepare(draft_id: String)   -> Result<SendStepView, FfiError>
send_dispatch(draft_id: String)  -> Result<SendStepView, FfiError>
send_retry(draft_id: String)     -> Result<SendStepView, FfiError>
send_reconcile(draft_id: String) -> Result<SendStepView, FfiError>
~~~

### 返回投影

~~~text
record SendStepView {
    draft_id: String,                 // 原样回传
    status: String,                   // queued/prepared/submitted/accepted/rejected/unknown/cancelled/failed
    submission_id: String?,           // 最新尝试的 SubmissionId
    attempt_index: u32?,              // 重建序号（0 = 首次尝试）
    detail: String,                   // 静态说明（无正文/密钥/URL）
    settled: bool,                    // false = 结果未定，仍需 reconcile，禁止重发
}
~~~

语义要点：

- `send_prepare` 会真实加密（**恰好一次**）并落 T2 尝试；`send_dispatch` 才真正提交节点；
  `send_retry` 只在“被拒绝/失败”后重建新 SubmissionId；`send_reconcile` 只按原 SubmissionId 查询，**绝不重发**。
- `settled = false` 的三种典型来源：节点结果未知（`unknown`）、租约/退避未到期、仍需对账；
  UI 不得把未确定结果画成“已发送成功”。
- 每次调用结束后内核会发布 `draft_changed` 事件（可 `poll_events` 取），并**无论成败都把 Store 句柄放回**。

### 错误码（FfiError::Store.code，全部来自冻结码表）

| 情形 | code |
|---|---|
| 未配置节点（所有 `send_*`） | `NODE_UNAVAILABLE` |
| 草稿不存在 | `INVALID_TARGET` |
| 草稿 ID 结构非法 | `INVALID_INPUT` |
| 本机身份未入库 / MLS 群未加入 / 指定群不存在 | `INVALID_STATE_TRANSITION` |
| 库里多个 MLS 群且未显式选群 | `INVALID_TARGET` |
| MLS 群状态损坏 | `STATE_CORRUPTED` |
| 节点端拒绝/未知 | 通过 `SendStepView.status` 表达（`rejected` / `unknown`），只有本地故障才抛错 |
| 已关闭 | `CORE_CLOSED` |

### 证据

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi --test send_contract   # 5 passed
cargo test --offline -p chat-ffi -p chat-core           # 25 + 81 passed
~~~

- `send_contract::send_steps_require_a_configured_node`：未配置时四个动作都返回 `NODE_UNAVAILABLE`，草稿状态仍可查询（对象没被破坏）。
- `send_contract::configure_send_node_validates_without_contacting_the_network`：坏 URL → `INVALID_TARGET`；坏公钥 → `INVALID_SIGNATURE`；空/超长群键 → `INVALID_INPUT`；合法配置成功且不联网。
- `send_contract::configured_but_unenrolled_database_fails_explicitly`：配置了节点但库未入库身份 → `INVALID_STATE_TRANSITION`，随后 `is_open()` 为真、状态查询与新建草稿仍可用。
- `send_contract::unknown_draft_is_rejected_with_a_precise_code`：未知草稿 → `INVALID_TARGET`；含空格的畸形 ID → `INVALID_INPUT`。
- `send_contract::send_after_shutdown_reports_closed`：`shutdown()` 后 `CORE_CLOSED`。

真节点侧证据见 [core.md 的“发送链路接真实节点”](core.md)：节点日志密文与本地耐久密文逐字节一致、重复 dispatch 零重发。

已知限制：

1. **没有后台发送 worker**：`send_dispatch` 是同步的一次尝试，UI 需要按 `settled` 决定是否继续/对账（不要在 UI 线程阻塞长任务）。
2. **群绑定是本切片约定**（协议群 ID 由 MLS 群键派生）；多群场景需显式 `configure_send_group`。
3. `configure_send_node` 只保存配置：不验证节点可达性，也不注册设备；设备注册/建群仍由既有流程完成。
4. 附件与正文的联合发送（引用块）不在本轮；`send_dispatch` 只提交正文条目。

## 附件上传/下载 FFI 契约（AttachmentFfi，ATT-U，task-34，2026-10-18，Windows x64）

代码：[crates/ffi/src/attachments_ffi.rs](../../crates/ffi/src/attachments_ffi.rs)（薄封装）+ [crates/core/src/attachment_node.rs](../../crates/core/src/attachment_node.rs)（装配：持久身份 → 设备凭据 → NodeBlobTransport → 编排层）；
测试：[crates/ffi/tests/attachment_node.rs](../../crates/ffi/tests/attachment_node.rs)（5 个，真 axum 节点 + 临时持久库）。

### 两种后端（由构造方式决定）

| 构造 | 后端 | 能力 |
|---|---|---|
| `AttachmentFfi::new(cache_root)` | 仅内存缓存 | import / status / export / cancel；`upload`/`download` → `INVALID_STATE_TRANSITION`（无持久记录与身份库） |
| `AttachmentFfi::open(database_path, master_key_hex, cache_root)` | 正式加密库 + 缓存 | 全部：状态真实落 attachments 表，upload/download 走真节点 |

`open` 的库是**独占单写者**：调用前必须释放其它持有同一库的对象（`ConversationCore` / `LocalStoreFfi`）。

### 方法

~~~text
upload(attachment_id, endpoint_url, node_id, node_public_key_hex?, group_id, source_seq) -> Result<AttachmentView, FfiError>
download(blob_id, target_path, endpoint_url, node_id, node_public_key_hex?, group_id, source_seq) -> Result<AttachmentDownloadView, FfiError>
~~~

- 设备签名钥由库里的**持久身份**派生（`load_identity_seeds` → `signing_key_from_seed`，与 send 链路同一条路径）；本层**不接受任何种子/口令入参**。
- `group_id` 是 PROTOCOL-001 群文本，`source_seq` 是本设备已接受的日志序号（NODE-012 的上传授权关联）；下载只需要凭据（NODE-015：上传设备本人恒可读），但端口构造仍要求两个参数合法（可传上传时相同的值）。
- `AttachmentView` 新增 `transferred_chunks`（已传块数；失败后保留，不是协议确认）；其余字段与旧契约一致。
- `AttachmentDownloadView { attachment_id, written_bytes, content_digest }`：不含正文/密钥。

### 状态与错误码

- 状态真实迁移：`imported → uploading → available`（storage 白名单）；成功后 `state = "available"`；失败保留缓存与进度、状态停在 `uploading` 可重试；已 `available` 时重复上传是**幂等无操作（零发送）**。
- 错误码（`FfiError::Store.code`，全部冻结）：`INVALID_TARGET`（端点/群/序号非法）、`INVALID_SIGNATURE`（节点公钥格式错）、`GROUP_NOT_FOUND` / `PERMISSION_DENIED`（节点授权）、`ATTACHMENT_NOT_FOUND`（记录不存在）、`ATTACHMENT_CORRUPTED`（远端数据与本地记录不一致）、`ATTACHMENT_KEY_UNAVAILABLE`（无进程内 FileKey）、`NODE_UNAVAILABLE` / `QUOTA_EXCEEDED` / `CONTENT_TOO_LARGE` 等节点码、`INVALID_STATE_TRANSITION`（内存后端调用 upload/download）。

### 证据

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi --test attachment_node   # 5 passed
cargo test --offline -p chat-ffi -p chat-core             # 30 + 81 passed
~~~

- `attachment_node::upload_reaches_the_node_and_download_returns_the_original_bytes`：导入 → 上传（state 真为 `available`、available_chunks = chunk_count）→ 从**节点**下载并认证解密，写出的字节与原文逐字节一致、回执摘要一致；重开对象后状态仍是 `available`；另一实例下载 → `ATTACHMENT_KEY_UNAVAILABLE` 且不产生文件。
- `attachment_node::repeated_upload_is_idempotent_and_sends_nothing`：上传成功后**关掉节点**再上传 → 仍返回 `available`（证明幂等路径零发送）。
- `attachment_node::wrong_credentials_and_unknown_groups_fail_explicitly`：未知群 / 非本设备 source_seq / 坏端点 / 坏公钥 / 设备未在节点注册，全部显式失败且状态**不会被标成 available**。
- `attachment_node::in_memory_object_refuses_upload_and_download`：内存后端调用 upload/download → `INVALID_STATE_TRANSITION`。
- `attachment_node::download_without_a_record_is_rejected`：未知附件 → `ATTACHMENT_NOT_FOUND`，不写文件。

已知限制：

1. **跨进程/跨实例下载**需要 E2EE SecretDescriptor（FileKey 不落盘），当前显式 `ATTACHMENT_KEY_UNAVAILABLE` —— 设计边界，不伪造可下载。
2. **节点块上限已对齐客户端分块**：节点默认 `max_chunk_bytes` 1 MiB（硬上限 4 MiB）≥ 客户端 256 KiB 密文块，多块附件可上传（见 [attachments.md](attachments.md) 的“节点块传输适配实现记录”）。
3. 下载不写本地加密缓存（直接写出目标路径），因此下载后不能离线再 export。
4. 上传/下载是**同步阻塞**调用（FFI 的 `Mutex` 串行化）：不要在 UI 线程上直接跑大文件。

## 群治理 FFI 契约（GroupFfi，2026-10-18，Windows x64）

代码：[crates/ffi/src/groups_ffi.rs](../../crates/ffi/src/groups_ffi.rs)（新，薄封装）+ [crates/core/src/service.rs](../../crates/core/src/service.rs) 的 `ConversationCore::group_*`；
测试：[crates/ffi/tests/groups_ffi.rs](../../crates/ffi/tests/groups_ffi.rs)（6 个用例，真 OpenMLS + 临时持久库，经 FFI 走完整链路）。

### 对象与方法

~~~text
GroupFfi::open(database_path, master_key_hex, node: String?) -> Arc<GroupFfi>   // 构造；node 只保存，不联网
group_create(name, local_credential, bootstrap_credential, members: Vec<GroupMemberInputFfi>) -> GroupViewFfi
group_snapshot(group_id) -> GroupViewFfi
group_set_role(group_id, actor_credential, target_credential, role) -> GroupViewFfi   // role: owner/admin/member
group_set_muted(group_id, actor_credential, target_credential, muted) -> GroupViewFfi  // 策略 + 自更新提交（pending）
group_remove_member(group_id, actor_credential, target_credential) -> GroupViewFfi
group_rename(group_id, actor_credential, new_name) -> GroupViewFfi                     // 策略 + 自更新提交（pending）
group_accept_pending(group_id) -> GroupViewFfi      // 接受并合并控制提交（CRYPTO-007）
group_discard_pending(group_id, reason) -> GroupViewFfi  // 丢弃回滚（CRYPTO-008）
node() -> String?    is_open() -> bool    shutdown()
~~~

`GroupMemberInputFfi { credential, key_package: Vec<u8> }`（追加成员的真实 KeyPackage）；
`GroupViewFfi { group_id, name, epoch, policy_version, role, control_head, mls_group_key, state_version,
created_at_ms, members, pending_members, pending_removed, pending, last_control_cleared }`；
成员 `GroupMemberFfi { credential, role, muted, is_local, pending }`；
pending `GroupPendingFfi { operation, base_epoch, target_epoch, added, removed, commit_digest }`；
丢弃审计 `GroupClearedFfi { operation, base_epoch, target_epoch, commit_digest, reason_code, detail, cleared_at_ms }`。

### 接线要点

- **独占单写者**：`GroupFfi::open` 持有正式加密库的写句柄，与 `LocalStoreFfi` / `AttachmentFfi`
  互斥（同一库同一时刻只能有一个句柄）；也正因如此，需要身份类数据时先关闭 `IdentityFfi`。
- **设备从库恢复**：除建群外，每个方法都会 `MlsDevice::import_from_store` 重新装载本机设备
  （不跨调用持有设备句柄）；建群时设备是新建的（群还不存在），群状态随即落库。
- **凭据来源**：`group_create` 的 `local_credential` 传空串时使用库里**持久身份**的设备 ID
  （未入库身份 → `INVALID_STATE_TRANSITION`）；非空时用调用方给的凭据文本。
- **角色是稳定文本**（`owner`/`admin`/`member`）；未知文本 → `INVALID_INPUT`；
  传 `owner` 会被 domain 拒绝（`PERMISSION_DENIED`，只能走转让）。
- **改名/禁言也会准备控制提交**（GROUPS-016）：`group_rename` / `group_set_muted` 走 core 的
  `*_with_commit(local_only = false)`——策略立即生效、同时准备一个 `SelfUpdate` 提交（`GroupViewFfi.pending`
  如实报告 `base_epoch → target_epoch`），**epoch 在准备阶段不推进**；接受仍走 `group_accept_pending`。
  因此这两次调用之后、接受之前，再发起第二次策略变更会得到 `INVALID_STATE_TRANSITION`。

### 错误码（`FfiError::Store.code`，全部冻结码）

| 情形 | code |
|---|---|
| 非群主/管理员治理、改群主、改自己、禁言自己 | `PERMISSION_DENIED` |
| 未知群 / 目标不是确认成员 / 本机不在该群 | `INVALID_TARGET` |
| 非法群名 / 非法角色文本 / 非法丢弃原因 | `INVALID_INPUT` |
| 群 ID 不是 PROTOCOL-001 文本 | `INVALID_ID` |
| 成员数或名称超出上限 | `GROUP_LIMIT_EXCEEDED` |
| 没有 pending / pending 与设备不一致 | `PENDING_MISMATCH` / `STATE_CORRUPTED` |
| 未决提交期间的策略写入或第二个提交 | `INVALID_STATE_TRANSITION` |
| 本机 epoch 与快照不一致 | `STALE_EPOCH` |
| 已关闭 | `CORE_CLOSED` |

### 证据

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi --test groups_ffi   # 6 passed
cargo test --offline -p chat-core -p chat-ffi -p chat-node-services   # 208 + 60 + 90 passed
~~~

- `groups_ffi::create_snapshot_rename_mute_and_remove_through_ffi`：建群（epoch 1/owner/全零控制头/两成员）→
  快照逐字段一致 → 改名（pv 2、**pending self_update base1→target2**、epoch 仍 1）→ 接受（epoch 2、名字保持、
  pending 清空）→ 同名幂等（pv 与 state_version 都不动）→ 禁言（pv 3、muted=true、pending 自更新）→
  接受（epoch 3、muted 保持）→ 移除（pv 4、正式成员只剩 alice、pending_removed=[bob]、
  pending.operation=remove_members base3→target4）→ 接受（epoch 4、pending 与 pending_removed 清空、pv 仍 4）
  → 快照一致。
- `groups_ffi::role_change_accept_and_discard_through_ffi`：改角色（pv 2、pending self_update）→ 丢弃
  （epoch 1、pending 清空、pv 不回退、审计 remove 记录 self_update/node_rejected/base1→target2、角色保留）→
  改回 member（pv 3、base_epoch 仍 1）→ 接受（epoch 2、角色 member）。
- `groups_ffi::error_codes_are_frozen_and_failures_leave_no_state`：未知群 `INVALID_TARGET`、畸形群 ID
  `INVALID_ID`、非法角色/非法名 `INVALID_INPUT`、非治理者三种操作 `PERMISSION_DENIED`、未知目标
  `INVALID_TARGET`、无 pending 的接受/丢弃 `PENDING_MISMATCH`、空原因 `INVALID_INPUT`；
  全部失败后快照与建群结果完全一致（无半状态）。
- `groups_ffi::empty_local_credential_uses_the_persisted_identity_device`：先用 `IdentityFfi` 在库里建立
  持久身份，再让 `group_create` 的 `local_credential` 留空 → 本地成员凭据就是该身份的设备 ID。
- `groups_ffi::create_without_identity_and_empty_credential_fails`：未入库身份 + 空凭据 → `INVALID_STATE_TRANSITION`；
  空 KeyPackage 的追加成员 → `INVALID_CANDIDATE`（对象仍可用）。
- `groups_ffi::closed_object_reports_core_closed`：`shutdown` 后快照/改名 → `CORE_CLOSED`。

已知限制：

1. **本地接受 ≠ 节点接受**：`group_accept_pending` 只收口本地状态，没有节点回执校验与日志对账（见 groups.md）。
2. **建群的引导成员由本进程临时创建设备**：crypto 只有双人引导路径，真实邀请加入（GROUPS-004）未实现。
3. **独占单写者**：不能与 `LocalStoreFfi` / `AttachmentFfi` 同时打开同一库；需要跨对象操作时要先关一个。
4. **同步阻塞**：所有群方法在 FFI 的 `Mutex` 内串行执行（含 MLS 提交生成），不要在 UI 线程做长循环。
5. `node` 参数只保存不联网；群治理当前不访问网络。

## 群治理经节点回执（GroupFfi.group_submit_control / group_accept_with_receipt，2026-10-18，Windows x64）

目标：把 `group_node` 的真节点回执路径接到 Kotlin——界面能"提交治理操作到节点、拿到回执、再接受"。

代码：[crates/ffi/src/groups_ffi.rs](../../crates/ffi/src/groups_ffi.rs)（+2 方法、+4 记录）+
[crates/core/src/service.rs](../../crates/core/src/service.rs) 的 `ConversationCore::group_submit_control` /
## 插件宿主持久化（PluginsFfi.open，2026-10-18）

~~~text
PluginsFfi::open(database_path, master_key_hex) -> Arc<PluginsFfi>   // 持久化模式（正式加密库）
PluginsFfi::new() -> Arc<PluginsFfi>                                  // 纯内存（不变）
skipped_on_load() -> u32
~~~

- 依赖 storage 迁移 **0010_plugins**（plugins / plugin_kv，值密封存储）；
- 打开时把已安装清单装回宿主，**运行状态不恢复**（stopped，需上层再 `start`）；
- install/start/stop/uninstall 会写 `plugins.state`；uninstall(`remove_data=false`) 只把行标记
  `uninstalled`（**保留 KV**），`true` 删行并级联清 KV；
- 持久化模式下 `storage_get/storage_set` 走库（能力门仍要求已安装 + `PrivateStorage`），
  因此重开后未 start 也能读回 KV；配额越界 → `PLUGIN_STORAGE_QUOTA_EXCEEDED` 且不写入；
- 清单损坏行跳过并计入 `skipped_on_load`；`shutdown` 释放库句柄（同库可重新 open）。
- 测试：[crates/ffi/tests/plugins_ffi_persist.rs](../../crates/ffi/tests/plugins_ffi_persist.rs)（4 个，真临时库）。

复现命令：

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi --test plugins_ffi_persist   # 4 passed
cargo test --offline -p chat-storage -p chat-ffi               # 94 + 57 passed

## 常驻同步 FFI 契约（SyncFfi，2026-10-18，Windows x64）

~~~text
SyncFfi::open(database_path, master_key_hex, endpoint_url, node_id, node_public_key_hex, group_id)
    -> Arc<SyncFfi>      // 构造：装配 NodeClient + 校验游标；**不启动**
start() -> SyncStatusFfi      stop() -> SyncStatusFfi      status() -> SyncStatusFfi
is_open() -> bool             group_id() -> String          shutdown()
SyncStatusFfi { state, applied_total, last_error_code, next_attempt_at_ms, resync_required,
                rounds, stopped_reason }
~~~

- 设备身份复用持久身份（`load_identity_seeds`）；未入库身份 → `INVALID_STATE_TRANSITION`；
- 游标未初始化 → `INVALID_STATE_TRANSITION`；`open` 不启动（status 恒为 idle）；
- `start` 重建配置（NodeClient 被运行时消费）；`stop` 幂等并保留最后状态
  （stopped_reason=`stop_requested`）；`shutdown` 幂等，之后一律 `CORE_CLOSED` 并释放库锁；
- 错误码：`CORE_CLOSED` / `INVALID_STATE_TRANSITION` / `INVALID_INPUT` / `INVALID_TARGET` /
  storage 冻结码 / `NODE_UNAVAILABLE` / `INTERNAL_FAILURE`；
- 测试：[crates/ffi/tests/sync_ffi.rs](../../crates/ffi/tests/sync_ffi.rs)（3 个，真 axum 节点 + 真库）。
  限制：单群单运行时、状态轮询式、启动/停止同步阻塞、运行期间库被运行时独占、`node_public_key_hex` 必填。

## 自移除提案编排 FFI（GroupFfi.group_prepare_leave / group_process_proposal / group_commit_proposals，2026-10-18）

~~~text
group_prepare_leave(group_id) -> LeaveProposalFfi
group_process_proposal(group_id, epoch, sender_identity, payload) -> ProposalViewFfi
group_commit_proposals(group_id) -> GroupViewFfi
LeaveProposalFfi { group_id, base_epoch, epoch, removed, has_welcome, proposal, proposal_digest,
                   leaving_at_ms, state_version }
ProposalViewFfi  { kind, by, epoch, targets, proposals_pending }
~~~

- 纯数据（提案字节可转发）；`GroupViewFfi` 含 `dissolved / dissolved_at_ms / left_at_ms / leaving_at_ms`；
- 错误码：`PERMISSION_DENIED`（群主发起）、`INVALID_STATE_TRANSITION`（已解散/重复发起/已有 pending）、
  `STALE_EPOCH` / `MISSING_LOG_ENTRIES`、`INVALID_CIPHERTEXT`、`NO_PENDING_PROPOSALS` /
  `PENDING_COMMIT_EXISTS`、`INVALID_CANDIDATE`；
- 测试：[crates/ffi/tests/groups_proposal_ffi.rs](../../crates/ffi/tests/groups_proposal_ffi.rs)（3 个）。

## 群列表 FFI（GroupFfi.group_list，2026-10-18）

~~~text
group_list() -> GroupListFfi
GroupListFfi   { groups: Vec<GroupSummaryFfi>, skipped: u32 }
GroupSummaryFfi{ group_id, name, epoch, role, policy_version, dissolved, left, pending }
~~~

- 纯数据；按最近写入时间倒序、一次最多 256 个群；读不出来的条目计入 `skipped`（不抛出、不静默丢）；
- 已关闭 → `CORE_CLOSED`；测试：[crates/ffi/tests/groups_list_ffi.rs](../../crates/ffi/tests/groups_list_ffi.rs)（1 个）。

## 插件宿主 FFI（PluginsFfi，纯内存，2026-10-18）

~~~text
PluginsFfi::new() -> Arc<PluginsFfi>
manifest_validate(manifest_json) -> ManifestViewFfi      install(manifest_json) -> String
start(plugin_id) -> ()                                   dispatch(plugin_id, extension_point,
    operation_id, input, now_unix_ms, deadline_unix_ms) -> DispatchPlanViewFfi
finish_dispatch(plugin_id, operation_id, output, now_unix_ms) -> ()
stop(plugin_id) -> bool                                  uninstall(plugin_id, remove_data) -> bool
storage_get(plugin_id, key) -> Vec<u8>?                  storage_set(plugin_id, key, value) -> ()
list_plugins() -> Vec<PluginInfoFfi>                     is_open() -> bool      shutdown()
~~~

- `dispatch` **只出计划**（预算 + 绑定），不调用插件；`finish_dispatch` 只校验输出/截止时间/幂等；
- 错误码原样透出 `chat-plugins` 稳定码（`PLUGIN_ID_CONFLICT` / `PLUGIN_NOT_RUNNING` /
  `PLUGIN_NOT_REGISTERED` / `CAPABILITY_NOT_DECLARED` / `CONTENT_TOO_LARGE` /
  `PLUGIN_OPERATION_CONFLICT` / `PLUGIN_OPERATION_UNKNOWN` / `UNKNOWN_EXTENSION_POINT`）；
- 存储读写需要 `PrivateStorage` 能力且只看自己的命名空间；
- 测试：[crates/ffi/tests/plugins_ffi.rs](../../crates/ffi/tests/plugins_ffi.rs)（7 个，纯内存）。
## 身份恢复文件 FFI（IdentityFfi，2026-10-18）

~~~text
IdentityFfi::open_recovery_target(database_path, master_key_hex, node) -> Arc<IdentityFfi>
IdentityFfi.export_recovery_bundle(passphrase) -> Vec<u8>
IdentityFfi.import_recovery_bundle(passphrase, bytes) -> RecoveryImportViewFfi
RecoveryImportViewFfi { account_id, device_id, account_public_key, device_public_key,
                        display_name, created_at_ms, devices_digest }
~~~

- **导出**：需 `open_persisted` 且库里有身份，否则 `INVALID_INPUT`；导出期间短暂释放库独占写锁
  （之后自动重开并恢复身份句柄）；对象已关闭 → `CORE_CLOSED`。
- **导入**：只在**空库**上允许，且对象必须已 `shutdown()`，否则 `INVALID_STATE_TRANSITION`；
  目标库已有身份 → `RECOVERY_TARGET_NOT_EMPTY`（绝不覆盖）；导入后仍需 `open_persisted` 才能用身份。
- **空库来源**：`open_recovery_target`（打开/创建库 → 确认无身份 → 立即关闭并保持关闭态）。
- 错误码原样透出：`RECOVERY_BAD_PASSPHRASE` / `RECOVERY_CORRUPTED` / `RECOVERY_UNSUPPORTED` /
  `RECOVERY_TARGET_NOT_EMPTY` / `INVALID_INPUT` / `CORE_CLOSED` / `INVALID_STATE_TRANSITION`。
- 测试：[crates/ffi/tests/identity_recovery_ffi.rs](../../crates/ffi/tests/identity_recovery_ffi.rs)（3 个，真临时库）。

复现命令：

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi --test identity_recovery_ffi   # 3 passed
cargo test --offline -p chat-ffi                                 # 60 passed（既有 57 继续通过）
~~~


~~~

## 第二批 32 轮期间新增的 FFI 面（补齐被截断章节的汇总，2026-10-19）

> 本节由 Lead 据各轮实现记录重建：bridge.md 曾在一次整文件重写中被截断（1002→796 行），
> 后续各轮又陆续追加了多节；这里给出**当前有效方法的汇总表**，避免历史细节丢失后无从查证。

| 对象 | 方法 | 语义要点 | 证据 |
|---|---|---|---|
| `LocalStoreFfi` | `open/create_draft/list_conversations/list_messages/message_attachment_ids/draft_status/cancel_draft/poll_events/send_*` | 发送编排 + 事件拉取 + 附件 id 查询（不解密） | core 232 / ffi 60 |
| `IdentityFfi` | `create/open_persisted/account/contacts/create_invite/preview_invite/accept_invite/confirm_identity_change` + `open_recovery_target/export_recovery_bundle/import_recovery_bundle` | 身份 + **恢复文件**（空库约束、非空库拒绝不覆盖） | identity 56 / ffi 60 |
| `AttachmentFfi` | `new/open/import/status/export/cancel/upload/download/records` | 导入导出、上传下载（回执 written_bytes）、批量记录（显示名/大小/状态） | core 232 / ffi 60 |
| `GroupFfi` | `open + group_create/snapshot/list/set_role/set_muted/rename/remove_member/accept_pending/discard_pending`、`group_submit_control/group_accept_with_receipt`、`group_prepare_leave/process_proposal/commit_proposals/submit_proposal` | 治理 + 节点回执 + **自移除提案编排**（提交侧需群主/管理员，提案人不得自打包） | core 232 / ffi 60 |
| `SyncFfi` | `open/start/stop/status/shutdown` | 常驻同步（游标未初始化显式失败、不自动启动） | core 232 / ffi 60 |
| `PluginsFfi` | `new/open + manifest_validate/install/start/stop/dispatch/finish_dispatch/uninstall/storage_get/storage_set/list_plugins/shutdown` | 宿主状态机与**派发计划**；**不加载也不执行插件代码**；持久化模式（迁移 0010） | ffi 60 / pluginSmoke PASS |

**跨对象共同约定**：错误统一 `FfiError::Store{code, reason}`，code 为 PROTOCOL-014 冻结码；同一库同一时刻只允许一个对象持有写锁（否则 `STORAGE_LOCKED`）；Kotlin 侧一律经 `shared/core-bridge` 的 `Native*Service` 串行化调用。
## 群列表摘要新字段（GroupSummaryFfi，2026-10-18）

~~~text
GroupSummaryFfi { group_id, name, epoch, role, policy_version, dissolved, left, pending,
                  muted_members: u32,      // 确认成员里被禁言的人数
                  pending_proposal: bool } // 已收下、等本机打包的对端提案（与 pending 分开）
~~~

- `pending` 仍是"待收口线索"（自己发起自移除 **或** 有待打包提案）；
- `pending_proposal` 只表示"**收下的对端**提案等本机打包"（自己发起的提案不算）；
- `group_list` 依旧只读快照、不导入 MLS 设备、损坏条目计入 `skipped`。
## 插件包结构校验（PluginsFfi.inspect_package，2026-10-18）

~~~text
PluginsFfi.inspect_package(bytes) -> PackageViewFfi      // 不执行任何插件代码
PackageViewFfi { plugin_id, plugin_version, entry_class, entry_file, spi_file,
                 entry_count, compressed_bytes, uncompressed_bytes }
~~~

- 复用 `chat_plugins::inspect_plugin_package`（纯字节解析，无第三方依赖）；
- 稳定码原样透出：`PLUGIN_PACKAGE_CORRUPTED` / `PLUGIN_PACKAGE_TOO_LARGE` /
  `PLUGIN_PACKAGE_MISSING_MANIFEST` / `PLUGIN_PACKAGE_MISSING_ENTRY` /
  `PLUGIN_PACKAGE_SPI_MISMATCH` / `PLUGIN_PACKAGE_UNSAFE_PATH` /
  `PLUGIN_PACKAGE_DUPLICATE_ENTRY` / `PLUGIN_PACKAGE_UNSUPPORTED_COMPRESSION` /
  `PLUGIN_PACKAGE_NOT_TEXT` / `INVALID_PLUGIN_PACKAGE`；关闭后 `CORE_CLOSED`；
- 限制：清单与 SPI 必须是 stored（method 0）条目（本层不做 inflate），也不校验 CRC/包签名。
- 测试：[crates/ffi/tests/plugins_package.rs](../../crates/ffi/tests/plugins_package.rs)（1 个，测试内构造最小 ZIP）。
## 插件安装落库（PluginsFfi.install_package，2026-10-18）

~~~text
PluginsFfi.install_package(bytes: Vec<u8>, trusted: bool) -> InstallOutcomeFfi
InstallOutcomeFfi { plugin_id, version, upgraded_from: Option<String> }
PluginInfoFfi     { plugin_id, running, extension_points, version, manifest: ManifestViewFfi }
~~~

- 需持久化对象（`PluginsFfi::open`）；纯内存对象 → `INVALID_STATE_TRANSITION`；
- `trusted=false` → `PLUGIN_TRUST_REQUIRED`；包校验失败 → `PLUGIN_PACKAGE_*`；
- 同版本 → `PLUGIN_STATE_CONFLICT`、降级 → `PLUGIN_VERSION_DOWNGRADE`（库与宿主都不变）；
- 成功后 0010 `plugins` 行 upsert + 内存宿主提交；重开 `list_plugins` 仍能看到版本与清单；
- 测试：[crates/ffi/tests/plugins_install_persist.rs](../../crates/ffi/tests/plugins_install_persist.rs)（5 个，真库）。
## 设备集合（IdentityFfi 设备三方法，2026-10-18）

~~~text
IdentityFfi.list_devices(include_revoked) -> Vec<DeviceViewFfi>
IdentityFfi.revoke_device(device_id, reason) -> bool
IdentityFfi.device_is_revoked(device_id) -> bool
DeviceViewFfi { device_id, account_id, label, device_public_key_hex, added_at_ms,
                revoked_at_ms, revoked_reason, is_local }
~~~

- 需 `open_persisted`（纯内存 → `INVALID_STATE_TRANSITION`；关闭 → `CORE_CLOSED`）；
- `list_devices` 懒登记本机设备（幂等），因此列表里一定有 `is_local = true` 的本机一台；
- `revoke_device`：本机 → `DEVICE_SELF_REVOKE`；未登记/跨账户 → `INVALID_INPUT`；幂等（重复撤销 false）；
- 撤销后默认列表隐藏，`include_revoked` 可见撤销时间与原因（行永不删除）；
- 测试：[crates/ffi/tests/identity_devices_ffi.rs](../../crates/ffi/tests/identity_devices_ffi.rs)（5 个，真库）。
## 设备登记（IdentityFfi.register_device，2026-10-18）

~~~text
IdentityFfi.register_device(device_id, device_public_key_hex, label) -> DeviceViewFfi
~~~

- 需 `open_persisted`（纯内存 → `INVALID_STATE_TRANSITION`；关闭 → `CORE_CLOSED`）；
- `device_id` 必须是 `dev:1:<64 hex>`；公钥必须正好 64 hex → 否则 `INVALID_INPUT`；
- 幂等（`added_at_ms` 不变、不新增行）；已属其它账户 → `INVALID_INPUT`；
- 已撤销设备重新登记**不复活**（返回视图仍带撤销时间与原因）；返回 `is_local`；
- 测试：[crates/ffi/tests/identity_device_register.rs](../../crates/ffi/tests/identity_device_register.rs)（6 个，真库）。
## 群摘要新增 revoked_devices_count（GroupSummaryFfi，2026-10-18）

`GroupSummaryFfi` 新增 `revoked_devices_count: u32`（与 core 的 `GroupSummaryView` 一一对应，
来自快照 `revoked_devices` 长度）。`GroupFfi.group_list` 的语义不变：只读快照、不导入 MLS 设备、
损坏条目计入 `skipped`。测试见 [crates/ffi/tests/groups_list_ffi.rs](../../crates/ffi/tests/groups_list_ffi.rs)
（默认 0）与 [crates/core/tests/revoked_devices.rs](../../crates/core/tests/revoked_devices.rs)（处理后 1）。
## 建群/入群播种与全库播种（2026-10-18）

- `GroupViewFfi.seeded_revoked_devices: u32`：建群/入群自动播种的条数（其它路径 0）；
  `GroupSummaryFfi.revoked_devices_count` 提供列表侧计数；
- `IdentityFfi.seed_revoked_devices_for_all_groups() -> u32`：对库里所有群播种一次账户撤销集合，
  幂等（重复调用 0），单群失败不阻断；恢复导入成功后自动播种；
- 测试：[crates/ffi/tests/identity_seed_groups.rs](../../crates/ffi/tests/identity_seed_groups.rs)（3 个，真库 + 真 OpenMLS）。
## 邀请投递（GroupFfi 邀请两方法，2026-10-18）

~~~text
group_submit_invite(group_id, meta, welcome_bytes, endpoint_url, node_id, node_public_key_hex?)
    -> InviteReceiptFfi { submission_id, seq, log_hash, node_id }
group_fetch_invite(group_id, endpoint_url, node_id, node_public_key_hex?)
    -> Option<GroupInviteBlobFfi { meta, welcome_bytes }>
InviteMetaFfi { group_id, mls_group_id, name, inviter_credential, owner_credential,
                invitee_credential, members, epoch }
~~~

- 复用 `chat_core::group_node::{submit_group_invite, fetch_group_invite}`（core 侧新增
  `ConversationCore::group_submit_invite / group_fetch_invite` 两个薄封装，设备签名钥来自持久身份）；
- 稳定码：`INVALID_TARGET`（未配置/坏端点/坏公钥）、`NODE_UNAVAILABLE`（不可达）、
  `INVALID_STATE_TRANSITION`（未入库身份）、`CORE_CLOSED`（已关闭）、节点侧稳定码原样透出；
- 幂等：同 (群, blob) 重复提交返回同一 `submission_id`；
- 纯数据、不含密钥；测试 [crates/ffi/tests/groups_invite_ffi.rs](../../crates/ffi/tests/groups_invite_ffi.rs)（4 个，真节点）。
## 邀请取回新增可选被邀请者过滤（2026-10-18）

~~~text
GroupFfi.group_fetch_invite(group_id, endpoint_url, node_id, node_public_key_hex?, invitee_credential?)
    -> Option<GroupInviteBlobFfi>
~~~

- `invitee_credential = None` → 不过滤（最近一条邀请，旧行为不变）；给了就只返回被邀请者匹配的最近一条；
- 别人的邀请 / 未知凭据 → `None`（不是错误）；页/网络错误仍显式失败；
- core 侧对应 `fetch_group_invite_for`（`fetch_group_invite` 是 `None` 别名），
  `ConversationCore::group_fetch_invite` 追加 `invitee_credential` 参数；
- 测试：[crates/ffi/tests/groups_invite_ffi.rs](../../crates/ffi/tests/groups_invite_ffi.rs)（5 个，真节点）。
## 建群返回 Welcome（GroupFfi.group_create_with_welcome，2026-10-18）

~~~text
group_create_with_welcome(name, bootstrap_credential, local_credential)
    -> CreatedGroupFfi { view: GroupViewFfi, welcome_bytes: Vec<u8>, invite: InviteMetaFfi }
~~~

- 与 `group_create` 同一路径，额外返回 MLS Welcome 原文（纯数据、无密钥）；
- Welcome > 64 KiB → `CONTENT_TOO_LARGE`（不截断）；空 Welcome → `STATE_CORRUPTED`；
- `invite` 与快照逐字段一致（被邀请者 = 引导成员，群主/邀请者 = 快照 owner/本机，成员 = 确认 ∪ pending）；
- 既有 `group_create` 行为不变；
- 限制：Welcome 绑定 core 内部一次性 bootstrap 设备 key package，外部设备接受会 `WELCOME_NOT_AUTHORIZED`
  （跨设备入群需要后续把真实 KeyPackage 作为入参）；
- 测试：[crates/ffi/tests/groups_create_welcome.rs](../../crates/ffi/tests/groups_create_welcome.rs)（3 个，真库 + 真 MLS）。

## GROUPS-004 · FFI 建群（真实被邀请者 KeyPackage，2026-10-20）

新增 `group_create_with_invitee_key_package(name, invitee_credential, key_package_bytes, local_credential) -> CreatedGroupFfi`。

- 与 `group_create_with_welcome` 只有来源不同：KeyPackage 由**另一台设备**导出后传入，因此返回的 Welcome 对方真的能用；
- 空 KeyPackage → `INVALID_INPUT`；Welcome 超过 64 KiB → `CONTENT_TOO_LARGE`；空 Welcome → `STATE_CORRUPTED`；
- `CreatedGroupFfi` 里的 `invite`（`GroupInviteMeta`）与快照逐字段一致，可直接配合邀请投递（GROUPS-019）使用。
## 导出本机 KeyPackage 与接受 Welcome（2026-10-18）

~~~text
IdentityFfi.export_key_package() -> Vec<u8>                       // 单次使用、只含公钥材料
IdentityFfi.join_group_from_welcome(name, welcome_bytes, invite: InviteMetaFfi) -> GroupJoinedFfi
GroupJoinedFfi { group_id, epoch, members }
~~~

- 导出复用 crypto 的 `MlsDevice::key_package()`（未改 crypto）；每次导出不同（单次使用语义）；
- 需持久化身份（纯内存 → `INVALID_STATE_TRANSITION`；关闭 → `CORE_CLOSED`）；字节原样返回不截断；
- MLS 设备句柄缓存在对象状态里，`join_group_from_welcome` 用同一实例接受 Welcome（否则 KeyPackage 私钥材料丢失）；
- 与 `GroupFfi.group_create_with_invitee_key_package` 配对即构成完整邀请闭环：
  B 导出 KeyPackage → A 建群拿 Welcome → B 接受加入；
- 测试：[crates/ffi/tests/key_package_export.rs](../../crates/ffi/tests/key_package_export.rs)（3 个，含端到端入群）。
## 应用信息与本机库解析（TASK-122，2026-10-18）

新增模块 [crates/ffi/src/native_path.rs](../../crates/ffi/src/native_path.rs)。

~~~text
app_info() -> AppInfoFfi { version, build_profile, ffi_version, platform }
native_library_path(file_name) -> String            // 命中的库路径；失败 NATIVE_LIBRARY_NOT_FOUND
~~~

- `app_info`：`version` 取 `env!("CARGO_PKG_VERSION")`；`build_profile` 为 `debug`/`release`
  （`cfg!(debug_assertions)`）；`ffi_version` 是**桥接契约版本**（`FFI_CONTRACT_VERSION = "1.0"`，
  记录/方法签名变化时递增，与库版本独立）；`platform` 是 `<OS>/<ARCH>`（`std::env::consts`）；
  纯编译期常量 + 环境常量，**不联网、不读库、不会失败**；
- `native_library_path` 候选顺序（**固定**）：可执行文件目录 → 其 `native/` → 其 `lib/` →
  环境变量 `CHAT_HOST_LIBRARY_DIR`（最后一个，部署方显式覆盖）；同一目录去重；
- **失败不静默回退**：返回 `NATIVE_LIBRARY_NOT_FOUND`，`reason` 里逐行列出**所有尝试过的路径**
  （便于定位"安装包里少了 dll"这类问题）；
- 纯函数拆分（`native_library_directories` / `resolve_in_directories`）便于用真文件系统单测。

### 证据

[crates/ffi/tests/app_info.rs](../../crates/ffi/tests/app_info.rs)（2 个）：字段非空、`version` 形如
`MAJOR.MINOR.PATCH` 且等于 crate 元数据、`build_profile` ∈ {@debug@, @release@}、`ffi_version` 一致、
`platform` 恰为 `<OS>/<ARCH>` 两段且与 `std::env::consts` 相符、重复调用稳定；
[crates/ffi/tests/native_path.rs](../../crates/ffi/tests/native_path.rs)（4 个，真文件系统）：
按序命中 exe 目录 → native/ → lib/ → 环境变量覆盖；全 miss 时错误列出每个候选路径；
`native_library_path` 的失败码与原因（含可执行文件目录）；环境变量名稳定。
模块内另有 3 个纯函数单测（含候选目录去重）。

复现：`cargo test --offline -p chat-ffi --test app_info --test native_path` → 6 passed；
`cargo test --offline -p chat-ffi` → **105 passed**（既有 96 继续通过）。

**已知限制**：只解析"文件是否存在"（不 dlopen 校验 ABI）；Android/iOS 的打包路径（assets/ framework）
需要各端在 `open_persisted` 之前显式设置 `CHAT_HOST_LIBRARY_DIR`，本层不做平台猜测。
## 身份登记进其它库（FIX-R179，2026-10-18）

~~~text
IdentityFfi.enroll_into_store(target_database_path, target_master_key_hex) -> AccountView
AccountView { account_id, device_id, display_name, fingerprint }
~~~

- 用途：消息库/群库/附件库需要本机身份（`load_identity_seeds`）才能提交，否则
  `INVALID_STATE_TRANSITION：the local identity is not enrolled`；
- 幂等（同账户同显示信息不写行、库版本不涨）；目标库绑定别的账户 → `IDENTITY_ACCOUNT_CONFLICT`
  原样透出、绝不覆盖；主密钥错/库损坏 → 存储层码原样透出；
- 需持久化且已打开的身份对象（纯内存 → `INVALID_STATE_TRANSITION`；关闭 → `CORE_CLOSED`）；
- 种子不进日志、不进返回值（返回脱敏 `AccountView`）；
- 测试：[crates/ffi/tests/identity_enroll.rs](../../crates/ffi/tests/identity_enroll.rs)（5 个，真库）。

## GROUPS-001b · FFI 群登记（2026-10-20）

新增 `GroupFfi::group_register_on_node(group_id, endpoint_url, node_id, node_public_key_hex) -> GroupRegistrationFfi`
（`GroupRegistrationFfi { group_id, registered_seq, node_id }`，纯公开数据、**不传任何密钥**）。

- 建群后必须调用一次，否则节点不认识该群、后续条目会被拒；
- 幂等：重复调用返回节点给出的同一结果（已登记时回读 head 取真实序号）；
- 错误：未登记身份 `INVALID_STATE_TRANSITION`、群不存在 `INVALID_TARGET`、不可达 `NODE_UNAVAILABLE`、节点拒绝原样透出。


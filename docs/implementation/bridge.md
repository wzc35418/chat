# FFI、回调、Flow 包装与资源生命周期

阶段：P0 → P2。状态：函数级设计；BRIDGE-001/002/003/004/005 的通用编排尚未实现，两个竖直切片的 UniFFI 对象已在 Windows x64 编译并实测：本地存储的 `LocalStoreFfi`（见“本地存储竖直切片（LocalStoreFfi）”）与身份/口令加好友的 `IdentityFfi`（见“身份与口令加好友竖直切片（IdentityFfi）”）。目标源码：[crates/ffi/src/bridge.rs](../../crates/ffi/src/bridge.rs)（P0 临时实验对象）、[crates/ffi/src/local_store.rs](../../crates/ffi/src/local_store.rs)（正式存储对象）与 [crates/ffi/src/identity_ffi.rs](../../crates/ffi/src/identity_ffi.rs)（身份/联系人对象）。

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
LocalStoreFfi::shutdown()         // 幂等，释放独占写锁
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

复现命令（全程离线，本轮实测）：

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi      # 2 passed（LocalStoreFfi 端到端 + 错误码映射）
cargo build --locked --offline -p chat-ffi   # 成功，Cargo.lock 未变化
~~~

证据：

- `local_store::tests::open_write_and_read_through_the_ffi_shape`：构造函数建库 → create_draft → draft_status → apply_verified_entry → list_messages（待发 + 正式）→ list_conversations（标题/预览/未读 None）→ close 后 `CORE_CLOSED`。
- `local_store::tests::failures_are_typed_with_stable_codes`：非规范会话 ID 与 `u64::MAX` 时间戳都返回 `FfiError::Store { code = "INVALID_INPUT", .. }`。

已知限制：本对象只覆盖竖直切片，没有事件订阅（BRIDGE-003/004/006）、没有异步/suspend 包装（BRIDGE-007）、没有平台回调（BRIDGE-001 的完整形态）；`#[uniffi::Object]` 内部的 `std::sync::Mutex` 会串行化所有调用，调用方不要在 UI 线程做长分页循环。Kotlin/Android 真机尚未实测（本轮只验证 Rust 侧编译与单元测试）。

## 身份与口令加好友竖直切片（IdentityFfi，Windows x64）

实现位置：[crates/ffi/src/identity_ffi.rs](../../crates/ffi/src/identity_ffi.rs)（`uniffi::Object`）。
与 `P0Core`、`LocalStoreFfi` 互不共享状态，但**复用同一个** `FfiError`（定义在
[local_store.rs](../../crates/ffi/src/local_store.rs)，UniFFI 允许同一 error 类型服务多个对象），
因此 Kotlin 侧仍然只 `catch (e: FfiException.Store)` 一处。方法名用 `shutdown`（不是 `close`），
避免与生成代码的 `AutoCloseable.close()` 冲突。

生成绑定的公开面（UniFFI 0.32，crate `chat-ffi`；与 Kotlin 侧逐字对接的形状）：

~~~text
#[derive(uniffi::Object)] IdentityFfi

#[uniffi::constructor]
IdentityFfi::create(display_name: String, node: String) -> Result<Arc<IdentityFfi>, FfiError>

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
cargo test --offline -p chat-ffi            # 12 passed（10 个 IdentityFfi + 2 个既有 LocalStoreFfi）
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

已知限制：

- **不持久化**：账户、设备密钥、联系人与口令消费状态都只在本次进程内（storage/PLATFORM-001 集成后补）。
  同一个进程里重启不了账户，也没有「我的口令」列表、撤销或一次性消费状态（identity 层同样未落盘）。
- **节点不接受昵称式字符串**：`node` 必须是 `node:1:<64 位小写 hex>`；两侧必须使用同一字符串，
  否则 `accept_invite` 返回 `INVALID_TARGET`。跨节点接受（设计稿把 node 当「提示」）留给后续
  多节点路由任务。
- **没有入站身份观测入口**：本切片只能通过口令建立 `requested` 联系人，没有任何入口把联系人推到
  `accepted` 或 `identity_changed`（那需要 core 的握手/凭据事件）。因此 `confirm_identity_change`
  当前只在「非法调用」路径上有测试；状态机本身（identity crate）已覆盖完整迁移。
- 没有事件订阅（BRIDGE-003/004/006）、没有 suspend 包装（BRIDGE-007）；`Mutex` 串行化所有调用，
  不要在 UI 线程做长循环。
- **Cargo.lock 变化**：`chat-ffi` 的依赖列表新增 `chat-identity`（path 依赖，指向已在 lock 中的
  工作区 crate），没有新增或改版本的外部包；`cargo build --locked` 通过。
- Kotlin/Android 真机尚未实测（本轮只验证 Rust 侧编译与单元测试）。



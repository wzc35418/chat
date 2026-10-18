# 核心接口与协议草案

版本：0.1 · 2026-10-18

本文件定义业务边界，代码是接口草案，不是已经编译或生成的 SDK。可省略的实现细节包括 DTO 的序列化注解、具体 FFI ABI、异常类型和完整枚举。Kotlin 接口位于 `shared/core-api`；原生导出接口和线协议各自维护，不直接将 UI DTO 当作网络格式。

## 1. 标识符与基础模型

所有 ID 在真实代码中使用独立类型，并验证长度、编码与版本。不要将任意用户输入的字符串直接当成合法身份。

```kotlin
data class AccountId(val value: String)
data class DeviceId(val value: String)
data class ConversationId(val value: String)
data class MessageId(val value: String)
data class DraftId(val value: String)
data class AttachmentId(val value: String)
data class PluginId(val value: String)
data class Cursor(val value: String)

enum class ConversationKind { DIRECT, GROUP, SELF_SYNC }
enum class GroupRole { OWNER, ADMIN, MEMBER }
enum class SendState { QUEUED, ACCEPTED, DELIVERED, FAILED }

sealed interface MessageContent {
    data class Text(val text: String) : MessageContent
    data class Attachment(
        val attachmentId: AttachmentId,
        val caption: String?
    ) : MessageContent
    data class Extension(
        val namespace: String,
        val schemaVersion: Int,
        val payload: ByteArray,
        val fallbackText: String
    ) : MessageContent
}

data class SendRequest(
    val conversationId: ConversationId,
    val clientOperationId: String,  // 每次用户意图唯一；防 UI 重复提交
    val content: MessageContent,
    val replyTo: MessageId? = null
)

data class SendTicket(val draftId: DraftId, val state: SendState)

data class MessageView(
    val id: MessageId,
    val conversationId: ConversationId,
    val author: AccountId,
    val authorDevice: DeviceId,
    val groupSequence: Long?,       // 节点接受后才有正式序号
    val content: MessageContent,
    val sendState: SendState,
    val createdAtMillis: Long,      // 作者时间仅用于展示，不作为权限顺序
    val origin: MessageOrigin
)

sealed interface MessageOrigin {
    data object User : MessageOrigin
    data class Plugin(val pluginId: PluginId) : MessageOrigin
}

data class Page<T>(val items: List<T>, val nextCursor: Cursor?)
```

DraftId 是本地发送意图；MessageId 是最终经过验证的应用事件 ID；SubmissionId 是一次具体密文提交的 ID。三者分开，旧 epoch 提交被拒绝后重新生成的密文不能伪装成同一密文重试。

同一 clientOperationId 的重复 UI 调用返回同一发送意图。网络重试使用同一 SubmissionId 与相同字节；重新加密产生新的 SubmissionId。最终 MessageId 与被接受密文中的认证事件 ID 绑定。

## 2. 应用核心接口

```kotlin
interface ChatCore {
    val identity: IdentityService
    val conversations: ConversationService
    val groups: GroupService
    val attachments: AttachmentService
    val sync: SyncService

    suspend fun start(options: CoreStartOptions)
    suspend fun close()

    // UI 通知流，不作为唯一的持久消息记录。
    fun observeEvents(): Flow<CoreEvent>
}

interface IdentityService {
    suspend fun createAccount(displayName: String): LocalAccount
    suspend fun currentAccount(): LocalAccount?
    suspend fun createContactInvite(expiresInSeconds: Long): Invite
    suspend fun acceptContactInvite(invite: String): ContactRequest
    suspend fun confirmContact(requestId: String): Contact
    suspend fun getVerificationFingerprint(accountId: AccountId): String

    // 新设备先生成本机密钥，主管设备经核对后签发凭据。
    suspend fun beginDeviceLink(): DeviceLinkRequest
    suspend fun authorizeDevice(request: DeviceLinkRequest): DeviceAuthorization
    suspend fun completeDeviceLink(result: DeviceAuthorization)
    suspend fun listDevices(): List<DeviceSummary>
    suspend fun revokeDevice(deviceId: DeviceId): RevocationJob

    // 显式用户操作；插件 SDK 不包含这些方法。
    suspend fun exportIdentityRecovery(destination: PlatformFileToken)
    suspend fun importIdentityRecovery(source: PlatformFileToken)
}

interface ConversationService {
    suspend fun openDirect(accountId: AccountId): ConversationId
    suspend fun listConversations(cursor: Cursor?, limit: Int): Page<ConversationView>
    suspend fun listMessages(
        conversationId: ConversationId,
        before: Cursor?,
        limit: Int
    ): Page<MessageView>
    suspend fun send(request: SendRequest): SendTicket
    suspend fun retry(draftId: DraftId): SendTicket
    suspend fun cancelQueued(draftId: DraftId)
    suspend fun getDraftStatus(draftId: DraftId): DraftStatus
    suspend fun getDelivery(messageId: MessageId): DeliverySummary
}

interface AttachmentService {
    // 文件选择由平台完成；只授权这一个文件，不传任意绝对路径。
    suspend fun importFile(source: PlatformFileToken): AttachmentId
    suspend fun status(id: AttachmentId): AttachmentStatus
    suspend fun download(id: AttachmentId)
    suspend fun exportFile(id: AttachmentId, destination: PlatformFileToken)
    suspend fun cancelTransfer(id: AttachmentId)
}

interface SyncService {
    fun observeStatus(): Flow<SyncStatus>
    suspend fun syncNow(): SyncResult
    suspend fun setLifecycle(state: AppLifecycleState)
    suspend fun setConnectivity(state: ConnectivityState)
    suspend fun configureNode(profile: NodeProfile)
}
```

未定义的 DTO 由实现补充，至少包括有效期、能力范围、错误码和操作状态。恢复导出需要用户明确发起并选择保存位置；备份文件本身采用独立口令/恢复密钥保护，不能以普通明文文件导出身份私钥。

revokeDevice 返回的是任务，不是“所有群瞬间撤销完成”。任务跟踪每个会话更新状态；协调节点不可用时显示待处理。cancelQueued 只取消尚未接受的操作，不保证撤回网络上已正式接受的消息。

## 3. 群服务接口

```kotlin
data class GroupSnapshot(
    val id: ConversationId,
    val name: String,
    val owner: AccountId,
    val members: List<GroupMember>,
    val controlHead: String,
    val policyVersion: Long,
    val mlsEpoch: Long,
    val verifiedSequence: Long,
    val coordinator: CoordinatorRef
)

data class GroupMember(
    val accountId: AccountId,
    val role: GroupRole,
    val deviceIds: List<DeviceId>,
    val muted: Boolean
)

sealed interface GroupOperation {
    data class InviteMember(val accountId: AccountId) : GroupOperation
    data class RemoveMember(val accountId: AccountId) : GroupOperation
    data class SetRole(val accountId: AccountId, val role: GroupRole) : GroupOperation
    data class SetMuted(val accountId: AccountId, val muted: Boolean) : GroupOperation
    data class Rename(val name: String) : GroupOperation
    data class TransferOwnership(val accountId: AccountId) : GroupOperation
    data object Leave : GroupOperation
    data object Dissolve : GroupOperation
}

interface GroupService {
    suspend fun create(request: CreateGroupRequest): ConversationId
    suspend fun snapshot(id: ConversationId): GroupSnapshot
    suspend fun propose(
        id: ConversationId,
        expectedControlHead: String,
        operation: GroupOperation,
        clientOperationId: String
    ): GroupOperationTicket
    suspend fun acceptInvite(inviteId: String): GroupJoinTicket
    suspend fun declineInvite(inviteId: String)
    suspend fun operationStatus(ticketId: String): GroupOperationStatus
}
```

propose 不是直接修改本地正式成员列表。内核校验权限、收集必要 KeyPackage、构建控制事件和 MLS Commit，节点确认后再推进正式快照。成员加入前需要接受邀请，不能将可联系用户无条件加入群。

发生 `STALE_CONTROL_HEAD` 或 `STALE_EPOCH` 时，先同步再重建操作；如果新状态已不允许该操作，返回权限错误，不盲目重试。普通管理操作不自动转为更高权限操作。

## 4. 核心事件与错误

```kotlin
sealed interface CoreEvent {
    data class ConversationChanged(val id: ConversationId) : CoreEvent
    data class MessagesChanged(val id: ConversationId) : CoreEvent
    data class DraftChanged(val id: DraftId) : CoreEvent
    data class GroupChanged(val id: ConversationId) : CoreEvent
    data class AttachmentChanged(val id: AttachmentId) : CoreEvent
    data class IdentityWarning(val accountId: AccountId, val code: String) : CoreEvent
    data class ResyncRequired(val reason: String) : CoreEvent
}

enum class CoreErrorCode {
    NETWORK_UNAVAILABLE, NODE_UNAVAILABLE, RATE_LIMITED, QUOTA_EXCEEDED,
    PERMISSION_DENIED, DEVICE_REVOKED, CONTACT_NOT_ACCEPTED,
    STALE_CONTROL_HEAD, STALE_EPOCH, MISSING_LOG_ENTRIES,
    INVALID_CREDENTIAL, INVALID_CIPHERTEXT, LOG_CONFLICT,
    PROTOCOL_UNSUPPORTED, CONTENT_TOO_LARGE,
    STORAGE_LOCKED, STORAGE_FAILURE, STATE_CORRUPTED
}
```

事件通知使用有界队列；溢出时发出重新查询标记，不能静默漏掉永久变化。返回前已完成本地事务的方法，即使 UI 回调未送达，也可以通过查询恢复状态。

错误应附 retryable、retryAfter、operationId 等字段。节点不可用属于等待/重试；签名冲突与损坏状态不能靠无限重试掩盖。

## 5. 平台与 FFI 边界

```kotlin
interface PlatformKeyProtector {
    suspend fun wrap(keyAlias: String, secret: ByteArray): WrappedSecret
    suspend fun unwrap(keyAlias: String, wrapped: WrappedSecret): ByteArray
    suspend fun delete(keyAlias: String)
}

interface PlatformNotifications {
    suspend fun show(notification: LocalNotification)
    suspend fun cancel(id: String)
}

interface PlatformFiles {
    suspend fun chooseImportFile(): PlatformFileToken?
    suspend fun chooseExportFile(suggestedName: String): PlatformFileToken?
}
```

PlatformKeyProtector 属于可信核心平台适配，暂时传入的秘密会出现在进程内存中；ByteArray 不是硬件隔离或自动安全清零的保证。实际实现评估敏感缓冲区生命周期，禁止记录这些参数。Linux 密钥服务不可用时显示锁定或要求用户设置口令，不能静默保存明文密钥。

FFI 导出使用简单 DTO、句柄和可释放回调。Kotlin coroutine/Flow 由 bridge 层包装，Rust 不直接导出 Kotlin Flow。Android URI 和桌面文件访问通过受控文件句柄或流适配，不让 FFI 猜测平台 URI 的语义。原生 panic 必须按绑定框架约束转换，禁止越过 FFI 边界传播。

## 6. 桌面插件 SDK

MVP 接口面向可信 Kotlin 插件。以下能力范围对正常插件是 API 约定，不是恶意 JVM 代码的安全沙箱。

```kotlin
interface DesktopPlugin {
    val id: String
    fun start(context: PluginContext)
    fun stop()
}

interface Registration { fun dispose() }

interface PluginContext {
    val commands: CommandRegistry
    val messageActions: MessageActionRegistry
    val settings: PluginSettings
    val messages: PluginMessageApi
    val logger: RedactedPluginLogger
    val tasks: PluginTaskScope
}

interface CommandRegistry {
    fun register(command: CommandDefinition): Registration
}

data class CommandDefinition(
    val name: String,
    val description: String,
    val execute: suspend (CommandInvocation) -> CommandResult
)

interface MessageActionRegistry {
    fun register(action: MessageActionDefinition): Registration
}

interface PluginMessageApi {
    // 只能通过有效用户动作取得上下文；示例不含全库读取 API。
    suspend fun selectedContent(context: UserActionToken): SelectedContent
    suspend fun proposeSend(
        context: UserActionToken,
        content: MessageContent
    ): SendProposal
}
```

proposeSend 默认填入编辑区，由用户确认后进入 ChatCore；MVP 不开放全群后台消息监听和自动发送。插件来源写入本地诊断和应用消息来源。未来机器人自动发送使用独立账户与明确授权，不借用 UI 操作令牌永久续期。

PluginTaskScope 中的任务在 stop 时取消，注册资源统一 dispose。对可信插件，任务取消是合作式的，宿主无法保证恶意插件终止。宿主不在 UI 线程调用未知耗时逻辑。

清单示例：

```json
{
  "id": "dev.example.message-formatter",
  "name": "消息格式工具",
  "version": "0.1.0",
  "sdkVersion": "1",
  "runtime": "trusted-jvm",
  "entrypoint": "dev.example.FormatterPlugin",
  "platforms": ["windows-x64", "linux-x64"],
  "requestedCapabilities": [
    "messages.selected.read",
    "messages.draft.propose"
  ],
  "contributions": ["commands", "message-actions", "settings"]
}
```

清单能力名只是 SDK v1 的草案；真实 schema 需包含字段长度、重复项、必需字段、兼容性和包大小约束。已授权能力由用户配置保存，不接受清单自称已授权。

## 7. 传输端口与节点端口

Rust 内部可按以下逻辑端口编排；它们不要求逐字导出到 Kotlin。

```rust
// 逻辑伪代码：未给出 async trait、错误与所有权的具体实现。
trait PeerTransport {
    async fn connect(&self, peer: VerifiedPeerAddress) -> Connection;
    async fn send(&self, peer: DeviceId, frame: ProtocolFrame) -> TransportAck;
    async fn shutdown(&self);
}

trait MailboxClient {
    async fn put(&self, item: AuthorizedMailboxItem) -> MailboxReceipt;
    async fn pull(&self, cursor: Option<Cursor>, limit: u32) -> MailboxPage;
    async fn ack(&self, item_ids: Vec<MailboxItemId>);
}

trait GroupLogClient {
    async fn append(&self, candidate: SignedCandidate) -> AppendResult;
    async fn fetch(&self, group: GroupId, after: u64, limit: u32) -> LogPage;
    async fn head(&self, group: GroupId) -> SignedLogHead;
}

trait CryptoEngine {
    fn prepare_application(&mut self, input: PlainApplicationEvent) -> PreparedWrite;
    fn prepare_control(&mut self, op: AuthorizedGroupOperation) -> PreparedWrite;
    fn process_verified_entry(&mut self, entry: VerifiedLogEntry) -> PreparedWrite;
}

trait CoreTransaction {
    fn persist_crypto_records(&mut self, changes: CryptoStorageChanges);
    fn persist_domain_changes(&mut self, changes: DomainChanges);
    fn enqueue_outbox(&mut self, job: OutboxJob);
    fn commit(self) -> Result<(), StorageError>;
}
```

PreparedWrite 是事务编排意图，不是先修改共享内存状态再随意提交。真实实现要让 crypto storage 的读写处于同一事务/串行会话任务中，并以提交成功后发布新内存状态。每个 MLS 群由单一串行执行器处理，防止并发任务交叉推进状态。

## 8. 线协议模型

协议采用有界、版本化的确定编码，签名基于规范编码的字节并加入协议域分隔符。字段排序、可选值、整数范围和签名覆盖范围在正式规范中冻结；不能直接对不确定键顺序的 JSON 字符串签名。

```text
SignedCandidate
  protocol_version
  submission_id
  group_id
  author_device_id
  kind                       # APPLICATION / CONTROL / DELIVERY_RECEIPT
  expected_control_head
  policy_version
  mls_epoch
  encrypted_payload
  control_metadata?          # 仅 CONTROL，含被授权的单一变更描述
  author_signature           # 覆盖以上字段及 payload 摘要

AcceptedLogEntry
  signed_candidate
  group_sequence
  previous_log_hash
  coordinator_signature      # 绑定候选全文摘要、序号、前序摘要与群 ID

MailboxItem
  item_id
  target_device_id
  accepted_log_entry_or_welcome
  delivery_capability
  expires_at
```

序号在节点接受后产生，因此不放入已经加密的发送正文；节点签名回执将正文摘要与序号绑定。MLS 解密后验证内部事件身份、作者和会话绑定，外层署名不能替代 MLS 凭据验证。

MVP APPEND 处理步骤：

1. 验证传输鉴权、作者凭据、签名、长度和速率限制。
2. 对相同 SubmissionId 返回原结果；相同 ID 不同摘要拒绝。
3. 在群事务锁下校验 current control_head/epoch 与角色、禁言等公开策略。
4. 为合法候选追加唯一序号，持久化日志及控制前置状态变更，返回签名回执。
5. 客户端取得缺失条目，验证签名链、业务权限、MLS 内容与控制结果。
6. 客户端在本地事务中推进验证游标、加密状态、消息物化视图及回执任务。

节点无法解密 MLS Commit，因此其检查不能证明密码学控制结果正确。恶意或错误控制事件由客户端检测；这种情况下暂停错误分支，而不是继续接受未知加密状态。实际成员变更、重加入和 pending Commit 恢复需要在正式协议中给出测试向量。

## 9. 节点 HTTP/WS 接口

下列路由定义资源和职责；鉴权字段和错误结构属于正式线协议，不采用“传个账户 ID 就算登录”。P2P 协议承载相同逻辑帧，避免维护两套消息语义。

| 方法与路径 | 职责 |
|---|---|
| GET /v1/capabilities | 协议主版本、大小限制、节点公钥与能力 |
| POST /v1/auth/challenges | 一次性鉴权挑战，限制滥用 |
| POST /v1/devices/register | 注册经验证凭据及邀请能力 |
| PUT /v1/discovery/{deviceId} | 发布签名设备地址记录 |
| GET /v1/discovery/{deviceId} | 按访问策略查询记录 |
| POST /v1/key-packages | 发布设备签名的 MLS KeyPackage |
| POST /v1/key-packages/claim | 按授权规则原子取出一次性 KeyPackage |
| POST /v1/mailbox/items | 持有效投递能力写入目标信箱 |
| GET /v1/mailbox/items?cursor=… | 当前设备拉取，分页与字节上限 |
| POST /v1/mailbox/acks | ACK 指定设备自己的信箱条目 |
| POST /v1/groups | 创建群及固定协调关系 |
| POST /v1/groups/{id}/entries | 幂等追加候选，返回接受结果或冲突 |
| GET /v1/groups/{id}/entries?after=… | 拉取获准范围内的群日志 |
| GET /v1/groups/{id}/head | 取得签名日志头，便于对账 |
| POST /v1/blobs/uploads | 建立受配额限制的密文分块上传 |
| PUT /v1/blobs/uploads/{id}/chunks/{index} | 幂等上传指定密文块 |
| POST /v1/blobs/uploads/{id}/complete | 校验密文清单、提交对象 |
| GET /v1/blobs/{id}/chunks/{index} | 持访问能力读取密文块 |
| GET /v1/events（WebSocket） | 有更新的提示，断线后仍以游标拉取 |

设备鉴权绑定请求方法、规范路径、请求摘要、节点身份、一次性挑战和有效期，防止跨节点与跨请求重放。WebSocket 升级使用短期、一次性授权，禁止把长期秘密放进 URL。管理接口与用户接口分开，默认不公开管理权限。

新增群成员不等于获得整个历史日志与附件下载能力；节点按加入位置和群历史策略提供范围。邀请/Welcome 通过目标设备授权信箱投递。元数据可见范围与内容历史范围需要分别校验。

## 10. 版本约定

- protocol major：核心线协议不兼容变化。
- content schema：结构化消息类型单独升级，未知类型显示 fallbackText。
- plugin SDK：与线协议无关；首版精确约束 SDK v1 与受支持 JVM/Kotlin 工具链。
- database schema：单调迁移，失败时不半应用；迁移前备份策略需实现。
- 能力协商：安全必需能力不支持则明确拒绝，不能自动关闭 E2EE。

完整错误、凭据规范、设备撤销传播和群恢复测试向量应在 `docs/protocol/` 固化。本草案用于划分工程边界，不能替代密码学协议审查或真实互操作测试。

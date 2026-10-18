# 类型字典与边界约束

这里定义函数计划中的设计类型；没有列出的 Context/Options/Report 在实施对应函数时补入版本化 DTO。它们不是已经生成的 SDK。用不同类型表达未验证/已验证/已授权/已提交状态，不以布尔 validated 字段替代安全边界。Rust 返回 Result<T, CoreError>；Kotlin 用稳定错误类型或等价结果包装。

## 身份、版本和时间

|类型|字段/语义|约束|
|---|---|---|
|AccountId|规范账户长期公钥派生值|域分隔、算法、编码、前缀在协议 P0 冻结；不同于显示名/节点地址|
|DeviceId|账户签发的设备身份|凭据绑定设备签名公钥和 EndpointId；每账户最多2有效设备|
|EndpointId|iroh 端点公开标识|不能自动视为账户授权|
|GroupId / ConversationId|协议群与 UI 会话关联|DIRECT/GROUP/SELF_SYNC 分开类别，共用协调顺序模型|
|CoordinatorId / PinnedNodeKey|固定协调节点身份与固定公钥|更换 URL 不自动更换节点公钥；旧群不隐式迁移|
|EventId / MessageId|MLS 内层认证后的应用事件身份|MessageId 与最终被接受尝试的 EventId 绑定；不能用外层伪造 ID 永久去重|
|DraftId|本地用户发送意图|重建尝试保持同 DraftId，正式 MessageId 可能晚到|
|OperationId|用户或后台业务动作幂等键|以本账户/动作范围唯一；同键不同请求摘要拒绝|
|SubmissionId / AttemptId|一份固定密文提交与本地尝试记录|网络重试同 ID 同字节；重加密换 SubmissionId；草稿到尝试是一对多|
|ControlHead|最后正式控制状态摘要/标识|与全部日志头 previous_log_hash 不同，不能混用|
|ProtocolVersion / SchemaVersion|线协议和数据库版本|分别版本化；安全必需能力未知则拒绝|
|Time / Duration|UTC 时间与相对时长|作者时间仅展示；权限顺序由 seq；过期允许偏差的策略冻结|
|Digest / CanonicalBytes|冻结算法的摘要与确定编码|每用途域分隔；不直接签键顺序不定的 JSON|

## 凭据与信任

DeviceCredential 至少包含 account_id、账户公钥或受信引用、device_id、设备签名公钥、endpoint_id、授权版本、not_before、expires_at、主管签名。VerifiedCredential 是通过可信账户根、签名、绑定、期限、撤销检查后的不可变内部类型。

TrustAnchor 只来自本机创建、用户核验邀请或明确确认的现有信任。联系人服务器记录不能静默成为新的根。Actor 是内核从可信凭据建立的账户+设备+必要权限，UI/插件不得指定自己充当别的 actor。UserConfirmation 绑定本次敏感操作摘要及短期有效时间，不能作为万能持久权限。

RecoverySecret / FileKey / SecretBytes / SecretBuffer / DeviceKeyRef 只用于可信核心路径，禁止普通 DTO 序列化或日志。存储主密钥与身份恢复文件密钥独立；系统封装、恢复 KDF、AEAD 的算法与资源预算另有 ADR。JVM ByteArray 无法保证硬件隔离或所有副本清零。

## 消息与投递

SendRequest：conversationId、clientOperationId、MessageContent、replyTo、可信调用路径设置的 MessageOrigin。Text 保留用户输入；Attachment 引用已完成加密上传的文件描述；Extension 含 namespace/schemaVersion/operationId/objectId/payload/fallbackText。64 KiB 限制在完整规范序列化后计算，同时受双方/节点较低限额约束。

SendTicket：draftId、sendState、可选等待原因。DraftStatus 补充 latestAttempt、pendingDependency、error、availableActions。MessageView 只含可见内容、作者账户/设备、正式序号或本地 pending 投影、展示时间、来源、送达状态；没有 MLS 对象或数据库句柄。

|证据类型|精确定义|可影响的用户状态|
|---|---|---|
|TransportAck|对端连接接收字节|仅网络诊断|
|MailboxReceipt|节点持久保存密文|显示信箱已存储，不能算送达|
|VerifiedAcceptedReceipt|节点签名绑定全文候选摘要/群/seq/前序摘要，客户端验证过|ACCEPTED|
|AuthenticatedReceipt|对方获准设备验证且本地持久化后的认证回执|DELIVERED；按账户聚合|
|ReadReceipt|显式已读事件|不进入 MVP|

DeliverySummary：按 message→device→account 的映射；对方任一获准设备成功即可计该账户一次；本人其他设备不计对方数量。RecipientSnapshot 固定消息时点的设备/账户范围，不能用今天成员表推断昨天收件人。

## 线协议验证类型

UnverifiedFrame → VerifiedCandidate → VerifiedLogEntry → FullyVerifiedEntry 是不同阶段。VerifiedLogEntry 只说明节点链与外层身份验证通过，不说明 MLS 解密/权限结果已通过；FullyVerifiedEntry 才能交 storage 正式应用。

SignedCandidate 字段沿原设计：protocol_version、submission_id、group_id、author_device_id、kind、expected_control_head、policy_version、mls_epoch、encrypted_payload、control_metadata?、author_signature。外层 ControlMetadata 描述单一受授权业务变更，不放群名正文；Rename 新名在密文中，公开层仅保留所需操作种类/摘要。

AcceptedLogEntry：完整候选 + group_sequence + previous_log_hash + coordinator_signature。节点签名覆盖候选全文摘要、群、序号、前序摘要。SignedLogHead 另用签名域并绑定当前 control_head/policy/epoch。日志摘要算法、初始 head、控制头更新规则冻结在 docs/protocol/WIRE_FORMAT.md。

## 群操作与状态

GroupSnapshot：id、kind、owner、members、controlHead、policyVersion、mlsEpoch、verifiedSequence、verifiedLogHash、coordinator、dissolved、pausedReason。GroupMember：accountId、role、deviceIds、muted、membershipWindow。

GroupOperation：InviteMember、RemoveMember、SetRole、SetMuted、Rename、TransferOwnership、Leave、Dissolve；设备授权/撤销产生显式 SystemDeviceAdd/SystemDeviceRemove 内部动作，仍需要协议定义的授权证明，不能借“system”绕过权限。

MVP 选择所有正式控制事件进入 MLS 的一个单一批次：包含业务意图与必要 Commit，角色/禁言/群名没有成员差异时采用库支持的合法空更新 Commit 或经 P0 验证的等价绑定策略。控制是否总推进 epoch 必须在 P0 ADR 固定；未固定前不得编写节点猜测 epoch += 1 的生产逻辑。批次格式与客户端实际结果对照见协议草案。

PreparedControl / PendingCommit：本机尚未被正式接受的 Commit 状态、候选摘要、前置 head/epoch、预期成员差异、待投递 Welcome。StagedCommit：OpenMLS 验证后但尚未 merge 的结果。VerifiedControlResult：实际设备差异与业务授权一致。GroupOperationTicket：意图持久化的票据，不表示生效。

## 事务与调度

CoreTransaction：受限单写事务，含 scope、group、expectedVersion、TxnCryptoProvider，不能跨 FFI。CryptoStorageChanges 是 provider 新增/更新/删除集合。PreparedWrite 含同事务产生的 crypto/domain/message/outbox 变更，不能与别的事务混合。CommitToken 只能由成功提交返回。

OutboxJob：jobId、kind、dedupKey、payloadRef、groupId?、dependsOn?、state、retryCount、retryAt、leaseOwner?、leaseUntil?、leaseVersion、cancelRequested。LeasedJob 附当前租约版本，旧 worker 不能更新新 lease。

CursorScope：账户/设备/会话/资源/排序版本。CursorPosition 使用稳定键集，不采用易跳页 OFFSET；客户端 pending 与正式 seq 的合并投影排序规则另定。MailboxCursor 与 verified group cursor 是两个不同类型，不能互换。

## 文件、插件和可观测性

ChunkContext：file_id、index、total_chunks、plain_length、is_final、format_version；认证所有边界避免重排/截断。PublicBlobDescriptor/CipherManifest 只有密文块数/大小/摘要，不含明文摘要/文件密钥；SecretDescriptor 在 E2EE 内含文件钥、原文摘要和下载能力。

FileToken：平台签发、短期、受用途限制的不透明 ID；不能构造任意路径。ControlledStream 需支持关闭/取消/有限读写。UserActionToken 绑定 pluginId、conversationId、selection、动作与期限；不能永久复用。Registration 有 owner 与幂等 dispose。

Metrics 只存聚合延迟/计数/失败类别/路径比例，不放原始正文/私钥/完整联系人关系。FaultPoint 用稳定枚举标识加密、DB提交、网络回应、ACK、文件发布等注入点。测试时钟与测试 RNG 可注入；生产路径必须使用安全随机。

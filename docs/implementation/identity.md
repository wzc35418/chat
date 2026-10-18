# 账户、联系人、双设备与恢复

阶段：P1 → P2。状态：函数级设计，尚未实现。目标源码：[crates/identity/src/service.rs](../../crates/identity/src/service.rs)。

AccountId 从规范长期公钥派生，设备密钥与传输端点分别绑定。初始设备保管主管身份密钥，第二设备不自动取得它。所有敏感读写经 storage 和系统密钥保护；签名与恢复 KDF 算法在 P0 锁定，不在此自造。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [IDENTITY-001 create_account](identity.md#identity-001)
- [IDENTITY-002 current_account](identity.md#identity-002)
- [IDENTITY-003 verify_credential](identity.md#identity-003)
- [IDENTITY-004 create_contact_invite](identity.md#identity-004)
- [IDENTITY-005 accept_contact_invite](identity.md#identity-005)
- [IDENTITY-006 confirm_contact](identity.md#identity-006)
- [IDENTITY-007 verification_fingerprint](identity.md#identity-007)
- [IDENTITY-008 begin_device_link](identity.md#identity-008)
- [IDENTITY-009 authorize_device](identity.md#identity-009)
- [IDENTITY-010 complete_device_link](identity.md#identity-010)
- [IDENTITY-011 list_devices](identity.md#identity-011)
- [IDENTITY-012 revoke_device](identity.md#identity-012)
- [IDENTITY-013 export_identity_recovery](identity.md#identity-013)
- [IDENTITY-014 import_identity_recovery](identity.md#identity-014)

<a id="identity-001"></a>

## IDENTITY-001 · create_account

~~~text
create_account(display_name: String, protector: KeyProtector) -> Result<LocalAccount>
~~~

**职责与输出**：原子建立账户、主管设备凭据和受保护密钥引用。

**输入与前置条件**：尚无本地账户；显示名有长度限制；安全随机与密钥保护可用。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验显示名。
2. 生成账户/设备密钥。
3. 派生 AccountId。
4. 签发初始设备凭据。
5. 包装存储主密钥。
6. 原子写入账户和凭据。

**持久化、并发与副作用**：T1：密钥引用/账户/初始凭据一事务；平台包装失败不写明文；孤立包装槽补偿删除。

**失败处理**：ACCOUNT_EXISTS，STORAGE_LOCKED，KEY_PROTECTION_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLATFORM-001 wrap_storage_key](platform.md#platform-001)、[IDENTITY-003 verify_credential](identity.md#identity-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 创建中崩溃只恢复完整账户。
- [ ] 昵称改变不影响 ID。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-002"></a>

## IDENTITY-002 · current_account

~~~text
current_account() -> Result<Option<LocalAccount>>
~~~

**职责与输出**：返回脱敏账户视图，无账户返回 None，锁定返回专门错误。

**输入与前置条件**：storage 已打开或明确处于锁定状态。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查锁状态。
2. 读取账户元数据。
3. 映射显示名/ID/设备摘要。
4. 过滤全部密钥字段。

**持久化、并发与副作用**：只读快照；不触发建号。

**失败处理**：STORAGE_LOCKED，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 首次启动 None。
- [ ] 数据库锁定不能伪装成无账户。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-003"></a>

## IDENTITY-003 · verify_credential

~~~text
verify_credential(credential: DeviceCredential, trust: TrustAnchor, now: Time) -> Result<VerifiedCredential>
~~~

**职责与输出**：校验设备公钥、端点、账户和有效期绑定。

**输入与前置条件**：主管账户公钥已由邀请/本地信任锚确定，时间偏差策略已配置。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 有界解码。
2. 重算账户 ID。
3. 验证账户签名。
4. 检查有效期/撤销版本。
5. 绑定设备签名公钥与 EndpointId。

**持久化、并发与副作用**：纯验证；服务端新公钥不能覆盖既有信任。

**失败处理**：INVALID_CREDENTIAL，DEVICE_REVOKED，IDENTITY_CHANGED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-001 parse_id](protocol.md#protocol-001)、[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 替换端点/设备公钥拒绝。
- [ ] 时间偏差边界。
- [ ] 信任根不同明确提示。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-004"></a>

## IDENTITY-004 · create_contact_invite

~~~text
create_contact_invite(expires_in: Duration) -> Result<ContactInvite>
~~~

**职责与输出**：生成带公钥指纹、节点提示和受限投递能力的一次性邀请。

**输入与前置条件**：已解锁账户；邀请有效期在本地/节点允许范围。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 分配邀请 ID/随机能力。
2. 限定 contact-request 作用域。
3. 签名正文。
4. 本地持久化期限和消费状态。
5. 编码二维码文本。

**持久化、并发与副作用**：T1：邀请记录先落盘后展示；失败不泄漏可用未记录能力。

**失败处理**：INVALID_EXPIRY，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 过期与重复消费拒绝。
- [ ] 邀请不能直接授权所有群/旧历史。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-005"></a>

## IDENTITY-005 · accept_contact_invite

~~~text
accept_contact_invite(encoded: String) -> Result<ContactRequest>
~~~

**职责与输出**：建立待确认联系人请求，不自动信任身份变化。

**输入与前置条件**：来自二维码/粘贴的输入完全不可信。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 限长解码。
2. 验证邀请签名与期限。
3. 核对已有联系人指纹。
4. 保存待确认请求。
5. 提交受限请求消息。

**持久化、并发与副作用**：本地请求与 outbox 同事务；网络失败仍可恢复待处理状态。

**失败处理**：INVITE_EXPIRED，IDENTITY_CHANGED，INVALID_CAPABILITY。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)、[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重复扫描返回同请求。
- [ ] 换公钥进入警告。
- [ ] 未确认不能正式私聊。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-006"></a>

## IDENTITY-006 · confirm_contact

~~~text
confirm_contact(request_id: RequestId) -> Result<Contact>
~~~

**职责与输出**：记录用户确认的信任锚和正常投递能力。

**输入与前置条件**：用户已核验 UI 展示的公钥指纹/请求 ID。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 锁请求。
2. 核对未过期未被拒绝。
3. 保存信任锚。
4. 完成双向能力握手。
5. 设置联系人 accepted。

**持久化、并发与副作用**：T1：联系人接受与待发送确认同事务；对端状态未完成时显示等待。

**失败处理**：CONTACT_REQUEST_NOT_FOUND，INVITE_EXPIRED，IDENTITY_CHANGED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重复确认幂等。
- [ ] 离线确认不假称对端已完成。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-007"></a>

## IDENTITY-007 · verification_fingerprint

~~~text
verification_fingerprint(account_id: AccountId) -> Result<String>
~~~

**职责与输出**：返回稳定可人工核验指纹。

**输入与前置条件**：账户公钥来自已固定信任记录。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取公钥。
2. 加入 fingerprint 用途域。
3. 按锁定算法计算。
4. 分组格式化。
5. 返回可扫码版本。

**持久化、并发与副作用**：只读；显示名/节点地址不参与指纹。

**失败处理**：CONTACT_NOT_FOUND，STORAGE_LOCKED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 更换节点指纹不变。
- [ ] 公钥改变指纹改变。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-008"></a>

## IDENTITY-008 · begin_device_link

~~~text
begin_device_link() -> Result<DeviceLinkRequest>
~~~

**职责与输出**：第二设备生成本机密钥和一次性短期配对请求。

**输入与前置条件**：未关联账户；设备本地密钥保护可用。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 生成设备/端点密钥。
2. 创建 nonce 和短期请求。
3. 持久化本地待配对秘密。
4. 输出公钥/端点/验证码二维码。

**持久化、并发与副作用**：本机待配对记录原子写入；私钥永不进入二维码。

**失败处理**：LINK_ALREADY_ACTIVE，KEY_PROTECTION_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLATFORM-001 wrap_storage_key](platform.md#platform-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重启恢复配对。
- [ ] 过期必须重新生成。
- [ ] 二维码无私钥。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-009"></a>

## IDENTITY-009 · authorize_device

~~~text
authorize_device(request: DeviceLinkRequest, confirmation: UserConfirmation) -> Result<DeviceAuthorization>
~~~

**职责与输出**：初始主管设备签发第二设备授权及会话更新任务。

**输入与前置条件**：持有主管身份密钥；用户已核对短码；当前有效设备少于2。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验请求持有证明与期限。
2. 比较确认绑定摘要。
3. 签发设备凭据。
4. 登记设备。
5. 为全部会话生成加入任务。

**持久化、并发与副作用**：凭据/设备计数/任务一事务；网络副作用提交后进行。

**失败处理**：DEVICE_LIMIT_EXCEEDED，LINK_EXPIRED，INVALID_PROOF。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)、[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 两次并发授权仅允许一台新增设备。
- [ ] 管理员身份不能替代账户主管授权。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-010"></a>

## IDENTITY-010 · complete_device_link

~~~text
complete_device_link(result: DeviceAuthorization) -> Result<LinkStatus>
~~~

**职责与输出**：新设备保存自己的已授权凭据，逐会话等待 Welcome。

**输入与前置条件**：result 与本机待配对 nonce、公钥、端点完全匹配。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证主管签名。
2. 检查一次性关联。
3. 保存账户公钥和设备凭据。
4. 消费 nonce。
5. 建立 self-sync 加入任务。

**持久化、并发与副作用**：T1：凭据与 nonce 消费原子化；不会获得主管私钥/旧历史。

**失败处理**：INVALID_CREDENTIAL，LINK_MISMATCH，LINK_EXPIRED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 授权给不同公钥拒绝。
- [ ] 重放不创建重复设备。
- [ ] 加入前历史不自动出现。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-011"></a>

## IDENTITY-011 · list_devices

~~~text
list_devices() -> Result<Vec<DeviceSummary>>
~~~

**职责与输出**：列出设备、撤销状态和各会话密钥更新进度。

**输入与前置条件**：账户已解锁；查询仅返回公有摘要。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读当前凭据。
2. 关联撤销任务。
3. 区分有效/已撤销/待更新。
4. 返回按稳定 ID 排序视图。

**持久化、并发与副作用**：只读快照；不以“最近在线”推断凭据有效。

**失败处理**：STORAGE_LOCKED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 离线设备仍显示。
- [ ] 部分群撤销未完成可追踪。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-012"></a>

## IDENTITY-012 · revoke_device

~~~text
revoke_device(device: DeviceId) -> Result<RevocationJob>
~~~

**职责与输出**：持久标记撤销并建立每会话移除设备任务。

**输入与前置条件**：主管权限明确；默认防止误删唯一可管理设备。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对归属/授权。
2. 递增撤销版本。
3. 禁用本地新投递能力。
4. 枚举相关会话。
5. 为每群排队密钥更新并发布撤销记录。

**持久化、并发与副作用**：T1：撤销/子任务/outbox 原子化；跨群完成非原子，显示每群状态。

**失败处理**：PERMISSION_DENIED，DEVICE_NOT_FOUND，LAST_DEVICE_REQUIRES_RECOVERY。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[SYNC-001 enqueue_job](sync.md#sync-001)、[GROUPS-003 propose_group_operation](groups.md#groups-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 部分节点离线任务保持 pending。
- [ ] 完成的群后续消息撤销设备不可解密。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-013"></a>

## IDENTITY-013 · export_identity_recovery

~~~text
export_identity_recovery(destination: FileToken, secret: RecoverySecret) -> Result<ExportReceipt>
~~~

**职责与输出**：导出只含身份恢复材料的认证加密文件。

**输入与前置条件**：显式用户操作；恢复秘密经安全输入取得；token 仅授权目标文件。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取主管材料。
2. 选择版本化 KDF/AEAD。
3. 生成独立盐和参数。
4. 加密认证。
5. 写临时受控文件并同步后原子完成。
6. 清理缓冲区。

**持久化、并发与副作用**：外部文件不在 DB 事务；成功后记录无秘密的导出时间；失败清临时文件。

**失败处理**：PERMISSION_DENIED，WEAK_RECOVERY_SECRET，FILE_IO_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLATFORM-006 open_file_token](platform.md#platform-006)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 错误口令不泄漏明文。
- [ ] 断电不产出貌似有效半文件。
- [ ] 文件不包含聊天历史。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-014"></a>

## IDENTITY-014 · import_identity_recovery

~~~text
import_identity_recovery(source: FileToken, secret: RecoverySecret) -> Result<RecoveryStatus>
~~~

**职责与输出**：恢复身份并生成新的本机设备，恢复后仍需处理旧设备和群加入。

**输入与前置条件**：无冲突本地身份；导入参数和 KDF 资源有上限。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 有界读取。
2. 验证版本/参数预算。
3. 解密并校验身份。
4. 生成新设备。
5. 包装存储密钥。
6. 原子保存。
7. 创建后续撤销/重新加入向导。

**持久化、并发与副作用**：账户/新设备一事务；失败不覆盖原账户；不能仅有身份钥就假称已有各群 MLS 状态。

**失败处理**：RECOVERY_AUTH_FAILED，RECOVERY_LIMIT_EXCEEDED，IDENTITY_CONFLICT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLATFORM-006 open_file_token](platform.md#platform-006)、[IDENTITY-001 create_account](identity.md#identity-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 伪造 KDF 极大内存拒绝。
- [ ] 错误口令无副作用。
- [ ] 旧历史仍不可恢复。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。


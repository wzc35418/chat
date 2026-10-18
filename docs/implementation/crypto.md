# MLS、单群串行状态与附件密码学

阶段：P0 → P3。状态：函数级设计；CRYPTO-011/012 附件分块 AEAD 与 P0-04 存储主密钥/敏感列加密基础已实现（Windows x64 实测，见下文实现记录），CRYPTO-001..010 的 MLS 语义仍未实现。目标源码：[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)、[crates/crypto/src/attachment.rs](../../crates/crypto/src/attachment.rs) 与 [crates/crypto/src/key_store.rs](../../crates/crypto/src/key_store.rs)。

必须使用成熟实现。TxnCryptoProvider 在同一 storage 事务中读写；如果 OpenMLS provider 不能安全接入，则先 ADR 决策并实现幂等日志恢复再继续。PreparedWrite 不是已正式发布的新内存状态。每群串行执行，网络等待不跨持有 SQLite 写事务。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## P0-04 存储主密钥与敏感列加密基础（2026-10-18，Windows x64）

CRYPTO-004/006/007/008 的 MLS 语义仍未实现；本轮实现的是它们共同依赖的“存储主密钥 + 敏感列 AEAD”基础，代码在 [crates/crypto/src/key_store.rs](../../crates/crypto/src/key_store.rs)（Argon2id 派生与密钥文件加载）与 [crates/storage/src/formal/seal.rs](../../crates/storage/src/formal/seal.rs)（子密钥派生与 ChaCha20-Poly1305 封装）。

- 派生：Argon2id（v0x13）输出固定 32 字节；内存成本 8 MiB..=1 GiB、迭代 1..=16、并行度 1..=8、盐 16..=64 字节、口令 1..=1024 字节，越界或畸形输入全部有界拒绝；参数与盐作为 KeyProvenance 写入库，重开必须完全一致。
- 加载：密钥文件必须恰好 32 字节，缺失、超出或非普通文件都显式失败，不截断也不补零。
- 使用：主密钥只以 ProtectedStorageKey 形式在可信核心路径内存在（Drop 清零、Debug 不输出字节、不实现 Clone）；草稿正文、MLS 记录值、密钥校验记录、隔离诊断各自派生独立子密钥；错误密钥、来源不一致、篡改密文统一返回 STATE_CORRUPTED。
- 尚未实现：MLS provider 接入（TxnCryptoProvider，CRYPTO-004/005/006 的真正语义）、附件分块加解密（CRYPTO-011/012）、系统密钥槽封装与恢复 KDF 的 ADR 决策、全库加密方案。

复现命令（全程离线）：

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-crypto --test formal_master_key   # 5 passed（派生确定性/盐与口令绑定/参数边界/密钥文件长度/store 端到端）
cargo test --offline -p chat-storage                           # 22 passed（含 AEAD 封装单测与磁盘明文/主密钥扫描）
```

## CRYPTO-011/012 附件分块 AEAD 实现记录（2026-10-18，Windows x64）

代码在 [crates/crypto/src/attachment.rs](../../crates/crypto/src/attachment.rs)；上层本地导入/导出在 [crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)（见 [attachments.md](attachments.md#attachments-001)）。

- 密钥与分块：每个附件独立随机 FileKey（32 字节，Drop 清零、Debug 不输出）与随机 FileId（16 字节）；固定 256 KiB 分块，最后一块可 0..=256 KiB，非最后一块必须满块。
- 密文格式：version(1) || nonce(12) || ciphertext || tag(16)，每块随机 nonce，密文自带认证标签。
- AAD：域分隔 + file_id + index + total_chunks + plain_length + is_final + format_version 的规范编码；交换块、换文件、伪造最后一块、改声明长度都会认证失败。
- 边界：单文件上限 25 MiB（100 块）、块数上限 4096、单块密文长度上界；越界或畸形上下文显式返回 CONTENT_TOO_LARGE / INVALID_CHUNK_CONTEXT。
- 失败语义：认证失败返回 ATTACHMENT_CORRUPTED，不返回任何未认证字节；解密结果用 Zeroizing 缓冲。

复现命令（全程离线）：

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-crypto --test attachment_chunks   # 9 passed
```

已知限制：未对照 RFC 8439 官方测试向量（实现来自 chacha20poly1305 =0.11.0）；nonce 随机生成而非冻结的计数器方案，唯一性靠 96-bit 随机 + 每附件 100 块量级的概率论证，尚无形式化证明；分块层不校验“块数覆盖”与整文件摘要，这两项由导出流程负责。

## 函数导航

- [CRYPTO-001 create_crypto_group](crypto.md#crypto-001)
- [CRYPTO-002 generate_key_package](crypto.md#crypto-002)
- [CRYPTO-003 validate_key_package](crypto.md#crypto-003)
- [CRYPTO-004 prepare_application](crypto.md#crypto-004)
- [CRYPTO-005 prepare_control](crypto.md#crypto-005)
- [CRYPTO-006 process_verified_entry](crypto.md#crypto-006)
- [CRYPTO-007 merge_accepted_pending](crypto.md#crypto-007)
- [CRYPTO-008 discard_rejected_pending](crypto.md#crypto-008)
- [CRYPTO-009 join_from_welcome](crypto.md#crypto-009)
- [CRYPTO-010 publish_committed_state](crypto.md#crypto-010)
- [CRYPTO-011 encrypt_attachment_chunk](crypto.md#crypto-011)
- [CRYPTO-012 decrypt_attachment_chunk](crypto.md#crypto-012)

<a id="crypto-001"></a>

## CRYPTO-001 · create_crypto_group

~~~text
create_crypto_group(ctx: GroupCreationContext, tx: CryptoTransaction) -> Result<PreparedWrite>
~~~

**职责与输出**：创建包含获准设备凭据的初始 MLS 状态。

**输入与前置条件**：P0 套件锁定；首个设备身份已验证。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 绑定 GroupId 和扩展上下文。
2. 创建 OpenMLS 群。
3. 写 provider 记录。
4. 输出初始状态差异和群建立候选。

**持久化、并发与副作用**：T2：MLS/业务快照/待发送建群记录统一事务；提交后才发布句柄。

**失败处理**：CRYPTO_INITIALIZATION_FAILED，INVALID_CREDENTIAL。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 群 ID 复用拒绝。
- [ ] 创建时崩溃不残留可用半群。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="crypto-002"></a>

## CRYPTO-002 · generate_key_package

~~~text
generate_key_package(device: VerifiedCredential, ttl: Duration, tx: CryptoTransaction) -> Result<PublicKeyPackage>
~~~

**职责与输出**：生成带私有配套记录的一次性 KeyPackage。

**输入与前置条件**：设备有效，套件和协议版本匹配。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 安全随机生成。
2. 绑定应用凭据。
3. 设置期限。
4. 存储私有 init material。
5. 返回签名公开包。

**持久化、并发与副作用**：T2：公包摘要/私有材料一起保存；发布通过 outbox 进行。

**失败处理**：DEVICE_REVOKED，CRYPTO_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 公包可验签。
- [ ] 私有材料不上传。
- [ ] 生成后重启仍能处理 Welcome。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="crypto-003"></a>

## CRYPTO-003 · validate_key_package

~~~text
validate_key_package(package: PublicKeyPackage, expected: DeviceId, now: Time) -> Result<VerifiedKeyPackage>
~~~

**职责与输出**：验证 MLS 签名、账户设备绑定、套件与有效期。

**输入与前置条件**：来源可能恶意；节点 claim 不能替代验证。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 有界解析。
2. OpenMLS 验签。
3. 验证应用凭据。
4. 核对目标设备/版本/有效期。
5. 校验未在本地使用。

**持久化、并发与副作用**：纯验证加只读消费查询；真正消费与控制操作事务绑定。

**失败处理**：INVALID_KEY_PACKAGE，KEY_PACKAGE_EXPIRED，KEY_PACKAGE_REUSED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)、[PROTOCOL-003 decode_bounded](protocol.md#protocol-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 替换凭据或旧套件拒绝。
- [ ] 重复包不能加入两次。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="crypto-004"></a>

## CRYPTO-004 · prepare_application

~~~text
prepare_application(input: AuthenticatedLocalEvent, tx: CryptoTransaction) -> Result<PreparedWrite>
~~~

**职责与输出**：生成一次密文、SubmissionId 与 MLS 状态变更。

**输入与前置条件**：持有该群串行执行权；已校验业务权限；使用当前确认 epoch。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 规范编码内层事件。
2. 在事务 provider 内 MLS encrypt。
3. 分配新 SubmissionId。
4. 保存密文不可变摘要。
5. 生成外层签名。
6. 返回状态差异。

**持久化、并发与副作用**：T2：加密记录/attempt/outbox 一事务；不可先改共享内存后提交。

**失败处理**：STALE_EPOCH，CRYPTO_FAILURE，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-008 encode_application_event](protocol.md#protocol-008)、[PROTOCOL-004 candidate_signing_bytes](protocol.md#protocol-004)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 每个 crash point 恢复不复用已消费状态。
- [ ] 相同重试不再次调用 encrypt。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="crypto-005"></a>

## CRYPTO-005 · prepare_control

~~~text
prepare_control(operation: AuthorizedGroupOperation, tx: CryptoTransaction) -> Result<PreparedControl>
~~~

**职责与输出**：建立与单一业务操作一致的 pending Commit/Welcome。

**输入与前置条件**：当前 head/epoch 已确认；目标已接受邀请且 KeyPackage 验证通过。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 生成预期业务差异。
2. 使用 OpenMLS 提案/Commit。
3. 比对设备增删集合。
4. 保存 pending 身份和候选。
5. 保留 Welcome 待接受后投递。

**持久化、并发与副作用**：T2：pending 状态/候选/任务一事务；未获回执不合并正式成员状态。

**失败处理**：STALE_CONTROL_HEAD，INVALID_KEY_PACKAGE，CRYPTO_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-004 apply_policy_operation](domain.md#domain-004)、[CRYPTO-003 validate_key_package](crypto.md#crypto-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 两个并发 Commit 只串行准备。
- [ ] 夹带移除成员拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="crypto-006"></a>

## CRYPTO-006 · process_verified_entry

~~~text
process_verified_entry(entry: VerifiedLogEntry, tx: CryptoTransaction) -> Result<PreparedWrite>
~~~

**职责与输出**：验证并暂存收件消息或控制事件的状态推进。

**输入与前置条件**：日志链连续；设备与群上下文可信；每群串行。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 识别本人 pending 回执路径。
2. OpenMLS 验证/解密或 stage Commit。
3. 验证内层作者。
4. 控制结果与业务比对。
5. 生成应用变更和回执意图。

**持久化、并发与副作用**：T3：MLS/消息/去重/游标/回执 outbox 一事务；失败整体回滚。

**失败处理**：INVALID_CIPHERTEXT，INVALID_CONTROL_RESULT，MISSING_LOG_ENTRIES。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-009 verify_inner_event](protocol.md#protocol-009)、[DOMAIN-005 verify_control_result](domain.md#domain-005)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 畸形 Commit 不推进任何游标。
- [ ] 本人已发消息不重复消费发送状态。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="crypto-007"></a>

## CRYPTO-007 · merge_accepted_pending

~~~text
merge_accepted_pending(pending: PendingCommit, entry: VerifiedLogEntry, tx: CryptoTransaction) -> Result<PreparedWrite>
~~~

**职责与输出**：仅将本机同一已接受 Commit 合并正式 MLS 状态。

**输入与前置条件**：回执绑定同 SubmissionId、完整候选摘要与当前前置状态。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证 pending 摘要。
2. 核对 control_head。
3. 通过 OpenMLS 正式 merge。
4. 校验结果成员。
5. 输出快照与状态更新。

**持久化、并发与副作用**：T3：merge 与正式日志/策略/游标同事务；不得再次把本机 Commit 当远端解密。

**失败处理**：PENDING_MISMATCH，LOG_CONFLICT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-005 verify_control_result](domain.md#domain-005)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 节点回执丢失后拉取同事件成功合并一次。
- [ ] 不同摘要必须暂停。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="crypto-008"></a>

## CRYPTO-008 · discard_rejected_pending

~~~text
discard_rejected_pending(pending: PendingCommit, rejection: AuthenticatedRejection, tx: CryptoTransaction) -> Result<RebuildIntent>
~~~

**职责与输出**：只清理明确未接受的 pending 操作，保留草稿意图。

**输入与前置条件**：先排除超时但已接受；P0 验证库的 pending 分支恢复语义。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 确认拒绝绑定候选。
2. 记录被拒尝试。
3. 使用库支持的分支清理。
4. 同步胜出 Commit。
5. 返回待重新授权意图。

**持久化、并发与副作用**：不可恢复旧已消费密钥快照；清理与尝试状态一事务。

**失败处理**：ACCEPTANCE_UNKNOWN，STATE_CORRUPTED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-010 classify_retry](domain.md#domain-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 未知结果先查询旧 SubmissionId。
- [ ] 清理后不复用发送代数。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="crypto-009"></a>

## CRYPTO-009 · join_from_welcome

~~~text
join_from_welcome(welcome: AuthorizedWelcome, tx: CryptoTransaction) -> Result<JoinedGroup>
~~~

**职责与输出**：消费本机 KeyPackage 并验证邀请与协调起点。

**输入与前置条件**：用户已接受邀请；Welcome 绑定本机与对应合法控制日志。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证邀请能力。
2. 匹配本机包。
3. 用 OpenMLS 建群。
4. 核对成员凭据和上下文。
5. 绑定加入位置。
6. 消费私有包。

**持久化、并发与副作用**：T3：群状态/包消费/加入游标一事务；重复 Welcome 返回同结果。

**失败处理**：WELCOME_NOT_AUTHORIZED，KEY_PACKAGE_REUSED，INVALID_CREDENTIAL。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CRYPTO-003 validate_key_package](crypto.md#crypto-003)、[DOMAIN-005 verify_control_result](domain.md#domain-005)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 为另一设备的 Welcome 拒绝。
- [ ] 重启不产生重复群。
- [ ] 无旧历史能力。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="crypto-010"></a>

## CRYPTO-010 · publish_committed_state

~~~text
publish_committed_state(group: GroupId, committed: CommitToken) -> Result<()>
~~~

**职责与输出**：数据库提交成功后替换群内存快照并发布变化通知。

**输入与前置条件**：CommitToken 由 storage 成功提交产生，不允许 UI 伪造。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取已提交版本。
2. 替换串行执行器状态。
3. 清理敏感临时缓冲。
4. 通知事件桥。

**持久化、并发与副作用**：事务后动作；通知失败可重查，不回滚持久化。

**失败处理**：STATE_REFRESH_FAILED 触发重新加载而非重加密。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[STORAGE-009 commit_transaction](storage.md#storage-009)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 提交前不显示正式成员。
- [ ] 回调丢失后查询仍正确。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="crypto-011"></a>

## CRYPTO-011 · encrypt_attachment_chunk

~~~text
encrypt_attachment_chunk(key: FileKey, ctx: ChunkContext, plaintext: SecretBytes) -> Result<CipherChunk>
~~~

**职责与输出**：输出独立认证分块；ctx 含 file/index/total/final/length。

**输入与前置条件**：每个文件随机密钥；nonce 构造与套件经 P0 锁定，index 不溢出。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证 chunk 边界。
2. 按冻结方案取得唯一 nonce。
3. 规范编码 AAD。
4. 调用标准 AEAD。
5. 输出带认证标签的块。

**持久化、并发与副作用**：纯密码函数；失败块不标上传成功；重试复用原密文。

**失败处理**：INVALID_CHUNK_CONTEXT，CRYPTO_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 交换 index/文件/结束边界均验签失败：`attachment_chunks::reordered_duplicated_or_foreign_chunks_are_rejected`、`attachment_chunks::forged_final_chunk_is_rejected`（index、total_chunks、is_final、file_id 全在 AAD 内）。
- [x] 不能对改变明文复用 nonce：`attachment_chunks::multi_chunk_roundtrip_and_size_helpers`（同上下文两次加密的密文不同，nonce 每块随机）。

**实现状态**：已实现（Windows x64 实测），代码在 [crates/crypto/src/attachment.rs](../../crates/crypto/src/attachment.rs)。固定 256 KiB 分块，密文格式 version(1)||nonce(12)||ciphertext||tag(16)；AAD = 域分隔 + file_id(16) + index(u32 BE) + total_chunks(u32 BE) + plain_length(u32 BE) + is_final + format_version；非最后一块必须是满块，最后一块 0..=256 KiB；单文件上限 25 MiB、块数上限 4096，越界显式拒绝。证据：`cargo test --offline -p chat-crypto --test attachment_chunks`（9 passed）。限制：未做 RFC 8439 官方测试向量对照（用的是 chacha20poly1305 =0.11.0 成熟实现）；nonce 随机而非常量计数器，未形式化证明唯一性。

<a id="crypto-012"></a>

## CRYPTO-012 · decrypt_attachment_chunk

~~~text
decrypt_attachment_chunk(key: FileKey, ctx: ChunkContext, chunk: CipherChunk) -> Result<SecretBytes>
~~~

**职责与输出**：先认证再返回明文，不把未认证字节导出。

**输入与前置条件**：元数据来自已验证 E2EE 附件描述。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对尺寸。
2. 重建 nonce/AAD。
3. 调用 AEAD verify/decrypt。
4. 验证声明长度。
5. 返回敏感缓冲。

**持久化、并发与副作用**：纯密码函数；完整文件摘要和结束边界仍须导出流程验证。

**失败处理**：ATTACHMENT_CORRUPTED，INVALID_CHUNK_CONTEXT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CRYPTO-011 encrypt_attachment_chunk](crypto.md#crypto-011)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 截断/位翻转/块替换拒绝：`attachment_chunks::truncation_and_impossible_lengths_are_rejected`、`attachment_chunks::single_byte_tampering_is_rejected`、`attachment_chunks::reordered_duplicated_or_foreign_chunks_are_rejected`。
- [x] 最后一块伪造拒绝：`attachment_chunks::forged_final_chunk_is_rejected`（把非最后一块当成单块文件的最后一块解密必须失败）。

**实现状态**：已实现（Windows x64 实测）。解密先按上下文核算期望密文长度（1+12+plain_length+16），再认证解密；失败一律返回 ATTACHMENT_CORRUPTED 或 INVALID_CHUNK_CONTEXT，绝不返回未认证字节，明文用 Zeroizing 缓冲返回。层内不解总数覆盖与完整摘要：这两项由 [ATTACHMENTS-006 export_attachment](attachments.md#attachments-006) 逐块认证 + 全文件摘要复核完成（`attachments::tests` 覆盖）。


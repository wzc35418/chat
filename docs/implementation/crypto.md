# MLS、单群串行状态与附件密码学

阶段：P0 → P3。状态：函数级设计；CRYPTO-005~009 群成员控制（pending 控制提交、对端条目处理、接受后合并、拒绝后回滚、Welcome 加入）、CRYPTO-011/012 附件分块 AEAD、P0-04 存储主密钥/敏感列加密基础、以及 CRYPTO-004/010 的**状态持久化切片**（已提交 MLS 群状态与 provider 秘密落到正式加密库、重启后可继续加解密）已实现（Windows x64 实测，见下文实现记录）。仍未实现：同事务 provider（TxnCryptoProvider）、PreparedWrite/外层候选签名与 T3 落库、正式群策略授权（DOMAIN-004）、控制 head 对账与分叉胜出合并。目标源码：[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)、[crates/crypto/src/control.rs](../../crates/crypto/src/control.rs)、[crates/crypto/src/attachment.rs](../../crates/crypto/src/attachment.rs) 与 [crates/crypto/src/key_store.rs](../../crates/crypto/src/key_store.rs)。

必须使用成熟实现。TxnCryptoProvider 在同一 storage 事务中读写；如果 OpenMLS provider 不能安全接入，则先 ADR 决策并实现幂等日志恢复再继续。PreparedWrite 不是已正式发布的新内存状态。每群串行执行，网络等待不跨持有 SQLite 写事务。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## P0-04 存储主密钥与敏感列加密基础（2026-10-18，Windows x64）

CRYPTO-004/006/007/008 的 MLS 语义仍未实现；本轮实现的是它们共同依赖的“存储主密钥 + 敏感列 AEAD”基础，代码在 [crates/crypto/src/key_store.rs](../../crates/crypto/src/key_store.rs)（Argon2id 派生与密钥文件加载）与 [crates/storage/src/formal/seal.rs](../../crates/storage/src/formal/seal.rs)（子密钥派生与 ChaCha20-Poly1305 封装）。

- 派生：Argon2id（v0x13）输出固定 32 字节；内存成本 8 MiB..=1 GiB、迭代 1..=16、并行度 1..=8、盐 16..=64 字节、口令 1..=1024 字节，越界或畸形输入全部有界拒绝；参数与盐作为 KeyProvenance 写入库，重开必须完全一致。
- 加载：密钥文件必须恰好 32 字节，缺失、超出或非普通文件都显式失败，不截断也不补零。
- 使用：主密钥只以 ProtectedStorageKey 形式在可信核心路径内存在（Drop 清零、Debug 不输出字节、不实现 Clone）；草稿正文、MLS 记录值、密钥校验记录、隔离诊断各自派生独立子密钥；错误密钥、来源不一致、篡改密文统一返回 STATE_CORRUPTED。
- 后续进展：MLS 状态的正式持久化切片已实现（见下文 CRYPTO-004/010 记录）；仍**未实现**同事务 `TxnCryptoProvider`（把 provider 写入并入调用方的 storage 事务）、pending Commit / 正式群操作语义（CRYPTO-005/006/007/008）、系统密钥槽封装与恢复 KDF 的 ADR 决策、全库加密方案。

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

## CRYPTO-004/010 MLS 状态持久化切片实现记录（2026-10-18，Windows x64）

目标：已提交的 MLS 群状态与 provider 秘密在**正式加密库**里耐久保存，进程重启后
仍能用同一个群继续加解密（对端能解密到原文）。存储侧见
[storage.md](storage.md) 的“MLS 群状态与 provider 记录持久化实现记录”。

实现位置：[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)

- `MlsDevice::export_to_store(&mut formal::Store) -> MlsExportSummary`（CRYPTO-010 切片）：
  取当前已提交群的 `group_id`/`epoch`，把 credential 名、签名公钥与群 ID 编码成群级
  头（版本化二进制，含长度上限），再把 `OpenMlsRustCrypto` 内存 provider 的**全部**
  记录按键排序快照，一次性写入 `mls_group_state` + `mls_record`（单笔
  `WriteScope::mls_state` 事务 = 要么全提交、要么全回滚）。
- `MlsDevice::import_from_store(&Store, group_key) -> MlsDevice`（恢复路径）：
  解密群级头与记录 → 重建 `OpenMlsRustCrypto` 的 provider 存储 → 用
  `SignatureKeyPair::read` 取回签名密钥 → `MlsGroup::load` 恢复群；随后校验群 ID、
  epoch、签名公钥三者一致，任何不符显式失败（不新建空群）。
- `MlsDevice::advance_epoch_with_self_update()`：本地自更新 commit 并立即合并，
  用于验证“epoch 前进后旧状态不可用/不可覆盖”。对端处理该 commit 属于 CRYPTO-007，
  未实现，因此调用方必须自己负责投递。
- 错误码：存储层的 `STALE_EPOCH`（旧 epoch 覆盖）、`STATE_CORRUPTED`（错误主密钥/
  篡改/挪行记录）与 `INVALID_INPUT`（越界或畸形输入）原样经 `anyhow` 传播，测试用
  `downcast_ref::<StorageError>()` 断言稳定码。

复现命令（全程离线，本轮实测）：

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-crypto --test formal_mls_state   # 4 passed
cargo test --offline -p chat-storage -p chat-crypto           # 全绿（28 + 65 passed）
```

证据（测试名 → 契约点）：

- `formal_mls_state::committed_group_survives_close_and_reopen`：两设备建群 → 加密一条 →
  双方导出 → 释放设备与库句柄（模拟进程退出）→ 同主密钥重开 → 双方恢复 → Alice 继续
  加密、Bob 解密到**原文**（含中文与 emoji），反方向同样可用；密文不含明文；
  同 epoch 再次导出是快照替换（`state_version` 递增）且不改变 epoch。
- `formal_mls_state::epoch_advance_makes_previous_state_stale`：自更新推进到 epoch 2 后
  再导出；用 epoch 1 的旧快照覆盖 → `STALE_EPOCH` 且库里仍是 epoch 2；重开恢复
  epoch 2；仍停留在 epoch 1 的设备**解不开** epoch 2 的消息（旧状态不可用于新消息）。
- `formal_mls_state::wrong_master_key_and_tampered_records_are_state_corrupted`：错误
  主密钥拒绝打开；篡改 provider 记录或群级状态 → 导入显式 `STATE_CORRUPTED`；
  不存在的群明确失败（不新建空群）。
- `formal_mls_state::failed_export_keeps_the_previous_complete_state`：非法写入被
  有界拒绝后，上一次提交的群状态完整可用且可继续加密（无半状态）。
- 崩溃断点（真实 `abort`）：`formal_crash_mls::mls_export_abort_before_commit_leaves_the_previous_epoch_only`（storage 侧）。

已知限制：

1. **不是同事务 provider**：本切片在 `export_to_store` 时一次性写库；CRYPTO-004 要求的
   “在调用方 T2 事务内由 provider 直接读写”仍未实现，因此应用消息的加密与其状态
   更新目前是两个动作（调用方必须在提交失败时回滚内存状态）。
2. **epoch 前进的持久化只覆盖已提交快照**：CRYPTO-005~009 的 pending Commit 与正式群
   操作已在 `control.rs` 实现（见下文实现记录），但本切片的 `export_to_store` 仍是全量
   快照替换，未与 T3 日志/游标同事务。
3. **provider 记录全量快照**：每次导出写全部记录（2 人群规模很小）；大群需要增量
   差异方案与写入预算。
4. 群级头里的名字/签名公钥来自本机 credential，未与 account/device 身份表交叉校验
   （身份绑定由 IDENTITY 与 CRYPTO-001/002 负责）。
5. 旧 `persistence.rs`（P0-03 未加密 SQLite 实验）与 `p0-crash-worker` 保持原样，
   正式路径不复用它们。

## CRYPTO-005~009 群成员控制实现记录（2026-10-18，Windows x64）

代码在 [crates/crypto/src/control.rs](../../crates/crypto/src/control.rs)（`impl MlsDevice` 的 CRYPTO-005~009 方法），复用
[engine.rs](../../crates/crypto/src/engine.rs) 的真实 OpenMLS provider/群句柄与
`export_to_store`/`import_from_store` 的正式加密持久化（mls_group_state/mls_record）。

- **pending 显式且不提前合并**：`prepare_control` 让 OpenMLS 进入 PendingCommit，本地
  epoch 不变；返回 `PendingControl { group_id, base_epoch, target_epoch = base+1,
  operation, added, removed, commit, welcome?, commit_digest }`。pending 元数据随群状态
  快照（群级头版本 2，兼容读取版本 1）密封写入正式库；打开时与 OpenMLS 的
  PendingCommit 状态交叉校验，不一致即 STATE_CORRUPTED。
- **合并需要接受证据**：`merge_accepted_pending(acceptance, store)` 核对
  group/base_epoch/target_epoch/提交摘要后才 `merge_pending_commit`，合并后 epoch 必须
  等于 target_epoch；成功才清 pending 并写已提交快照。证据不符 → PENDING_MISMATCH
  （epoch 不变、pending 保留）；合并失败或 epoch 不符 → 设备 fail-closed。
- **拒绝后回滚**：`discard_rejected_pending(rejection, store)` 只在拒绝证据绑定同一候选、
  且正式库里的 pending 分支仍停在 base_epoch 时 `clear_pending_commit`，随后写回已提交
  快照。旧 epoch 密钥不受影响，可继续加解密。
- **对端条目**：`process_verified_entry(entry)` 先校验 group_id、epoch（落后
  STALE_EPOCH、超前 MISSING_LOG_ENTRIES）与 MLS 消息 epoch，再认证解密/stage；作者凭据
  与条目声称身份不符 → INVALID_CIPHERTEXT；对端 Commit 用 `CommitExpectation` 比对实际
  增删，不一致 → INVALID_CONTROL_RESULT 并停止该群分支；本机有 pending 时收到竞争
  Commit → PENDING_MISMATCH（必须先显式解决本分支）。
- **Welcome 加入**：`join_from_welcome(welcome, acceptance, store)` 用 `StagedWelcome`
  建群，核对群 ID/epoch/成员集合/本机身份后立即持久化；拒绝时删除刚写入 provider 的
  群状态；重复加入 → INVALID_STATE_TRANSITION。

复现命令（全程离线）：

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-crypto --test mls_control   # 12 passed
cargo test --offline -p chat-crypto                      # 40 passed（既有 28 个继续全绿）
```

已知限制：控制操作没有账户/角色授权与 control-head 对账（DOMAIN-004 与日志层未接入）；
接受/拒绝证据由调用方构造，未做节点回执验签；合并/回滚未与 T3 日志/游标同事务（属
STORAGE-008）；提案（ProposalMessage）只入 proposal store，未实现提案式控制流；外部加入
提案与分叉胜出合并不支持。

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

- [ ] 每个 crash point 恢复不复用已消费状态。（未验证：同事务 provider 未实现，见下）
- [x] 相同重试不再次调用 encrypt：持久化切片保证同一 `(group_id, epoch)` 的导出是幂等快照替换（`formal_mls_state::committed_group_survives_close_and_reopen` 的再次导出断言 `state_version` 递增但 epoch 不变），不会因重开/重试产生新 epoch 或重新加密既有消息。

**实现状态**：部分实现（仅状态持久化切片）。本轮实现的是 CRYPTO-004 依赖的“已提交群状态 + provider 秘密落正式加密库、可恢复继续加解密”：
`MlsDevice::export_to_store` / `MlsDevice::import_from_store`（见 [engine.rs](../../crates/crypto/src/engine.rs)）配合 STORAGE 的 `mls_group_state`/`mls_record` 表。证据：`cargo test --offline -p chat-crypto --test formal_mls_state`（4 passed）。
**仍未实现**：真正在调用方 T2 事务内由 provider 直接加密并写 attempt/outbox（`TxnCryptoProvider`）、`PreparedWrite` 生成、外层候选签名与 SubmissionId 分配——因此“加密一次 + 同事务落盘”的完整语义尚不可用。

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

- [x] 两个并发 Commit 只串行准备：`mls_control::pending_commit_does_not_advance_epoch_and_new_epoch_messages_are_rejected`（已有 pending 时第二个 prepare 返回 INVALID_STATE_TRANSITION / PendingCommitExists）、`mls_control::key_package_and_member_inputs_are_validated`。
- [x] 夹带移除成员拒绝：发送侧在 staging 后比对 pending StagedCommit 的实际 add/remove 数量与请求（不一致即 clear pending 并返回 INVALID_CONTROL_RESULT，见 control.rs 步骤 3）；接收侧由 `mls_control::control_result_mismatch_stops_the_group_branch` 证明“提交实际成员变化与已验证业务不一致”会被拒绝。

**实现状态**：已实现（Windows x64 实测），代码在 [crates/crypto/src/control.rs](../../crates/crypto/src/control.rs) 的 `prepare_control`。Add/Remove/SelfUpdate 三种控制提交由 OpenMLS 真实生成 Commit（Add 另产生 Welcome）；staging 后**不推进本地 epoch**（base_epoch/target_epoch 显式），pending 元数据（Commit/Welcome/摘要/操作/增删数）随群状态快照写入正式加密库。Add 的 KeyPackage 会重新做 MLS 验签、去重与“已是成员/本机”拒绝。证据：`cargo test --offline -p chat-crypto --test mls_control`（12 passed）。限制：未接入 DOMAIN-004 的账户/角色授权与 STALE_CONTROL_HEAD 对账（外层日志不存在），只做 MLS 层与结构校验。

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

- [x] 畸形 Commit 不推进任何游标：`mls_control::entry_validation_rejects_wrong_group_epoch_sender_and_tampering`（群不符/epoch 落后/epoch 超前/条目与密文 epoch 不符/篡改字节/重复提交全部显式拒绝，epoch 不变）、`mls_control::entry_commit_is_rejected_while_a_local_control_commit_is_pending`。
- [x] 本人已发消息不重复消费发送状态：同一用例（本机 pending 提交被回显 → PENDING_MISMATCH，pending 与 epoch 都不变；自己的应用消息回显 → OwnPrivateMessage 被拒绝，不合并、不推进）。

**实现状态**：已实现（Windows x64 实测）。`process_verified_entry` 先校验群 ID、条目 epoch 与 MLS 消息 epoch、作者凭据，再交给 OpenMLS 认证解密/stage；应用消息返回明文，提案入 proposal store，对端 Commit 在校验预期增删（CommitExpectation）后才合并并核对 epoch 恰好 +1。认证失败：握手失败会让该群分支 fail-closed（该代密钥已消耗、不能假设无副作用），应用消息失败只拒绝该条。证据：`--test mls_control` 12 passed。限制：不做 T3 落库（消息/去重/游标/回执 outbox 一事务属 STORAGE-008）、不做外部队列与 log 连续性（MISSING_LOG_ENTRIES 只按 epoch 判定）、不做提案式授权（直接 Add/Remove 提交）。

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

- [x] 节点回执丢失后拉取同事件成功合并一次：`mls_control::add_member_welcome_join_and_bidirectional_roundtrip`（同一接受证据只合并一次；再次合并返回 PENDING_MISMATCH 且 epoch 保持 2）、`mls_control::pending_and_committed_states_survive_close_and_reopen`（回执丢失后重开仍能用同证据合并）。
- [x] 不同摘要必须暂停：同一用例（摘要/target_epoch 不符的接受证据 → PENDING_MISMATCH，pending 保留、epoch 不变，必须用正确证据）。

**实现状态**：已实现（Windows x64 实测）。`merge_accepted_pending` 先核对接受证据（group_id/base_epoch/target_epoch/提交摘要）与本地 epoch，再 `merge_pending_commit` 消费该提交密钥；合并后 epoch 必须等于 target_epoch，随后写已提交快照。合并失败或 epoch 不符时设备 fail-closed（丢弃内存群句柄，必须从已提交存储重新 import）。证据：`--test mls_control` 12 passed。限制：接受证据由调用方从节点回执构造，未实现回执验签（T4/NODE 侧）；合并未与 T3 日志/游标同事务。

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

- [ ] 未知结果先查询旧 SubmissionId。（未实现：节点回执对账属 NODE/SYNC 侧；本实现在“没有明确拒绝证据”时拒绝丢弃，绝不把未知当拒绝）
- [x] 清理后不复用发送代数：`mls_control::rejected_pending_discards_and_old_epoch_still_works`（丢弃后 epoch 仍为 1，重新准备的控制提交字节与摘要都与被拒候选不同，不复用同一代材料）。

**实现状态**：已实现（Windows x64 实测，仅本地回滚语义）。`discard_rejected_pending` 要求拒绝证据绑定同一 group/base_epoch/提交摘要，且正式库里的 pending 分支仍停在 base_epoch（否则 STALE_EPOCH，不用旧快照覆盖新状态）；随后 `clear_pending_commit` 回到 Operational 并写回已提交快照。旧 epoch 的密钥状态不变，丢弃后可继续用旧 epoch 加解密（用例覆盖）。证据：`--test mls_control` 12 passed。限制：不记录“被拒尝试”历史表、不返回 RebuildIntent 的业务字段（草稿/意图重建属 DOMAIN-010）。

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

- [x] 为另一设备的 Welcome 拒绝：`mls_control::welcome_join_rejects_unauthorized_bindings_and_replays`（外来设备的 Welcome 无法 stage → WELCOME_NOT_AUTHORIZED；群 ID/epoch/成员集合绑定不符同样拒绝并清理半加入的群状态）。
- [x] 重启不产生重复群：`mls_control::pending_add_with_welcome_survives_reopen`（重开后仍只有一个 mls_group_state 行）、同一用例的重复加入返回 INVALID_STATE_TRANSITION。
- [x] 无旧历史能力：`mls_control::add_member_welcome_join_and_bidirectional_roundtrip`（Carol 加入 epoch 2 后无法解密 Alice 在 epoch 1 发的消息）。

**实现状态**：已实现（Windows x64 实测）。`join_from_welcome` 用 OpenMLS `StagedWelcome` 建群，核对 group_id、epoch、成员凭据集合与本机身份后立即 `export_to_store`；拒绝时删除刚写入 provider 的群状态，不留半加入的群。证据：`--test mls_control` 12 passed。限制：邀请能力/账户设备授权（IDENTITY-003、DOMAIN-005）未接入，只做 MLS 结构绑定；被拒 Welcome 会消耗本机 KeyPackage 私有材料，必须换新 KeyPackage 与邀请（用例注释已说明）。

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

- [ ] 提交前不显示正式成员。（未验证：提交回调由 core 接线，未实现）
- [ ] 回调丢失后查询仍正确。（未验证：事件桥未实现；但持久化状态可从库重读，见下）

**实现状态**：部分实现（持久化切片）。`MlsDevice::export_to_store` 是“把已提交状态写成正式持久状态”的实现路径：`MlsDevice::import_from_store` 可在任意时刻从库重读同一状态（等价于“回调丢失后重新查询”），并被 `formal_mls_state::committed_group_survives_close_and_reopen` 覆盖。
**仍未实现**：与 `STORAGE-009 commit_transaction` 返回的 `CommitToken` 绑定的提交后替换流程、事件桥通知与串行执行器状态替换；目前的导出不校验 CommitToken（调用方在正式提交后调用）。

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


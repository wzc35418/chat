# 文件导入、分块上传、下载和导出

阶段：P2。状态：函数级设计；本地导入/导出与本地状态机（ATTACHMENTS-001/002/004/006/007 的本地部分）已实现；ATTACHMENTS-003 上传与 ATTACHMENTS-005 下载已通过可注入的块传输端口（`BlobTransport`）接通编排与状态机（节点客户端由适配层 `NodeBlobTransport` 实现并已接真节点（NODE-012~015，crates/core/src/attachment_transport.rs））；节点配额与回执验签已由适配层完成；含 FileKey 的 E2EE SecretDescriptor、跨进程下载能力与持久化进度仍未实现。FFI 侧通过 `AttachmentFfi::open` 暴露 upload/download 给 Kotlin（ATT-U，见文末记录）。目标源码：[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)（编排）与 [crates/core/src/attachment_transport.rs](../../crates/core/src/attachment_transport.rs)（节点适配）。

平台 file token 仅代表用户授权的一次访问，不将任意路径传入内核。默认单附件 25 MiB，按完整原文大小计；节点配额按密文实际字节计。文件密钥只在受保护本地记录和 E2EE 描述中，不能放节点 URL 或公开清单。

## 本地导入/导出实现记录（2026-10-18，Windows x64）

代码在 [crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)；分块 AEAD 在 [crates/crypto/src/attachment.rs](../../crates/crypto/src/attachment.rs)（见 [crypto.md CRYPTO-011/012](crypto.md#crypto-011)）。本轮只做本地路径，不发起任何网络请求，也不写节点：

- import_file：只接受平台令牌提供的普通文件且必须声明大小；大于 25 MiB 在读取前拒绝；按 256 KiB 流式读取，逐块用 CRYPTO-011 加密后写入缓存根下的 att-<hex>/chunk-<index>.bin；同时计算明文 SHA-256 与密文摘要；任何失败删除整个未引用缓存目录。大小漂移（流比声明更长/更短）显式 FILE_IO_FAILURE，不静默截断。
- build_attachment_descriptor：返回只含公开字段的描述符（ID、明文大小、块大小/块数、明文摘要、展示文件名），不含 FileKey 与明文；另有 attachment_manifest 只给密文块数/大小/摘要（TYPES.md 的 PublicBlobDescriptor）。
- export_attachment：按 index 逐块认证解密到同目录临时文件，全部分块通过且总长与明文摘要都匹配后才 rename 成目标文件；任何失败删除临时文件，不产生未验证成品。
- attachment_status / cancel_transfer：进程内状态机（staging/imported/uploading/available/downloading/failed/cancelled）；available_chunks 按缓存实际文件计数，transferred_chunks 在网络接入前恒为 0；取消幂等、保留本地缓存与进度，available 状态拒绝取消（TRANSFER_ALREADY_COMPLETE）。

复现命令（全程离线）：

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --lib    # 9 passed（附件本地导入/导出）
cargo test --offline -p chat-crypto        # 附件分块 9 + 既有用例全绿
```

已知限制：附件记录只在进程内，重启后缓存视为未引用（需接入 storage 的 attachments 表与 ATTACHMENTS-008 回收）；未知大小流按 fail-closed 显式拒绝（分块 AAD 必须绑定 total_chunks）；上传/下载、持久化进度、磁盘满处理、含 FileKey 的 E2EE SecretDescriptor 与下载能力未实现；导出取消（EXPORT_CANCELLED）未实现（没有导出中的取消入口）。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [ATTACHMENTS-001 import_file](attachments.md#attachments-001)
- [ATTACHMENTS-002 attachment_status](attachments.md#attachments-002)
- [ATTACHMENTS-003 upload_attachment](attachments.md#attachments-003)
- [ATTACHMENTS-004 build_attachment_descriptor](attachments.md#attachments-004)
- [ATTACHMENTS-005 download_attachment](attachments.md#attachments-005)
- [ATTACHMENTS-006 export_attachment](attachments.md#attachments-006)
- [ATTACHMENTS-007 cancel_transfer](attachments.md#attachments-007)
- [ATTACHMENTS-008 collect_unreferenced_attachments](attachments.md#attachments-008)

<a id="attachments-001"></a>

## ATTACHMENTS-001 · import_file

~~~text
import_file(source: FileToken) -> Result<AttachmentId>
~~~

**职责与输出**：流式导入并建立加密附件记录/上传任务。

**输入与前置条件**：token 来自系统选择器；已解锁；文件类型/名字只作展示。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 打开受控流。
2. 检查声明大小并逐块累计。
3. 生成 FileId/随机密钥。
4. 加密块并写受保护缓存。
5. 计算摘要。
6. 完成记录。
7. 排队上传。

**持久化、并发与副作用**：短分块检查点事务；完整导入前为 staging；失败清理未引用缓存。

**失败处理**：CONTENT_TOO_LARGE，FILE_IO_FAILURE，INVALID_FILE_TOKEN。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLATFORM-006 open_file_token](platform.md#platform-006)、[CRYPTO-011 encrypt_attachment_chunk](crypto.md#crypto-011)、[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 未知大小流超25 MiB即停止。（未实现：分块 AAD 必须绑定 total_chunks，未知大小流按 fail-closed 返回 INVALID_FILE_TOKEN，不接受“边读边猜总数”的导入）
- [ ] 峰值内存受块预算限制。（未用分配器实测；实现按单块 256 KiB 缓冲 + 流式读写，`attachments::tests::oversized_or_misdeclared_streams_stop_at_the_bound` 只证明越界/大小漂移被拒绝且缓存被清理）

**实现状态**：部分实现（Windows x64 实测）。`import_file` 校验平台令牌与声明大小、25 MiB 上限（读前拒绝）、逐块加密写缓存、明文/密文摘要、失败清理；证据 `cargo test --offline -p chat-core --lib` 的 `import_then_export_roundtrip_and_cache_has_no_plaintext`、`exactly_25_mib_is_accepted_and_oversize_is_rejected`、`oversized_or_misdeclared_streams_stop_at_the_bound`。限制：记录只在内存、重启丢失；未持久化 staging/上传任务（需接入 storage attachments 表与 SYNC-001）；未知大小流不支持。

<a id="attachments-002"></a>

## ATTACHMENTS-002 · attachment_status

~~~text
attachment_status(id: AttachmentId) -> Result<AttachmentStatus>
~~~

**职责与输出**：返回 Imported/Uploading/Available/Downloading/Failed/Cancelled 和进度。

**输入与前置条件**：附件可见且归当前账户；进度非协议确认。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 查询文件/块/任务。
2. 聚合完成字节。
3. 附错误和可重试操作。
4. 不返回文件密钥。

**持久化、并发与副作用**：只读快照；同块重复上传不累计两次。

**失败处理**：ATTACHMENT_NOT_FOUND，PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 取消后可查询：`attachments::tests::cancel_transfer_state_machine_preserves_progress`（取消后 attachment_status 仍返回 Cancelled 且缓存块数不变）。
- [x] 进度最大值不超过实际大小：同一用例与 `import_then_export_roundtrip_and_cache_has_no_plaintext`（available_chunks/transferred_chunks 都被 min 到 chunk_count）、`attachments::tests::upload_failure_keeps_progress_and_retry_is_idempotent`（上传中断后 transferred_chunks = 已发送块数 2、状态停在 uploading、缓存块数仍为 chunk_count）。

**实现状态**：部分实现（本地内存状态机 + 进程内网络进度）。查询聚合的是本地缓存实际块数与传输进度（ATTACHMENTS-003/005 上传/下载会更新 transferred_chunks，同一块重复上传不累计两次），不返回 FileKey；取消是终态之一且可重复查询。限制：进度不落盘（重启后 transferred_chunks 回到 0，附件表没有进度列）、无错误重试信息（failure 只是进程内静态码）；PERMISSION_DENIED 需要账户作用域，当前单账户本地库未实现。

<a id="attachments-003"></a>

## ATTACHMENTS-003 · upload_attachment

~~~text
upload_attachment(id: AttachmentId) -> Result<UploadResult>
~~~

**职责与输出**：按不可变块幂等上传并完成节点密文对象。

**输入与前置条件**：附件本地加密已完成；配额/能力有效。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 建立上传会话。
2. 逐块发送同密文/摘要。
3. 保存已确认位图。
4. 完成密文清单。
5. 校验节点回执。
6. 标 available。

**持久化、并发与副作用**：网络事务外；块 ACK 检查点短事务；完成前引用消息等待。

**失败处理**：QUOTA_EXCEEDED，UPLOAD_EXPIRED，NODE_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[NODE-012 begin_blob_upload](node.md#node-012)、[NODE-013 put_blob_chunk](node.md#node-013)、[NODE-014 complete_blob_upload](node.md#node-014)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 响应丢失重复块不会加倍计配额：`attachments::tests::upload_sends_the_cached_ciphertext_chunks_verbatim`（重复上传已 available 的附件是幂等无操作：不建会话、不发块、不重复 complete）与 `upload_failure_keeps_progress_and_retry_is_idempotent`（重试按同一序号重发同一密文字节；端口契约要求同会话同序号幂等）。节点侧“同块只计一次配额”的记账属 NODE-013，不在本 crate。
- [x] 重启从位图续传：`attachments::tests::upload_resumes_after_restart_and_sends_only_missing_chunks`（第 3 块失败 → 重开 store → 复用持久会话、跳过已确认的 2 块、只补发剩余 1 块：`put 增量 == 1`，完成后进度清零；节点侧同序号幂等仍由 blob 端口契约保证）。

**实现状态**：已实现（编排 + 状态机，Windows x64 实测）。`PersistedAttachmentStore::upload_attachment(attachment_id, &mut transport)`：按 storage 白名单 imported → uploading → available 迁移；从本地加密缓存按序读取**密文块**（不重新加密、不发明文）→ `BlobTransport::begin_upload` →逐块 `put_chunk` → `complete(整文件密文摘要)` → 标 available；已 available 时重复调用是幂等无操作；中途失败保留缓存与已发送块计数、状态停在 uploading 可重试；任何网络失败都不会把记录标成 available。证据：`attachments::tests::upload_sends_the_cached_ciphertext_chunks_verbatim`（节点收到的每块与本地缓存密文逐字节一致、节点永不收到明文、请求字段与公开清单一致、重复上传零发送）、`upload_failure_keeps_progress_and_retry_is_idempotent`（第 3 块失败时已发 2 块、缓存与进度保留、状态 uploading、重试用同一密文成功）。断点续传（迁移 0008）：每成功一块把“已确认位图 + 节点会话”写入正式库（单事务提交），重开进程后复用会话并只补发剩余块；节点会话失效（UPLOAD_EXPIRED）时重新 begin、位图清零后重发一次，避免本地与节点长期不一致。证据：`upload_resumes_after_restart_and_sends_only_missing_chunks`、`transfer_progress_corruption_is_state_corrupted`（位图被篡改 → STATE_CORRUPTED）、`cargo test --offline -p chat-storage --test attachment_progress`。限制：进度按“已确认前缀”推进，不做并行块；节点配额/回执验证由 `NodeBlobTransport` 适配层完成（见“节点块传输适配实现记录”）。

<a id="attachments-004"></a>

## ATTACHMENTS-004 · build_attachment_descriptor

~~~text
build_attachment_descriptor(id: AttachmentId) -> Result<SecretDescriptor>
~~~

**职责与输出**：生成 E2EE 附件内容含密钥/大小/摘要/块规则和受限下载能力。

**输入与前置条件**：上传完成且摘要确认；不得发给未获准会话。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取受保护元数据。
2. 组装版本化 descriptor。
3. 加入文件名安全展示值。
4. 限制整体消息大小。
5. 返回敏感缓冲。

**持久化、并发与副作用**：只读；只交 crypto 内层事件编码，不写公开日志。

**失败处理**：ATTACHMENT_NOT_READY，CONTENT_TOO_LARGE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-008 encode_application_event](protocol.md#protocol-008)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 节点 API/日志无文件密钥：`attachments::tests::import_then_export_roundtrip_and_cache_has_no_plaintext`（描述符类型没有密钥字段；attachment_manifest 只含密文摘要；缓存文件里不出现明文摘要/正文）。
- [ ] 新设备不能拿旧 descriptor 下载历史。（未实现：受限下载能力与设备作用域绑定属于 E2EE SecretDescriptor/能力令牌，本轮未做）

**实现状态**：部分实现。只产出公开描述符 AttachmentDescriptor（ID、明文大小、块参数、明文摘要、展示文件名）与公开清单 AttachmentManifest（仅密文块数/大小/摘要）；不含 FileKey 与明文。ATTACHMENTS-004 契约要求的是含文件密钥/受限下载能力的 E2EE SecretDescriptor，本轮未实现，因此不标完整。

<a id="attachments-005"></a>

## ATTACHMENTS-005 · download_attachment

~~~text
download_attachment(id: AttachmentId) -> Result<TransferTicket>
~~~

**职责与输出**：耐久排队下载认证密文，随后可离线导出。

**输入与前置条件**：描述来自已认证消息；当前可见；能力未过期。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验 manifest 规则/大小。
2. 建立下载任务。
3. 查询已有块。
4. 只拉缺块。
5. 认证每块。
6. 保存加密缓存。
7. 校验完整摘要。

**持久化、并发与副作用**：每块短事务；完整文件未验证不标 available。

**失败处理**：INVALID_ATTACHMENT_DESCRIPTOR，ATTACHMENT_CORRUPTED，CAPABILITY_EXPIRED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CRYPTO-012 decrypt_attachment_chunk](crypto.md#crypto-012)、[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 重排/替换/缺最后块失败：`attachments::tests::download_rejects_tampered_chunks_and_leaves_no_partial_file`（替换第 2 块 → 分块 AAD 认证失败 → ATTACHMENT_CORRUPTED，删除半成品、不产生目标文件与临时文件）、`download_works_after_restart_and_rejects_unknown_blobs`（节点缺块/未知 blob → ATTACHMENT_NOT_FOUND）、`download_rejects_a_wrong_manifest_before_reading_any_chunk`（与本地记录不一致的 manifest 在任何网络读取前被拒）。
- [x] 断点续传（重启后只拉缺块）：`attachments::tests::download_resumes_after_restart_and_reads_only_missing_chunks`（第 3 块读取失败 → 前 2 块已解密写出并持久化前缀摘要 → 重开 store → 只读剩余 1 块、输出与原文字节一致、进度与半成品清理；半成品被篡改时拒绝续传并从头重下）。取消下载仍没有单独入口（取消走 ATTACHMENTS-007）。

**实现状态**：已实现（编排 + 状态机 + 断点续传，Windows x64 实测）。`PersistedAttachmentStore::download_attachment(blob_id, expected_manifest, destination, &mut transport)`：只接受 available/downloading 记录；先用 expected_manifest 与本地持久化记录（块数/块大小/明文大小/密文摘要）比对，不一致在任何网络读取前 `ATTACHMENT_CORRUPTED`；按迁移 0008 的进度位图续传：跳过已确认块、追加写同目录半成品、每块成功后持久化“已确认位图 + 已写前缀的明文摘要”，续传前重新读取本地前缀并核对摘要（不符则丢弃进度从头重下）；全部块认证通过后校验**明文大小 + 明文 SHA-256**（从头开始的完整下载额外校验密文整文件摘要）再 rename；损坏类错误删除半成品与进度，传输类错误保留两者供续传；解密用进程内 FileKey，缺失时先从 `attachment_secrets` 恢复（重启后仍可下载），密钥记录确实缺失才 `ATTACHMENT_KEY_UNAVAILABLE`。下载成功后把**收到的分块密文原样**写入本地加密缓存（复用 import 的缓存目录/文件名与同一 FileKey，不重新生成密钥），并按缓存块重算密文摘要校验后才把记录标 available；此后 `attachment_status` 的 available_chunks 为满、`export_attachment` 不联网也能导出（见"下载写入本地缓存与终态进度清理"）。证据：`download_roundtrip_matches_the_original_plaintext`、`download_works_after_restart_and_rejects_unknown_blobs`、`download_resumes_after_restart_and_reads_only_missing_chunks`、`download_fills_the_local_cache_for_offline_export`、`failed_download_rolls_back_new_cache_chunks`、上面三条拒绝用例。续传完成的下载也会补齐缓存（前缀块从节点补读 + 逐块 AEAD 校验后写入，见"续传下载补齐缓存与本地缓存回收"）；下载进度与半成品绑定同一目标路径（换目标目录视为新下载）；无 CAPABILITY_EXPIRED（下载能力令牌属 E2EE SecretDescriptor）；节点读取授权由 `NodeBlobTransport` 适配层承载（NODE-015：上传设备本人恒可读）。

<a id="attachments-006"></a>

## ATTACHMENTS-006 · export_attachment

~~~text
export_attachment(id: AttachmentId, destination: FileToken) -> Result<ExportReceipt>
~~~

**职责与输出**：仅完整认证文件流式输出到用户选择目标。

**输入与前置条件**：用户显式导出；所有块可读或已完成下载；平台支持安全输出策略。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 打开受控临时输出。
2. 依 index 认证解密。
3. 更新完整摘要。
4. 核对 total/final/size。
5. 同步并完成目标。
6. 清理敏感缓冲。

**持久化、并发与副作用**：外部输出不在 DB 事务；失败删除临时文件；不可提前暴露未验证成品。

**失败处理**：ATTACHMENT_CORRUPTED，FILE_IO_FAILURE，EXPORT_CANCELLED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLATFORM-006 open_file_token](platform.md#platform-006)、[CRYPTO-012 decrypt_attachment_chunk](crypto.md#crypto-012)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 导出中取消没有假完整文件。（导出未提供取消入口；失败路径删除同目录临时文件、不产生假完整文件，由 `attachments::tests::tampered_chunks_are_rejected_on_export` 与 `export_rejects_a_digest_mismatch` 覆盖失败语义）
- [x] 攻击文件名不能穿越路径：`attachments::tests::invalid_inputs_fail_explicitly`（展示名含分隔符/控制字符/过长/./.. 一律 INVALID_DISPLAY_NAME；畸形或穿越形式的附件 ID 一律 ATTACHMENT_NOT_FOUND）与 `cache_paths_derive_only_from_attachment_ids`（缓存路径只由附件 ID 派生，展示名不进路径）。

**实现状态**：已实现（本地导出，Windows x64 实测）。逐块认证解密 → 全文件总长与明文 SHA-256 复核 → rename 目标；任何失败删除临时文件。证据：`cargo test --offline -p chat-core --lib`（tampered_chunks_are_rejected_on_export / export_rejects_a_digest_mismatch / empty_file_roundtrip）。限制：无下载来源（只用本地缓存）、无 EXPORT_CANCELLED、无平台受控输出句柄。

<a id="attachments-007"></a>

## ATTACHMENTS-007 · cancel_transfer

~~~text
cancel_transfer(id: AttachmentId) -> Result<TransferStatus>
~~~

**职责与输出**：取消当前上传/下载任务，发送引用另有状态。

**输入与前置条件**：附件归当前账户，操作可重复。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 标 cancel_requested。
2. 取消当前网络流。
3. 保存位图。
4. 对未完成上传排清理。
5. 不删除已发送消息引用块。

**持久化、并发与副作用**：短事务；与上传完成竞争以耐久结果为准。

**失败处理**：ATTACHMENT_NOT_FOUND，TRANSFER_ALREADY_COMPLETE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 取消/完成同时到达最终状态一致。（未验证：没有网络流与并发完成竞争；当前状态机在单 Mutex 下串行，只有单元级终态语义）
- [x] 共享附件引用不误删：`attachments::tests::cancel_transfer_state_machine_preserves_progress`（取消只置 cancel_requested，不删除本地缓存块，取消后仍可查询与导出）。

**实现状态**：部分实现（本地状态机）。已实现：可重复取消幂等、available 拒绝取消（TRANSFER_ALREADY_COMPLETE）、取消保留缓存与进度。限制：无网络流取消、无持久化 cancel_requested、无“与上传完成竞争以耐久结果为准”的竞争模型。

<a id="attachments-008"></a>

## ATTACHMENTS-008 · collect_unreferenced_attachments

~~~text
collect_unreferenced_attachments(now: Time, budget: GcBudget) -> Result<GcReport>
~~~

**职责与输出**：回收 staging/取消/过期且没有消息/任务引用的缓存。

**输入与前置条件**：宽限期明确；扫描不持长事务。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 短事务选候选并标回收。
2. 检查引用计数。
3. 删除受控缓存。
4. 记录完成。
5. 失败下次幂等重做。

**持久化、并发与副作用**：两阶段 GC 记录避免 DB 与文件系统假原子；仅工作缓存内路径。

**失败处理**：FILE_IO_FAILURE，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 有附件记录引用的缓存不删：`attachments::tests::collect_removes_only_unreferenced_cache_directories` 与 `collect_keeps_record_referenced_directories_even_without_grace`（有记录的缓存目录原样保留，含其分块文件）。
- [x] 活跃上传/已接受消息引用不能删除（消息引用部分）：`attachments::tests::collect_keeps_message_referenced_staging_records_until_detached`（staging 记录 + 过期 + 被消息引用 → 记录与缓存都保留；解除引用后 → 记录与缓存一起回收）与 `attachments::tests::deleting_a_message_makes_its_staging_attachment_collectible`（消息**落库时自动登记引用**；删消息 → 引用级联清除 → 可回收）。任务/上传队列引用（outbox job 级）仍未接入。
- [ ] crash 后能继续回收。

**实现状态**：本地缓存回收已实现（记录 + 消息引用计数 + 宽限期，Windows x64 实测）。`PersistedAttachmentStore::collect_unreferenced_attachments() -> CollectReport` 使用默认宽限期 `DEFAULT_COLLECT_GRACE_MS = 24h`，`collect_unreferenced_attachments_with_grace(grace_ms)` 可配置（负数 `INVALID_INPUT`）。流程：短事务读「有记录引用的附件 ID」与「被消息引用的附件 ID」（storage 迁移 0009 的 `message_referenced_attachment_ids`/`attachment_ids`），再：① 清 staging 残留（state=staging、created_at 早于 now-grace、无消息引用 → 删记录连带缓存，计入 `removed_records`）；② 扫缓存根目录，只删名字符合 `att-<32hex>`、无记录/消息引用、且目录 mtime 早于 now-grace 的目录，计入 `removed_directories/freed_bytes`；名字异常的目录与散落文件不动；扫描项数有界（`MAX_COLLECT_SCAN_ENTRIES = 10_000`）。返回 `{ removed_directories, removed_records, freed_bytes, scanned_entries }`；幂等（无孤儿时 0）。证据：`collect_removes_only_unreferenced_cache_directories`、`collect_keeps_message_referenced_staging_records_until_detached`、`collect_respects_the_grace_period_and_is_idempotent`、`collect_keeps_record_referenced_directories_even_without_grace`；storage `message_attachments` 4 个用例（幂等引用/级联删除/未知行拒绝/引用集合与 staging 候选）。消息引用现在由**消息落库路径自动登记**（`apply_verified_entry_with_attachments`，同一笔事务）：见下文"消息落库自动登记附件引用（R-49）"。限制：任务/上传队列（outbox job）级引用未接入；没有“两阶段 GC 标记”，文件系统与 DB 不是原子的（失败下次幂等重做）；staging 记录来源（导入中断）当前实现不会产生，用例用 storage API 直接构造。通过本函数用例不自动代表整个模块完成。

## 上传/下载接块传输端口实现记录（ATTACHMENTS-003 / 005，2026-10-18，Windows x64）

代码在 [crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)（与本地导入/导出同文件），**不依赖 node-client**：节点交互抽象成可注入的块传输端口，真实节点适配与测试替身各自实现。本轮没有新增依赖、没有迁移（状态迁移复用 storage 的 `mark_attachment_state` 白名单 `ATTACHMENT_STATE_TRANSITIONS`）。

新增公开面：

~~~text
pub struct UploadRequest { attachment_id, plain_size, chunk_size, chunk_count, ciphertext_bytes, ciphertext_digest }
pub struct UploadSession { .. }            // new / as_str / into_id（不透明文本）
pub struct DownloadReceipt { attachment_id, written_bytes, content_digest }

pub trait BlobTransport {
    fn begin_upload(&mut self, request: UploadRequest) -> AttachmentResult<UploadSession>;
    fn put_chunk(&mut self, session: &str, index: u32, bytes: &[u8]) -> AttachmentResult<()>;
    fn complete(&mut self, session: &str, total_ciphertext_digest: &str) -> AttachmentResult<()>;
    fn read_chunk(&mut self, blob_id: &str, index: u32) -> AttachmentResult<Vec<u8>>;
}

impl PersistedAttachmentStore {
    pub fn upload_attachment<T: BlobTransport + ?Sized>(&self, attachment_id: &str, transport: &mut T) -> AttachmentResult<AttachmentStatus>;
    pub fn download_attachment<T: BlobTransport + ?Sized>(&self, blob_id: &str, expected_manifest: &AttachmentManifest, destination: &FileToken, transport: &mut T) -> AttachmentResult<DownloadReceipt>;
}

// 新增稳定错误码：ATTACHMENT 端口失败走 AttachmentError::Transport { code, detail }（code 由实现方给冻结码，例如 NODE_UNAVAILABLE）
~~~

上传语义（ATTACHMENTS-003）：

1. 只对已导入的记录工作，状态走白名单：`imported → uploading → available`；`available` 时重复调用是**幂等无操作**（不建会话、不发块、不重复 complete）；`cancelled/failed/downloading/staging` 显式 `ATTACHMENT_NOT_READY`。
2. 从本地缓存按序读取**分块密文**（不重新加密、不读明文），先 `begin_upload(UploadRequest)`（请求只含公开清单字段：块数/块大小/明文大小/密文总字节/密文摘要），再逐块 `put_chunk`，最后 `complete(整文件密文摘要)` 才把记录标 `available`。
3. 中途失败：本地密文缓存与已发送块计数（进程内 `transferred_chunks`）保留，状态停在 `uploading`，原样重试即可；每次尝试新建会话并按序重发同一密文字节（同会话同序号幂等由端口契约保证）。网络失败绝不标 available。
4. `blob_id` 本轮就是本地附件 ID（节点侧 blob 标识复用附件 ID，不新增第二种 ID）。

下载语义（ATTACHMENTS-005）：

1. 只接受 `available` / `downloading`（可重试）记录；`expected_manifest` 必须与本地持久化记录一致（附件 ID、块数、块大小、明文大小、密文摘要），不一致在**任何网络读取之前** `ATTACHMENT_CORRUPTED`。
2. 逐块 `read_chunk` → CRYPTO-012 分块 AAD 认证解密 → 同目录临时文件；全部块通过后再校验**密文整文件摘要 + 明文总长 + 明文 SHA-256**，三者都一致才 rename 成目标文件。
3. 任何失败（块缺失/篡改/manifest 不符/IO）删除临时文件，绝不写出未验证成品；失败时记录停在 `downloading`（可重试）。
4. 解密需要 FileKey：本进程有缓存时直接用；重启后从正式库的 `attachment_secrets` 取回（见下文 ADR 记录），只有密钥记录真的缺失时才显式 `ATTACHMENT_KEY_UNAVAILABLE`，且在拿到密钥前不发起任何块读取。

复现命令（全程离线，本轮实测）：

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --lib attachments   # 20 passed（14 既有 + 6 新增：上传 2 + 下载 4）
cargo test --offline -p chat-core                     # 70 passed（既有 64 个继续通过）
~~~

证据（测试名 → 覆盖的契约点）：

- `attachments::tests::upload_sends_the_cached_ciphertext_chunks_verbatim`：上传后节点收到的**每一块**都与本地缓存密文逐字节相同、节点收到的内容里不含明文；`UploadRequest` 的字段与公开清单一致（块数/块大小/明文大小/密文字节/密文摘要）；记录落 `available`；重复上传不建会话、不发块（幂等）。
- `attachments::tests::upload_failure_keeps_progress_and_retry_is_idempotent`：第 3 块失败时已发送 2 块、失败码来自端口（`NODE_UNAVAILABLE`）、状态停在 `uploading`、缓存块数仍等于块数、进程内进度 = 2；重试（新会话）成功后节点各块与本地密文一致、记录 `available`。
- `attachments::tests::download_roundtrip_matches_the_original_plaintext`：上传→下载的明文与原始正文逐字节一致，回执 `written_bytes`/`content_digest` 与描述符一致，块读取次数 = 块数。
- `attachments::tests::download_rejects_tampered_chunks_and_leaves_no_partial_file`：替换第 2 块 → `ATTACHMENT_CORRUPTED`，目标文件不存在、`*.download-partial` 也不存在，记录停在 `downloading`（可重试）。
- `attachments::tests::download_rejects_a_wrong_manifest_before_reading_any_chunk`：密文摘要被改 / manifest 属于另一个附件 → `ATTACHMENT_CORRUPTED` 且**零块读取**。
- `attachments::tests::download_works_after_restart_and_rejects_unknown_blobs`：节点缺块或未知附件 ID → `ATTACHMENT_NOT_FOUND`；重启后从 `attachment_secrets` 取回 FileKey，下载真实成功且字节一致；显式删除密钥记录后 → `ATTACHMENT_KEY_UNAVAILABLE` 且零读取、不写文件。

已知限制（本切片不覆盖）：

1. **节点适配在独立文件**：端口 `BlobTransport` 的节点实现是 [crates/core/src/attachment_transport.rs](../../crates/core/src/attachment_transport.rs) 的 `NodeBlobTransport`（NODE-012~015，含节点配额与回执验签），本文件（编排层）不含任何 HTTP/节点依赖；见下文“节点块传输适配实现记录”。
2. **进度不落盘**：附件表没有进度列，`transferred_chunks` 只在进程内；重启后重试从头重发（同序号同字节，端口侧幂等）。验收项“重启从位图续传”因此未勾选。
3. **下载不写本地加密缓存**：本轮直接认证解密写到目标路径（不重新加密入缓存），因此下载后不能离线 `export_attachment`；“只拉缺块”的位图也未实现。
4. **FileKey 已持久化（本轮）**：下载与导出在重启后可用（storage 的 `attachment_secrets`）；仍未实现的是**跨设备**派发（E2EE SecretDescriptor 的包装密钥派生/送达）与下载能力令牌/CAPABILITY_EXPIRED。
5. **取消与传输的竞争**：`cancel_transfer` 会把状态置 cancelled；若此时上传/下载已在进行，端口调用不会被中断（没有网络流句柄），完成回写会因白名单拒绝而显式失败（cancelled 是终态）。
6. **节点侧“同块只计一次配额”**属于 NODE-013 的记账，本层只保证重发同一字节。

## 节点块传输适配实现记录（NodeBlobTransport，ATTACHMENTS-003/005 的节点侧，2026-10-18，Windows x64）

实现位置：[crates/core/src/attachment_transport.rs](../../crates/core/src/attachment_transport.rs)（新增，`lib.rs` 加一行），测试：[crates/core/tests/attachment_transport.rs](../../crates/core/tests/attachment_transport.rs)（6 个用例，真 axum 节点 + 真 node-client）。编排层与端口契约不变：本文件只补“端口接到真节点”的实现、标识映射、错误码保真与已知差异。

公开面：

~~~text
pub struct NodeBlobTransport { .. }   // impl BlobTransport
impl NodeBlobTransport {
    pub fn new(client: NodeClient, group_id: impl Into<String>, source_seq: u64) -> AttachmentResult<Self>;
    pub fn device_id(&self) -> &str;  pub fn group_id(&self) -> &str;  pub fn source_seq(&self) -> u64;
    pub fn stats(&self) -> TransportStats;   // begin/put/complete/read 调用计数（证明没重发/没重复会话）
}
pub fn blob_object_id_for(attachment_id: &str) -> AttachmentResult<String>;          // att-… → obj:1:<hex>
pub fn upload_operation_id_for(attachment_id: &str, digest: &str) -> AttachmentResult<String>; // op:1:<hex>
pub const CIPHERTEXT_CHUNK_OVERHEAD: usize;   // 1 + nonce(12) + tag(16) = 29（CRYPTO-011 分块格式）
~~~

关键设计：

1. **同步/异步边界**：端口是同步的、NodeClient 是异步的，因此沿用 `node_submitter` 的做法——独立线程 + current-thread 运行时做桥（`TransportBridge`），任何上下文都能安全调用；桥断开时显式 `NODE_UNAVAILABLE`，绝不静默成功。
2. **标识映射（节点 blob ≠ 本地附件 ID）**：节点要求 `ObjectId` 文本与 `(设备, operation)` 幂等键，因此
   - blob 标识 = `obj:1:sha256("chat/attachment/blob-id/v1" || attachment_id)`（确定性，跨重启下载端无需注册表）；
   - begin 的 operation = `op:1:sha256("chat/attachment/upload-operation/v1" || attachment_id || ciphertext_digest)`（重试复用同一会话；不同附件/不同密文得到新会话）。
3. **声明块大小**：节点要求“非末块满块、末块 ≤ 声明块大小”。客户端分块密文 = 明文块 + 29 字节封装开销，因此 `chunk_count > 1` 时 `chunk_bytes = chunk_size + 29`；`chunk_count == 1` 时 `chunk_bytes = ciphertext_bytes`。发送前先做本地一致性校验（`ciphertext_bytes == plain_size + chunk_count * 29`），矛盾清单不发网络、也绝不篡改尺寸去凑策略。
4. **关联授权**：`source_seq` 必须是“作者正是本设备”的已接受日志条目（NODE-012 授权），由构造参数绑定；构造时校验群 ID 为 PROTOCOL-001 文本、序号非零。
5. **回执复核**：complete 的节点回执必须绑定本次清单（块数/总密文）与派生出的 blob 标识，否则 `INVALID_RECEIPT_SCOPE` 显式失败（不把“形状不对的回执”当成功）。

错误码映射（`AttachmentError::Transport { code, detail }`，`code` 优先节点稳定码，其次 PROTOCOL-014 码）：

| 节点/客户端错误 | 保留的稳定码 | 说明 |
|---|---|---|
| 节点返回的业务码 | `BLOB_UPLOAD_CONFLICT` / `CHUNK_CONFLICT` / `MANIFEST_MISMATCH` / `CHUNK_OUT_OF_ORDER` / `UPLOAD_EXPIRED` / `UPLOAD_NOT_FOUND` / `BLOB_NOT_FOUND` / `MISSING_CHUNKS` / `QUOTA_EXCEEDED` / `PERMISSION_DENIED` / `CONTENT_TOO_LARGE` / `RATE_LIMITED` | 直接透传节点码 |
| 网络/连接失败 | `NETWORK_UNAVAILABLE` | 调用方可重试 |
| 桥线程不可用 | `NODE_UNAVAILABLE` | 本地故障，状态不变 |
| 回执不绑定本次清单 | `INVALID_RECEIPT_SCOPE` | 协议违规，显式失败 |

复现命令（全程离线；节点在进程内起真 axum 服务，loopback）：

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test attachment_transport   # 6 passed
cargo test --offline -p chat-core                                # 76 passed（既有 70 个继续通过）
~~~

证据（测试名 → 覆盖点）：

- `attachment_transport::upload_publishes_chunks_the_node_returns_verbatim`：上传后节点读回的密文块与本地缓存**逐字节一致**、节点内容里没有明文；`TransportStats` 显示 begin/put/complete 各 1 次；记录落 available；重复上传不建会话、不发块（幂等）。
- `attachment_transport::download_from_the_node_decrypts_the_original_plaintext`：新 transport 从节点取回分块密文并解出原文（与源文件逐字节一致、回执摘要一致、读取次数 = 块数）；与本地记录不一致的清单在**任何节点读取之前**被 `ATTACHMENT_CORRUPTED` 拒绝。
- `attachment_transport::broken_link_keeps_the_attempt_retryable_and_retry_is_idempotent`：节点不可达 → `NETWORK_UNAVAILABLE`，状态停在 uploading、缓存块完整、记录未 available；换真节点重试成功（同 operation、同密文字节），之后再上传幂等无操作。
- `attachment_transport::multi_chunk_attachments_pass_the_node_chunk_policy_and_publish`：3 块（256 KiB 明文/块）附件在默认节点策略下真正发布为 `available`，本地尺寸未被篡改（节点块上限对齐后由后续任务改为正例断言）。
- `attachment_transport::published_blob_survives_a_node_restart`：文件库节点上传→**关闭节点→用同一库文件重启**→新客户端仍能下载并解出原文。
- `attachment_transport::node_errors_keep_their_stable_codes`：设备配额只够一份附件时第二份被 `QUOTA_EXCEEDED` 拒绝；签名钥错误时保留鉴权类冻结码；错误文本不含内容。

已知限制（本切片不覆盖）：

1. **节点块上限已对齐客户端分块（后续任务修正）**：节点 `BlobPolicy` 现在提供 `CLIENT_ATTACHMENT_CIPHERTEXT_CHUNK_BYTES`（256 KiB + 29 字节封装开销），默认 `max_chunk_bytes` 1 MiB、硬上限 4 MiB，且编译期断言 `DEFAULT_BLOB_CHUNK_BYTES >= CLIENT_ATTACHMENT_CIPHERTEXT_CHUNK_BYTES`。因此 256 KiB 分块的多块附件可以真正发布（用例 `multi_chunk_attachments_pass_the_node_chunk_policy_and_publish`）；本层的 `chunk_bytes` 换算不变，若节点策略再次收紧，该用例会立刻变红。
2. **断链时无“部分进度>0”的真实场景**：单块附件在第一次块写入前就失败（transferred = 0）；真正的“已发 2 块后失败”由编排层单元测试（假端口）覆盖。多块场景受限制 1 阻断。
3. **没有取消/超时的网络流句柄**：`cancel_transfer` 只改本地状态，进行中的 HTTP 请求不会被中断（与编排层记录的限制一致）。
4. **下载已可跨重启**（FileKey 从 `attachment_secrets` 取回）；仍未实现的是跨**设备**下载（需要把 E2EE SecretDescriptor 随消息送达）。
5. **一个 transport 绑定一个群 + 一条 source 条目**：多附件（不同 source seq）需要按消息构造 transport 或多绑定表；本轮按“一消息一附件一组”的切片构造。
6. **节点授权需先有本设备的已接受日志条目**：附件上传前必须已有一条本设备条目（测试用 append 真实写入），不存在“先传附件再发消息”的路径。

## EXPD 实现记录：exporter 包装密钥 + 协议字段 57（2026-10-19，Windows x64）

源码 [crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)、
[crates/core/src/attachment_message.rs](../../crates/core/src/attachment_message.rs)；
测试 [crates/core/tests/attachment_descriptor_protocol.rs](../../crates/core/tests/attachment_descriptor_protocol.rs)。

1. **包装密钥改为真实 exporter**：`attachment_exporter_wrapping_key(device, store, attachment_id,
   conversation_id, mls_epoch)` → `MlsDevice::export_key(store, "chat attachment file key",
   attachment_wrapping_context(attachment_id, conversation_id, mls_epoch), 32)`。crypto 的前置失败按**稳定码原样透出**
   （`EXPORTER_PENDING_COMMIT` / `EXPORTER_NOT_PERSISTED` / `STALE_EPOCH` / `USE_AFTER_EVICTION` / `CRYPTO_FAILURE`），
   新增 `AttachmentError::Exporter { code, detail }` 承载，绝不降级到占位派生。
   旧的 P0 派生只保留为 `attachment_legacy_wrapping_key`（仅用于打开 legacy v1 描述符）。
2. **描述符进协议字段 57**：`encode_attachment_metadata(body)` 产出 ≤512 字节的紧凑字节
   （version ‖ file_id(16) ‖ plain_size(8) ‖ ciphertext_bytes(8) ‖ chunk_bytes(4) ‖ chunk_count(4) ‖
   content_digest(32) ‖ ciphertext_digest(32) ‖ descriptor_len(2) ‖ descriptor ‖ binding(32)），
   attachment_id 由 file_id 确定性派生（`att-<hex>`）；`parse_attachment_metadata` 严格反解。
   消息正文只留 `[附件]`，**不含任何密钥材料**。
3. **接收侧优先级**：`PersistedAttachmentStore::adopt_attachment_event(metadata, legacy_body,
   exporter_key, legacy_key)` —— 有 57 字段就按描述符派生来源选钥匙（v2 → exporter、v1 → legacy）；
   没有 57 字段则回退旧正文路径；两路都没有描述符 → `ATTACHMENT_KEY_UNAVAILABLE`。
4. **legacy v1 仍可解**：`open_secret_descriptor` 两个版本都读；采用时用 legacy 密钥。

验证证据（`cargo test --offline -p chat-core`，**196 passed**，含新增 3 例）：

- `descriptor_travels_in_field_57_and_the_other_device_decrypts_independently`：描述符在字段 57
  （`attachment_metadata_from_event` 非空、旧正文为空）、正文只有 `[附件]`；**两个独立库**导出的 exporter
   key 相同；B 库采用后独立下载解密，明文逐字节一致。
- `legacy_body_with_v1_descriptor_is_still_accepted`：v1（legacy P0）描述符经旧正文路径仍可解；绑定不符先被拒。
- `exporter_preconditions_fail_explicitly_without_writes`：从未持久化群状态时 exporter 显式失败（稳定码）且零写入。

已知限制：

1. 两库导出的密钥相同这一点由“同一份已提交群状态写入两个库”验证（同一设备状态、两个独立数据库）；
   **两个真实不同设备**的等价性由 crypto 的 exporter 语义保证（同 epoch 导出相同字节），core 侧未再单独构造第二个 MLS 设备。
2. 字段 57 的 `display_name` 不随公开元数据走：接收侧用固定占位 `attachment`，原始文件名不进公开层。
3. 描述符仍在消息事件里（字段 57），只是不再占正文；正文摘要 `[附件]` 是既有占位约定。

## SECD 实现记录：附件密钥描述符随消息投递（2026-10-19，Windows x64）

源码 [crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)（编解码 + 采用）与
[crates/core/src/attachment_message.rs](../../crates/core/src/attachment_message.rs)（落库）；
测试 [crates/core/tests/attachment_descriptor.rs](../../crates/core/tests/attachment_descriptor.rs)。

解决的问题：FileKey 以前只在本机（迁移 0007），其它设备拿到同一条附件消息也解不开。现在把
**chat-crypto 的 E2EE SecretDescriptor** 放进受 MLS 加密的消息正文里随消息投递。

### 消息体（只在 MLS 内层，绝不进明文日志）

`@json
{ "kind": "chat.attachment.v1", "attachment_id": "att-<hex>", "file_id": "<32 hex>",
  "plain_size": 123, "content_digest": "<64 hex>", "ciphertext_bytes": 456,
  "chunk_bytes": 262144, "chunk_count": 2, "ciphertext_digest": "<64 hex>",
  "secret_descriptor": "<hex>", "binding": "<64 hex>" }
`@

- 发送侧 `PersistedAttachmentStore::encode_attachment_message_body(attachment_id, wrapping_key)`：
  从 0007 取 FileKey、用 `seal_secret_descriptor` 封装（AAD = attachment_id），把清单 + 描述符 +
  绑定摘要序列化成 JSON（正文由调用方放进 `ApplicationContent::Text`，受 MLS 加密）。
- 绑定(`binding`)：`SHA-256(域 ‖ attachment_id ‖ file_id ‖ ciphertext_bytes ‖ plain_size ‖
  content_digest ‖ chunk_bytes ‖ chunk_count ‖ ciphertext_digest)` —— 描述符/清单任一字段被改都会
  在落库前失败。
- 接收侧 `adopt_attachment_message(body, wrapping_key)`：绑定校验 → `open_secret_descriptor`（AAD =
  attachment_id）→ 已存在则逐字段核对并幂等返回 → 写附件记录 + 0007 密钥记录 → 之后
  `download_attachment` 用本机 FileKey 逐块认证解密。失败**零写入**（写入阶段的失败会删记录级联删密钥）。
- 落库两阶段（存储是单写者独占，附件库与内核不能同时持句柄）：先在附件库 `adopt`，再用
  `apply_attachment_message_event(core, event, attachment_id)` 写消息与引用；描述符不可用一定发生在
  任何消息写入之前。

### 错误语义

| 情形 | 码 |
|---|---|
| 描述符缺失/打不开（换会话/换 epoch/被篡改） | `ATTACHMENT_KEY_UNAVAILABLE` |
| 绑定不符（清单/摘要被改）、消息体畸形 | `ATTACHMENT_CORRUPTED` |
| 旧式 `ApplicationContent::Attachment`（无描述符）在独立库解析不到引用 | `ATTACHMENT_NOT_FOUND@（行为与 SECD 之前一致） |
| 旧式引用本机有记录但无 FileKey（下载/导出） | `ATTACHMENT_KEY_UNAVAILABLE` |

### 证据（`cargo test --offline -p chat-core --test attachment_descriptor`，真 MLS + 两个独立库）

- `another_store_can_decrypt_the_attachment_with_the_descriptor`：A 导入/上传 + 生成消息体；**B 在全新库里**
  仅凭消息体 + 会话包装密钥采用描述符、落消息与引用，随后独立下载并解密，导出内容与 A 的明文逐字节一致。
- `tampered_descriptor_or_manifest_is_rejected_without_writes`：改密文摘要 → `ATTACHMENT_CORRUPTED`；
  改描述符字节 / 换会话包装密钥 → `ATTACHMENT_KEY_UNAVAILABLE`；三种情况都**没有**留下附件记录。
- `legacy_attachment_reference_without_descriptor_cannot_be_downloaded`：旧式引用在本机（有解析端口）行为不变，
  在独立库被显式拒绝；本机无记录/无密钥时 `ATTACHMENT_NOT_FOUND`（下载路径对应 `ATTACHMENT_KEY_UNAVAILABLE`）。

### 已知限制

1. **包装密钥仍是 P0 派生（crypto 侧已备好 exporter，core 待切换）**：crypto 本轮已提供
   `MlsDevice::export_key(store, "chat attachment file key", context, len)`（OpenMLS
   `export_secret`，见 [crypto.md 的 R-76 记录](crypto.md)）与
   `attachment_wrapping_context(attachment_id, conversation_id, mls_epoch)`；**core 的**
   `attachment_message_wrapping_key(conversation_id, mls_epoch)` 仍是域分隔 SHA-256 占位派生
   （crates/core 不在 R-76 写范围）。切换时：core 用会话的 `MlsDevice` 调 `export_key` 得到包装密钥，
   并把描述符升级为 v2（exporter 派生）；**老 v1 legacy 描述符仍可读**（crypto 的
   `SecretDescriptor::derivation()` 可区分），因此历史消息不会因切换而不可解。描述符只经 MLS 密文
   传输，机密性来自 MLS；该密钥提供“同会话同 epoch 绑定 + 换会话/epoch 打不开”的纵深防御。
2. 明文摘要（`content_digest`）与明文大小进入消息体（按 TYPES.md 允许进入 E2EE 内层与本地记录），
   因此接收方在下载前就知道明文大小/摘要；它们不是秘密，但也不得进入节点公开清单。
3. 单附件：协议内层事件目前只允许一个附件引用；消息体也按单附件建模。
4. 采用后记录状态为 `downloading`，完成下载/导出后由既有状态机继续推进；本轮不改上传/下载状态机。

## 附件上传/下载接 FFI（ATT-U，2026-10-18，Windows x64）

代码：[crates/ffi/src/attachments_ffi.rs](../../crates/ffi/src/attachments_ffi.rs)（UniFFI 薄封装）+ [crates/core/src/attachment_node.rs](../../crates/core/src/attachment_node.rs)（装配：持久身份 → 设备凭据 → `NodeBlobTransport` → ATTACHMENTS-003/005 编排）；
测试：[crates/ffi/tests/attachment_node.rs](../../crates/ffi/tests/attachment_node.rs)（5 个，真 axum 节点 + 临时持久库）。

### 公开面（Kotlin 侧可见）

~~~text
// 持久后端：正式加密库（attachments 表 + 持久身份）+ 加密缓存目录
AttachmentFfi::open(database_path, master_key_hex, cache_root) -> Arc<AttachmentFfi>
// 旧的内存后端（保留兼容）：只有 import/status/export/cancel，upload/download 显式失败
AttachmentFfi::new(cache_root) -> Arc<AttachmentFfi>

upload(attachment_id, endpoint_url, node_id, node_public_key_hex?, group_id, source_seq) -> AttachmentView
download(blob_id, target_path, endpoint_url, node_id, node_public_key_hex?, group_id, source_seq) -> AttachmentDownloadView
~~~

- `AttachmentView` 新增 `transferred_chunks`（已传块数，失败后保留；不是协议确认）；
- `AttachmentDownloadView { attachment_id, written_bytes, content_digest }`；
- 设备签名钥由库里的**持久身份**派生（`load_identity_seeds` → `signing_key_from_seed`，与 send 链路同一条路径），FFI **不接受任何种子/口令入参**；
- `group_id`/`source_seq` 是 NODE-012 的上传授权关联（本设备已接受条目）；下载只需凭据（NODE-015），但端口构造仍要求两参数合法。

### 状态与错误（真实迁移，不伪造）

- 状态走 storage 白名单 `imported → uploading → available`；成功后 `state = "available"`、`available_chunks = chunk_count`；
  失败保留本地缓存与进度、状态停在 `uploading` 可重试；已 `available` 时重复上传是**幂等无操作（零发送）**。
- 错误码全部冻结：`INVALID_TARGET` / `INVALID_SIGNATURE` / `GROUP_NOT_FOUND` / `PERMISSION_DENIED` /
  `ATTACHMENT_NOT_FOUND` / `ATTACHMENT_CORRUPTED` / `ATTACHMENT_KEY_UNAVAILABLE` / `NODE_UNAVAILABLE` /
  `QUOTA_EXCEEDED` / `CONTENT_TOO_LARGE` / `INVALID_STATE_TRANSITION`（内存后端调用 upload/download）。

复现命令（全程离线；节点在进程内起真 axum 服务，loopback）：

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi --test attachment_node   # 5 passed
cargo test --offline -p chat-ffi -p chat-core             # 30 + 81 passed（既有 25 / 81 继续通过）
~~~

证据：

- `attachment_node::upload_reaches_the_node_and_download_returns_the_original_bytes`：导入 → 上传（`available`）→ 从**节点**下载并认证解密，写出字节与原文逐字节一致、回执摘要一致；重开对象后状态仍 `available`；另一实例下载 → `ATTACHMENT_KEY_UNAVAILABLE` 且不产生文件。
- `attachment_node::repeated_upload_is_idempotent_and_sends_nothing`：上传成功后**关掉节点**再上传 → 仍 `available`（幂等路径零发送）。
- `attachment_node::wrong_credentials_and_unknown_groups_fail_explicitly`：未知群 / 非本设备 `source_seq` / 坏端点 / 坏公钥 / 设备未在节点注册，全部显式失败，状态不会被标成 `available`。
- `attachment_node::in_memory_object_refuses_upload_and_download`：内存后端 → `INVALID_STATE_TRANSITION`。
- `attachment_node::download_without_a_record_is_rejected`：未知附件 → `ATTACHMENT_NOT_FOUND`，不写文件。

### 已知限制

1. **跨设备下载**仍需要 E2EE SecretDescriptor 的包装密钥派生与送达（crypto 的封装格式已实现并可导出描述符；MLS application secret 接线未做），本机跨重启已通过 `attachment_secrets` 支持。
2. **节点块上限已对齐客户端分块**：默认 1 MiB（硬上限 4 MiB）≥ 客户端 256 KiB 密文块，因此多块附件也能上传；见上文节点块传输适配记录与 `multi_chunk_attachments_pass_the_node_chunk_policy_and_publish`。
3. `AttachmentFfi::open` 的库是**独占单写者**：与 `ConversationCore`/`LocalStoreFfi` 不能同时打开同一库，调用方需先关闭另一个对象。
4. 上传/下载是同步阻塞调用（FFI `Mutex` 串行化），大文件不要在 UI 线程直接跑。
5. `Cargo.lock` 变化：`chat-ffi` 新增 dev-dependencies（`chat-node-api`/`chat-node-client`/`chat-node-services`/`axum`/`tokio`/`ed25519-dalek`）供端到端用例起真节点；这些包在锁文件中已存在，无版本变动。

## FileKey 持久化与 E2EE SecretDescriptor（ADR 摘要，R-36/X 包，2026-10-18，Windows x64）

ADR 见 [docs/design/FILE_KEY_PERSISTENCE.md](../design/FILE_KEY_PERSISTENCE.md)（状态：第 1 步已落地、
第 2 步格式已落地、第 3 步依赖 PLATFORM）。本轮把 FileKey 真正持久化，使“关掉重开仍可导出/下载”
成为可验证行为，而不是靠进程内缓存。

### 决策摘要

<a id="expd"></a>

## EXPD：core 切换到 exporter + 协议 57 字段（task-78）

- **包装密钥**：附件消息体的 wrapping key 改为 `MlsDevice::export_key(store, "chat attachment file key", attachment_wrapping_context(attachment_id, conversation_id, mls_epoch), 32)`；crypto 的稳定码（`EXPORTER_NOT_PERSISTED`/`STALE_EPOCH`/`EXPORTER_PENDING_COMMIT`/`USE_AFTER_EVICTION`）原样透出，**不伪造成功**；P0 占位派生已从新写入路径移除。
- **协议承载**：描述符写入 `AttachmentRef.attachment_metadata`（PROTOCOL 字段 **57**，≤512 字节），正文只留人类可读摘要（不再把密钥材料塞正文）。读侧 `attachment_metadata_from_event` 优先取 57；缺失时回退旧的正文内嵌路径（向后兼容）；两条路都没有描述符 → `ATTACHMENT_KEY_UNAVAILABLE`。
- **legacy 兼容**：v1 legacy 描述符仍可解析/打开（`SecretDescriptor::is_legacy()`），exporter 派生的 v2 才用于新写入。
- **验证**：`cargo test -p chat-core` → 196 passed（新增 3：描述符只出现在 57 字段且正文不含密钥材料、跨设备独立下载解密逐字节一致、exporter 前置不满足时显式失败且零写入）。
- **限制**：export_key 要求"当前 epoch 已持久化"；未做多 epoch 密钥历史（v2 描述符只能用写入时 epoch 的 exporter 值打开）；旧消息仍走正文路径。
## 字段 57 承载展示名（ATTACHMENTS metadata v2，2026-10-18，Windows x64）

背景：协议字段 57（`attachment_metadata`）此前没有文件名，对端显示名是固定占位 `attachment`。

代码：[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)（紧贴既有 EXPD 段定点改动）；
测试：[crates/core/tests/attachment_display_name.rs](../../crates/core/tests/attachment_display_name.rs)（4 个，真双设备两库）。

### 布局（版本化）

~~~text
v1（老格式，仍可解析）：version=1 ‖ file_id(16) ‖ plain_size(8) ‖ ciphertext_bytes(8)
    ‖ chunk_bytes(4) ‖ chunk_count(4) ‖ content_digest(32) ‖ ciphertext_digest(32)
    ‖ descriptor_len(2) ‖ descriptor ‖ binding(32)
v2（新写入）：同 v1，但在 descriptor 之后、binding 之前插入
    name_len(1) ‖ display_name(UTF-8 ≤128 字节)
~~~

- 解析器接受 v1 与 v2：**v1 没有名字 → 回退占位名** `attachment`（老客户端发的附件不会失效）；
  新写入一律 v2，带清洗后的名字。
- 名字长度前缀 u8 + 总长上限 `MAX_ATTACHMENT_METADATA_BYTES = 512` 双重校验；
  超长（含名字 129 字节）**显式拒绝** `CONTENT_TOO_LARGE`，绝不静默截断。
- v2 解析要求"名字段 + binding 恰好到字节末尾"（`metadata.len() == cursor + name_len + 32`），
  多一字节少一字节都按 `ATTACHMENT_CORRUPTED` 拒绝。

### 名字清洗

`sanitize_metadata_display_name(name)`：过滤 **ASCII 控制字符**（含 ``）、裁剪首尾空白、
空名回退 `attachment`；`metadata_display_name_for_encode` 在此基础上超过 128 字节 → 显式拒绝。
名字**只作展示**，永不参与路径拼接。

### 接收侧

`parse_attachment_metadata` 把（清洗后的）名字写进 `AttachmentMessageBody.display_name`，
`adopt_attachment_metadata` → `adopt_attachment_message` 落 0007 附件记录，
因此对端 `attachment_record(id).display_name` 就是发送方的文件名。

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test attachment_display_name   # 4 passed
cargo test --offline -p chat-core                                   # 212 passed（既有 208 继续通过）
~~~

### 证据

- `attachment_display_name::chinese_display_name_survives_the_cross_device_metadata_path`：
  中文名 `会议纪要.txt` 编码进字段 57 → 本机解析一致 → 对端（独立库 B + exporter 密钥）采用后
  附件记录的 `display_name` 一致。
- `attachment_display_name::a_name_of_128_bytes_is_accepted_and_129_is_rejected`：128 字节接受并可回读；
  129 字节 → `CONTENT_TOO_LARGE`；清洗函数同样拒绝超长。
- `attachment_display_name::legacy_metadata_without_a_name_falls_back_to_the_placeholder`：
  去掉名字段、版本改回 1 后仍能解析，展示名回退 `attachment`。
- `attachment_display_name::control_characters_are_filtered_and_blank_names_fall_back`：
  `\t \n \u{7f}` 被过滤、首尾空白裁剪（`  a\tb\nc\u{7f}.txt  ` → `abc.txt`）；
  纯空白 → `attachment`；对端落库的展示名是清洗后的 `报告.pdf`。

### 已知限制

1. **名字占用字段 57 的预算**：描述符很大（接近 256 字节上限）时，长名字会让总长超过 512 字节而被拒绝
   （显式 `CONTENT_TOO_LARGE`，不是静默截断）。
2. **只做清洗不做规范化**：不做 Unicode NFC/NFKC 归一，不同归一形式会被视为不同名字（显示无碍）。
3. **路径分隔符不拦**：`sanitize` 只过滤控制字符；文件名里的 `/` 或 `\` 原样保留（展示用，
   永不参与路径拼接；落库前的 `validate_display_name` 仍会拒绝分隔符）。
4. **老客户端收不到名字**：v2 字段对 v1 解析器是新版本，会按"不支持版本"拒绝——需要老客户端升级
   才能读新消息的字段 57（v1 解析器只认 version=1）。
## 字段 57 双版本兼容窗口（ATTACHMENTS metadata v1↔v2，2026-10-18，Windows x64）

上一轮把展示名放进了字段 57，但放在 binding **之前**——老 v1 解析器会把名字段当 binding，
等于"老客户端读新消息即损坏"。本轮把展示名改成**尾部追加段**并开兼容窗口。

代码：[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)（定点）；
测试：[crates/core/tests/attachment_metadata_compat.rs](../../crates/core/tests/attachment_metadata_compat.rs)（4 个，字节级）。

### 最终布局

~~~text
所有版本共享的前缀（v1 逐字节不变）：
  version(1) ‖ file_id(16) ‖ plain_size(8) ‖ ciphertext_bytes(8) ‖ chunk_bytes(4)
  ‖ chunk_count(4) ‖ content_digest(32) ‖ ciphertext_digest(32)
  ‖ descriptor_len(2) ‖ descriptor ‖ binding(32)
v2 在**末尾追加**：name_len(1) ‖ display_name(UTF-8 ≤128 字节)
~~~

版本登记表：`ATTACHMENT_METADATA_VERSIONS = [1, 2]`，`attachment_metadata_version_supported(v)`
是唯一判定入口；**未登记版本（0、3、9、255…）一律显式拒绝**（`ATTACHMENT_CORRUPTED`），绝不猜布局。

### 兼容窗口语义

| 输入 | 结果 |
|---|---|
| v1 主体（无尾部） | 正常解析，展示名回退 `attachment` |
| v1 主体 + 任意尾部多余字节（发送方是 v2） | **忽略尾部**、回退占位名、不报错 |
| v2 主体 + 合法 `name_len‖name` | 用尾部段里的名字（清洗后） |
| v2 但完全没有尾部段 | `ATTACHMENT_CORRUPTED`（v2 必须带名字段） |
| v2 但前缀长度与实际尾部不符（多/少一字节） | `ATTACHMENT_CORRUPTED` |
| v2 名字 > 128 字节或不是 UTF-8 | `ATTACHMENT_CORRUPTED` |

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test attachment_metadata_compat   # 4 passed
cargo test --offline -p chat-core --test attachment_display_name      # 4 passed（布局改动后仍绿）
cargo test --offline -p chat-core                                     # 216 passed（既有 212 继续通过）
~~~

### 证据

- `attachment_metadata_compat::a_v1_view_accepts_v2_bytes_and_falls_back_to_the_placeholder`：
  v2 字节用 v2 视角解析得到真实名字；把版本字节改回 1（模拟老解析器）后仍能解析，展示名回退占位，
  且 `file_id/binding/chunk_count` 与 v2 视角完全一致。
- `attachment_metadata_compat::plain_v1_metadata_is_unchanged`：纯 v1 正常解析；v1 后跟 9 个多余字节
  也被忽略（binding 不变）。
- `attachment_metadata_compat::unknown_versions_are_rejected_and_are_not_registered`：
  登记表就是 `[1, 2]`；版本 0/3/9/255 全部 `ATTACHMENT_CORRUPTED`。
- `attachment_metadata_compat::v2_trailer_bounds_and_shape_are_still_enforced`：
  名字 129 字节、尾部多一字节、尾部被截断、v2 无尾部段、非 UTF-8 名字全部拒绝；
  128 字节边界可以解析。

### 已知限制

1. **老解析器仍会把 v2 的版本字节当未知版本拒绝**：兼容窗口在**新版解析器**里实现（v1 语义读 v2 字节）。
   真正让"未升级的旧客户端"读新消息，需要旧客户端本身按 v1 语义忽略尾部——即必须发一个只放宽
   版本检查、不解释尾部的过渡版本。
2. **尾部段无长度校验上限之外的语义**：名字仍在 512 字节总预算内；描述符接近 256 上限时，
   长名字会让总长超限而显式 `CONTENT_TOO_LARGE`。
3. **v2 必须带名字段**（哪怕空名也要写 `name_len=0` 的回退字节）：这是"版本=结构存在"的约定，
   避免同一版本有两种形状。



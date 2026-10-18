# 文件导入、分块上传、下载和导出

阶段：P2。状态：函数级设计；本地导入/导出与本地状态机（ATTACHMENTS-001/002/004/006/007 的本地部分）已实现；ATTACHMENTS-003 上传与 ATTACHMENTS-005 下载已通过可注入的块传输端口（`BlobTransport`）接通编排与状态机（节点客户端由适配层实现，本 crate 不依赖 node-client）；含 FileKey 的 E2EE SecretDescriptor、真实节点配额/回执校验与持久化进度仍未实现。目标源码：[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)。

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
- [ ] 重启从位图续传。（未实现：进度只在进程内，附件表没有进度列；重启后重试会新建会话并从第 0 块重发——同序号同字节，节点侧仍幂等）

**实现状态**：已实现（编排 + 状态机，Windows x64 实测）。`PersistedAttachmentStore::upload_attachment(attachment_id, &mut transport)`：按 storage 白名单 imported → uploading → available 迁移；从本地加密缓存按序读取**密文块**（不重新加密、不发明文）→ `BlobTransport::begin_upload` →逐块 `put_chunk` → `complete(整文件密文摘要)` → 标 available；已 available 时重复调用是幂等无操作；中途失败保留缓存与已发送块计数、状态停在 uploading 可重试；任何网络失败都不会把记录标成 available。证据：`attachments::tests::upload_sends_the_cached_ciphertext_chunks_verbatim`（节点收到的每块与本地缓存密文逐字节一致、节点永不收到明文、请求字段与公开清单一致、重复上传零发送）、`upload_failure_keeps_progress_and_retry_is_idempotent`（第 3 块失败时已发 2 块、缓存与进度保留、状态 uploading、重试用同一密文成功）。限制：节点配额/回执验证、位图续传、持久化进度未实现（见“上传/下载接块传输端口实现记录”）。

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

- [x] 重排/替换/缺最后块失败：`attachments::tests::download_rejects_tampered_chunks_and_leaves_no_partial_file`（替换第 2 块 → 分块 AAD 认证失败 → ATTACHMENT_CORRUPTED，不产生目标文件与临时文件）、`download_rejects_unknown_blobs_and_missing_keys`（节点缺块/未知 blob → ATTACHMENT_NOT_FOUND）、`download_rejects_a_wrong_manifest_before_reading_any_chunk`（与本地记录不一致的 manifest 在任何网络读取前被拒）。
- [ ] 取消下载保留可恢复进度。（未实现：下载没有取消入口；失败时状态保留在 downloading 且不写任何文件，可重新调用重试）

**实现状态**：已实现（编排 + 状态机，Windows x64 实测）。`PersistedAttachmentStore::download_attachment(blob_id, expected_manifest, destination, &mut transport)`：只接受 available/downloading 记录；先用 expected_manifest 与本地持久化记录（块数/块大小/明文大小/密文摘要）比对，不一致在任何网络读取前 `ATTACHMENT_CORRUPTED`；逐块 `read_chunk` → 分块 AAD 认证解密 → 同目录临时文件 → 校验**密文整文件摘要 + 明文大小 + 明文 SHA-256** 后 rename；任何失败删除临时文件、绝不写出未验证文件；解密需要进程内 FileKey（重启后显式 `ATTACHMENT_KEY_UNAVAILABLE`）。证据：`download_roundtrip_matches_the_original_plaintext`（下载得到的明文与原始正文逐字节一致、回执摘要一致）、上面三条拒绝用例。限制：不写本地加密缓存（本轮直接写出目标路径）、无“只拉缺块”的位图、无 CAPABILITY_EXPIRED（下载能力令牌属 E2EE SecretDescriptor）。

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

- [ ] 活跃上传/已接受消息引用不能删除。
- [ ] crash 后能继续回收。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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
4. 解密需要进程内的 FileKey（不持久化）：重启后（没有进程内记录）显式 `ATTACHMENT_KEY_UNAVAILABLE`，且在拿到密钥前不发起任何块读取。

复现命令（全程离线，本轮实测）：

~~~powershell
. .	oolsactivate.ps1
cargo test --offline -p chat-core --lib attachments   # 20 passed（14 既有 + 6 新增：上传 2 + 下载 4）
cargo test --offline -p chat-core                     # 70 passed（既有 64 个继续通过）
~~~

证据（测试名 → 覆盖的契约点）：

- `attachments::tests::upload_sends_the_cached_ciphertext_chunks_verbatim`：上传后节点收到的**每一块**都与本地缓存密文逐字节相同、节点收到的内容里不含明文；`UploadRequest` 的字段与公开清单一致（块数/块大小/明文大小/密文字节/密文摘要）；记录落 `available`；重复上传不建会话、不发块（幂等）。
- `attachments::tests::upload_failure_keeps_progress_and_retry_is_idempotent`：第 3 块失败时已发送 2 块、失败码来自端口（`NODE_UNAVAILABLE`）、状态停在 `uploading`、缓存块数仍等于块数、进程内进度 = 2；重试（新会话）成功后节点各块与本地密文一致、记录 `available`。
- `attachments::tests::download_roundtrip_matches_the_original_plaintext`：上传→下载的明文与原始正文逐字节一致，回执 `written_bytes`/`content_digest` 与描述符一致，块读取次数 = 块数。
- `attachments::tests::download_rejects_tampered_chunks_and_leaves_no_partial_file`：替换第 2 块 → `ATTACHMENT_CORRUPTED`，目标文件不存在、`*.download-partial` 也不存在，记录停在 `downloading`（可重试）。
- `attachments::tests::download_rejects_a_wrong_manifest_before_reading_any_chunk`：密文摘要被改 / manifest 属于另一个附件 → `ATTACHMENT_CORRUPTED` 且**零块读取**。
- `attachments::tests::download_rejects_unknown_blobs_and_missing_keys`：节点缺块或未知附件 ID → `ATTACHMENT_NOT_FOUND`；“重启”后没有进程内 FileKey → `ATTACHMENT_KEY_UNAVAILABLE` 且零读取、不写文件。

已知限制（本切片不覆盖）：

1. **真实节点适配未接线**：`BlobTransport` 是端口，NODE-012/013/014 的 begin/put/complete 与节点配额、回执验证由适配层实现（参考 `node_submitter` 的适配方式），本 crate 不含任何 HTTP/节点依赖。
2. **进度不落盘**：附件表没有进度列，`transferred_chunks` 只在进程内；重启后重试从头重发（同序号同字节，端口侧幂等）。验收项“重启从位图续传”因此未勾选。
3. **下载不写本地加密缓存**：本轮直接认证解密写到目标路径（不重新加密入缓存），因此下载后不能离线 `export_attachment`；“只拉缺块”的位图也未实现。
4. **FileKey 仍只在进程内**：下载与导出都要求“本进程导入过”，E2EE SecretDescriptor（含 FileKey 的受保护记录）与下载能力令牌/CAPABILITY_EXPIRED 仍未实现。
5. **取消与传输的竞争**：`cancel_transfer` 会把状态置 cancelled；若此时上传/下载已在进行，端口调用不会被中断（没有网络流句柄），完成回写会因白名单拒绝而显式失败（cancelled 是终态）。
6. **节点侧“同块只计一次配额”**属于 NODE-013 的记账，本层只保证重发同一字节。



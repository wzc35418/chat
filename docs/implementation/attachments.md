# 文件导入、分块上传、下载和导出

阶段：P2。状态：函数级设计；本地导入/导出与本地状态机（ATTACHMENTS-001/002/004/006/007 的本地部分）已实现，上传/下载、持久化与含 FileKey 的 E2EE SecretDescriptor 未实现。目标源码：[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)。

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
- [x] 进度最大值不超过实际大小：同一用例与 `import_then_export_roundtrip_and_cache_has_no_plaintext`（available_chunks/transferred_chunks 都被 min 到 chunk_count；网络接入前 transferred_chunks 恒为 0）。

**实现状态**：部分实现（本地内存状态机）。查询聚合的是本地缓存实际块数与已记录进度，不返回 FileKey；取消是终态之一且可重复查询。限制：无持久化（重启丢失状态）、无网络进度与错误重试信息；PERMISSION_DENIED 需要账户作用域，当前单账户本地库未实现。

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

- [ ] 响应丢失重复块不会加倍计配额。
- [ ] 重启从位图续传。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 重排/替换/缺最后块失败。
- [ ] 取消下载保留可恢复进度。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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


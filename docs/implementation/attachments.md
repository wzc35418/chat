# 文件导入、分块上传、下载和导出

阶段：P2。状态：函数级设计，尚未实现。目标源码：[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)。

平台 file token 仅代表用户授权的一次访问，不将任意路径传入内核。默认单附件 25 MiB，按完整原文大小计；节点配额按密文实际字节计。文件密钥只在受保护本地记录和 E2EE 描述中，不能放节点 URL 或公开清单。

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

- [ ] 未知大小流超25 MiB即停止。
- [ ] 峰值内存受块预算限制。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 取消后可查询。
- [ ] 进度最大值不超过实际大小。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 节点 API/日志无文件密钥。
- [ ] 新设备不能拿旧 descriptor 下载历史。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 导出中取消没有假完整文件。
- [ ] 攻击文件名不能穿越路径。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 取消/完成同时到达最终状态一致。
- [ ] 共享附件引用不误删。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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


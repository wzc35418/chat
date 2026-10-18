package chat.presentation.preview

import chat.api.LocalAttachment

/**
 * 预览里的“附件（本机）”状态。
 *
 * 当前只覆盖本机链路（导入/状态/导出/取消）；上传下载（NODE-012~015）端点已就绪但尚未接线，
 * 界面必须如实显示“未上传”，不能把 imported 说成可下载。
 */
data class PreviewAttachmentState(
    val ready: Boolean = false,
    val busy: Boolean = false,
    val attachments: List<LocalAttachment> = emptyList(),
    val notice: String? = null,
    val lastErrorCode: String? = null,
    val importPath: String = "",
    val exportPath: String = "",
)

fun PreviewAttachmentState.withImported(attachment: LocalAttachment): PreviewAttachmentState {
    val merged = LinkedHashMap<String, LocalAttachment>()
    attachments.forEach { merged[it.attachmentId] = it }
    merged[attachment.attachmentId] = attachment
    return copy(
        attachments = merged.values.toList(),
        busy = false,
        notice = "已加密导入：" + attachment.displayName + "（" + attachment.plainSize + " 字节，" +
            attachment.chunkCount + " 块）；上传未接线，不会离开本机。",
        lastErrorCode = null,
    )
}

fun PreviewAttachmentState.withUpdated(attachment: LocalAttachment): PreviewAttachmentState {
    val merged = LinkedHashMap<String, LocalAttachment>()
    attachments.forEach { merged[it.attachmentId] = it }
    merged[attachment.attachmentId] = attachment
    return copy(attachments = merged.values.toList(), busy = false, lastErrorCode = null)
}

fun PreviewAttachmentState.withExported(attachmentId: String, writtenBytes: Long, target: String): PreviewAttachmentState = copy(
    busy = false,
    notice = "已解密导出 " + writtenBytes + " 字节到 " + target + "（原文摘要已校验）。",
    lastErrorCode = null,
)

fun PreviewAttachmentState.updateImportPath(value: String): PreviewAttachmentState = copy(importPath = value)

fun PreviewAttachmentState.updateExportPath(value: String): PreviewAttachmentState = copy(exportPath = value)

fun PreviewAttachmentState.beginBusy(): PreviewAttachmentState = copy(busy = true, notice = null)

fun PreviewAttachmentState.withError(code: String, message: String? = null): PreviewAttachmentState = copy(
    busy = false,
    lastErrorCode = code,
    notice = message ?: "附件操作没有成功；界面不会把失败当成功。",
)

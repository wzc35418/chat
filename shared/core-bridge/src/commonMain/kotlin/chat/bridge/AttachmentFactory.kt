package chat.bridge

import chat.api.AttachmentService

/** 创建本地附件服务桥（ATTACHMENTS 本机链路）。 */
expect fun createAttachmentService(cacheRoot: String): AttachmentService

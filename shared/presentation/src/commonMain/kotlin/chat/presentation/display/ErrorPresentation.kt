package chat.presentation.display

import chat.api.CoreFailure

enum class ErrorAction {
    RETRY,
    RECONNECT,
    UNLOCK,
    VERIFY_IDENTITY,
    WAIT,
    RESYNC,
    NONE,
}

/**
 * 已脱敏、可直接展示的错误。diagnosticCode / diagnosticId 只保留稳定且
 * 无秘密的标识，不包含异常原文、堆栈、邀请口令或消息正文。
 */
data class UserActionableError(
    val title: String,
    val detail: String,
    val actionLabel: String?,
    val action: ErrorAction,
    /** 用户能否手动重试（由底层 CoreFailure.retryable 决定）。 */
    val retryable: Boolean,
    val diagnosticCode: String?,
    val diagnosticId: String?,
    /** 底层是否允许自动退避重试：只取自 PROTOCOL-014 冻结规则，与手动重试分开。 */
    val autoRetryAllowed: Boolean = false,
    /** 底层原因链（已截断、已脱敏）：给用户排障用，可空。 */
    val reason: String? = null,
)

private data class ErrorCopy(
    val title: String,
    val detail: String,
    val action: ErrorAction,
    val actionLabel: String?,
)

/**
 * PROTOCOL-014 冻结的 41 个错误码，与 crates/protocol/src/error.rs 的 ErrorCode::ALL 对齐。
 * 展示层必须为每个码提供稳定文案，不能落到未知错误回退。
 */
val FROZEN_ERROR_CODES: Set<String> = setOf(
    "NETWORK_UNAVAILABLE", "NODE_UNAVAILABLE", "RATE_LIMITED", "QUOTA_EXCEEDED",
    "PERMISSION_DENIED", "DEVICE_REVOKED", "CONTACT_NOT_ACCEPTED", "STALE_CONTROL_HEAD",
    "STALE_EPOCH", "MISSING_LOG_ENTRIES", "INVALID_CREDENTIAL", "INVALID_CIPHERTEXT",
    "LOG_CONFLICT", "PROTOCOL_UNSUPPORTED", "CONTENT_TOO_LARGE", "STORAGE_LOCKED",
    "STORAGE_FAILURE", "STATE_CORRUPTED", "INVALID_ID", "MALFORMED_FRAME",
    "INVALID_CANDIDATE", "INVALID_SIGNATURE", "INVALID_SEQUENCE", "INVALID_EXTENSION",
    "INVALID_CURSOR", "CURSOR_EXPIRED", "INVALID_CURSOR_SCOPE", "INVALID_TARGET",
    "GROUP_LIMIT_EXCEEDED", "DEVICE_LIMIT_EXCEEDED", "OWNER_TRANSFER_REQUIRED", "NOT_MEMBER",
    "INVALID_STATE_TRANSITION", "INVALID_RECEIPT_SCOPE", "INVALID_REPLY", "MESSAGE_NOT_VISIBLE",
    "HISTORY_NOT_AUTHORIZED", "INVALID_EVENT_BINDING", "REQUIRED_CAPABILITY_MISSING",
    "UNKNOWN_RESULT", "INTERNAL_FAILURE",
)

/** 冻结规则：只有这 7 个码允许自动退避重试；验证/损坏/权限类一律不可自动重试。 */
val FROZEN_AUTO_RETRY_CODES: Set<String> = setOf(
    "NETWORK_UNAVAILABLE",
    "NODE_UNAVAILABLE",
    "RATE_LIMITED",
    "STALE_CONTROL_HEAD",
    "STALE_EPOCH",
    "MISSING_LOG_ENTRIES",
    "STORAGE_LOCKED",
)

private val knownErrors: Map<String, ErrorCopy> = mapOf(
    "STORAGE_LOCKED" to ErrorCopy(
        title = "本地数据已锁定",
        detail = "解锁后才能读取或发送消息。",
        action = ErrorAction.UNLOCK,
        actionLabel = "解锁",
    ),
    "STATE_CORRUPTED" to ErrorCopy(
        title = "本地状态需要恢复",
        detail = "检测到无法安全读取的本地状态，已停止继续操作。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "INVALID_CURSOR" to ErrorCopy(
        title = "列表已更新",
        detail = "当前页面的定位已失效，请重新加载。",
        action = ErrorAction.RESYNC,
        actionLabel = "重新加载",
    ),
    "PERMISSION_DENIED" to ErrorCopy(
        title = "没有执行此操作的权限",
        detail = "请确认当前群成员权限，或联系群管理员。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "CONTENT_TOO_LARGE" to ErrorCopy(
        title = "消息内容过大",
        detail = "请缩短消息或移除附件后再发送。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "STORAGE_FAILURE" to ErrorCopy(
        title = "本地保存失败",
        detail = "消息没有保存到本地，请检查存储空间后重试。",
        action = ErrorAction.RETRY,
        actionLabel = "重试",
    ),
    "RETRY_NOT_ALLOWED" to ErrorCopy(
        title = "此消息不能重试",
        detail = "当前状态不允许再次发送这条消息。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "ACCEPTANCE_UNKNOWN" to ErrorCopy(
        title = "发送结果待确认",
        detail = "网络或节点没有返回确定结果，不会自动重发。",
        action = ErrorAction.WAIT,
        actionLabel = "等待对账",
    ),
    "STALE_CONTROL_HEAD" to ErrorCopy(
        title = "群状态已变化",
        detail = "群设置已被其他操作更新，请刷新后再试。",
        action = ErrorAction.RESYNC,
        actionLabel = "刷新群信息",
    ),
    "STALE_EPOCH" to ErrorCopy(
        title = "正在同步群密钥",
        detail = "群成员或密钥状态已更新，正在同步，请稍后重试。",
        action = ErrorAction.WAIT,
        actionLabel = "等待同步完成",
    ),
    "LOG_CONFLICT" to ErrorCopy(
        title = "群同步已暂停",
        detail = "检测到冲突的群日志，已暂停推进以避免数据损坏，请保留现场并等待处理。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "CORE_CLOSED" to ErrorCopy(
        title = "会话已关闭",
        detail = "当前会话已经结束，需要重新打开后再操作。",
        action = ErrorAction.RECONNECT,
        actionLabel = "重新打开",
    ),
    "NETWORK_UNAVAILABLE" to ErrorCopy(
        title = "网络不可用",
        detail = "当前无法连接服务，请检查网络后重试。",
        action = ErrorAction.RETRY,
        actionLabel = "重试",
    ),
    "UNSUPPORTED_RENDER_TYPE" to ErrorCopy(
        title = "消息格式不受支持",
        detail = "已按纯文本显示可读部分，不会执行其中的扩展内容。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "NODE_UNAVAILABLE" to ErrorCopy(
        title = "节点暂时不可用",
        detail = "当前应用节点没有响应；网络恢复后会自动重试，持续失败请检查节点状态。",
        action = ErrorAction.WAIT,
        actionLabel = "稍后自动重试",
    ),
    "RATE_LIMITED" to ErrorCopy(
        title = "操作过于频繁",
        detail = "节点正在限流，请稍等；客户端会按建议退避后自动重试。",
        action = ErrorAction.WAIT,
        actionLabel = "稍后自动重试",
    ),
    "QUOTA_EXCEEDED" to ErrorCopy(
        title = "超出容量上限",
        detail = "已超出节点或群允许的容量上限，请清理后再试。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "DEVICE_REVOKED" to ErrorCopy(
        title = "这台设备已被撤销",
        detail = "账户主管已撤销此设备，无法继续收发；请使用未撤销的设备重新配对。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "CONTACT_NOT_ACCEPTED" to ErrorCopy(
        title = "对方尚未接受",
        detail = "双方确认成为好友之后才能发送消息。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "MISSING_LOG_ENTRIES" to ErrorCopy(
        title = "缺少群日志",
        detail = "本地缺少部分群日志，正在补齐；补齐后会自动继续。",
        action = ErrorAction.WAIT,
        actionLabel = "等待补齐",
    ),
    "INVALID_CREDENTIAL" to ErrorCopy(
        title = "设备凭据无效",
        detail = "凭据校验未通过，请核对设备身份后重新加入。",
        action = ErrorAction.VERIFY_IDENTITY,
        actionLabel = "核对身份",
    ),
    "INVALID_CIPHERTEXT" to ErrorCopy(
        title = "消息校验失败",
        detail = "密文校验未通过，已拒绝显示这条内容；不会重试，也不会覆盖本地状态。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "PROTOCOL_UNSUPPORTED" to ErrorCopy(
        title = "协议版本不兼容",
        detail = "对端或节点使用的协议版本不受支持，请升级客户端后再试。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "INVALID_ID" to ErrorCopy(
        title = "标识格式无效",
        detail = "提交或收到的标识不符合规范，本次操作已取消。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "MALFORMED_FRAME" to ErrorCopy(
        title = "数据帧无效",
        detail = "收到的数据帧格式不正确，已丢弃，不会继续处理其中的内容。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "INVALID_CANDIDATE" to ErrorCopy(
        title = "群更新候选无效",
        detail = "群更新候选校验失败，已丢弃；群状态没有改变。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "INVALID_SIGNATURE" to ErrorCopy(
        title = "签名校验失败",
        detail = "签名与预期不符，已拒绝该操作；不会因为重试而放行。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "INVALID_SEQUENCE" to ErrorCopy(
        title = "序号不连续",
        detail = "日志序号不连续，已停止推进，等待补齐正确条目。",
        action = ErrorAction.RESYNC,
        actionLabel = "补齐日志",
    ),
    "INVALID_EXTENSION" to ErrorCopy(
        title = "扩展内容无效",
        detail = "消息扩展不符合约定，已按纯文本安全回退显示。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "CURSOR_EXPIRED" to ErrorCopy(
        title = "翻页定位已过期",
        detail = "本次分页定位过期，请重新加载列表后再继续。",
        action = ErrorAction.RESYNC,
        actionLabel = "重新加载",
    ),
    "INVALID_CURSOR_SCOPE" to ErrorCopy(
        title = "翻页定位不匹配",
        detail = "分页定位与当前会话或列表不一致，已重新加载。",
        action = ErrorAction.RESYNC,
        actionLabel = "重新加载",
    ),
    "INVALID_TARGET" to ErrorCopy(
        title = "操作目标无效",
        detail = "目标会话、消息或成员不存在或已失效。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "GROUP_LIMIT_EXCEEDED" to ErrorCopy(
        title = "超出群容量",
        detail = "群成员或设备数量已超过上限，请减少后重试。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "DEVICE_LIMIT_EXCEEDED" to ErrorCopy(
        title = "超出设备上限",
        detail = "每个账户最多 2 台设备；请先在旧设备上移除，或使用恢复流程。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "OWNER_TRANSFER_REQUIRED" to ErrorCopy(
        title = "需要先转让群主",
        detail = "群主不能直接退出；请先转让群主，或解散群。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "NOT_MEMBER" to ErrorCopy(
        title = "已不在该群",
        detail = "当前账户不在这个群里，无法执行该操作。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "INVALID_STATE_TRANSITION" to ErrorCopy(
        title = "当前状态不允许该操作",
        detail = "这一步与当前状态不符，请刷新后重试。",
        action = ErrorAction.RESYNC,
        actionLabel = "刷新状态",
    ),
    "INVALID_RECEIPT_SCOPE" to ErrorCopy(
        title = "回执范围不符",
        detail = "回执与本次提交不匹配，已拒绝，不会据此推进状态。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "INVALID_REPLY" to ErrorCopy(
        title = "回复目标无效",
        detail = "回复引用的消息不存在或对当前设备不可见。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "MESSAGE_NOT_VISIBLE" to ErrorCopy(
        title = "消息不可见",
        detail = "这条消息对当前设备不可见（可能是加入该群之前的历史）。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "HISTORY_NOT_AUTHORIZED" to ErrorCopy(
        title = "没有这段历史的权限",
        detail = "当前设备没有解密这段历史的权限，内容不会显示。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "INVALID_EVENT_BINDING" to ErrorCopy(
        title = "事件绑定校验失败",
        detail = "事件与提交的绑定关系不一致，已拒绝。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "REQUIRED_CAPABILITY_MISSING" to ErrorCopy(
        title = "缺少必需的能力",
        detail = "对端不支持本次操作需要的能力，操作已取消，不会降级安全条件。",
        action = ErrorAction.NONE,
        actionLabel = null,
    ),
    "UNKNOWN_RESULT" to ErrorCopy(
        title = "结果未确定",
        detail = "节点没有返回确定结果，已转为对账；不会自动重发，也不会重新加密。",
        action = ErrorAction.WAIT,
        actionLabel = "等待对账",
    ),
    "INTERNAL_FAILURE" to ErrorCopy(
        title = "内部处理失败",
        detail = "内部处理出错，已记录诊断码；可以稍后重试，或重启客户端。",
        action = ErrorAction.RETRY,
        actionLabel = "重试",
    ),
)

fun presentError(error: CoreFailure): UserActionableError {
    val copy = knownErrors[error.code]
    // 自动重试只认冻结规则；未知码与验证/损坏/权限类一律不自动重试。
    val autoRetry = error.code in FROZEN_AUTO_RETRY_CODES
    if (copy == null) {
        val action = if (error.retryable) ErrorAction.RETRY else ErrorAction.NONE
        return UserActionableError(
            title = "操作未完成",
            detail = "遇到暂未识别的错误。可以稍后重试，或返回当前页面后重新操作。",
            actionLabel = if (action == ErrorAction.RETRY) "重试" else null,
            action = action,
            retryable = error.retryable,
            diagnosticCode = safeDiagnosticToken(error.code),
            diagnosticId = safeDiagnosticToken(error.operationId),
            autoRetryAllowed = autoRetry,
        )
    }
    val action = if (copy.action == ErrorAction.RETRY && !error.retryable) ErrorAction.NONE else copy.action
    return UserActionableError(
        title = copy.title,
        detail = copy.detail,
        actionLabel = if (action == ErrorAction.NONE) null else copy.actionLabel,
        action = action,
        retryable = error.retryable,
        diagnosticCode = safeDiagnosticToken(error.code),
        diagnosticId = safeDiagnosticToken(error.operationId),
        autoRetryAllowed = autoRetry,
    )
}

/** 只接受短的大写/小写字母、数字、下划线和连字符，避免把未知字符串当展示内容。 */
private fun safeDiagnosticToken(raw: String?): String? {
    val value = raw?.trim().orEmpty()
    if (value.isEmpty() || value.length > 64) return null
    return value.takeIf { token ->
        token.all { character ->
            character in 'A'..'Z' ||
                character in 'a'..'z' ||
                character in '0'..'9' ||
                character == '_' ||
                character == '-'
        }
    }
}

/** 传输/信任类底层标记：出现这些说明是**本机信任配置**的问题，需要明确的排障指引。 */
private val TRANSPORT_TRUST_MARKERS = listOf("PIN_MISMATCH", "TLS_TRUST_REQUIRED", "INSECURE_TRANSPORT", "TLS_CERT_INVALID")

/**
 * 由**原始码 + 原始原因**生成可展示错误（UI 直接用，不必经过 CoreFailure）。
 *
 * 语义：
 * - 文案仍按 PROTOCOL-014 冻结表给（未知码走回退文案）；
 * - **原因链单独保留**（截断 300 字符、压平换行），不当成标题；
 * - 原因里出现传输/信任标记时，覆盖成可执行的排障指引（"去关于面板核对证书指纹"），
 *   因为这类失败的通用文案（"操作未完成"）帮不到用户。
 */
fun presentRawError(
    code: String,
    reason: String?,
    // 默认按 PROTOCOL-014 的**冻结可重试码表**判定——这不是猜，而是同一张表；
    // 调用方若拿到内核的明确 retryable（CoreFailure 路径），可以覆盖它。
    retryable: Boolean = code.trim().ifEmpty { "UNKNOWN_RESULT" } in FROZEN_AUTO_RETRY_CODES,
): UserActionableError {
    val trimmedCode = code.trim().ifEmpty { "UNKNOWN_RESULT" }
    val safeReason = reason
        ?.replace('\n', ' ')
        ?.trim()
        ?.take(300)
        ?.ifEmpty { null }
    val marker = TRANSPORT_TRUST_MARKERS.firstOrNull { marker -> safeReason?.contains(marker) == true }
    val base = knownErrors[trimmedCode]
    val transportTitle = marker?.let {
        when (it) {
            "PIN_MISMATCH" -> "节点证书与固定指纹不匹配"
            "TLS_TRUST_REQUIRED" -> "节点要求加密连接（缺信任配置）"
            "INSECURE_TRANSPORT" -> "配置了指纹但端点是明文"
            else -> "传输信任配置无效"
        }
    }
    if (transportTitle != null) {
        return UserActionableError(
            title = transportTitle,
            detail = "打开「关于」面板 → 传输安全（TLS）：填写节点证书的 SHA-256 指纹后重试；若刚换过证书，把新旧指纹一起填上。",
            actionLabel = "去核对指纹",
            action = ErrorAction.NONE,
            retryable = false,
            diagnosticCode = safeDiagnosticToken(trimmedCode),
            diagnosticId = null,
            autoRetryAllowed = false,
            reason = safeReason,
        )
    }
    if (base == null) {
        val action = if (retryable) ErrorAction.RETRY else ErrorAction.NONE
        return UserActionableError(
            title = "操作未完成",
            detail = "遇到暂未识别的错误。可以稍后重试，或返回当前页面后重新操作。",
            actionLabel = if (action == ErrorAction.RETRY) "重试" else null,
            action = action,
            retryable = retryable,
            diagnosticCode = safeDiagnosticToken(trimmedCode),
            diagnosticId = null,
            autoRetryAllowed = trimmedCode in FROZEN_AUTO_RETRY_CODES,
            reason = safeReason,
        )
    }
    val action = if (base.action == ErrorAction.RETRY && !retryable) ErrorAction.NONE else base.action
    return UserActionableError(
        title = base.title,
        detail = base.detail,
        actionLabel = if (action == ErrorAction.NONE) null else base.actionLabel,
        action = action,
        retryable = retryable,
        diagnosticCode = safeDiagnosticToken(trimmedCode),
        diagnosticId = null,
        autoRetryAllowed = trimmedCode in FROZEN_AUTO_RETRY_CODES,
        reason = safeReason,
    )
}

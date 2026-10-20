package chat.presentation.preview

/**
 * 失败投影（PRESENTATION 收敛）：把任意异常统一成 `(稳定码, 说明)`。
 *
 * 语义：
 * - 有 `code` 属性的异常（FFI 的 `LocalStoreException` 等）用它自己的稳定码；
 * - 其它异常一律 `UNKNOWN_RESULT`（**不**把异常类名当错误码，避免 UI 里出现 `NullPointerException` 这种码）；
 * - 说明为空时退回异常类名，保证界面上至少有东西可看。
 */
data class ProjectedFailure(val code: String, val reason: String)

/** 从异常投影：只认“带稳定码”的异常，其余归为 UNKNOWN_RESULT。 */
fun projectFailure(error: Throwable): ProjectedFailure {
    val code = error.stableCodeOrNull() ?: "UNKNOWN_RESULT"
    val reason = error.message?.takeIf { it.isNotBlank() } ?: (error::class.simpleName ?: "unknown failure")
    return ProjectedFailure(code = code, reason = reason)
}

/**
 * 取异常的稳定码：优先 `code` 属性（FFI 生成的异常都有），其次 `errorCode` 约定。
 *
 * 这里用**结构化访问**而不是给每个类型写分支：presentation 模块不依赖 FFI 类型。
 */
private fun Throwable.stableCodeOrNull(): String? {
    val value = runCatching {
        javaClass.methods.firstOrNull { it.name == "getCode" && it.parameterCount == 0 }?.invoke(this) as? String
    }.getOrNull()
    return value?.takeIf { it.isNotBlank() && it.length <= 64 && it.all { ch -> ch.isLetterOrDigit() || ch == '_' } }
}

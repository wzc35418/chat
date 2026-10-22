package chat.desktop

/**
 * 把异常压成**稳定错误码**——全应用只有这一处实现。
 *
 * 为什么走 `projectFailure` 而不是自己强转：带稳定码的异常不止 `LocalStoreException` 一种
 * （插件/平台/传输层各有自己的异常类型），统一交给展示层的投影逻辑，才不会漏掉码。
 * 兜底仍是 `UNKNOWN_RESULT`。
 */
internal fun failureCode(error: Throwable): String =
    chat.presentation.preview.projectFailure(error).code

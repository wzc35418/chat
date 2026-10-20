package chat.desktop

import chat.api.LocalStoreException

/**
 * 把异常压成**稳定错误码**（唯一实现，避免 30+ 处各写一遍 `(error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT"`）。
 *
 * 语义：FFI 抛出的 [LocalStoreException] 自带稳定码（冻结表见 PROTOCOL-014）；其他异常一律归为 `UNKNOWN_RESULT`，
 * 由展示层（`presentRawError`）决定标题/说明/是否可重试。
 */
internal fun failureCode(error: Throwable): String =
    (error as? LocalStoreException)?.code ?: "UNKNOWN_RESULT"

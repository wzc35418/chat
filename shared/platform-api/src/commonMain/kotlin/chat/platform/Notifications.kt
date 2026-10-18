package chat.platform

/** 通知投递结果：不支持时显式失败，不假装已提示。 */
class NotificationException(val code: String, message: String) : Exception(message)

object NotificationCodes {
    const val UNAVAILABLE = "KEY_PROTECTION_UNAVAILABLE"
    const val INVALID_INPUT = "INVALID_INPUT"
    const val DENIED = "PERMISSION_DENIED"
}

data class NotificationRequest(
    val title: String,
    val body: String,
    /** 去重/更新用；同 id 重复 show 视为更新（桌面实现尽力而为）。 */
    val id: String,
)

/**
 * PLATFORM-007/008：系统通知。
 *
 * 契约：不可用/无权限时必须显式失败（调用方可回退到应用内提示），
 * 绝不在没提示的情况下当作“已通知”。
 */
interface Notifications {
    fun isSupported(): Boolean

    suspend fun show(request: NotificationRequest)

    suspend fun cancel(id: String)
}

/** PLATFORM-009/010：生命周期与桌面退出。 */
interface PlatformLifecycle {
    /** 当前生命周期状态：foreground / background / shutting_down。 */
    fun state(): String

    fun onForeground()

    fun onBackground()

    /** 请求退出：实现方负责停止同步/关闭库并退出进程；返回 false 表示拒绝（例如仍在提交中）。 */
    suspend fun requestExit(): Boolean
}

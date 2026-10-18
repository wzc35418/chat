package chat.desktop

import chat.platform.NotificationCodes
import chat.platform.NotificationException
import chat.platform.NotificationRequest
import chat.platform.Notifications

/**
 * PLATFORM-007/008 的桌面实现：Swing/AWT 系统托盘气泡。
 *
 * 诚实边界：无系统托盘（headless、部分 Linux 桌面）时**显式不可用**，
 * 调用方应回退到应用内提示；不做“假装已弹出”。
 */
class TrayNotifications : Notifications {

    private val tray: java.awt.SystemTray? = runCatching {
        if (java.awt.SystemTray.isSupported()) java.awt.SystemTray.getSystemTray() else null
    }.getOrNull()

    override fun isSupported(): Boolean = tray != null

    override suspend fun show(request: NotificationRequest) {
        requireValid(request)
        val systemTray = tray ?: throw NotificationException(NotificationCodes.UNAVAILABLE, "当前环境没有系统托盘，无法弹出通知")
        val icon = currentTrayIcon(systemTray) ?: throw NotificationException(
            NotificationCodes.UNAVAILABLE,
            "系统托盘未初始化",
        )
        icon.displayMessage(request.title, request.body, java.awt.TrayIcon.MessageType.INFO)
    }

    override suspend fun cancel(id: String) {
        // AWT 托盘气泡没有按 id 撤销的公开 API；不伪造成功。
        throw NotificationException(NotificationCodes.UNAVAILABLE, "AWT 托盘不支持按 id 撤销通知")
    }

    /** 由宿主在启动时安装托盘图标（未安装时 show 会显式失败）。 */
    fun installIcon(image: java.awt.Image = java.awt.image.BufferedImage(16, 16, java.awt.image.BufferedImage.TYPE_INT_ARGB)) {
        val systemTray = tray ?: throw NotificationException(NotificationCodes.UNAVAILABLE, "当前环境没有系统托盘")
        val icon = java.awt.TrayIcon(image, "Chat")
        icon.isImageAutoSize = true
        runCatching { systemTray.add(icon) }
    }

    private fun currentTrayIcon(systemTray: java.awt.SystemTray): java.awt.TrayIcon? =
        systemTray.trayIcons.firstOrNull()

    private fun requireValid(request: NotificationRequest) {
        if (request.title.isBlank() || request.title.length > 120) {
            throw NotificationException(NotificationCodes.INVALID_INPUT, "通知标题不合法")
        }
        if (request.body.length > 512) {
            throw NotificationException(NotificationCodes.INVALID_INPUT, "通知正文超过 512 字符")
        }
        if (request.id.isBlank() || request.id.length > 64) {
            throw NotificationException(NotificationCodes.INVALID_INPUT, "通知 id 不合法")
        }
    }
}

/** PLATFORM-009/010 的桌面实现：状态机 + 退出钩子。 */
class DesktopLifecycle(
    private val onExit: () -> Unit,
) : chat.platform.PlatformLifecycle {

    @Volatile
    private var current = "foreground"

    override fun state(): String = current

    override fun onForeground() {
        if (current == "shutting_down") return
        current = "foreground"
    }

    override fun onBackground() {
        if (current == "shutting_down") return
        current = "background"
    }

    override suspend fun requestExit(): Boolean {
        if (current == "shutting_down") return false
        current = "shutting_down"
        onExit()
        return true
    }
}

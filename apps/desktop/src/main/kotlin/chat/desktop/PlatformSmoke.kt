package chat.desktop

import chat.platform.NotificationCodes
import chat.platform.NotificationException
import chat.platform.NotificationRequest
import kotlinx.coroutines.runBlocking

/**
 * PLATFORM-007~010 桌面竖切冒烟：通知与生命周期。
 *
 * 诚实证据：无系统托盘时 show 显式不可用（不假装已提示）；参数校验拒绝非法请求；
 * 生命周期状态机按 foreground/background/shutting_down 迁移且退出只执行一次。
 */
fun main() = runBlocking {
    val notifications = TrayNotifications()
    println("      系统托盘支持：" + notifications.isSupported())

    // 1) 参数校验：非法标题/超长正文/id 非法 → INVALID_INPUT（先于可用性检查）。
    val invalid = runCatching {
        notifications.show(NotificationRequest(title = " ", body = "x", id = "n1"))
    }.exceptionOrNull()
    check(invalid is NotificationException) { "空标题必须显式失败" }
    check((invalid as NotificationException).code == NotificationCodes.INVALID_INPUT)

    val tooLong = runCatching {
        notifications.show(NotificationRequest(title = "t", body = "x".repeat(513), id = "n1"))
    }.exceptionOrNull()
    check(tooLong is NotificationException)
    check((tooLong as NotificationException).code == NotificationCodes.INVALID_INPUT)

    // 2) 可用性：无托盘 → 显式 UNAVAILABLE，绝不静默成功。
    val showResult = runCatching {
        notifications.show(NotificationRequest(title = "测试通知", body = "正文", id = "n1"))
    }
    if (!notifications.isSupported()) {
        val error = showResult.exceptionOrNull()
        check(error is NotificationException) { "无托盘时必须显式失败" }
        check((error as NotificationException).code == NotificationCodes.UNAVAILABLE)
        println("      无系统托盘：show 诚实失败 " + error.code)
    } else {
        println("      有系统托盘：show 已尝试投递（真实弹窗，不可自动断言）")
    }

    // 3) 生命周期状态机 + 退出只执行一次。
    var exits = 0
    val lifecycle = DesktopLifecycle { exits++ }
    check(lifecycle.state() == "foreground")
    lifecycle.onBackground()
    check(lifecycle.state() == "background")
    lifecycle.onForeground()
    check(lifecycle.state() == "foreground")
    check(lifecycle.requestExit()) { "首次退出应被接受" }
    check(lifecycle.state() == "shutting_down")
    check(!lifecycle.requestExit()) { "退出中再次请求应被拒绝" }
    lifecycle.onForeground()
    check(lifecycle.state() == "shutting_down") { "退出中不得回到前台" }
    check(exits == 1) { "退出钩子只能执行一次，实际 $exits" }

    println("PASS platform slice: notifications honest-unavailable, lifecycle transitions, single exit")
}

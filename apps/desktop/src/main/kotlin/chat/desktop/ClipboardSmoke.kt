package chat.desktop

import java.awt.Toolkit
import java.awt.datatransfer.DataFlavor
import java.awt.datatransfer.StringSelection

/** 复制到系统剪贴板（失败返回 false，不抛给 UI）。 */
fun copyToClipboard(text: String): Boolean = runCatching {
    val clipboard = Toolkit.getDefaultToolkit().systemClipboard
    clipboard.setContents(StringSelection(text), null)
    true
}.getOrDefault(false)

/** 从剪贴板读取文本（失败返回 null）；主要用于自检。 */
fun readClipboard(): String? = runCatching {
    val clipboard = Toolkit.getDefaultToolkit().systemClipboard
    clipboard.getContents(null)?.takeIf { it.isDataFlavorSupported(DataFlavor.stringFlavor) }
        ?.getTransferData(DataFlavor.stringFlavor) as? String
}.getOrNull()

/**
 * 剪贴板自检：写一段**带中文和换行**的文本再读回，断言完全一致。
 * 为什么值得测：邀请口令/KeyPackage/邀请包都要靠“复制”交到对方手里，
 * 复制失败会让用户拿到半截文本，而这在界面上看不出来。
 */
fun main() {
    val sample = "chat-clipboard-test-中文-换行\n第二行-" + System.currentTimeMillis()
    val wrote = copyToClipboard(sample)
    val back = readClipboard()
    if (!wrote) {
        println("FAIL 写入剪贴板失败（可能没有桌面会话）")
        kotlin.system.exitProcess(3)
    }
    if (back != sample) {
        println("FAIL 读回内容不一致：wrote=" + sample.length + " 字符，read=" + (back?.length ?: -1))
        kotlin.system.exitProcess(1)
    }
    println("PASS clipboard round-trip（" + sample.length + " 字符含中文/换行）")
    println("CLIPBOARD SMOKE: PASS")
}

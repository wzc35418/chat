package chat.desktop

import chat.platform.KeyProtectionCodes
import java.io.File

/**
 * PLATFORM-004/005/006 的桌面实现：文件选择与受控令牌。
 *
 * 契约：选择只发生在用户显式动作；返回令牌只描述“用户选中的普通文件”；
 * 绝对路径不进入日志/错误文本；拿到令牌后仍由 Rust 侧做大小与类型校验。
 * 无图形界面（headless）时显式返回不可用，不静默失败。
 */
data class ChosenFile(
    val displayName: String,
    val sizeBytes: Long,
    /** 不透明令牌：桌面实现里是路径摘要，真正的路径单独返回给导入调用。 */
    val token: String,
    val path: String,
)

class PlatformFileException(val code: String, message: String) : Exception(message)

class DesktopFilePicker(private val parent: java.awt.Component? = null) {

    fun chooseImportFile(title: String = "选择要加密导入的文件"): ChosenFile {
        val file = choose(title, save = false) ?: throw PlatformFileException(KeyProtectionCodes.USER_AUTH_CANCELLED, "用户取消选择")
        return describe(file)
    }

    fun chooseExportTarget(defaultName: String = "export.bin"): ChosenFile {
        val file = choose("选择导出位置", save = true, defaultName = defaultName)
            ?: throw PlatformFileException(KeyProtectionCodes.USER_AUTH_CANCELLED, "用户取消选择")
        return describe(file, requireExisting = false)
    }

    fun describe(file: File, requireExisting: Boolean = true): ChosenFile {
        if (requireExisting && !file.isFile) {
            throw PlatformFileException(KeyProtectionCodes.INVALID_INPUT, "选中的不是普通文件")
        }
        val digest = java.security.MessageDigest.getInstance("SHA-256")
            .digest(file.absolutePath.toByteArray(Charsets.UTF_8))
        val token = "file:" + digest.joinToString("") { "%02x".format(it) }.take(32)
        return ChosenFile(
            displayName = file.name.take(120),
            sizeBytes = if (requireExisting) file.length() else 0L,
            token = token,
            path = file.absolutePath,
        )
    }

    private fun choose(title: String, save: Boolean, defaultName: String? = null): File? {
        if (java.awt.GraphicsEnvironment.isHeadless()) {
            throw PlatformFileException(KeyProtectionCodes.UNAVAILABLE, "当前环境没有图形界面，无法打开文件选择器")
        }
        val chooser = javax.swing.JFileChooser().apply {
            dialogTitle = title
            isAcceptAllFileFilterUsed = true
            fileSelectionMode = javax.swing.JFileChooser.FILES_ONLY
            if (defaultName != null) selectedFile = File(defaultName)
        }
        val result = if (save) chooser.showSaveDialog(parent) else chooser.showOpenDialog(parent)
        return if (result == javax.swing.JFileChooser.APPROVE_OPTION) chooser.selectedFile else null
    }
}

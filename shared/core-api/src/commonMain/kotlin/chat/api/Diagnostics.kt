package chat.api

/** 应用/构建诊断信息（纯只读，不含任何密钥材料）。 */
data class AppInfo(
    val version: String,
    val buildProfile: String,
    val ffiVersion: String,
    val platform: String,
)

/** 诊断服务：版本信息与本机库解析结果。 */
interface DiagnosticsService {
    fun appInfo(): AppInfo

    /** 解析 host 动态库的真实路径；失败时抛出带全部尝试路径的错误。 */
    fun hostLibraryPath(fileName: String): String
}

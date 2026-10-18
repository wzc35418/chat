package chat.platform

// token 只能由平台适配签发；不能当作任意路径直接打开。
data class PlatformFileToken internal constructor(val opaqueId: String)
interface PlatformFiles {
    suspend fun chooseImportFile(): PlatformFileToken?
    suspend fun chooseExportFile(suggestedName: String): PlatformFileToken?
}

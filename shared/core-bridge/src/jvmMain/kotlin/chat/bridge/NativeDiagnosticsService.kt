package chat.bridge

import chat.api.AppInfo
import chat.api.DiagnosticsService

/** 诊断服务实现：直接读 Rust FFI 的编译期常量与路径解析结果。 */
class NativeDiagnosticsService : DiagnosticsService {
    override fun appInfo(): AppInfo {
        val info = uniffi.chat_ffi.appInfo()
        return AppInfo(
            version = info.version,
            buildProfile = info.buildProfile,
            ffiVersion = info.ffiVersion,
            platform = info.platform,
        )
    }

    override fun hostLibraryPath(fileName: String): String = uniffi.chat_ffi.nativeLibraryPath(fileName)
}

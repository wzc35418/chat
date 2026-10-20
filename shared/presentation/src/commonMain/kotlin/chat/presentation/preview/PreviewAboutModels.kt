package chat.presentation.preview

import chat.api.AppInfo

/** 「关于 / 诊断」面板状态。 */
data class PreviewAboutState(
    val appInfo: AppInfo? = null,
    val hostLibraryPath: String = "",
    val dataDir: String = "",
    val notice: String? = null,
    val lastErrorCode: String? = null,
)

fun PreviewAboutState.withAppInfo(info: AppInfo): PreviewAboutState = copy(appInfo = info, lastErrorCode = null)

fun PreviewAboutState.withHostLibraryPath(path: String): PreviewAboutState =
    copy(hostLibraryPath = path, lastErrorCode = null)

fun PreviewAboutState.withDataDir(path: String): PreviewAboutState = copy(dataDir = path)

fun PreviewAboutState.withAboutError(code: String, notice: String?): PreviewAboutState =
    copy(lastErrorCode = code, notice = notice)

fun PreviewAboutState.withScaleNotice(factor: Float): PreviewAboutState =
    copy(notice = "界面缩放已设为 " + factor + "（已保存到本机）", lastErrorCode = null)

package chat.api

/** 插件清单视图（来自 manifest_validate / list_plugins）。 */
data class PluginManifestView(
    val id: String,
    val version: String,
    val capabilities: List<String>,
    val extensionPoints: List<String>,
    val entryClass: String,
)

/** 插件运行状态。 */
data class PluginStateView(
    val id: String,
    val version: String,
    /** installed / started / stopped */
    val state: String,
    val extensionPoints: List<String>,
)

/** 派发计划：宿主据此执行插件代码，本层不执行。 */
data class DispatchPlanView(
    val pluginId: String,
    val extensionPoint: String,
    val operationId: String,
    val inputBytes: Int,
    val outputLimitBytes: Int,
    val timeoutMillis: Long,
)

/**
 * 插件宿主服务（PLUGINS 模块的 Kotlin 侧契约）。
 *
 * 诚实语义：dispatch 只返回**执行计划**；本层不加载 JVM 插件包、不执行插件代码。
 */
interface PluginService {
    /** P4：不执行代码地校验插件包结构（ZIP 目录/清单/入口类/SPI）。 */
    suspend fun inspectPackage(bytes: ByteArray): PluginPackageView

    suspend fun validateManifest(json: String): PluginManifestView

    suspend fun install(json: String)

    suspend fun start(pluginId: String)

    suspend fun stop(pluginId: String)

    suspend fun uninstall(pluginId: String, deleteData: Boolean)

    suspend fun listPlugins(): List<PluginStateView>

    suspend fun dispatch(pluginId: String, extensionPoint: String, input: String): DispatchPlanView

    suspend fun finishDispatch(operationId: String, output: String)

    suspend fun storageGet(pluginId: String, key: String): String?

    suspend fun storageSet(pluginId: String, key: String, value: String)

    suspend fun shutdown()
}
/** 插件包结构校验结果（来自 Rust 侧纯字节解析）。 */
data class PluginPackageView(
    val pluginId: String,
    val pluginVersion: String,
    val entryClass: String,
    val entryFile: String,
    val spiFile: String,
    val entryCount: Int,
    val compressedBytes: Long,
    val uncompressedBytes: Long,
)

package chat.bridge

import chat.api.DispatchPlanView
import chat.api.LocalStoreException
import chat.api.PluginManifestView
import chat.api.PluginService
import chat.api.PluginStateView
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.chat_ffi.DispatchPlanViewFfi
import uniffi.chat_ffi.FfiException
import uniffi.chat_ffi.ManifestViewFfi
import uniffi.chat_ffi.PluginInfoFfi
import uniffi.chat_ffi.PluginsFfi

/**
 * UniFFI 插件宿主桥：串行化 + 切 IO + 稳定错误码透传。
 *
 * 诚实语义：dispatch 只返回执行计划，本层不加载/不执行插件代码。
 */
class NativePluginService(private val native: PluginsFfi) : PluginService {
    private val mutex = Mutex()

    private suspend fun <T> call(block: (PluginsFfi) -> T): T = mutex.withLock {
        withContext(Dispatchers.IO) {
            try {
                block(native)
            } catch (error: FfiException.Store) {
                throw LocalStoreException(error.code, error.reason)
            }
        }
    }

    private fun nowMillis(): ULong = (System.currentTimeMillis() / 1000L).toULong()

    override suspend fun inspectPackage(bytes: ByteArray): chat.api.PluginPackageView = call {
        val view = it.inspectPackage(bytes)
        chat.api.PluginPackageView(
            pluginId = view.pluginId,
            pluginVersion = view.pluginVersion,
            entryClass = view.entryClass,
            entryFile = view.entryFile,
            spiFile = view.spiFile,
            entryCount = view.entryCount.toInt(),
            compressedBytes = view.compressedBytes.toLong(),
            uncompressedBytes = view.uncompressedBytes.toLong(),
        )
    }

    override suspend fun validateManifest(json: String): PluginManifestView =
        call { it.manifestValidate(json) }.toView()

    override suspend fun install(json: String): Unit = call { it.install(json) }

    override suspend fun start(pluginId: String): Unit = call { it.start(pluginId) }

    override suspend fun stop(pluginId: String): Unit = call { it.stop(pluginId) }

    override suspend fun uninstall(pluginId: String, deleteData: Boolean): Unit =
        call { it.uninstall(pluginId, deleteData) }

    override suspend fun listPlugins(): List<PluginStateView> = call { it.listPlugins() }.map { info: PluginInfoFfi ->
        PluginStateView(
            id = info.pluginId,
            version = "",
            state = if (info.running) "started" else "installed",
            extensionPoints = info.extensionPoints,
        )
    }

    override suspend fun dispatch(pluginId: String, extensionPoint: String, input: String): DispatchPlanView {
        // PROTOCOL-001 operation id 文本：op:1:<64 hex>
        val random = java.security.SecureRandom()
        val bytes = ByteArray(32).also { random.nextBytes(it) }
        val hex = bytes.joinToString("") { byte -> "%02x".format(byte) }
        val operationId = "op:1:" + hex
        val plan: DispatchPlanViewFfi = call {
            val now = nowMillis()
            it.dispatch(
                pluginId,
                extensionPoint,
                operationId,
                input.toByteArray(Charsets.UTF_8),
                now,
                now + 5_000UL,
            )
        }
        operationOwners[plan.operationId] = plan.pluginId
        return DispatchPlanView(
            pluginId = plan.pluginId,
            extensionPoint = plan.extensionPoint,
            operationId = plan.operationId,
            inputBytes = plan.inputBytes.toInt(),
            outputLimitBytes = plan.outputLimitBytes.toInt(),
            timeoutMillis = plan.timeoutMs.toLong(),
        )
    }

    private val operationOwners = HashMap<String, String>()

    override suspend fun finishDispatch(operationId: String, output: String): Unit = call {
        // finish 需要 pluginId：operation id 本身不含插件信息，桥内记录对应关系。
        val pluginId = operationOwners[operationId]
            ?: throw LocalStoreException("PLUGIN_OPERATION_UNKNOWN", "unknown dispatch operation")
        it.finishDispatch(pluginId, operationId, output.toByteArray(Charsets.UTF_8), nowMillis())
    }

    override suspend fun storageGet(pluginId: String, key: String): String? =
        call { it.storageGet(pluginId, key) }?.toString(Charsets.UTF_8)

    override suspend fun storageSet(pluginId: String, key: String, value: String): Unit =
        call { it.storageSet(pluginId, key, value.toByteArray(Charsets.UTF_8)) }

    override suspend fun shutdown(): Unit = mutex.withLock {
        withContext(Dispatchers.IO) { runCatching { native.shutdown() } }
        Unit
    }
}

private fun ManifestViewFfi.toView() = PluginManifestView(
    id = pluginId,
    version = version,
    capabilities = capabilities,
    extensionPoints = emptyList(),
    entryClass = entryClass,
)
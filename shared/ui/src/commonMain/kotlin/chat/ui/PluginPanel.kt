package chat.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import chat.api.DispatchPlanView
import chat.api.PluginStateView
import chat.presentation.preview.PreviewPluginState

/** 插件面板交互回调。 */
data class PluginHandlers(
    val updateManifest: (String) -> Unit = {},
    val chooseManifestFile: () -> Unit = {},
    val loadList: () -> Unit = {},
    val install: () -> Unit = {},
    val start: (String) -> Unit = {},
    val stop: (String) -> Unit = {},
    val uninstall: (String) -> Unit = {},
    val dispatch: (String) -> Unit = {},
    val storageGet: (String) -> Unit = {},
    val chooseJar: () -> Unit = {},
    val inspectPackage: () -> Unit = {},
    val toggleTrusted: (Boolean) -> Unit = {},
    val loadJar: () -> Unit = {},
    val runAction: () -> Unit = {},
    /** 记住当前插件的签名指纹（下次自动按它校验）。 */
    val rememberSignerFingerprint: () -> Unit = {},
    val unloadJar: () -> Unit = {},
) {
    companion object {
        val None = PluginHandlers()
    }
}

/**
 * 插件宿主面板：清单校验/安装、运行控制、派发计划、私有存储只读。
 *
 * 诚实语义：这里**不加载也不执行插件代码**；dispatch 只拿到执行计划（operation/预算/上限）。
 */
@Composable
fun PluginPanel(
    state: PreviewPluginState,
    handlers: PluginHandlers = PluginHandlers.None,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier.widthIn(min = 320.dp, max = 720.dp).fillMaxWidth(),
        shape = RoundedCornerShape(16.dp),
        color = Color(0xFFF7F9F7),
    ) {
        Column(
            modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text("插件宿主", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            Text(
                "本面板只做清单校验/安装/启停/派发计划；不加载 JVM 插件包，也不执行插件代码。",
                style = MaterialTheme.typography.bodySmall,
                color = Color(0xFF5F6B63),
            )
            if (state.notice.isNotBlank()) {
                Text(
                    state.notice,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.semantics { contentDescription = "插件提示" },
                )
            }
            ErrorBanner(state.lastErrorCode, state.notice)

            HorizontalDivider(color = Color(0xFFE1E6E1))
            Text("安装", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
            OutlinedTextField(
                value = state.manifestJson,
                onValueChange = handlers.updateManifest,
                label = { Text("清单 JSON") },
                modifier = Modifier.fillMaxWidth().heightIn(min = 96.dp, max = 160.dp),
            )
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                TextButton(
                onClick = handlers.rememberSignerFingerprint,
                enabled = state.loadedPlugins.isNotEmpty(),
                modifier = Modifier.semantics { contentDescription = "记住当前插件的签名指纹" },
            ) { Text("记住签名指纹", style = MaterialTheme.typography.labelMedium) }
            TextButton(
                    onClick = handlers.chooseManifestFile,
                    modifier = Modifier.semantics { contentDescription = "选择清单文件" },
                ) { Text("选择文件", style = MaterialTheme.typography.labelMedium) }
                TextButton(
                    onClick = handlers.install,
                    enabled = !state.busy,
                    modifier = Modifier.semantics { contentDescription = "安装插件" },
                ) { Text("校验并安装", style = MaterialTheme.typography.labelMedium) }
                TextButton(
                    onClick = handlers.loadList,
                    enabled = !state.busy,
                    modifier = Modifier.semantics { contentDescription = "刷新插件列表" },
                ) { Text("刷新列表", style = MaterialTheme.typography.labelMedium) }
            }

            HorizontalDivider(color = Color(0xFFE1E6E1))
            Text("已安装（" + state.plugins.size + "）", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
            if (state.plugins.isEmpty()) {
                Text("还没有插件；安装后出现在这里。", style = MaterialTheme.typography.bodySmall)
            } else {
                state.plugins.forEach { plugin -> PluginRow(plugin, state, handlers) }
            }

            HorizontalDivider(color = Color(0xFFE1E6E1))
            Text("P4：真正加载插件包（无沙箱）", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
            Text(
                "顺序：选 JAR → Rust 结构校验 → 你确认信任 → 同 JVM 加载执行。超时只能放弃等待，无法强杀线程。",
                style = MaterialTheme.typography.labelSmall,
                color = Color(0xFF5F6B63),
            )
            Text(
                if (state.jarPath.isBlank()) "尚未选择插件包" else "已选：" + state.jarPath,
                style = MaterialTheme.typography.bodySmall,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                TextButton(
                    onClick = handlers.chooseJar,
                    modifier = Modifier.semantics { contentDescription = "选择插件 JAR" },
                ) { Text("选择 JAR", style = MaterialTheme.typography.labelMedium) }
                TextButton(
                    onClick = handlers.inspectPackage,
                    enabled = !state.busy && state.jarPath.isNotBlank(),
                    modifier = Modifier.semantics { contentDescription = "校验插件包结构" },
                ) { Text("校验结构", style = MaterialTheme.typography.labelMedium) }
            }
            state.packageView?.let { view ->
                Text(
                    "包：" + view.pluginId + " " + view.pluginVersion + " · " + view.entryCount + " 条目 · " +
                        view.uncompressedBytes + " 字节（解压）",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                TextButton(
                    onClick = { handlers.toggleTrusted(!state.trusted) },
                    modifier = Modifier.semantics { contentDescription = "确认或取消信任该插件" },
                ) { Text(if (state.trusted) "已确认信任 ✓" else "确认信任（无沙箱）", style = MaterialTheme.typography.labelMedium) }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                TextButton(
                    onClick = handlers.loadJar,
                    enabled = !state.busy && state.packageView != null && state.trusted,
                    modifier = Modifier.semantics { contentDescription = "加载插件到 JVM" },
                ) { Text("加载", style = MaterialTheme.typography.labelMedium) }
                TextButton(
                    onClick = handlers.runAction,
                    enabled = !state.busy && state.loadedPlugins.isNotEmpty(),
                    modifier = Modifier.semantics { contentDescription = "执行插件动作" },
                ) { Text("执行动作", style = MaterialTheme.typography.labelMedium) }
                TextButton(
                    onClick = handlers.unloadJar,
                    enabled = !state.busy && state.loadedPlugins.isNotEmpty(),
                    modifier = Modifier.semantics { contentDescription = "卸载已加载插件" },
                ) { Text("卸载", style = MaterialTheme.typography.labelMedium) }
            }
            if (state.loadedPlugins.isNotEmpty()) {
                Text("已加载：" + state.loadedPlugins.joinToString("、"), style = MaterialTheme.typography.bodySmall)
            }

            state.lastPlan?.let { plan ->
                HorizontalDivider(color = Color(0xFFE1E6E1))
                Text("最近一次派发计划", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                Text(
                    describePlan(plan),
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.semantics { contentDescription = "派发计划详情" },
                )
            }
            state.lastStorageValue?.let {
                Text(
                    "私有存储：" + it,
                    style = MaterialTheme.typography.labelSmall,
                    color = Color(0xFF5F6B63),
                )
            }
        }
    }
}

@Composable
private fun PluginRow(plugin: PluginStateView, state: PreviewPluginState, handlers: PluginHandlers) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(plugin.id + " · " + plugin.state, style = MaterialTheme.typography.bodySmall, fontWeight = FontWeight.Medium)
            Text(
                if (plugin.extensionPoints.isEmpty()) {
                    "扩展点：未声明"
                } else {
                    "扩展点：" + plugin.extensionPoints.joinToString("、")
                },
                style = MaterialTheme.typography.labelSmall,
                color = Color(0xFF5F6B63),
            )
        }
        TextButton(
            onClick = { handlers.start(plugin.id) },
            enabled = !state.busy,
            modifier = Modifier.semantics { contentDescription = "启动插件 " + plugin.id },
        ) { Text("启动", style = MaterialTheme.typography.labelSmall) }
        TextButton(
            onClick = { handlers.stop(plugin.id) },
            enabled = !state.busy,
            modifier = Modifier.semantics { contentDescription = "停止插件 " + plugin.id },
        ) { Text("停止", style = MaterialTheme.typography.labelSmall) }
        TextButton(
            onClick = { handlers.dispatch(plugin.id) },
            enabled = !state.busy,
            modifier = Modifier.semantics { contentDescription = "生成派发计划 " + plugin.id },
        ) { Text("派发计划", style = MaterialTheme.typography.labelSmall) }
        TextButton(
            onClick = { handlers.uninstall(plugin.id) },
            enabled = !state.busy,
            modifier = Modifier.semantics { contentDescription = "卸载插件 " + plugin.id },
        ) { Text("卸载", style = MaterialTheme.typography.labelSmall) }
    }
}

private fun describePlan(plan: DispatchPlanView): String =
    "插件 " + plan.pluginId + " · 扩展点 " + plan.extensionPoint +
        " · 操作 " + plan.operationId +
        " · 输入 " + plan.inputBytes + " 字节" +
        " · 输出上限 " + plan.outputLimitBytes + " 字节" +
        " · 超时预算 " + plan.timeoutMillis + " ms"

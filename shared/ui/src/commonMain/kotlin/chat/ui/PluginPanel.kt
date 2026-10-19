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
            state.lastErrorCode?.let {
                Text(
                    "错误码 " + it,
                    style = MaterialTheme.typography.labelSmall,
                    color = Color(0xFFB3261E),
                    modifier = Modifier.semantics { contentDescription = "插件错误码" },
                )
            }

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

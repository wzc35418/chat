package chat.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import chat.api.P0Delivery
import chat.presentation.P0_MAX_MESSAGE_BYTES
import chat.presentation.P0Operation
import chat.presentation.P0Presenter
import chat.presentation.P0_SENDERS
import chat.presentation.P0UiState
import chat.presentation.display.UiCopy
import chat.presentation.display.isCompactWidth

private val Ink = Color(0xFF172B29)
private val Green = Color(0xFF246B58)
private val Muted = Color(0xFF596762)
private val SoftGreen = Color(0xFFE5EDE5)
private val Canvas = Color(0xFFF3F5F1)
private val ErrorBackground = Color(0xFFFFEDE8)
private val ErrorInk = Color(0xFF943C28)

@Composable
fun P0Screen(presenter: P0Presenter) {
    val state by presenter.state.collectAsState()
    var detailsVisible by remember { mutableStateOf(false) }

    MaterialTheme(
        colorScheme = lightColorScheme(
            primary = Green,
            surface = Color.White,
            background = Canvas,
        ),
    ) {
        BoxWithConstraints(
            Modifier
                .fillMaxSize()
                .background(MaterialTheme.colorScheme.background)
                .safeDrawingPadding()
                .imePadding(),
        ) {
            // 宽度阈值与预览共用 PRESENTATION-011 的纯布局决策，避免两处各写一份。
            val compact = isCompactWidth(maxWidth.value.toInt())
            val scrollable = compact || maxHeight < 620.dp
            val outerPadding = if (compact) 16.dp else 32.dp
            val gap = if (compact) 12.dp else 16.dp
            val scrollState = rememberScrollState()
            Column(
                Modifier
                    .fillMaxSize()
                    .then(if (scrollable) Modifier.verticalScroll(scrollState) else Modifier)
                    .padding(outerPadding),
                verticalArrangement = Arrangement.spacedBy(gap),
            ) {
                P0Header(state)
                Hero(compact)
                DeviceRow(state, compact)
                ConnectRow(presenter, state)
                if (state.requiresRebuild) {
                    RebuildNotice(busy = state.busy, onReconnect = presenter::connect)
                }
                state.error
                    ?.takeUnless { it.code == "P0_SEND_UNKNOWN" || it.code == "P0_SEND_CANCELLED" }
                    ?.let { error -> ErrorBanner(error, presenter::dismissError) }
                if (scrollable) {
                    // 窄屏或短窗口整页可滚动，固定消息区高度，键盘/缩放后仍能到达输入框。
                    MessagePanel(state, Modifier.fillMaxWidth().height(280.dp))
                } else {
                    MessagePanel(state, Modifier.weight(1f).fillMaxWidth())
                }
                ComposerSection(presenter, state, compact)
                if (detailsVisible) {
                    VerificationDetails(state)
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        UiCopy.P0_EPHEMERAL_NOTE,
                        style = MaterialTheme.typography.labelSmall,
                        color = Muted,
                        modifier = Modifier.weight(1f),
                    )
                    TextButton(
                        onClick = { detailsVisible = !detailsVisible },
                        modifier = Modifier.semantics {
                            contentDescription = if (detailsVisible) "收起验证详情" else "展开验证详情"
                            stateDescription = if (detailsVisible) "已展开" else "已收起"
                        },
                        contentPadding = PaddingValues(horizontal = 6.dp, vertical = 2.dp),
                    ) {
                        Text(
                            if (detailsVisible) UiCopy.P0_DETAILS_HIDE else UiCopy.P0_DETAILS,
                            style = MaterialTheme.typography.labelMedium,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun P0Header(state: P0UiState) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Surface(color = Green, shape = RoundedCornerShape(14.dp)) {
            Text(
                "C",
                Modifier.padding(horizontal = 14.dp, vertical = 8.dp),
                color = Color.White,
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.Bold,
            )
        }
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(
                UiCopy.P0_TITLE,
                color = Ink,
                style = MaterialTheme.typography.headlineSmall,
                fontWeight = FontWeight.Bold,
            )
            Text(
                UiCopy.P0_SUBTITLE,
                style = MaterialTheme.typography.bodySmall,
                color = Green,
            )
        }
        if (state.busy) {
            CircularProgressIndicator(Modifier.size(22.dp), strokeWidth = 2.dp)
            Spacer(Modifier.width(8.dp))
        }
        if (state.busy) {
            Text(
                when (state.operation) {
                    P0Operation.CONNECTING -> "连接中"
                    P0Operation.SENDING -> "发送中"
                    null -> "处理中"
                },
                style = MaterialTheme.typography.labelMedium,
                color = Green,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
private fun Hero(compact: Boolean) {
    Column(verticalArrangement = Arrangement.spacedBy(if (compact) 4.dp else 6.dp)) {
        Text(
            UiCopy.P0_HERO,
            style = if (compact) MaterialTheme.typography.headlineSmall else MaterialTheme.typography.headlineMedium,
            color = Ink,
            fontWeight = FontWeight.Bold,
        )
        Text(
            UiCopy.P0_DESCRIPTION,
            style = if (compact) MaterialTheme.typography.bodySmall else MaterialTheme.typography.bodyMedium,
            color = Muted,
        )
    }
}

@Composable
private fun DeviceRow(state: P0UiState, compact: Boolean) {
    Row(horizontalArrangement = Arrangement.spacedBy(if (compact) 8.dp else 12.dp)) {
        DeviceCard("Alice", state.session?.aliceEndpoint, Modifier.weight(1f))
        DeviceCard("Bob", state.session?.bobEndpoint, Modifier.weight(1f))
    }
}

@Composable
private fun DeviceCard(name: String, endpoint: String?, modifier: Modifier) {
    Surface(modifier, shape = RoundedCornerShape(14.dp), color = SoftGreen) {
        Column(Modifier.padding(horizontal = 12.dp, vertical = 10.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text(name, color = Ink, fontWeight = FontWeight.SemiBold)
            Text(
                if (endpoint == null) "等待连接" else "独立设备 · ${endpoint.take(12)}…",
                color = Green,
                style = MaterialTheme.typography.bodySmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
private fun ConnectRow(presenter: P0Presenter, state: P0UiState) {
    val connectLabel = when {
        state.operation == P0Operation.CONNECTING -> UiCopy.P0_CONNECTING
        state.session == null -> UiCopy.P0_CONNECT
        else -> UiCopy.P0_RECONNECT
    }
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
        Button(
            onClick = presenter::connect,
            enabled = !state.busy && !state.closed,
            modifier = Modifier.semantics { contentDescription = connectLabel },
            contentPadding = PaddingValues(horizontal = 16.dp, vertical = 10.dp),
        ) {
            Text(connectLabel)
        }
        Text(
            state.status,
            style = MaterialTheme.typography.bodySmall,
            color = Muted,
            modifier = Modifier.weight(1f),
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun ErrorBanner(
    error: chat.presentation.P0UiError,
    onDismiss: () -> Unit,
) {
    Surface(
        color = ErrorBackground,
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier.semantics {
            liveRegion = LiveRegionMode.Assertive
            contentDescription = "错误提示：${error.title}。${error.detail}。${error.action}"
        },
    ) {
        Row(Modifier.fillMaxWidth().padding(start = 12.dp, top = 10.dp, bottom = 10.dp), verticalAlignment = Alignment.Top) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text("错误提示 · ${error.title}", color = ErrorInk, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
                Text(error.detail, color = ErrorInk, style = MaterialTheme.typography.bodySmall)
                Text(error.action, color = ErrorInk, style = MaterialTheme.typography.labelSmall)
            }
            TextButton(
                onClick = onDismiss,
                modifier = Modifier.semantics { contentDescription = "关闭错误提示" },
                contentPadding = PaddingValues(horizontal = 8.dp),
            ) {
                Text("知道了", style = MaterialTheme.typography.labelMedium, color = ErrorInk)
            }
        }
    }
}

@Composable
private fun RebuildNotice(busy: Boolean, onReconnect: () -> Unit) {
    Surface(
        color = ErrorBackground,
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier.semantics {
            liveRegion = LiveRegionMode.Assertive
            contentDescription = "发送结果未知，需要重建会话。接收设备是否已处理上一条消息无法确认；不会自动重发或重新加密。"
        },
    ) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(
                    "发送结果未知 · 需要重建会话",
                    color = ErrorInk,
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.SemiBold,
                )
                Text(
                    "接收设备是否已处理上一条消息无法确认；不会自动重发或重新加密。草稿会保留。",
                    color = ErrorInk,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Button(
                onClick = onReconnect,
                enabled = !busy,
                modifier = Modifier.semantics { contentDescription = "重建会话以继续发送" },
            ) {
                Text(UiCopy.P0_RECONNECT)
            }
        }
    }
}

@Composable
private fun MessagePanel(state: P0UiState, modifier: Modifier) {
    val listDescription = if (state.messages.isEmpty()) "消息列表为空" else "消息列表，共 ${state.messages.size} 条"
    Surface(
        modifier.semantics { contentDescription = listDescription },
        shape = RoundedCornerShape(18.dp),
        color = Color.White,
    ) {
        if (state.messages.isEmpty()) {
            Box(Modifier.fillMaxSize().padding(20.dp), contentAlignment = Alignment.Center) {
                Column(
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    val (title, hint) = when {
                        state.requiresRebuild -> "需要重建会话" to "上一条发送结果未知；重建后可继续使用保留的草稿。"
                        state.session == null -> UiCopy.P0_EMPTY_TITLE to UiCopy.P0_EMPTY_WAITING
                        else -> UiCopy.P0_EMPTY_TITLE to UiCopy.P0_EMPTY_READY
                    }
                    Text(title, color = Ink, style = MaterialTheme.typography.titleLarge)
                    Text(hint, color = Muted, style = MaterialTheme.typography.bodySmall)
                }
            }
        } else {
            LazyColumn(
                Modifier.fillMaxSize().padding(horizontal = 12.dp, vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
                reverseLayout = true,
            ) {
                items(state.messages.asReversed(), key = { it.exchangeIndex }) { message ->
                    MessageBubble(message)
                }
            }
        }
    }
}

@Composable
private fun MessageBubble(message: P0Delivery) {
    Surface(color = Color(0xFFF0F5EF), shape = RoundedCornerShape(12.dp), modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(horizontal = 14.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
            Text(
                "${message.sender} → ${message.receiver}",
                color = Green,
                style = MaterialTheme.typography.labelMedium,
                fontWeight = FontWeight.SemiBold,
            )
            Text(message.text, color = Ink, style = MaterialTheme.typography.bodyLarge)
            Text(
                "${UiCopy.P0_VERIFIED} · 密文 ${message.ciphertextBytes} 字节 · ${message.elapsedMs} ms",
                color = Muted,
                style = MaterialTheme.typography.labelSmall,
            )
        }
    }
}

@Composable
private fun ComposerSection(presenter: P0Presenter, state: P0UiState, compact: Boolean) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            if (!compact) {
                Text(UiCopy.P0_SENDER_LABEL, style = MaterialTheme.typography.labelLarge, color = Muted)
            }
            P0_SENDERS.forEach { device ->
                FilterChip(
                    selected = state.sender == device,
                    onClick = { presenter.selectSender(device) },
                    enabled = !state.closed,
                    modifier = Modifier.semantics {
                        contentDescription = "发送设备 $device"
                        stateDescription = if (state.sender == device) "已选择" else "未选择"
                    },
                    label = { Text(device, style = MaterialTheme.typography.labelMedium) },
                )
            }
            Spacer(Modifier.weight(1f))
        }
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            OutlinedTextField(
                value = state.composer,
                onValueChange = presenter::updateComposer,
                modifier = Modifier.weight(1f).semantics {
                    contentDescription = "消息输入框，最多 $P0_MAX_MESSAGE_BYTES 个 UTF-8 字节"
                },
                label = { Text(UiCopy.P0_COMPOSER_LABEL) },
                minLines = 1,
                maxLines = 4,
                enabled = !state.closed,
                supportingText = {
                    Text(
                        "${state.composerBytes} / $P0_MAX_MESSAGE_BYTES UTF-8 字节",
                        style = MaterialTheme.typography.labelSmall,
                    )
                },
            )
            Button(
                onClick = { presenter.send() },
                enabled = state.canSend && !state.busy && !state.closed && state.composer.isNotBlank(),
                modifier = Modifier.semantics { contentDescription = "发送消息" },
                contentPadding = PaddingValues(horizontal = 18.dp, vertical = 12.dp),
            ) {
                Text(
                    if (state.operation == P0Operation.SENDING) UiCopy.P0_SENDING else UiCopy.P0_SEND,
                )
            }
        }
    }
}

@Composable
private fun VerificationDetails(state: P0UiState) {
    Surface(color = SoftGreen, shape = RoundedCornerShape(12.dp)) {
        Column(Modifier.fillMaxWidth().padding(12.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text("验证详情", color = Ink, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
            val session = state.session
            if (session == null) {
                Text("等待连接后显示本次 P0 传输与 MLS 参数。", color = Muted, style = MaterialTheme.typography.bodySmall)
            } else {
                Text(session.transport, color = Green, style = MaterialTheme.typography.bodySmall)
                Text("MLS epoch: ${session.epoch} · ${session.ciphersuite}", color = Muted, style = MaterialTheme.typography.labelSmall)
                Text(
                    "Alice: ${session.aliceEndpoint}\nBob: ${session.bobEndpoint}",
                    color = Muted,
                    style = MaterialTheme.typography.labelSmall,
                )
                Text(
                    "公共中继与发现：${if (session.publicInfrastructure) "已启用" else "关闭"}",
                    color = Muted,
                    style = MaterialTheme.typography.labelSmall,
                )
            }
        }
    }
}

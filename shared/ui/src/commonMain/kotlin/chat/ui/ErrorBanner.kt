package chat.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp

/**
 * 统一的错误/提示条（PRESENTATION 收敛）。
 *
 * 分层展示：标题（人话）→ 说明 → 原因链（等宽小字）→ 建议 → 可重试性 → 稳定码。
 * 文案与可重试性都来自 presentRawError（PROTOCOL-014 冻结表），不另写一套。
 */
@Composable
fun ErrorBanner(
    code: String?,
    message: String?,
    modifier: Modifier = Modifier,
) {
    val body = message?.takeIf { it.isNotBlank() } ?: return
    val presented = code?.let { chat.presentation.display.presentRawError(it, body) }
    val isError = code != null
    Surface(
        color = if (isError) Color(0xFFFDECEA) else Color(0xFFEDF1E9),
        shape = RoundedCornerShape(10.dp),
        modifier = modifier.fillMaxWidth().semantics {
            liveRegion = LiveRegionMode.Polite
            contentDescription = presented?.title ?: body
        },
    ) {
        Column(Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
            Text(presented?.title ?: body, style = MaterialTheme.typography.bodySmall)
            presented?.let { view ->
                if (view.detail.isNotBlank() && view.detail != view.title) {
                    Text(view.detail, style = MaterialTheme.typography.labelSmall)
                }
                view.reason?.takeIf { it != view.title && it.isNotBlank() }?.let { reason ->
                    Text(
                        "原因：" + reason,
                        style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                        color = Color(0xFF5F6B63),
                    )
                }
                view.actionLabel?.takeIf { it.isNotBlank() }?.let { label ->
                    Text("建议：" + label, style = MaterialTheme.typography.labelSmall, color = Color(0xFF246B58))
                }
                Text(
                    if (view.retryable) "可重试（允许自动退避）" else "此错误不会自动重试",
                    style = MaterialTheme.typography.labelSmall,
                    color = Color(0xFF5F6B63),
                )
            }
            if (isError) {
                Text(
                    "错误码：" + code,
                    style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                    color = Color(0xFFB3261E),
                )
            }
        }
    }
}

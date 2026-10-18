package chat.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
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
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import chat.presentation.display.ContactStateUi
import chat.presentation.display.InviteCheck
import chat.presentation.preview.PreviewIdentityState

/** 真实身份面板的交互回调；由桌面入口把每次点击接到 suspend 服务调用上。 */
data class IdentityHandlers(
    val generateInvite: () -> Unit = {},
    val updatePastedToken: (String) -> Unit = {},
    val previewInvite: () -> Unit = {},
    val acceptInvite: () -> Unit = {},
    val refreshContacts: () -> Unit = {},
) {
    companion object {
        val None = IdentityHandlers()
    }
}

/**
 * “真实身份（本机）”面板：展示 crates/identity 的真实账户/指纹/口令/联系人。
 *
 * 明确的诚实边界：本机生成的口令需要人工复制给对方，节点投递与联系人持久化尚未接入，
 * 面板上不会出现“已送达/已确认”之类的正式状态。
 */
@Composable
fun RealIdentityPanel(
    state: PreviewIdentityState,
    handlers: IdentityHandlers,
    onClose: () -> Unit,
) {
    Box(
        Modifier
            .fillMaxSize()
            .padding(16.dp),
        contentAlignment = Alignment.Center,
    ) {
        Surface(
            modifier = Modifier
                .fillMaxWidth(0.96f)
                .widthIn(max = 720.dp)
                .semantics { contentDescription = "真实身份（本机）面板" },
            color = Color.White,
            shape = RoundedCornerShape(18.dp),
        ) {
            Column(
                Modifier
                    .padding(horizontal = 18.dp, vertical = 16.dp)
                    .verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("真实身份（本机）", style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.Bold)
                        Text(
                            "密码学运算来自 Rust identity crate；口令需人工复制，节点投递与联系人持久化尚未接入。",
                            style = MaterialTheme.typography.labelSmall,
                            color = Color(0xFF5F6B63),
                        )
                    }
                    TextButton(
                        onClick = onClose,
                        modifier = Modifier.semantics { contentDescription = "关闭真实身份面板" },
                    ) { Text("关闭") }
                }

                state.notice?.let { notice ->
                    Surface(
                        color = if (state.lastErrorCode == null) Color(0xFFEDF1E9) else Color(0xFFFDECEA),
                        shape = RoundedCornerShape(10.dp),
                        modifier = Modifier.fillMaxWidth().semantics {
                            liveRegion = LiveRegionMode.Polite
                            stateDescription = notice
                        },
                    ) {
                        Text(
                            notice,
                            Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
                state.lastErrorCode?.let { code ->
                    Text("错误码：" + code, style = MaterialTheme.typography.labelSmall, color = Color(0xFFB3261E))
                }

                val account = state.account
                if (account == null) {
                    Text(
                        if (state.busy) "正在准备本机身份…" else "本机身份尚未就绪。",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    return@Column
                }

                Text("本机账户", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                Text(account.displayName + " · " + account.accountId, style = MaterialTheme.typography.bodySmall)
                Text(account.deviceId, style = MaterialTheme.typography.bodySmall, color = Color(0xFF5F6B63))
                Text(
                    "指纹 " + account.fingerprint,
                    style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
                )
                Text(
                    "昵称只作展示；核对身份必须比对指纹。",
                    style = MaterialTheme.typography.labelSmall,
                    color = Color(0xFFB3261E),
                )

                HorizontalDivider(color = Color(0xFFE1E6E1))

                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Button(
                        onClick = handlers.generateInvite,
                        enabled = !state.busy,
                        modifier = Modifier.semantics { contentDescription = "生成一次性真实口令" },
                    ) { Text("生成一次性口令") }
                    TextButton(
                        onClick = handlers.refreshContacts,
                        enabled = !state.busy,
                        modifier = Modifier.semantics { contentDescription = "刷新真实联系人列表" },
                    ) { Text("刷新联系人") }
                }

                state.invite?.let { invite ->
                    Text("本机口令（复制给对方，24 小时内有效）", style = MaterialTheme.typography.labelLarge)
                    Surface(color = Color(0xFFF5F7F2), shape = RoundedCornerShape(8.dp), modifier = Modifier.fillMaxWidth()) {
                        Text(
                            invite.token,
                            Modifier.padding(10.dp),
                            style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace),
                        )
                    }
                    Text(
                        "口令 ID " + invite.inviteId + " · 本机指纹 " + invite.fingerprint,
                        style = MaterialTheme.typography.labelSmall,
                        color = Color(0xFF5F6B63),
                    )
                }

                HorizontalDivider(color = Color(0xFFE1E6E1))

                Text("粘贴对方口令并校验", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                OutlinedTextField(
                    value = state.pastedToken,
                    onValueChange = handlers.updatePastedToken,
                    modifier = Modifier.fillMaxWidth().semantics {
                        contentDescription = "对方的一次性口令输入框"
                    },
                    minLines = 2,
                    maxLines = 4,
                    label = { Text("口令（inv1.…）") },
                    textStyle = MaterialTheme.typography.labelSmall,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(
                        onClick = handlers.previewInvite,
                        enabled = !state.busy && state.pastedToken.isNotBlank(),
                        modifier = Modifier.semantics { contentDescription = "校验口令签名" },
                    ) { Text("校验") }
                    Button(
                        onClick = handlers.acceptInvite,
                        enabled = !state.busy && state.preview?.valid == true,
                        modifier = Modifier.semantics { contentDescription = "把已校验的口令接受为联系人请求" },
                    ) { Text("接受为联系人") }
                }

                state.preview?.let { preview ->
                    val color = if (preview.check == InviteCheck.VALID) Color(0xFF2F6B4F) else Color(0xFFB3261E)
                    Surface(color = Color(0xFFF5F7F2), shape = RoundedCornerShape(10.dp), modifier = Modifier.fillMaxWidth()) {
                        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                            Text(preview.title, color = color, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
                            Text(preview.detail, style = MaterialTheme.typography.bodySmall)
                            preview.fingerprint?.let {
                                Text("对方指纹 " + it, style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace))
                            }
                            preview.accountId?.let { Text("账户 " + it, style = MaterialTheme.typography.labelSmall) }
                            preview.remainingMinutes?.let { Text("剩余有效期约 " + it + " 分钟", style = MaterialTheme.typography.labelSmall) }
                        }
                    }
                }

                HorizontalDivider(color = Color(0xFFE1E6E1))
                Text("真实联系人（本机）", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                if (state.contacts.isEmpty()) {
                    Text("还没有真实联系人；接受一条口令后会出现 requested 状态。", style = MaterialTheme.typography.bodySmall)
                } else {
                    state.contacts.forEach { contact ->
                        Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(contact.displayName, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium)
                                Text(
                                    "  " + contact.badge,
                                    style = MaterialTheme.typography.labelSmall,
                                    color = if (contact.state == ContactStateUi.IDENTITY_CHANGED) Color(0xFFB3261E) else Color(0xFF2F6B4F),
                                )
                                Text(
                                    if (contact.canSend) "  可发送" else "  已阻断",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = Color(0xFF5F6B63),
                                )
                            }
                            Text(contact.fingerprint, style = MaterialTheme.typography.labelSmall.copy(fontFamily = FontFamily.Monospace))
                            contact.blockingReason?.let {
                                Text(it, style = MaterialTheme.typography.labelSmall, color = Color(0xFF5F6B63))
                            }
                        }
                    }
                }
            }
        }
    }
}

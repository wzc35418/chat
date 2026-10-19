package chat.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
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
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import chat.presentation.preview.PreviewGroupState
import chat.presentation.preview.memberRows
import chat.presentation.preview.statusLabel

/** 群面板回调。 */
data class GroupHandlers(
    val updateCreateName: (String) -> Unit = {},
    val createGroup: () -> Unit = {},
    val refresh: () -> Unit = {},
    val updateNewName: (String) -> Unit = {},
    val rename: () -> Unit = {},
    val toggleMute: (String, Boolean) -> Unit = { _, _ -> },
    val setRole: (String, String) -> Unit = { _, _ -> },
    val removeMember: (String) -> Unit = {},
    val acceptPending: () -> Unit = {},
    val discardPending: () -> Unit = {},
    val loadGroupList: () -> Unit = {},
    val selectGroup: (String) -> Unit = {},
) {
    companion object {
        val None = GroupHandlers()
    }
}

/**
 * “群（真实 MLS）”面板：快照 + 成员 + 治理动作。
 *
 * 诚实边界：pending 单独展示（未合并提交）；本地接受不等于节点确认；
 * 成员凭据是本机派生文本，不是真实账户目录 ID。
 */
@Composable
fun GroupPanel(
    state: PreviewGroupState,
    handlers: GroupHandlers,
    onClose: () -> Unit,
) {
    Box(Modifier.fillMaxSize().padding(16.dp), contentAlignment = Alignment.Center) {
        Surface(
            modifier = Modifier
                .fillMaxWidth(0.96f)
                .widthIn(max = 820.dp)
                .heightIn(max = 720.dp)
                .semantics { contentDescription = "群（真实 MLS）面板" },
            color = Color.White,
            shape = RoundedCornerShape(18.dp),
        ) {
            Column(
                Modifier.padding(horizontal = 18.dp, vertical = 16.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("群（真实 MLS）", style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.Bold)
                        Text(
                            "建群/角色/禁言/移除/改名都是真实 OpenMLS 与策略操作；本地接受 ≠ 节点确认。",
                            style = MaterialTheme.typography.labelSmall,
                            color = Color(0xFF5F6B63),
                        )
                    }
                    TextButton(onClick = onClose) { Text("关闭") }
                }

                state.notice?.let { notice ->
                    Surface(
                        color = if (state.lastErrorCode == null) Color(0xFFEDF1E9) else Color(0xFFFDECEA),
                        shape = RoundedCornerShape(10.dp),
                        modifier = Modifier.fillMaxWidth().semantics {
                            liveRegion = LiveRegionMode.Polite
                            contentDescription = notice
                        },
                    ) { Text(notice, Modifier.padding(12.dp), style = MaterialTheme.typography.bodySmall) }
                }
                state.lastErrorCode?.let { code ->
                    Text("错误码：" + code, style = MaterialTheme.typography.labelSmall, color = Color(0xFFB3261E))
                }

                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    OutlinedTextField(
                        value = state.createName,
                        onValueChange = handlers.updateCreateName,
                        modifier = Modifier.weight(1f),
                        singleLine = true,
                        label = { Text("新群名称") },
                    )
                    Button(
                        onClick = handlers.createGroup,
                        enabled = state.ready && !state.busy && state.createName.isNotBlank(),
                    ) { Text("建群") }
                    TextButton(
                        onClick = handlers.refresh,
                        enabled = state.ready && !state.busy,
                    ) { Text("刷新快照") }
                }

                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text("本机群列表", style = MaterialTheme.typography.labelLarge, fontWeight = FontWeight.SemiBold)
                    TextButton(
                        onClick = handlers.loadGroupList,
                        enabled = state.ready && !state.busy,
                        modifier = Modifier.semantics { contentDescription = "加载本机群列表" },
                    ) { Text("加载群列表", style = MaterialTheme.typography.labelMedium) }
                    if (state.skippedGroups > 0) {
                        Text(
                            "跳过 " + state.skippedGroups + " 条损坏记录",
                            style = MaterialTheme.typography.labelSmall,
                            color = Color(0xFFB26A00),
                        )
                    }
                }
                if (state.summaryList.isEmpty()) {
                    Text("还没有群列表；点「加载群列表」后出现。", style = MaterialTheme.typography.bodySmall)
                } else {
                    state.summaryList.forEach { summary ->
                        Row(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            TextButton(
                                onClick = { handlers.selectGroup(summary.groupId) },
                                enabled = state.ready && !state.busy,
                                modifier = Modifier.semantics {
                                    contentDescription = "切换到群 " + summary.name
                                },
                            ) {
                                Text(
                                    (if (summary.groupId == state.selectedGroupId) "● " else "○ ") + summary.name,
                                    style = MaterialTheme.typography.bodySmall,
                                    fontWeight = if (summary.groupId == state.selectedGroupId) FontWeight.SemiBold else FontWeight.Normal,
                                )
                            }
                            Text(
                                "#" + summary.epoch + " · " + summary.role + " · " + summary.statusLabel() +
                                    if (summary.mutedMembers > 0) " · 禁言 " + summary.mutedMembers + " 人" else "" +
                                    if (summary.pendingProposal) " · 待打包对端提案" else "",
                                style = MaterialTheme.typography.labelSmall,
                                color = Color(0xFF5F6B63),
                            )
                        }
                    }
                }

                val snapshot = state.snapshot
                if (snapshot == null) {
                    Text("还没有群快照；建群或刷新后出现。", style = MaterialTheme.typography.bodySmall)
                } else {
                    HorizontalDivider(color = Color(0xFFE1E6E1))
                    Text(snapshot.name, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                    Text(
                        "epoch " + snapshot.epoch + " · 策略版本 " + snapshot.policyVersion +
                            " · 角色 " + snapshot.role + " · 状态版本 " + snapshot.stateVersion,
                        style = MaterialTheme.typography.labelSmall,
                        color = Color(0xFF5F6B63),
                    )
                    Text(
                        "控制头 " + snapshot.controlHead.take(24) + "…",
                        style = MaterialTheme.typography.labelSmall,
                        color = Color(0xFF5F6B63),
                    )
                    snapshot.pending?.let { pending ->
                        Text(
                            "未合并提交：" + pending.operation + "（" + pending.baseEpoch + "→" + pending.targetEpoch +
                                "，+" + pending.added + "/-" + pending.removed + "）",
                            style = MaterialTheme.typography.bodySmall,
                            color = Color(0xFFB26A00),
                        )
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            TextButton(
                                onClick = handlers.acceptPending,
                                enabled = state.ready && !state.busy,
                                modifier = Modifier.semantics { contentDescription = "接受并合并当前控制提交（本地）" },
                            ) { Text("接受（本地）", style = MaterialTheme.typography.labelMedium) }
                            TextButton(
                                onClick = handlers.discardPending,
                                enabled = state.ready && !state.busy,
                            ) { Text("丢弃并回滚", style = MaterialTheme.typography.labelMedium) }
                        }
                    }

                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                        OutlinedTextField(
                            value = state.newName,
                            onValueChange = handlers.updateNewName,
                            modifier = Modifier.weight(1f),
                            singleLine = true,
                            label = { Text("新名称") },
                        )
                        TextButton(
                            onClick = handlers.rename,
                            enabled = state.ready && !state.busy && state.newName.isNotBlank(),
                        ) { Text("改名", style = MaterialTheme.typography.labelMedium) }
                    }

                    HorizontalDivider(color = Color(0xFFE1E6E1))
                    Text("成员（确认 + pending）", style = MaterialTheme.typography.labelLarge)
                    snapshot.memberRows().forEach { member ->
                        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                            Text(
                                member.credential.take(28) + "…" +
                                    (if (member.isLocal) "（本机）" else "") +
                                    (if (member.pending) " · pending" else ""),
                                style = MaterialTheme.typography.bodySmall,
                                fontWeight = if (member.isLocal) FontWeight.SemiBold else FontWeight.Normal,
                            )
                            Text(
                                "角色 " + member.role + (if (member.muted) " · 已禁言" else ""),
                                style = MaterialTheme.typography.labelSmall,
                                color = Color(0xFF5F6B63),
                            )
                            if (!member.isLocal) {
                                Row(horizontalArrangement = Arrangement.spacedBy(2.dp)) {
                                    TextButton(
                                        onClick = { handlers.toggleMute(member.credential, !member.muted) },
                                        enabled = state.ready && !state.busy,
                                    ) { Text(if (member.muted) "解除禁言" else "禁言", style = MaterialTheme.typography.labelSmall) }
                                    TextButton(
                                        onClick = {
                                            handlers.setRole(
                                                member.credential,
                                                if (member.role == "admin") "member" else "admin",
                                            )
                                        },
                                        enabled = state.ready && !state.busy,
                                    ) {
                                        Text(
                                            if (member.role == "admin") "降为成员" else "升为管理员",
                                            style = MaterialTheme.typography.labelSmall,
                                        )
                                    }
                                    TextButton(
                                        onClick = { handlers.removeMember(member.credential) },
                                        enabled = state.ready && !state.busy && !member.pending && member.role != "owner",
                                    ) { Text("移除", style = MaterialTheme.typography.labelSmall) }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

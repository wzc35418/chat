package chat.presentation.preview

import chat.api.GroupMember
import chat.api.GroupSnapshot

/**
 * 预览里的“群（真实 MLS）”状态。
 *
 * 所有字段来自 GroupService 的真实快照；pending 表示“本地有未合并的控制提交”，
 * 界面必须显示它，且不得把 pending 成员说成已入群。
 */
data class PreviewGroupState(
    val ready: Boolean = false,
    val busy: Boolean = false,
    val snapshot: GroupSnapshot? = null,
    val rooms: List<GroupSnapshot> = emptyList(),
    val notice: String? = null,
    val lastErrorCode: String? = null,
    val newName: String = "",
    val createName: String = "",
)

fun PreviewGroupState.withRooms(rooms: List<GroupSnapshot>): PreviewGroupState =
    copy(rooms = rooms, busy = false, snapshot = rooms.firstOrNull() ?: snapshot)

fun PreviewGroupState.withSnapshot(snapshot: GroupSnapshot): PreviewGroupState {
    val merged = LinkedHashMap<String, GroupSnapshot>()
    rooms.forEach { merged[it.groupId] = it }
    merged[snapshot.groupId] = snapshot
    return copy(
        snapshot = snapshot,
        rooms = merged.values.toList(),
        busy = false,
        notice = describe(snapshot),
        lastErrorCode = null,
    )
}

fun PreviewGroupState.updateNewName(value: String): PreviewGroupState = copy(newName = value)

fun PreviewGroupState.updateCreateName(value: String): PreviewGroupState = copy(createName = value)

fun PreviewGroupState.beginBusy(): PreviewGroupState = copy(busy = true, notice = null)

fun PreviewGroupState.withError(code: String, message: String? = null): PreviewGroupState = copy(
    busy = false,
    lastErrorCode = code,
    notice = message ?: "群操作没有成功；界面不会把失败当成功。",
)

/** 诚实文案：pending 与"已入群"分开说。 */
fun describe(snapshot: GroupSnapshot): String {
    val confirmed = snapshot.members.count { !it.pending }
    val pendingPart = snapshot.pending?.let {
        "；未合并提交 " + it.operation + "（" + it.baseEpoch + "→" + it.targetEpoch +
            "，+" + it.added + "/-" + it.removed + "）"
    } ?: ""
    return snapshot.name + " · epoch " + snapshot.epoch + " · 策略版本 " + snapshot.policyVersion +
        " · 确认成员 " + confirmed + pendingPart
}

/** 群成员列表用于界面渲染（含 pending 标记与角色）。 */
fun GroupSnapshot.memberRows(): List<GroupMember> = members + pendingMembers

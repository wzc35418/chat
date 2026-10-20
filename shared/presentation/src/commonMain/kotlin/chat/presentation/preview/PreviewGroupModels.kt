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
    /** 群列表摘要（多群切换用）。 */
    val summaryList: List<GroupSummary> = emptyList(),
    val selectedGroupId: String? = null,
    val skippedGroups: Int = 0,
    /** 待投递邀请的被邀请者凭据。 */
    val inviteeCredential: String = "",
    /** 最近一次邀请提交回执。 */
    val inviteReceipt: chat.api.InviteReceipt? = null,
    /** 被邀请者 KeyPackage 文件路径（用于真实建群 + 可用 Welcome）。 */
    val inviteeKeyPackagePath: String = "",
    /** 最近一次用 KeyPackage 建群的结果（含可用 Welcome）。 */
    val createdGroup: chat.api.CreatedGroup? = null,
    /** 最近一次节点登记回执。 */
    val registration: chat.api.GroupRegistration? = null,
    /** 最近一次从节点取回的邀请。 */
    val fetchedInvite: chat.api.GroupInviteBlob? = null,
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
/** 群列表摘要（GROUP 列表页用；不含成员详情）。 */
data class GroupSummary(
    val groupId: String,
    val name: String,
    val epoch: Int,
    val role: String,
    val policyVersion: Long,
    val dissolved: Boolean,
    val left: Boolean,
    val pending: Boolean,
    val mutedMembers: Int = 0,
    val pendingProposal: Boolean = false,
    val revokedDevicesCount: Int = 0,
)

fun GroupSummary.statusLabel(): String = when {
    dissolved -> "已解散"
    left -> "已退出"
    pending -> "待收口"
    else -> "正常"
}

/** 列表加载：保留当前选中群（若仍在列表里），否则选第一个。 */
fun PreviewGroupState.withSummaries(summaries: List<GroupSummary>, skipped: Int): PreviewGroupState {
    val stillThere = snapshot?.groupId?.let { id -> summaries.any { it.groupId == id } } ?: false
    return copy(
        summaryList = summaries,
        skippedGroups = skipped,
        busy = false,
        notice = if (skipped > 0) {
            "已列出 " + summaries.size + " 个群；" + skipped + " 条记录损坏被跳过（未计入）。"
        } else {
            "已列出 " + summaries.size + " 个群"
        },
        lastErrorCode = null,
        selectedGroupId = if (stillThere) selectedGroupId else summaries.firstOrNull()?.groupId,
    )
}

fun PreviewGroupState.selectGroup(groupId: String): PreviewGroupState = copy(selectedGroupId = groupId)

fun PreviewGroupState.updateInvitee(value: String): PreviewGroupState = copy(inviteeCredential = value)

fun PreviewGroupState.withInviteReceipt(receipt: chat.api.InviteReceipt): PreviewGroupState = copy(
    inviteReceipt = receipt,
    busy = false,
    notice = "邀请已提交：seq " + receipt.seq,
    lastErrorCode = null,
)

fun PreviewGroupState.withFetchedInvite(blob: chat.api.GroupInviteBlob?): PreviewGroupState = copy(
    fetchedInvite = blob,
    busy = false,
    notice = if (blob == null) "节点上没有可取的邀请" else "已取回邀请：" + blob.meta.name,
    lastErrorCode = null,
)

fun PreviewGroupState.updateInviteeKeyPackagePath(path: String): PreviewGroupState = copy(
    inviteeKeyPackagePath = path,
    busy = false,
    notice = "已选择 KeyPackage：" + path,
    lastErrorCode = null,
)

fun PreviewGroupState.withCreatedGroup(created: chat.api.CreatedGroup): PreviewGroupState = copy(
    snapshot = created.snapshot,
    createdGroup = created,
    busy = false,
    notice = "已用对方 KeyPackage 建群：epoch " + created.snapshot.epoch +
        "，Welcome " + created.welcomeBytes.size + " 字节（可用于提交邀请）",
    lastErrorCode = null,
)

fun PreviewGroupState.withRegistration(view: chat.api.GroupRegistration): PreviewGroupState = copy(
    registration = view,
    busy = false,
    notice = "已在节点登记：" + view.groupId + "（seq " + view.registeredSeq + "）",
    lastErrorCode = null,
)

package chat.api

/** 群成员（GROUP 视图）；不含密钥。 */
data class GroupMember(
    val credential: String,
    /** owner / admin / member */
    val role: String,
    val muted: Boolean,
    val isLocal: Boolean,
    val pending: Boolean,
)

data class GroupPending(
    /** add_members / remove_members / self_update */
    val operation: String,
    val baseEpoch: Int,
    val targetEpoch: Int,
    val added: Int,
    val removed: Int,
)

data class GroupSnapshot(
    val groupId: String,
    val name: String,
    /** MLS 群的本地键（用于把"发送"绑定到具体群；不含密钥材料）。 */
    val mlsGroupKey: String = "",
    val epoch: Int,
    val policyVersion: Long,
    val role: String,
    val controlHead: String,
    val stateVersion: Int,
    val createdAtMillis: Long,
    val members: List<GroupMember>,
    val pendingMembers: List<GroupMember>,
    val pending: GroupPending?,
)

/**
 * 群服务（GROUP 模块）。
 *
 * 语义边界：所有操作都是本机真实 MLS/策略操作；接受只收口本地状态（节点回执校验见 GROUP 的后续任务），
 * 绝不把"本地接受"显示成"节点已确认"。
 */
interface GroupService {
    /** 建群（本机为 owner；引导成员由本进程临时设备承担）。 */
    suspend fun createGroup(name: String): GroupSnapshot

    /**
     * 用**真实被邀请者的 KeyPackage** 建群，返回快照与可用的 Welcome 字节。
     *
     * 这是 GROUPS-004 的闭环：返回的 Welcome 能被该设备真正接受入群。
     */
    suspend fun createWithInviteeKeyPackage(
        name: String,
        inviteeCredential: String,
        keyPackageBytes: ByteArray,
        localCredential: String,
    ): CreatedGroup

    /**
     * 在节点登记本群（把本机设备登记为 owner）。**建群后必须调用**，否则提交会被节点拒绝。
     *
     * 幂等：重复登记返回同一群的登记结果；不可达 → `NODE_UNAVAILABLE`；非群主被拒原样透出。
     */
    suspend fun registerOnNode(
        groupId: String,
        endpointUrl: String,
        nodeId: String,
        nodePublicKeyHex: String?,
    ): GroupRegistration

    /** 把邀请（元数据 + Welcome）提交到节点；回执真实、幂等可重投。 */
    suspend fun submitInvite(
        groupId: String,
        meta: GroupInviteMeta,
        welcomeBytes: ByteArray,
        endpointUrl: String,
        nodeId: String,
        nodePublicKeyHex: String?,
    ): InviteReceipt

    /** 从节点取回最近一条邀请；没有邀请返回 null（与"出错"区分开）。 */
    suspend fun fetchInvite(
        groupId: String,
        endpointUrl: String,
        nodeId: String,
        nodePublicKeyHex: String?,
    ): GroupInviteBlob?

    /** 本机群列表（只读投影；skipped 是解析失败被跳过的条数）。 */
    suspend fun listGroups(): GroupList

    suspend fun snapshot(groupId: String): GroupSnapshot

    suspend fun rename(groupId: String, newName: String): GroupSnapshot

    suspend fun setMuted(groupId: String, target: String, muted: Boolean): GroupSnapshot

    suspend fun setRole(groupId: String, target: String, role: String): GroupSnapshot

    suspend fun removeMember(groupId: String, target: String): GroupSnapshot

    suspend fun acceptPending(groupId: String): GroupSnapshot

    suspend fun discardPending(groupId: String, reason: String): GroupSnapshot

    /** 释放独占写锁；之后所有调用返回 CORE_CLOSED。 */
    suspend fun shutdown()
}
/** 群摘要（列表页用；不含成员详情）。 */
data class GroupSummaryEntry(
    val groupId: String,
    val name: String,
    val epoch: Int,
    val role: String,
    val policyVersion: Long,
    val dissolved: Boolean,
    val left: Boolean,
    /** 待收口线索：自己发起自移除，或有提案待打包。 */
    val pending: Boolean,
    /** 确认成员里被禁言的人数。 */
    val mutedMembers: Int = 0,
    /** 是否有一条**对端**提案等本机打包。 */
    val pendingProposal: Boolean = false,
    /** 本机快照里已传播的设备撤销条数。 */
    val revokedDevicesCount: Int = 0,
)

data class GroupList(
    val groups: List<GroupSummaryEntry>,
    /** 损坏或不合规而被跳过的条目数（不静默丢）。 */
    val skipped: Int,
)

/** 邀请元数据（与 core GroupInviteMeta 一致；纯数据）。 */
data class GroupInviteMeta(
    val groupId: String,
    val mlsGroupId: String,
    val name: String,
    val inviterCredential: String,
    val ownerCredential: String,
    val inviteeCredential: String,
    val members: List<String>,
    val epoch: Int,
)

/** 邀请提交回执（节点真实回执，不伪造）。 */
data class InviteReceipt(
    val submissionId: String,
    val seq: Long,
    val logHash: String,
    val nodeId: String,
)

/** 从节点取回的邀请（Welcome 字节原样）。 */
data class GroupInviteBlob(
    val meta: GroupInviteMeta,
    val welcomeBytes: ByteArray,
)

/** 建群结果：快照 + 可用的 Welcome 字节 + 与之一致的邀请元数据。 */
data class CreatedGroup(
    val snapshot: GroupSnapshot,
    val welcomeBytes: ByteArray,
    val invite: GroupInviteMeta,
)

/** 节点登记回执（纯数据）。 */
data class GroupRegistration(
    val groupId: String,
    val registeredSeq: Long,
    val nodeId: String,
)

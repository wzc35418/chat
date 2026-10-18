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
    suspend fun snapshot(groupId: String): GroupSnapshot

    suspend fun rename(groupId: String, newName: String): GroupSnapshot

    suspend fun setMuted(groupId: String, target: String, muted: Boolean): GroupSnapshot

    suspend fun setRole(groupId: String, target: String, role: String): GroupSnapshot

    suspend fun removeMember(groupId: String, target: String): GroupSnapshot

    suspend fun acceptPending(groupId: String): GroupSnapshot

    suspend fun discardPending(groupId: String, reason: String): GroupSnapshot
}

package chat.bridge

import chat.api.GroupMember
import chat.api.GroupPending
import chat.api.GroupService
import chat.api.GroupSnapshot
import chat.api.LocalStoreException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.chat_ffi.FfiException
import uniffi.chat_ffi.GroupFfi
import uniffi.chat_ffi.GroupViewFfi

actual fun createGroupService(databasePath: String, masterKeyHex: String, node: String?): GroupService =
    NativeGroupService(databasePath, masterKeyHex, node)

/**
 * UniFFI 群服务桥：原生调用切 IO 调度并串行化（MLS 提交生成是 CPU 工作，不阻塞 UI 线程）。
 * 错误映射到带稳定码的 LocalStoreException；关闭后调用同样抛 CORE_CLOSED。
 */
class NativeGroupService(
    databasePath: String,
    masterKeyHex: String,
    node: String?,
) : GroupService {
    private val native: GroupFfi = try {
        GroupFfi.open(databasePath, masterKeyHex, node)
    } catch (error: FfiException.Store) {
        throw LocalStoreException(error.code, error.reason)
    }
    private val mutex = Mutex()

    override suspend fun createGroup(name: String): GroupSnapshot = call { native ->
        // 演示用固定凭据：真实产品应由身份目录提供设备凭据（GROUP 与 IDENTITY 打通属后续任务）。
        native.groupCreate(name, "local-device", "bootstrap-device", emptyList()).toSnapshot()
    }

    override suspend fun listGroups(): chat.api.GroupList = mutex.withLock {
        withContext(Dispatchers.IO) {
            try {
                val list = native.groupList()
                chat.api.GroupList(
                    groups = list.groups.map {
                        chat.api.GroupSummaryEntry(
                            groupId = it.groupId,
                            name = it.name,
                            epoch = it.epoch.toInt(),
                            role = it.role,
                            policyVersion = it.policyVersion.toLong(),
                            dissolved = it.dissolved,
                            left = it.left,
                            pending = it.pending,
                            mutedMembers = it.mutedMembers.toInt(),
                            pendingProposal = it.pendingProposal,
                            revokedDevicesCount = it.revokedDevicesCount.toInt(),
                        )
                    },
                    skipped = list.skipped.toInt(),
                )
            } catch (error: FfiException.Store) {
                throw LocalStoreException(error.code, error.reason)
            }
        }
    }

    override suspend fun submitInvite(
        groupId: String,
        meta: chat.api.GroupInviteMeta,
        welcomeBytes: ByteArray,
        endpointUrl: String,
        nodeId: String,
        nodePublicKeyHex: String?,
    ): chat.api.InviteReceipt = call {
        val receipt = it.groupSubmitInvite(
            groupId,
            uniffi.chat_ffi.InviteMetaFfi(
                groupId = meta.groupId,
                mlsGroupId = meta.mlsGroupId,
                name = meta.name,
                inviterCredential = meta.inviterCredential,
                ownerCredential = meta.ownerCredential,
                inviteeCredential = meta.inviteeCredential,
                members = meta.members,
                epoch = meta.epoch.toULong(),
            ),
            welcomeBytes,
            endpointUrl,
            nodeId,
            nodePublicKeyHex,
        )
        chat.api.InviteReceipt(
            submissionId = receipt.submissionId,
            seq = receipt.seq.toLong(),
            logHash = receipt.logHash,
            nodeId = receipt.nodeId,
        )
    }

    override suspend fun fetchInvite(
        groupId: String,
        endpointUrl: String,
        nodeId: String,
        nodePublicKeyHex: String?,
    ): chat.api.GroupInviteBlob? {
        val blob = call { it.groupFetchInvite(groupId, endpointUrl, nodeId, nodePublicKeyHex) } ?: return null
        return chat.api.GroupInviteBlob(
            meta = chat.api.GroupInviteMeta(
                groupId = blob.meta.groupId,
                mlsGroupId = blob.meta.mlsGroupId,
                name = blob.meta.name,
                inviterCredential = blob.meta.inviterCredential,
                ownerCredential = blob.meta.ownerCredential,
                inviteeCredential = blob.meta.inviteeCredential,
                members = blob.meta.members,
                epoch = blob.meta.epoch.toInt(),
            ),
            welcomeBytes = blob.welcomeBytes,
        )
    }

    override suspend fun snapshot(groupId: String): GroupSnapshot = call { it.groupSnapshot(groupId).toSnapshot() }

    override suspend fun rename(groupId: String, newName: String): GroupSnapshot =
        call { native ->
            native.groupRename(groupId, localCredential(native, groupId), newName).toSnapshot()
        }

    override suspend fun setMuted(groupId: String, target: String, muted: Boolean): GroupSnapshot =
        call { native ->
            native.groupSetMuted(groupId, localCredential(native, groupId), target, muted).toSnapshot()
        }

    override suspend fun setRole(groupId: String, target: String, role: String): GroupSnapshot =
        call { native ->
            native.groupSetRole(groupId, localCredential(native, groupId), target, role).toSnapshot()
        }

    override suspend fun removeMember(groupId: String, target: String): GroupSnapshot =
        call { native ->
            native.groupRemoveMember(groupId, localCredential(native, groupId), target).toSnapshot()
        }

    override suspend fun acceptPending(groupId: String): GroupSnapshot =
        call { it.groupAcceptPending(groupId).toSnapshot() }

    override suspend fun discardPending(groupId: String, reason: String): GroupSnapshot =
        call { it.groupDiscardPending(groupId, reason).toSnapshot() }

    /** 本机成员凭据：从真实快照里找 isLocal 的成员；找不到就显式失败（不猜）。 */
    private fun localCredential(native: GroupFfi, groupId: String): String {
        val view = native.groupSnapshot(groupId)
        return view.members.firstOrNull { it.isLocal }?.credential
            ?: throw LocalStoreException("INVALID_STATE_TRANSITION", "本机不在该群的确认成员里")
    }

    // 泛型化：既要返回快照，也要返回邀请回执/blob（返回类型由调用点决定）。
    private suspend fun <T> call(block: (GroupFfi) -> T): T = mutex.withLock {
        withContext(Dispatchers.IO) {
            try {
                block(native)
            } catch (error: FfiException.Store) {
                throw LocalStoreException(error.code, error.reason)
            }
        }
    }

    override suspend fun shutdown(): Unit = mutex.withLock {
        withContext(Dispatchers.IO) { runCatching { native.shutdown() } }
        Unit
    }
}

private fun GroupViewFfi.toSnapshot() = GroupSnapshot(
    groupId = groupId,
    name = name,
    epoch = epoch.toInt(),
    policyVersion = policyVersion.toLong(),
    role = role,
    controlHead = controlHead,
    stateVersion = stateVersion.toInt(),
    createdAtMillis = createdAtMs,
    members = members.map { it.toMember() },
    pendingMembers = pendingMembers.map { it.toMember() },
    pending = pending?.let {
        GroupPending(
            operation = it.operation,
            baseEpoch = it.baseEpoch.toInt(),
            targetEpoch = it.targetEpoch.toInt(),
            added = it.added.toInt(),
            removed = it.removed.toInt(),
        )
    },
)

private fun uniffi.chat_ffi.GroupMemberFfi.toMember() = GroupMember(
    credential = credential,
    role = role,
    muted = muted,
    isLocal = isLocal,
    pending = pending,
)
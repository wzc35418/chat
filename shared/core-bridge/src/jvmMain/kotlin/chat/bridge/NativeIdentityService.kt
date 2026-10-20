package chat.bridge

import chat.api.IdentityAccount
import chat.api.IdentityContact
import chat.api.IdentityException
import chat.api.IdentityInvite
import chat.api.IdentityInviteSummary
import chat.api.IdentityService
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.chat_ffi.FfiException
import uniffi.chat_ffi.IdentityFfi

actual fun createIdentityService(): IdentityService = NativeIdentityService()

/**
 * UniFFI 身份桥。
 *
 * - 原生调用切到 IO 调度，同一实例互斥串行化；
 * - 原生错误转成 [IdentityException]（code 是 PROTOCOL-014 稳定码）；
 * - close 之后拒绝调用；重复 close 幂等。
 */
class NativeIdentityService : IdentityService {
    private val mutex = Mutex()
    private var native: IdentityFfi? = null

    override suspend fun create(displayName: String, node: String): IdentityAccount = mutex.withLock {
        check(native == null) { "身份服务已经创建" }
        withContext(Dispatchers.IO) {
            val created = guard { IdentityFfi.create(displayName, node) }
            native = created
            created.account().toAccount()
        }
    }

    override suspend fun openPersisted(
        databasePath: String,
        masterKeyHex: String,
        node: String,
    ): IdentityAccount = mutex.withLock {
        check(native == null) { "身份服务已经创建" }
        withContext(Dispatchers.IO) {
            val opened = guard { IdentityFfi.openPersisted(databasePath, masterKeyHex, node) }
            native = opened
            opened.account().toAccount()
        }
    }

    override suspend fun account(): IdentityAccount = withIdentity { it.account().toAccount() }

    override suspend fun createInvite(ttlMillis: Long): IdentityInvite = withIdentity { identity ->
        val invite = guard { identity.createInvite(ttlMillis.toULong()) }
        IdentityInvite(
            token = invite.token,
            inviteId = invite.inviteId,
            expiresAtMillis = invite.expiresAtMs.toLong(),
            accountId = invite.accountId,
            deviceId = invite.deviceId,
            fingerprint = invite.fingerprint,
        )
    }

    override suspend fun previewInvite(token: String): IdentityInviteSummary = withIdentity { identity ->
        val summary = guard { identity.previewInvite(token) }
        IdentityInviteSummary(
            inviteId = summary.inviteId,
            accountId = summary.accountId,
            deviceId = summary.deviceId,
            fingerprint = summary.fingerprint,
            node = summary.node,
            expiresAtMillis = summary.expiresAtMs.toLong(),
        )
    }

    override suspend fun acceptInvite(token: String, displayName: String): IdentityContact =
        withIdentity { identity ->
            guard { identity.acceptInvite(token, displayName) }.toContact()
        }

    override suspend fun contacts(): List<IdentityContact> = withIdentity { identity ->
        identity.contacts().map { it.toContact() }
    }

    override suspend fun confirmIdentityChange(contactId: String): IdentityContact =
        withIdentity { identity ->
            guard { identity.confirmIdentityChange(contactId) }.toContact()
        }


    override suspend fun exportRecoveryBundle(passphrase: String): ByteArray = mutex.withLock {
        withContext(Dispatchers.IO) {
            val identity = native ?: throw IdentityException("CORE_CLOSED", "identity is not open")
            guard { identity.exportRecoveryBundle(passphrase) }
        }
    }

    override suspend fun importRecoveryBundle(
        passphrase: String,
        bundle: ByteArray,
    ): chat.api.RecoveryImportView = mutex.withLock {
        withContext(Dispatchers.IO) {
            val identity = native ?: throw IdentityException("CORE_CLOSED", "identity is not open")
            val view = guard { identity.importRecoveryBundle(passphrase, bundle) }
            chat.api.RecoveryImportView(
                accountId = view.accountId,
                deviceId = view.deviceId,
                accountPublicKey = view.accountPublicKey,
                devicePublicKey = view.devicePublicKey,
                displayName = view.displayName,
                createdAtMillis = view.createdAtMs,
                devicesDigest = view.devicesDigest,
            )
        }
    }

    override suspend fun exportKeyPackage(): ByteArray = mutex.withLock {
        withContext(Dispatchers.IO) {
            val identity = native ?: throw IdentityException("CORE_CLOSED", "identity is not open")
            guard { identity.exportKeyPackage() }
        }
    }

    override suspend fun enrollIntoStore(
        targetDatabasePath: String,
        targetMasterKeyHex: String,
    ): chat.api.IdentityAccount = mutex.withLock {
        withContext(Dispatchers.IO) {
            val identity = native ?: throw IdentityException("CORE_CLOSED", "identity is not open")
            val view = guard { identity.enrollIntoStore(targetDatabasePath, targetMasterKeyHex) }
            chat.api.IdentityAccount(
                accountId = view.accountId,
                deviceId = view.deviceId,
                displayName = view.displayName,
                fingerprint = view.fingerprint,
            )
        }
    }

    override suspend fun listDevices(includeRevoked: Boolean): List<chat.api.IdentityDevice> = mutex.withLock {
        withContext(Dispatchers.IO) {
            val identity = native ?: throw IdentityException("CORE_CLOSED", "identity is not open")
            guard { identity.listDevices(includeRevoked) }.map { view ->
                chat.api.IdentityDevice(
                    deviceId = view.deviceId,
                    accountId = view.accountId,
                    label = view.label,
                    devicePublicKeyHex = view.devicePublicKeyHex,
                    addedAtMillis = view.addedAtMs.toLong(),
                    revokedAtMillis = view.revokedAtMs?.toLong(),
                    revokedReason = view.revokedReason,
                    isLocal = view.isLocal,
                )
            }
        }
    }

    override suspend fun registerDevice(
        deviceId: String,
        devicePublicKeyHex: String,
        label: String,
    ): chat.api.IdentityDevice = mutex.withLock {
        withContext(Dispatchers.IO) {
            val identity = native ?: throw IdentityException("CORE_CLOSED", "identity is not open")
            val view = guard { identity.registerDevice(deviceId, devicePublicKeyHex, label) }
            chat.api.IdentityDevice(
                deviceId = view.deviceId,
                accountId = view.accountId,
                label = view.label,
                devicePublicKeyHex = view.devicePublicKeyHex,
                addedAtMillis = view.addedAtMs.toLong(),
                revokedAtMillis = view.revokedAtMs?.toLong(),
                revokedReason = view.revokedReason,
                isLocal = view.isLocal,
            )
        }
    }

    override suspend fun revokeDevice(deviceId: String, reason: String): Boolean = mutex.withLock {
        withContext(Dispatchers.IO) {
            val identity = native ?: throw IdentityException("CORE_CLOSED", "identity is not open")
            guard { identity.revokeDevice(deviceId, reason) }
        }
    }

    override suspend fun close(): Unit = withContext(NonCancellable) {
        mutex.withLock {
            withContext(Dispatchers.IO) {
                val current = native
                native = null
                if (current != null) {
                    try {
                        current.shutdown()
                    } catch (_: Throwable) {
                        // 关闭期错误不再上抛：句柄随即销毁，避免泄漏更重要。
                    } finally {
                        current.destroy()
                    }
                }
            }
        }
    }

    private suspend fun <T> withIdentity(block: (IdentityFfi) -> T): T = mutex.withLock {
        val identity = native ?: throw IdentityException("CORE_CLOSED", "身份服务尚未创建或已关闭")
        withContext(Dispatchers.IO) { guard { block(identity) } }
    }

    private inline fun <T> guard(block: () -> T): T = try {
        block()
    } catch (error: FfiException.Store) {
        throw IdentityException(error.code, error.reason)
    }
}

private fun uniffi.chat_ffi.AccountView.toAccount() = IdentityAccount(
    accountId = accountId,
    deviceId = deviceId,
    displayName = displayName,
    fingerprint = fingerprint,
)

private fun uniffi.chat_ffi.ContactView.toContact() = IdentityContact(
    contactId = contactId,
    accountId = accountId,
    displayName = displayName,
    fingerprint = fingerprint,
    state = state,
    canSend = canSend,
)
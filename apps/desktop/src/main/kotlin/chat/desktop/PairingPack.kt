package chat.desktop

import uniffi.chat_ffi.SharedInviteFfi
import uniffi.chat_ffi.SharedPullStateFfi
import java.util.Base64

/**
 * 配对包（copy/paste 文本）的编解码。
 *
 * 两种包：
 * - **KeyPackage 包**：邀请方需要的“被邀请者材料”（凭据 + KeyPackage 原文）；
 * - **邀请包**：被邀请者需要的“入群材料”（MLS Welcome + 群元数据 + 已验证起点 head）。
 *
 * 格式是**行式 key=value**（首行是版本），便于用户把它当普通文本发出去；
 * 解析失败一律抛 [IllegalArgumentException] 并说明缺哪一项（**不猜默认值**）。
 */
object PairingPack {
    const val KEY_PACKAGE_HEADER = "chat-kp-v1"
    const val INVITE_HEADER = "chat-invite-v1"

    /** 被邀请者材料：凭据 + KeyPackage 原始字节。 */
    data class KeyPackagePack(
        val credential: String,
        val keyPackage: ByteArray,
    )

    /** 邀请包：Welcome 原文 + 群元数据 + 已验证起点。 */
    data class InvitePack(
        val invite: SharedInviteFfi,
        val head: SharedPullStateFfi,
    )

    fun encodeKeyPackage(credential: String, keyPackage: ByteArray): String = buildString {
        appendLine(KEY_PACKAGE_HEADER)
        appendLine("credential=" + credential)
        appendLine("keypackage=" + Base64.getEncoder().encodeToString(keyPackage))
    }.trimEnd()

    fun parseKeyPackage(text: String): KeyPackagePack {
        val fields = parseLines(text, KEY_PACKAGE_HEADER)
        val credential = require(fields, "credential")
        val bytes = decodeBase64(require(fields, "keypackage"), "keypackage")
        return KeyPackagePack(credential, bytes)
    }

    fun encodeInvite(
        invite: SharedInviteFfi,
        head: SharedPullStateFfi,
    ): String = buildString {
        appendLine(INVITE_HEADER)
        appendLine("groupId=" + invite.groupId)
        appendLine("mlsGroupId=" + invite.mlsGroupId)
        appendLine("name=" + invite.name)
        appendLine("inviterCredential=" + invite.inviterCredential)
        appendLine("ownerCredential=" + invite.ownerCredential)
        appendLine("inviteeCredential=" + invite.inviteeCredential)
        appendLine("members=" + invite.members.joinToString(","))
        appendLine("epoch=" + invite.epoch)
        appendLine("cursorSeq=" + head.cursorSeq)
        appendLine("cursorLogHash=" + head.cursorLogHash)
        appendLine("controlHead=" + head.controlHead)
        appendLine("policyVersion=" + head.policyVersion)
        appendLine("mlsEpoch=" + head.mlsEpoch)
        appendLine("welcome=" + Base64.getEncoder().encodeToString(invite.welcomeBytes))
    }.trimEnd()

    fun parseInvite(text: String): InvitePack {
        val fields = parseLines(text, INVITE_HEADER)
        val invite = SharedInviteFfi(
            groupId = require(fields, "groupId"),
            mlsGroupId = require(fields, "mlsGroupId"),
            name = require(fields, "name"),
            inviterCredential = require(fields, "inviterCredential"),
            ownerCredential = require(fields, "ownerCredential"),
            inviteeCredential = require(fields, "inviteeCredential"),
            members = require(fields, "members").split(",").filter { it.isNotBlank() },
            epoch = require(fields, "epoch").toULong(),
            welcomeBytes = decodeBase64(require(fields, "welcome"), "welcome"),
        )
        val head = SharedPullStateFfi(
            cursorSeq = require(fields, "cursorSeq").toULong(),
            cursorLogHash = require(fields, "cursorLogHash"),
            controlHead = require(fields, "controlHead"),
            policyVersion = require(fields, "policyVersion").toULong(),
            mlsEpoch = require(fields, "mlsEpoch").toULong(),
        )
        return InvitePack(invite, head)
    }

    private fun parseLines(text: String, header: String): Map<String, String> {
        val lines = text.lines().map { it.trim() }.filter { it.isNotEmpty() }
        require(lines.isNotEmpty() && lines[0] == header) {
            "不是 " + header + " 格式（第一行应为 " + header + "）"
        }
        val fields = LinkedHashMap<String, String>()
        for (line in lines.drop(1)) {
            val at = line.indexOf("=")
            require(at > 0) { "无法解析的行：" + line.take(40) }
            fields[line.substring(0, at).trim()] = line.substring(at + 1).trim()
        }
        return fields
    }

    private fun require(fields: Map<String, String>, name: String): String =
        fields[name] ?: throw IllegalArgumentException("缺少字段：" + name)

    private fun decodeBase64(text: String, name: String): ByteArray = try {
        Base64.getDecoder().decode(text)
    } catch (error: IllegalArgumentException) {
        throw IllegalArgumentException(name + " 不是合法 base64：" + (error.message ?: ""))
    }
}

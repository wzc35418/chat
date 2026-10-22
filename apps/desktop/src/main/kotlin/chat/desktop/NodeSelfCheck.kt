package chat.desktop

import java.security.MessageDigest
import java.security.SecureRandom
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

/** 自检里的一个步骤结果。 */
data class SelfCheckStep(val name: String, val ok: Boolean, val detail: String)

/**
 * 节点连通性自检（应用内可直接跑，用于部署排障）。
 *
 * 顺序很重要：**先取服务端证书指纹**（这一步不强制 pinning，只做“记录 + 比对”），
 * 再发强制 pinning 的请求。这样“指纹配错”时用户看到的是「配置 A 与服务端 B 不一致」，
 * 而不是先被一句“握手失败”挡住、根本不知道服务端到底是哪张证书。
 *
 * 每一步都是真实网络请求；失败一律给出稳定原因文本，不静默。
 */
object NodeSelfCheck {

    fun run(endpointUrl: String, fingerprints: List<String>, expectedNodeId: String): List<SelfCheckStep> {
        val steps = mutableListOf<SelfCheckStep>()
        val base = endpointUrl.trim().trimEnd('/')
        if (base.isEmpty()) {
            steps += SelfCheckStep("endpoint", false, "节点地址为空：先在首启引导里配置")
            return steps
        }
        val isHttps = base.startsWith("https://", ignoreCase = true)
        val started = System.nanoTime()

        // 1. 服务端证书指纹（诊断用：不校验，只记录并比对）
        val served = if (isHttps) runCatching { servedFingerprint(base) }.getOrNull() else null
        if (isHttps) {
            if (served == null) {
                steps += SelfCheckStep("cert_fingerprint", false, "拿不到服务端证书（TLS 握手失败）")
            } else if (fingerprints.isEmpty()) {
                steps += SelfCheckStep("cert_fingerprint", true, "服务端证书 " + served.take(16) + "…（本地未配置指纹）")
            } else {
                val configured = fingerprints.map { it.trim().lowercase() }
                steps += if (served in configured) {
                    SelfCheckStep("cert_fingerprint", true, "服务端证书 = 配置指纹（" + served.take(16) + "…）")
                } else {
                    SelfCheckStep(
                        "cert_fingerprint",
                        false,
                        "配置 " + configured.first().take(16) + "… 与服务端 " + served.take(16) + "… 不一致（换过证书，或指纹抄错）",
                    )
                }
            }
        }

        // 2. health（https 时强制 pinning：这一步才是真正的“信任校验”）
        val healthBody = runCatching { request(base + "/v1/health", isHttps, fingerprints).second }.getOrElse { error ->
            steps += SelfCheckStep("health", false, error.message ?: error::class.simpleName ?: "请求失败")
            return steps
        }
        steps += SelfCheckStep("health", true, "HTTP 200")

        // 3. node_id 是否与配置一致（防连错节点）
        val nodeId = Regex("\"(?:node_id|nodeId)\"\\s*:\\s*\"([^\"]+)\"").find(healthBody)?.groupValues?.get(1)
        steps += when {
            nodeId == null -> SelfCheckStep("node_id", false, "响应里没有 node_id")
            expectedNodeId.isBlank() -> SelfCheckStep("node_id", true, "实际 " + nodeId + "（未配置期望值，跳过比对）")
            nodeId == expectedNodeId -> SelfCheckStep("node_id", true, "与配置一致：" + nodeId)
            else -> SelfCheckStep("node_id", false, "期望 " + expectedNodeId + "，实际 " + nodeId + "（连错节点？）")
        }

        // 4. 证书状态端点（只有 https 才有意义）
        if (isHttps) {
            val tlsBody = runCatching { request(base + "/v1/tls/info", true, fingerprints).second }.getOrNull()
            if (tlsBody == null) {
                steps += SelfCheckStep("tls_info", true, "读不到 /v1/tls/info（不影响连通，只是看不到剩余天数）")
            } else {
                val days = Regex("\"days_remaining\"\\s*:\\s*(-?\\d+)").find(tlsBody)?.groupValues?.get(1)?.toIntOrNull()
                steps += when {
                    days == null -> SelfCheckStep("tls_info", true, "响应里没有 days_remaining")
                    days <= 0 -> SelfCheckStep("tls_info", false, "证书已过期（" + days + " 天）")
                    else -> SelfCheckStep("tls_info", true, "证书剩余 " + days + " 天")
                }
            }
        }

        val elapsed = (System.nanoTime() - started) / 1_000_000
        steps += SelfCheckStep("latency", true, elapsed.toString() + " ms")
        return steps
    }

    
/**
 * 从 `/v1/health` 取节点公钥（https 时**按指纹固定证书**，不全信任）。
 * 首启没填公钥时用这个；取不到就返回 null，由调用方显式提示。
 */
internal fun fetchNodePublicKeyHex(endpointUrl: String, fingerprints: List<String>): String? = runCatching {
    val url = endpointUrl.trimEnd('/') + "/v1/health"
    val connection = java.net.URI(url).toURL().openConnection() as? javax.net.ssl.HttpsURLConnection ?: return null
    if (fingerprints.isNotEmpty()) {
        connection.sslSocketFactory = pinnedContext(fingerprints).socketFactory
    }
    connection.connectTimeout = 5000
    connection.readTimeout = 5000
    val body = connection.inputStream.bufferedReader().readText()
    Regex("\"node_public_key\"\\s*:\\s*\"([0-9a-fA-F]+)\"").find(body)?.groupValues?.get(1)
}.getOrNull()
/** 只记录服务端证书指纹（不校验），用于诊断“配的指纹 vs 服务端的指纹”。 */
    private fun servedFingerprint(url: String): String? {
        var captured: String? = null
        val recorder = object : X509TrustManager {
            override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) = Unit

            override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {
                captured = sha256Hex(chain.first())
            }

            override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
        }
        val context = SSLContext.getInstance("TLS")
        context.init(null, arrayOf<TrustManager>(recorder), SecureRandom())
        val connection = java.net.URI(url).toURL().openConnection() as? HttpsURLConnection ?: return null
        connection.sslSocketFactory = context.socketFactory
        connection.connectTimeout = 5000
        connection.readTimeout = 5000
        runCatching { connection.responseCode }
        return captured
    }

    /** 发一个 GET，返回 (状态码, 响应体)；https 时必须给指纹，且只接受匹配的那张证书。 */
    private fun request(url: String, isHttps: Boolean, fingerprints: List<String>): Pair<Int, String> {
        val connection = java.net.URI(url).toURL().openConnection()
        connection.connectTimeout = 5000
        connection.readTimeout = 5000
        if (connection is HttpsURLConnection) {
            if (fingerprints.isEmpty()) throw IllegalStateException("TLS_TRUST_REQUIRED: https 端点必须配置证书指纹")
            connection.sslSocketFactory = pinnedContext(fingerprints).socketFactory
        }
        val status = (connection as java.net.HttpURLConnection).responseCode
        val stream = if (status in 200..299) connection.inputStream else connection.errorStream
        val body = stream?.bufferedReader()?.readText().orEmpty()
        if (status !in 200..299) throw IllegalStateException("HTTP " + status)
        return status to body
    }

    private fun pinnedContext(fingerprints: List<String>): SSLContext {
        val expected = fingerprints.map { it.trim().lowercase() }.filter { it.isNotEmpty() }.toSet()
        val trust = object : X509TrustManager {
            override fun checkClientTrusted(chain: Array<X509Certificate>, authType: String) =
                throw CertificateException("client auth not supported")

            override fun checkServerTrusted(chain: Array<X509Certificate>, authType: String) {
                val actual = sha256Hex(chain.first())
                if (actual !in expected) throw CertificateException("PIN_MISMATCH: " + actual)
            }

            override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
        }
        val context = SSLContext.getInstance("TLS")
        context.init(null, arrayOf<TrustManager>(trust), SecureRandom())
        return context
    }

    private fun sha256Hex(certificate: X509Certificate): String =
        MessageDigest.getInstance("SHA-256").digest(certificate.encoded)
            .joinToString("") { b -> String.format("%02x", b.toInt() and 0xff) }
}

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
 * 步骤（每一步都是**真实网络请求**）：
 * 1. `/v1/health` 可达且 200；
 * 2. 返回的 `node_id` 与配置一致（防止连错节点）；
 * 3. https 时**证书指纹必须匹配**（自定义 TrustManager，不全信任）；
 * 4. `/v1/tls/info` 能读到剩余天数（节点未启用 TLS 时这一步跳过并说明）；
 * 5. 往返耗时（毫秒）。
 *
 * 语义边界：不做 ICMP/TCP 探测（应用层只需知道“HTTP 是否可用”）；失败一律给出稳定原因文本，不静默。
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

        val healthBody = runCatching {
            request(base + "/v1/health", isHttps, fingerprints).second
        }.getOrElse { error ->
            steps += SelfCheckStep("health", false, error.message ?: error::class.simpleName ?: "请求失败")
            return steps
        }
        steps += SelfCheckStep("health", true, "HTTP 200")

        val nodeId = Regex("\"(?:node_id|nodeId)\"\\s*:\\s*\"([^\"]+)\"").find(healthBody)?.groupValues?.get(1)
        steps += when {
            nodeId == null -> SelfCheckStep("node_id", false, "响应里没有 node_id")
            expectedNodeId.isBlank() -> SelfCheckStep("node_id", true, "实际 " + nodeId + "（未配置期望值，跳过比对）")
            nodeId == expectedNodeId -> SelfCheckStep("node_id", true, "与配置一致：" + nodeId)
            else -> SelfCheckStep("node_id", false, "期望 " + expectedNodeId + "，实际 " + nodeId + "（连错节点？）")
        }

        if (isHttps) {
            val pinned = runCatching { request(base + "/v1/health", true, fingerprints) }.isSuccess
            steps += if (fingerprints.isEmpty()) {
                SelfCheckStep("tls_pinning", false, "https 端点未配置指纹（不能校验证书）")
            } else {
                SelfCheckStep("tls_pinning", pinned, if (pinned) "证书指纹匹配（" + fingerprints.size + " 张）" else "证书指纹不匹配或握手失败")
            }
        } else {
            steps += SelfCheckStep("tls_pinning", true, "明文 HTTP（不做 TLS 校验）")
        }

        val tlsBody = runCatching { request(base + "/v1/tls/info", isHttps, fingerprints).first to request(base + "/v1/tls/info", isHttps, fingerprints).second }.getOrNull()
        if (isHttps) {
            if (tlsBody == null) {
                steps += SelfCheckStep("tls_info", true, "读不到 /v1/tls/info（不影响连通，只是看不到剩余天数）")
            } else {
                val days = Regex("\"days_remaining\"\\s*:\\s*(-?\\d+)").find(tlsBody.second)?.groupValues?.get(1)?.toIntOrNull()
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

    /** 发一个 GET，返回 (状态码, 响应体)。https 时必须给指纹，且只接受匹配的那张证书。 */
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
                val der = chain.first().encoded
                val actual = MessageDigest.getInstance("SHA-256").digest(der)
                    .joinToString("") { b -> String.format("%02x", b.toInt() and 0xff) }
                if (actual !in expected) throw CertificateException("PIN_MISMATCH: " + actual)
            }

            override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
        }
        val context = SSLContext.getInstance("TLS")
        context.init(null, arrayOf<TrustManager>(trust), SecureRandom())
        return context
    }
}

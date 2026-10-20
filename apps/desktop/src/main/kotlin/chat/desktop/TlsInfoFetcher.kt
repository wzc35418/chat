package chat.desktop

import java.security.MessageDigest
import java.security.SecureRandom
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

/** 证书状态（来自节点 `/v1/tls/info`，公开信息）。 */
data class TlsCertificateInfo(
    val daysRemaining: Int,
    val expired: Boolean,
    val warning: Boolean,
    val certSha256: String,
)

/**
 * 读取节点公开的证书状态。
 *
 * 语义边界：
 * - https 端点**必须**给指纹，并使用只接受该指纹的 `X509TrustManager`（**绝不全信任**）；
 * - 节点未启用 TLS → 该端点 404，这里返回 null 并说明；
 * - 解析失败一律抛错，不返回“看起来正常”的默认值。
 */
object TlsInfoFetcher {

    fun fetch(endpointUrl: String, fingerprints: List<String>): TlsCertificateInfo? {
        val base = endpointUrl.trim().trimEnd('/')
        if (base.isEmpty()) throw IllegalArgumentException("endpoint is empty")
        val url = java.net.URI(base + "/v1/tls/info").toURL()
        val isHttps = base.startsWith("https://", ignoreCase = true)
        if (isHttps && fingerprints.isEmpty()) {
            throw IllegalStateException("TLS_TRUST_REQUIRED: https 端点必须配置指纹")
        }
        val connection = url.openConnection()
        if (connection is HttpsURLConnection) {
            connection.sslSocketFactory = pinnedContext(fingerprints).socketFactory
        }
        connection.connectTimeout = 5000
        connection.readTimeout = 5000
        connection.setRequestProperty("accept", "application/json")
        val status = try {
            (connection as java.net.HttpURLConnection).responseCode
        } catch (error: Throwable) {
            throw IllegalStateException("TLS_INFO_UNAVAILABLE: " + (error.message ?: error::class.simpleName))
        }
        if (status == 404) return null
        if (status != 200) throw IllegalStateException("TLS_INFO_UNAVAILABLE: http " + status)
        val body = connection.inputStream.bufferedReader().readText()
        val days = Regex("\"days_remaining\"\\s*:\\s*(-?\\d+)").find(body)?.groupValues?.get(1)?.toIntOrNull()
            ?: throw IllegalStateException("TLS_INFO_UNAVAILABLE: response has no days_remaining")
        val sha = Regex("\"cert_sha256\"\\s*:\\s*\"([0-9a-f]{64})\"").find(body)?.groupValues?.get(1) ?: ""
        return TlsCertificateInfo(
            daysRemaining = days,
            expired = days <= 0,
            warning = days in 1..29,
            certSha256 = sha,
        )
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

package chat.presentation.display

/** 节点地址的「可达性类型」——用来在配错地址时**说清楚原因**，而不是只报“连不上”。 */
enum class EndpointScope {
    /** 127.0.0.0/8、::1、localhost：只在同一台机器可用。 */
    Loopback,

    /** 10/8、172.16/12、192.168/16：内网地址，跨机器需要端口转发或中继。 */
    PrivateNet,

    /** 100.64.0.0/10：运营商级 NAT（CGNAT），公网**不可直连**。 */
    Cgnat,

    /** 其它 IPv4/IPv6：看起来是公网地址。 */
    Public,

    /** 域名：无法离线判断解析结果。 */
    Hostname,
}

/**
 * 判断节点地址属于哪类网络。
 *
 * 语义边界：**只按字面 IP 判断**，不做 DNS 解析（离线环境无法可靠解析，也不该在 UI 线程阻塞）。
 * 域名一律当作 [EndpointScope.Hostname]——它可能是公网，也可能是内网域名，交给“连不上”的实际结果判断。
 */
fun classifyEndpointHost(host: String): EndpointScope {
    val trimmed = host.trim().trim('[', ']').lowercase()
    if (trimmed.isEmpty()) return EndpointScope.Hostname
    if (trimmed == "localhost") return EndpointScope.Loopback
    if (trimmed == "::1") return EndpointScope.Loopback
    val octets = parseIpv4(trimmed)
    if (octets == null) return EndpointScope.Hostname
    val first = octets[0]
    val second = octets[1]
    return when {
        first == 127 -> EndpointScope.Loopback
        first == 10 -> EndpointScope.PrivateNet
        first == 172 && second in 16..31 -> EndpointScope.PrivateNet
        first == 192 && second == 168 -> EndpointScope.PrivateNet
        first == 169 && second == 254 -> EndpointScope.PrivateNet
        first == 100 && second in 64..127 -> EndpointScope.Cgnat
        else -> EndpointScope.Public
    }
}

/**
 * 给出**可执行的**说明；null 表示“不用提示”（公网地址）。
 *
 * 这些提示存在的意义：当前版本**只支持直连**，没有 NAT 打洞/中继；
 * 与其让用户对着超时发呆，不如直接告诉他这种地址为什么不行、要怎么办。
 */
fun endpointScopeGuidance(scope: EndpointScope): String? = when (scope) {
    EndpointScope.Public -> null
    EndpointScope.Hostname -> null
    EndpointScope.Loopback ->
        "这是本机回环地址，只能连本机上的节点；要让其它机器访问，请填对方的公网 IP 或局域网 IP。"
    EndpointScope.PrivateNet ->
        "这是内网地址：只有同一局域网能直连。跨网络需要端口转发到公网，或等中继功能（当前版本只支持直连）。"
    EndpointScope.Cgnat ->
        "这是运营商级 NAT（100.64/10）地址，公网无法直连；需要公网 IP、端口转发，或等中继功能。"
}

private fun parseIpv4(value: String): IntArray? {
    val parts = value.split('.')
    if (parts.size != 4) return null
    val octets = IntArray(4)
    for (index in parts.indices) {
        val part = parts[index]
        if (part.isEmpty() || part.length > 3) return null
        val number = part.toIntOrNull() ?: return null
        if (number !in 0..255) return null
        octets[index] = number
    }
    return octets
}

/** 从 `http(s)://host:port/path` 里取 host（解析失败返回空串）。 */
fun endpointHost(url: String): String {
    val withoutScheme = url.trim().removePrefix("http://").removePrefix("https://")
    val hostPort = withoutScheme.substringBefore('/').substringBefore('?')
    return if (hostPort.startsWith("[")) {
        hostPort.substringAfter('[').substringBefore(']')
    } else {
        hostPort.substringBefore(':')
    }
}

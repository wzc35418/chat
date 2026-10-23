package chat.desktop

/**
 * `DiagnoseHints` 的自检：用**构造出来的**自检结果断言建议映射，不需要网络。
 * 退出码：0 = PASS，1 = FAIL。
 */
fun main() {
    var failures = 0
    fun check(name: String, condition: Boolean, detail: String = "") {
        if (condition) {
            println("PASS " + name)
        } else {
            println("FAIL " + name + (if (detail.isBlank()) "" else "：" + detail))
            failures += 1
        }
    }

    // 1) 指纹未配置 → 提示去填
    val unset = DiagnoseHints.suggest(listOf(SelfCheckStep("cert_fingerprint", true, "服务端证书 13be2f93…（本地未配置指纹）")))
    check("未配置指纹时给出“去填指纹”建议", unset.any { it.contains("fingerprint.txt") }, unset.toString())

    // 2) 指纹不一致 → 提示核对/轮换
    val mismatch = DiagnoseHints.suggest(listOf(SelfCheckStep("cert_fingerprint", false, "配置 aaaa 与服务端 bbbb 不一致（换过证书，或指纹抄错）")))
    check("指纹不一致时给出核对建议", mismatch.any { it.contains("换过证书") }, mismatch.toString())

    // 3) health 失败 → 提示查进程/防火墙
    val down = DiagnoseHints.suggest(listOf(SelfCheckStep("health", false, "连接被拒绝")))
    check("health 失败时给出进程/防火墙建议", down.any { it.contains("8443") }, down.toString())

    // 4) node_id 不一致
    val badNode = DiagnoseHints.suggest(listOf(SelfCheckStep("node_id", false, "期望 node-public，实际 server")))
    check("node_id 不一致时提示改设置", badNode.any { it.contains("node_id") }, badNode.toString())

    // 5) 全绿 → 不给任何建议（避免噪音）
    val allOk = DiagnoseHints.suggest(listOf(
        SelfCheckStep("cert_fingerprint", true, "服务端证书 = 配置指纹（13be2f93…）"),
        SelfCheckStep("health", true, "HTTP 200"),
        SelfCheckStep("node_id", true, "与配置一致：node-public"),
        SelfCheckStep("latency", true, "10297 ms"),
        SelfCheckStep("identity", true, "本地身份库已打开"),
        SelfCheckStep("group", true, "已绑定群"),
        SelfCheckStep("store", true, "本地消息库已打开"),
    ))
    check("全绿时不产生建议", allOk.isEmpty(), allOk.toString())

    // 6) 未知失败模式不瞎猜（返回空而不是假建议）
    val unknown = DiagnoseHints.suggest(listOf(SelfCheckStep("some_new_step", false, "某种未知错误")))
    check("未知失败模式不给假建议", unknown.isEmpty(), unknown.toString())

    if (failures == 0) {
        println("DIAGNOSE HINTS: PASS")
        kotlin.system.exitProcess(0)
    }
    println("DIAGNOSE HINTS: FAIL（" + failures + " 项）")
    kotlin.system.exitProcess(1)
}

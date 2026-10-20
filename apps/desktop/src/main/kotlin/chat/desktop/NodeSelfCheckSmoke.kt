package chat.desktop

/**
 * 节点自检冒烟：对一个**真实运行中的节点**跑 `NodeSelfCheck`，把每步结果打印出来。
 *
 * 用法：
 *   java ... chat.desktop.NodeSelfCheckSmokeKt <endpoint> <nodeId> [fingerprintHex|'-']
 *
 * 退出码：0 = 所有步骤通过；1 = 有步骤失败；2 = 参数不足。
 */
fun main(args: Array<String>) {
    // 参数可以给命令行，也可以用环境变量（Gradle 任务里传参容易被引号搞坏）。
    val endpoint = args.getOrNull(0) ?: System.getenv("SELFCHECK_ENDPOINT") ?: ""
    val nodeId = args.getOrNull(1) ?: System.getenv("SELFCHECK_NODE_ID") ?: ""
    val fingerprintRaw = args.getOrNull(2) ?: System.getenv("SELFCHECK_FP") ?: ""
    if (endpoint.isBlank() || nodeId.isBlank()) {
        println("usage: NodeSelfCheckSmokeKt <endpoint> <nodeId> [fingerprintHex|-]  (or SELFCHECK_ENDPOINT/SELFCHECK_NODE_ID/SELFCHECK_FP)")
        kotlin.system.exitProcess(2)
    }
    val fingerprint = if (fingerprintRaw.trim() == "-") "" else fingerprintRaw.trim()
    val fingerprints = if (fingerprint.isEmpty()) emptyList() else listOf(fingerprint)
    val steps = NodeSelfCheck.run(endpoint, fingerprints, nodeId)
    var failures = 0
    steps.forEach { step ->
        val mark = if (step.ok) "PASS" else "FAIL"
        if (!step.ok) failures++
        println(mark + " selfcheck " + step.name + ": " + step.detail)
    }
    if (failures == 0) {
        println("PASS node self-check: " + steps.size + " steps")
        kotlin.system.exitProcess(0)
    } else {
        println("FAIL node self-check: " + failures + " step(s) failed")
        kotlin.system.exitProcess(1)
    }
}

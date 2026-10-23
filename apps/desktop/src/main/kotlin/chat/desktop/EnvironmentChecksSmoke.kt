package chat.desktop

import java.io.File
import java.nio.file.Files

/**
 * `EnvironmentChecks` 的冒烟：检查项的形状与判定必须可复现（含一条**故意不可达**的负例）。
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

    val dir = Files.createTempDirectory("env-checks-").toFile()
    // 正例：本机应当剪贴板可用、目录可写、磁盘充足；端口用本机不存在的端口做负例。
    val checks = EnvironmentChecks.run(dir, "https://127.0.0.1:59999", 800)
    val names = checks.map { it.name }
    check("包含 clipboard/data_dir/disk_space/node_port 四项", names.containsAll(listOf("clipboard", "data_dir", "disk_space", "node_port")), names.toString())
    check("数据目录可写", checks.first { it.name == "data_dir" }.ok)
    check("磁盘余量 >= 100MB", checks.first { it.name == "disk_space" }.ok, checks.first { it.name == "disk_space" }.detail)
    check("**不可达端口被判定为失败**", !checks.first { it.name == "node_port" }.ok, checks.first { it.name == "node_port" }.detail)

    // 剪贴板在无桌面会话时可能不可用：这里只要求“有结论”，不强行要求 true。
    val clipboard = checks.first { it.name == "clipboard" }
    check("剪贴板检查给出明确结论", clipboard.detail.isNotBlank(), clipboard.detail)

    // 地址不可解析时也要给失败结论，而不是抛异常。
    val bad = EnvironmentChecks.run(dir, "not-a-url", 500)
    check("非法地址给出失败结论", bad.first { it.name == "node_port" }.ok == false, bad.first { it.name == "node_port" }.detail)

    if (failures == 0) {
        println("ENV CHECKS: PASS")
        kotlin.system.exitProcess(0)
    }
    println("ENV CHECKS: FAIL（" + failures + " 项）")
    kotlin.system.exitProcess(1)
}

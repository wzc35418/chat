package chat.desktop

/**
 * 把自检结果翻译成**可执行的修复建议**（纯函数，无网络、可单测）。
 *
 * 设计原则：只对**已知失败模式**给建议；未知失败不瞎猜，明说“看报告里的原因”。
 */
object DiagnoseHints {
    fun suggest(steps: List<SelfCheckStep>): List<String> {
        val hints = mutableListOf<String>()
        for (step in steps) {
            when (step.name) {
                "cert_fingerprint" -> {
                    if (step.ok) {
                        if (step.detail.contains("未配置指纹")) {
                            hints += "建议：把服务端指纹填进「设置」——文件在服务器 C:\\chat-node\\fingerprint.txt（或 tools\\inspect-node.ps1 打印）"
                        }
                    } else if (step.detail.contains("不一致")) {
                        hints += "建议：服务端换过证书。核对 fingerprint.txt；确认无误后把新指纹填进「设置」（可同时保留新旧指纹便于轮换）"
                    } else {
                        hints += "建议：TLS 握手失败。先确认端口通（Test-NetConnection 主机 8443），再看服务端 cert.pem/key.pem 是否匹配"
                    }
                }
                "health" -> if (!step.ok) {
                    hints += "建议：节点没有正常应答。服务器上确认 chat-node 进程在跑（deploy\\inspect-node.ps1 -Local），防火墙放行 8443"
                }
                "node_id" -> if (!step.ok) {
                    hints += "建议：「设置」里的 node_id 与节点不一致。用 deploy\\inspect-node.ps1 看真实 node_id（通常是 node-public），改完保存"
                }
                "tls_info" -> if (step.detail.contains("已过期")) {
                    hints += "建议：证书过期。服务器上跑 deploy\\rotate-certificate.ps1，然后把新指纹填进「设置」"
                }
                "latency" -> if (!step.ok) {
                    hints += "建议：延迟高多半是跨洲网络，不是故障；如果长时间 > 20s，检查服务器出口带宽"
                }
                "identity" -> if (!step.ok) {
                    hints += "建议：身份库打不开。先确认没有其它 Chat 实例在跑；仍失败则看报告里的原始原因（可能需要改名旧库）"
                }
                "group" -> if (!step.ok) {
                    hints += "建议：还没有可用的群。点左下角「新对话」建一个（会自动登记到节点）"
                }
                "store" -> if (!step.ok) {
                    hints += "建议：消息库被占用或未打开。关掉其它 Chat 实例再试"
                }
            }
        }
        return hints.distinct()
    }
}

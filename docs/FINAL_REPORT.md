# 项目最终报告（120 轮后）

生成时间：2026-10-21（第 114 轮）

## 一、这是什么

一个**自托管端到端加密聊天系统**：Rust 内核（MLS/OpenMLS）+ 自建节点 + Kotlin Multiplatform 客户端（Compose Desktop 为主）。

## 二、现在能做什么（有证据）

| 能力 | 证据 |
| --- | --- |
| 建身份、加密落库 | `identityPersistSmoke`、storage 迁移 0001–0014 |
| 建群、群治理（改名/禁言/移除/接受） | `groupSmoke` + `presentationChecks` |
| 发消息经真实节点并被接受 | `verify-package-e2e`：`sendDispatch accepted` |
| 从节点拉回消息落库 | 同上：`syncOnce fetched=2 applied=1` |
| **两台设备互通** | `demo-two-devices`：A/B 全链路 PASS |
| **对方离线也能送达** | `verify-offline-delivery`：四进程顺序，`applied=1` |
| **不依赖回环的传输** | `verify-lan`：`192.168.x` 上 accepted + applied |
| **https + 严格 pinning** | `verify-tls`（节点侧 6 条）+ `verify-tls-e2e`（打包产物正/负例） |
| **证书到期可见** | 节点 `/v1/tls/info` + 关于面板「节点证书」 |
| 插件：权限门 + 签名 + 信任列表 | `pluginJvmSmoke`（签名/篡改/信任列表） |
| 免 JDK 运行 | `Chat.exe`（jlink+jpackage）+ 包内冒烟 + GUI 存活 |
| Markdown 渲染/输入、选区格式化、界面缩放 | `presentationChecks` + 手工指南 |

## 三、怎么验证（一条命令）

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -ExecutionPolicy Bypass -File tools\verify-all.ps1     # 11 个阶段
```

手工验收：`docs/manual-test-guide.md`（20 节）+ `docs/ACCEPTANCE_0.1.0.md`。

## 四、**不能**做什么（诚实清单）

1. **跨机器 / NAT 穿透 / 中继服务**：所有证据都是同一台机器（LAN 地址也不算跨主机）。要真跨机器需要：可达节点（公网或 VPN）+ TLS 指纹分发；我们没做 NAT 打洞，也没有中继。
2. **非 Windows 平台**：TLS/凭据库/DPAPI 都是 Windows 实现；macOS/Linux 走口令派生但未验证。
3. **插件沙箱**：权限门 + 签名是真的，但**JVM 内不是安全边界**（反射/Unsafe/本地库可绕过），超时也无法强杀线程。
4. **证书生命周期自动化**：能检测到期、能轮换（指纹集合），但**没有**自动续签、没有 OCSP/CRL、没有到期自动告警横幅。
5. **生产运维**：没有安装器（MSI 需联网下载 WiX）、没有自动更新、没有 soak 测试、没有指标/告警面板。
6. **后台通知**：只有应用内提示，没有系统通知/托盘常驻。

## 五、关键设计选择（为什么这样做）

| 选择 | 理由 |
| --- | --- |
| 严格 pinning（只认 SHA-256 指纹，系统根不参与） | 自托管场景没有 CA 链；固定证书比信任一堆根更可审计 |
| 指纹用**集合** | 换证书时新旧并存，避免“一次全换否则全断” |
| KeyPackage 私钥加密落库 | 否则进程重启就无法入群（这曾是硬限制） |
| 插件默认要求签名 | 未签名插件等于“谁都能替换字节”；宁可默认拒绝 |
| 错误信息分层（码/说明/原因/建议） | 用户遇到 TLS 失败时，只看到“连不上”无法自救 |
| 诚实降级：没有 LAN 地址时 `exit 3 = SKIP` | 不把“没测到”伪装成“通过” |

## 六、规模

- Rust：14 个 crate，工作区测试约 **1160** 项；已单独核对的：`chat-ffi` **124+**、`chat-node-client` **8 套件**（含 7 例 TLS）、`chat-node-services` 含 `cert_validity` 3 例。
- Kotlin：Compose 桌面 + Android（`assembleDebug` 通过），13 个桌面冒烟任务。
- 迁移：storage 0001–0014、node-services 0001–0009。
- 验证脚本：`verify-all`（11 阶段）、`verify-package`、`verify-package-e2e`、`verify-lan`、`verify-tls`、`verify-tls-e2e`、`verify-offline-delivery`、`demo-two-devices`。

## 七、下一阶段建议（按价值排序）

1. **跨机器联调**（唯一外部依赖项）：拿到可达节点后，把现有脚本的 endpoint 换成公网地址即可复用全部断言。
2. **证书生命周期**：自动续签 + 到期告警横幅 + 信任列表管理页（现在只有 CLI/面板单点操作）。
3. **`AppFacade` 深层收敛**：把各面板各自调 service 的重复收掉（技术债，影响后续迭代速度）。
4. **生产化**：安装器/自动更新、后台通知、soak、指标。

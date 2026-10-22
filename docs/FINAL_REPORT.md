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

## 八、怎么验收（10 分钟）

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -ExecutionPolicy Bypass -File tools\verify-all.ps1      # 11 阶段，期望 overall: PASS
```

手工检查点（细节见 `docs/manual-test-guide.md`）：

1. 双击 `build\dist\app-image\Chat\Chat.exe` → 能启动、不弹引导（已配过则不再弹）；
2. 「本机存储」发一条消息 → 状态 `queued → prepared → accepted`（节点在线时）；点「收一轮」→ 消息入库；
3. 「关于」→「传输安全」填节点证书指纹 → 模式变 `pinned N 张证书`；「节点证书」自动显示 `剩余 N 天`；
4. 把指纹改错 → 发消息**必须失败**（这是 pinning 真的在生效的证据）。

## 九、下次接手从哪开始（5 步）

1. **跨机器联调**（唯一外部依赖）：`deploy/install-node.sh`（服务器一键部署 + 打印指纹）与 `tools/verify-remote.ps1`（本机一键验证）已备好，拿到公网 endpoint/node_id/指纹后直接跑。历史方案：把 `tools/verify-lan.ps1` 与 `tools/verify-tls-e2e.ps1` 里的 endpoint 换成该地址即可复用全部断言；同时确认节点 `--bind` 与防火墙。
2. **证书生命周期自动化**：把「启动读一次」升级为定时刷新 + 到期弹窗；补自动续签与信任列表导入/导出/吊销。
3. **`AppFacade` 深层收敛**：目前只收敛了失败投影与错误展示；面板仍各自调 service（40+ 处理器），收敛后迭代速度会明显提升。
4. **生产化**：安装器（MSI 需联网取 WiX）、后台通知、soak 测试、指标/告警。
5. **多平台**：macOS 钥匙串、Linux Secret Service、路径与文件权限（当前只有 Windows 实现）。

## 十、交接提示（容易踩的坑）

- 改 Rust FFI 后**必须** `gradlew :prepareRustHost --rerun-tasks`，并确认 `build/generated/chat-ffi/uniffi/chat_ffi/chat_ffi.kt` 里出现新符号（否则 Kotlin 侧根本看不到）；
- uniffi 绑定是从 **`target/debug/chat_ffi.dll`** 生成的：如果你用 `CARGO_TARGET_DIR` 构建，记得把 dll 同步过去，否则绑定是旧的；
- 脚本里给 `java.exe` 传空字符串参数会被丢弃，用 `-` 占位；
- PowerShell 5.1 没有 `Invoke-WebRequest -SkipCertificateCheck`，TLS 检查统一用 `openssl s_client`；
- 冻结码表（`FROZEN_AUTO_RETRY_CODES`）决定“能不能重试”，不要凭记忆判断（`STORAGE_LOCKED` 在表里）。

## 十一、最终状态（第 140 轮，2026-10-21）

| 口径 | 数值 | 依据 |
| --- | --- | --- |
| **A：功能闭环** | **100%** | 12 阶段 `verify-all` 全 PASS，其中 `remote` 阶段 7 条断言直连公网节点（证书 pin / 节点身份 / 打包产物 accepted+applied / UI 一步发送 / 错误指纹被拒） |
| **B：生产就绪** | **≈ 82%** | 缺 NAT 打洞与中继、证书自动续签、MSI 安装器、后台通知、非 Windows 平台、soak 数据 |
| 可复跑性 | 每条结论都有命令 | `docs/DELIVERY_CHECKLIST.md` 第二节 |

### 这个项目现在能做什么（一句话）

> 在两台机器之间、用自签证书 + SHA-256 指纹固定的 TLS，把消息加密后提交到自建节点并拉回，且客户端不会因为指纹配错而误连。

### 不能说什么（诚实边界）

- ❌ **不能说“生产可用”** —— 没有安装器、没有后台通知、没有 soak、没有非 Windows 验证；
- ❌ **不能说“穿透 NAT”** —— 但要说准确：**消息路径走自建 HTTP 节点**（`https://节点/v1/...`），因此当前架构不依赖 NAT 打洞；
  P2P 传输层（iroh 1.3.0）已具备**显式中继配置**（`EndpointConfig::with_relay_urls`，https 任意主机 / http 仅回环 / 去重保序 / 拒绝凭据与片段）并通过 **78 个 transport 测试**，
  但**尚未接入消息路径**，也没有暴露给用户配置——所以“穿透 NAT”这句话现在不能说，理由不是没代码，而是没接线、没实测。
- ❌ **不能说“证书自动轮换”** —— 只有告警，轮换要人工；
- ❌ **不能说“已经端到端加密到联系人级别”** —— 群消息走 MLS，但多设备/多联系人的完整社交场景未验证。

### 下次接手第一步

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -ExecutionPolicy Bypass -File tools\verify-all.ps1 `
  -RemoteEndpoint https://106.54.223.171:8443 -RemoteNodeId node-public `
  -RemoteFingerprint 13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053
```

先确认 12 阶段仍然全绿，再动代码；节点若已关停，把 `-RemoteEndpoint` 去掉即可（`remote` 会显式 SKIP，不算通过）。

## 十二、第 152 轮最终状态（2026-10-23）

| 口径 | 数值 | 依据 |
| --- | --- | --- |
| **A：功能闭环** | **100%** | 12 阶段 `verify-all` 全 PASS；`remote` 阶段 7 条断言直连公网（证书 pin / 节点身份 / 打包产物 accepted+applied / **UI 一步发送** / 错误指纹被拒） |
| **B：生产就绪** | **≈ 85%** | 相比 140 轮增加了：证书轮换与双指纹窗口、运维手册与巡检脚本、soak（冷启动+稳态）、产物新鲜度断言、交付 zip 验证、后台自动同步与托盘通知；仍缺长稳/并发/资源曲线/故障注入、MSI、后台常驻、多平台、`AppFacade` 重写、P2P 中继接线 |

### 这两批次（121–152）新增的可核对能力

| 能力 | 证据 |
| --- | --- |
| 公网跨机器收发 | `remote` 阶段 7 断言 PASS（`106.54.223.171:8443`） |
| 证书轮换 | `deploy/rotate-certificate.ps1` + 双指纹实测（`pinned 2 certificate(s)` → accepted → applied） |
| 节点巡检 | `deploy/inspect-node.ps1` 远程 `INSPECT: HEALTHY`；本机正确报 2 问题 |
| 冷启动 soak | 本机 20 轮 / 公网 10 轮 **100% 成功** |
| 稳态 soak | 本机 **144 ms/条**、公网 4.4 s/条（各 10 条 100% 成功） |
| 交付物可运行 | `tools/verify-zip.ps1` → `ZIP CHECK: PASS`（从解压副本跑冒烟） |
| 防旧产物 | `tools/check-artifact-freshness.ps1` 已接入远程验证与 soak |

### 仍然不能说的话（本轮复核后不变）

- ❌ **不能说我生产可用** —— 缺长稳/并发/故障注入数据、无安装器、无后台常驻、只验证了 Windows；
- ❌ **不能说它穿透 NAT** —— 消息走自建 HTTP 节点；iroh 中继有配置与 78 个测试但未接线；
- ❌ **不能说证书自动轮换** —— 有轮换脚本与到期告警，但要人工执行；
- ❌ **不能说多平台支持** —— 只有 Windows 凭据实现经验证。

### 下次接手第一步（不变）

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -ExecutionPolicy Bypass -File tools\verify-all.ps1 `
  -RemoteEndpoint https://106.54.223.171:8443 -RemoteNodeId node-public `
  -RemoteFingerprint 13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053
```

先确认 12 阶段全绿再动代码。节点若已关停，去掉 `-RemoteEndpoint`（`remote` 会显式 SKIP，不算通过）。

## 十三、第 164 轮最终交代（2026-10-23）

| 口径 | 数值 | 依据 |
| --- | --- | --- |
| **A：功能闭环** | **100%** | 12 阶段 + 公网 7 断言 + 三种故障注入 |
| **B：生产就绪** | **≈ 88%** | 相比 152 轮增加：`unknown` 不再被当成功、门控式自适应预检、存储只读/网络分区/节点死亡三类故障测试、资源采样 |

### 这 12 轮（153–164）最值钱的三件事

1. **修掉“结果未确定被当成成功”** —— 这是唯一一个会导致用户以为发出去了、其实没发出去的产品 bug；
2. **两次自我推翻**：无条件预检在公网实测更差 → 回滚；改成失败窗口自适应 → 健康链路零开销；
3. **三种故障注入全通过**：节点死亡、半死链路、数据库只读，客户端**一律如实失败**。

### 仍然不能说的话（复核后不变）

- ❌ 不能说**生产可用**：无长稳/并发数据、无安装器、无后台常驻、只验证 Windows；
- ❌ 不能说**穿透 NAT**：消息走自建 HTTP 节点，iroh 中继未接线；
- ❌ 不能说**证书自动轮换**：只有脚本与告警；
- ❌ 不能说**多平台支持**：只有 Windows 凭据经验证；
- ❌ 不能说**拉取路径的故障行为已验证**：这是本轮明确留下的空白。

### 六个已知空白（按价值排序）

1. 拉取路径在链路死掉时的语义（缺测试夹具：游标需要登记后才能播种）；
2. 失败窗口的端到端验证（只有单元测试；`CHAT_DOUBLE_TRY` 可观察但无净收益）；
3. 磁盘**写满**（`SQLITE_FULL`）与写入中途失败；
4. 网络**丢包/抖动**（只测全黑障）；
5. 长稳与并发（soak 仍是秒级样本、串行）；
6. P2P 中继接线 / 多平台凭据 / MSI + 签名。

### 下次接手第一步

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -ExecutionPolicy Bypass -File tools\verify-all.ps1 `
  -RemoteEndpoint https://106.54.223.171:8443 -RemoteNodeId node-public `
  -RemoteFingerprint 13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053
```

先确认 12 阶段全绿，再跑 `soak-faults` / `soak-partition` / `soak-disk` 确认故障语义没退化，最后才动代码。
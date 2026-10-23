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
## 十四、第 179 轮交付基线（2026-10-23，最终数字）

| 口径 | 数值 | 依据 |
| --- | --- | --- |
| **A：功能闭环** | **100%** | 12 阶段 + 应用真实链路 + 双人配对 + 3 soak + 3 故障注入，全部 PASS |
| **B：生产就绪** | **≈ 90%** | 相比 164 轮增加：真实界面可用（会话=群/发送/同步/群管理/配对/自检）、`Chat.exe` FFI 修复、出站消息落库、打包原子化 |

### 关键数字（第 177 轮回归，日志 `build/final-regression-177.log`）

| 项目 | 结果 |
| --- | --- |
| `verify-all` 12 阶段（含公网跨机器） | PASS，remote **59.7 s** |
| 应用真实链路（公网） | PASS，`dispatch=accepted settled=true`，`sync applied=1` |
| 双人配对（本地节点） | PASS，Welcome 921 B，两端同 epoch |
| 冷启动 soak 10 轮 | 10/10，p50 **2018 ms**，内存 13.8→14.1 MB |
| 公网稳态 soak 10 条 | 10/10，p50 **3041 ms**，p95 6857 ms |
| 杀节点 / 半死链路 / 库只读 | 全 PASS，零谎报（半死链路 26371 ms 后如实失败） |

**口径**：公网 p50 3–4 s 属跨洲网络正常（新西兰 ↔ 上海），**不作缺陷、不作优化指标**。

### 交付产物（当前哈希）

| 产物 | 路径 | SHA-256 | 备注 |
| --- | --- | --- | --- |
| 免 JDK 启动器 | `build/dist/app-image/Chat/Chat.exe` | `644E16725B6F5E61B693158F25877DB1A7B04827BE27912C12A9B9FC8FE5BD36` | 启动器本身不含业务代码 |
| 业务包 | `build/dist/app-image/Chat/app/app.jar` | `FA29A6EAA0821159F819BC053E8F5237015416C0D2C0B4843E5545FF3E2DCC33` | 界面 + 内核调用 |
| 完整分发包 | `build/dist/Chat-app-0.1.0.zip` | `9BF370CD018396E1B7F951BC7A9E895535F83B1FC7D55ADF371C3B9C1A9405D5` | 82.8 MB |

### 仍然不能说的话（第 179 轮复核）

1. ❌ **不能说“两台真机通过公网配对成功”** —— 配对只在**本地节点**验证过（`PAIRING: PASS`）；两台机器 + 你的公网节点这条路**还没跑过**；
2. ❌ 不能说生产可用：无长稳/并发数据、无安装器、无后台常驻、只验 Windows；
3. ❌ 不能说穿透 NAT：消息走自建 HTTP 节点，iroh 中继未接线；
4. ❌ 不能说证书自动轮换：只有脚本与告警；
5. ❌ 不能说多平台支持：只有 Windows 凭据经验证；
6. ❌ 不能说配对可中断续做：KeyPackage 私钥在内存，重启需重新导出。

### 下次接手第一步

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -ExecutionPolicy Bypass -File tools\verify-all.ps1 `
  -RemoteEndpoint https://106.54.223.171:8443 -RemoteNodeId node-public `
  -RemoteFingerprint 13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053
```

先确认 12 阶段全绿 → 再跑 `verify-app-flow`（应用链路）与 `verify-pairing`（配对）→ 最后才动代码。
## 十五、第 180 轮收尾（目标状态与三条接手路径）

### 目标状态：保持 `active`，不标完成

理由不是“还没写完”，而是**六条能力仍然不成立**（见第十四节）：两台真机通过公网配对未验证、无长稳/并发、无安装器、无后台常驻、不透 NAT、配对不可中断续做。标完成等于宣称“可以交给别人生产用”，而它现在还不能。

### 你现在能做什么（有证据）

1. 用 `Chat.exe` 打开界面，配置节点（地址 + `node-public` + 证书指纹），点「自检」逐项看 PASS/FAIL；
2. 新建会话（= 新建群）、发消息（`prepare → accepted`）、看到自己刚发的消息、30 秒自动同步；
3. 看/改群名、移除成员、接受待处理变更、生成邀请口令；
4. 用「配对」三个按钮：导出 KeyPackage → 用对方 KeyPackage 建群产邀请包 → 用邀请包入群（**本地节点已验证**）；
5. 用 `tools\verify-all.ps1` 等 8 个套件自查（全绿）。

### 你现在不能做什么（不要误用）

1. 不能把它当正式 IM 交给别人用（无安装器/无后台常驻/无长稳压测）；
2. 不能声称已实现“跨公网双人聊天”（配对仅本地节点验证；`two-device` 阶段虽是公网 PASS，但那是脚本 A/B 两端，不是界面配对）；
3. 不能跨平台（只验 Windows）；
4. 不能指望证书自动轮换、托盘常驻、附件发送。

### 三条接手路径

**路径 1：继续开发（推荐）** —— 第一步跑 `tools\verify-all.ps1`（12 阶段，含公网）；接着按 `docs/BATCH_165_180.md` 第六节做：两台机走一次真实配对 → 接附件 → 配对状态持久化。

**路径 2：上线试用（小范围）** —— 只给 2–3 个自己人；先 `register-service.ps1` 把节点注册成开机任务；用 `deploy\inspect-node.ps1 -Local` 做日常巡检；不要对外宣传“加密聊天产品”。

**路径 3：归档** —— 交付物 = `build\dist\Chat-app-0.1.0.zip`（`9BF370CD…405D5`）+ `deploy\node-bundle` + `docs/` 全套；节点密钥与 `seed.hex` 另行备份（丢了群和身份就找不回）。

### 164 → 180 这 16 轮的净变化

| 项 | 164 轮 | 180 轮 |
| --- | --- | --- |
| 默认入口 | P0 实验室界面 | **真实聊天界面**（`--p0` 才进实验室） |
| 界面能力 | 无（预览台） | 会话=群、发消息、自动同步、群管理、配对、自检 |
| 打包产物可用性 | **`Chat.exe` 里 FFI 加载失败**（等于不可用） | 修复 `-Djna.library.path`，冷启动实测可用 |
| 出站消息 | 同步永远拿不回（看不到自己发的） | 发送成功即落库，实测 1 条不重复 |
| 双人 | 无 | KeyPackage → 邀请包 → 入群（本地节点 PASS） |
| B 口径 | ≈ 88% | **≈ 90%** |

### 最后一句

这 180 轮里，最值钱的不是加了多少功能，而是**学会了用真实界面截图和真实错误码来推翻自己的判断**——`UniffiLib` 加载失败、`grp id / conv id` 混用、出站消息不落库，这三个如果只看代码“看起来对”，一个都发现不了。
## 十六、第 196 轮交付定稿（最终数字与产物）

| 口径 | 数值 | 依据 |
| --- | --- | --- |
| **A：功能闭环** | **100%** | 12 阶段 + 应用链路 + 配对 + 诊断 JSON + 3 soak + 3 故障 + 真实库发送，全部 PASS（第 194 轮） |
| **B：生产就绪** | **≈ 90%** | 界面可用、可诊断、可自检；仍缺真机公网配对与长稳/并发 |

### 产物（当前）

| 产物 | 路径 | SHA-256 |
| --- | --- | --- |
| 免 JDK 启动器 | `build/dist/app-image/Chat/Chat.exe` | `644E16725B6F5E61B693158F25877DB1A7B04827BE27912C12A9B9FC8FE5BD36` |
| 业务包 | `build/dist/app-image/Chat/app/app.jar` | `E73B069B7045BEF29B1FD81ACC3BE45F127E1FBCD1E762604D161790ADA3FED7` |
| 完整分发包 | `build/dist/Chat-app-0.1.0.zip` | `699FBFD666C4BE120B3E244DDCD4EBB69E5EDF8DD334A8ADC5441F17C039734B`（82.91 MB） |

### 现在可用的诊断入口（三条）

```powershell
# 1) 人读报告（写 UTF-8 文件到 ~/.chat-desktop/diagnose-<ts>.txt）
build\dist\app-image\Chat\Chat.exe --diagnose
# 2) 机器读 JSON（供脚本断言）
build\dist\app-image\Chat\Chat.exe --diagnose --json
# 3) 自动断言（12 项检查 + allOk 一致性）
powershell -ExecutionPolicy Bypass -File tools\verify-diagnose.ps1
```

### 唯一剩余的硬缺口

**两台真机通过公网配对 → 建群 → 双向发消息**。操作单已写好：`docs/TWO_DEVICE_CHECKLIST.md`（6 步 + 记录表 + 已知会失败的地方）。这一步不需要写代码，只需要第二台设备。
## 十七、第 198 轮复核：六句「不能说的话」逐条验证（不照抄旧结论）

做法：对每句话找**当前代码/日志证据**，能推翻的推翻、该改口径的改口径。

| # | 原说法 | 本轮复核 | 结论 |
| --- | --- | --- | --- |
| 1 | 不能说「两台真机通过公网配对成功」 | `tools/verify-pairing.ps1` 用的是**本地节点**（127.0.0.1:8496）；`verify-all` 的 `two-device` 阶段是**脚本 A/B**（`TwoDeviceSmoke`），`remote` 阶段是**单客户端**跨机器。三处都不等于“两台机器的界面互相配对”。`docs/TWO_DEVICE_CHECKLIST.md` 尚未执行 | **仍成立** |
| 2 | 不能说生产可用 | soak 是**秒级样本**（10 条）；`tools/` 下没有 `*concurrent*` / `*stress*` 脚本；`build/dist` 无 `.msi`；`ChatApp` 里没有 autostart/常驻代码 | **仍成立** |
| 3 | 不能说穿透 NAT | `crates/core` 里 `iroh` 只出现在一行**注释**（`p0 contains real ephemeral MLS/iroh verification`），消息路径是自建 HTTP 节点（`node_submitter` / `attachment_node`），**没有任何 iroh 调用** | **仍成立** |
| 4 | 不能说证书自动轮换 | 全仓 `renew_certificate|autoRenew|auto_renew` 匹配数 = **0**；只有手动 `deploy/rotate-certificate.ps1` 与`OPS_RUNBOOK` 的操作步骤 | **仍成立** |
| 5 | 不能说多平台 | 凭据实现只有 `DpapiKeySlot.kt`（Windows DPAPI）+ `CredentialManagerSmoke.kt`（Windows 凭据管理器）；Android 仅编译通过（`android` 阶段），未跑过运行时 | **仍成立** |
| 6 | 不能说配对可中断续做 | `ChatEnvironment.pairingCore` 是**内存字段**（`private var pairingCore: ChatCoreFfi?`），没有任何落盘；关掉应用即失效 | **仍成立** |

### 复核中需要**改口径**的两处（以前说得不准确）

1. **“没有托盘通知”是错的**：`TrayNotifications.kt` 存在并实现了 `java.awt.SystemTray` 通知，`PlatformSmoke` 与 `PreviewApp` 都在用。准确说法是：**托通知已实现，但正式聊天界面（`ChatApp`）没有使用它**（`ChatApp.kt` 中 `TrayNotifications|Notifications` 匹配数 = 0）。
2. **“没有后台自动同步”也不准确**：`ChatApp` 有 `LaunchedEffect` 每 **30 秒**自动同步（发送后立刻同步一次）。准确说法是：**有前台轮询式自动同步，没有后台常驻/开机自启**（autostart 相关代码匹配数 = 0）。

### 复核结论

- 六句**全部仍然成立**，但其中两句的**理由**要改写：缺的不是“托盘/自动同步的实现”，而是**正式界面没接线 + 没有常驻形态**；
| 这也解释了为什么 B 口径只能到 ≈90%：差的不是功能数量，而是**真机端到端证据**与**交付形态**（安装器/常驻/压测）。
## 十八、第 200 轮最终交代

### 一句话

> 这套系统现在**能被验证**（8 类套件全绿、三条诊断入口、每条结论都能对上一条命令）；但**还不能被别人拿去用**——差一次真机端到端和一种交付形态。

### 目标状态（本轮读到的真实值）

```
goal    goal-f70ea7f4-1af9-41c8-a39c-9643e7cce618
phase   active（未标 complete）
rounds  200 / 200
A 口径  100%（功能闭环，有回归证据）
B 口径  ≈90%（界面可用、可诊断；缺真机端到端与交付形态）
```

不标完成的三条硬理由与改判条件见 [docs/GOAL_STATUS_199.md](GOAL_STATUS_199.md)。

### 你现在该做的第一件事

```powershell
# 1) 确认手上是这一版（顶栏应显示 · R-328）
build\dist\app-image\Chat\Chat.exe
# 2) 看一眼全绿（12 项检查 + 4 项环境检查）
build\dist\app-image\Chat\Chat.exe --diagnose
```

然后按 **[docs/TWO_DEVICE_CHECKLIST.md](TWO_DEVICE_CHECKLIST.md)** 用两台机器跑一遍（6 步、约 15 分钟）。**这是唯一能把 B 口径从 ≈90% 往上推的动作**，而且不需要我写代码。

### 三条接手路径

| 路径 | 适合 | 第一步 | 注意 |
| --- | --- | --- | --- |
| **继续开发** | 想把它做成产品 | `tools\verify-all.ps1`（12 阶段）确认基线全绿 | 先做真机端到端，再谈新功能 |
| **小范围试用** | 给自己人用 | 服务器跑 `register-service.ps1` 注册开机任务；日常 `deploy\inspect-node.ps1 -Local` | 别对外称“加密聊天产品”；节点 `seed.hex` 必须备份 |
| **归档** | 暂时不做 | 保存 `build\dist\Chat-app-0.1.0.zip` + `deploy\node-bundle` + `docs/` | 私钥/种子丢了，群和身份就找不回 |

### 全量证据一览（都能自己跑）

| 套件 | 命令 | 最近结果 |
| --- | --- | --- |
| 12 阶段（含公网） | `tools\verify-all.ps1 -RemoteEndpoint …` | PASS（remote 54.7s） |
| 应用链路 | `tools\verify-app-flow.ps1` | PASS（`dispatch accepted settled=true`） |
| 双人配对（本地节点） | `tools\verify-pairing.ps1` | PASS（两端同 epoch） |
| 诊断自断言 | `tools\verify-diagnose.ps1` | PASS（12 项 0 失败） |
| 真实库发送 | `gradlew :apps:desktop:userDbSmoke -Puser.*` | PASS（`dispatch accepted`） |
| soak 冷/公网 | `tools\soak.ps1 -Iterations 10` / `-Reuse 10` | 10/10（p50 1948ms / 3501ms） |
| 三种故障 | `soak-faults` / `soak-partition` / `soak-disk` | 全 PASS，零谎报 |
| UI 无死按钮 | `node tools\check-ui-dialogs.cjs` | PASS（4 个对话框都有关闭路径） |

### 这 200 轮我最想留下的话

1. **先怀疑工具，再怀疑产品**：一次“卡住”是我脚本里 `openssl s_client` 等 stdin；一次“界面空白”是 Gradle daemon 锁。
2. **验证路径必须等于用户路径**：`java -cp` 全绿，而 `Chat.exe` 里 FFI 根本没加载；`verify-app-flow` 全绿，而应用从未把身份 enroll 进消息库。
3. **优化不要塞进启动路径**：为“切换更快”把群登记挪进 `start()`，换来“会话列表空白”。
4. **失败信息要自带码与原因**：`发送失败（）` 这种空提示，等于没有信息。
5. **做不了的功能，说真话比做假按钮有价值**：“邀请进已有群”内核没有接口，我在界面上写明，而不是放一个点了没反应的按钮。

### 交接清单（最简版）

- 产物：`Chat.exe` `644E1672…5BD36`；`app.jar` `E73B069B…FED7`；zip `699FBFD6…734B`（82.91 MB）
- 节点：`https://106.54.223.171:8443` / `node-public` / 指纹 `13be2f93…a053`（约 3647 天后到期，到期前需 `rotate-certificate.ps1`）
- 文档入口：`docs/DELIVERY_CHECKLIST.md`（一页验收）→ `docs/TWO_DEVICE_CHECKLIST.md`（真机操作单）→ `docs/GOAL_STATUS_199.md`（状态与改判条件）
- 已知缺口：真机公网配对未验 / 无 MSI / 无后台常驻 / 附件未接线 / 加入已有群无接口 / 无长稳并发 / 仅 Windows
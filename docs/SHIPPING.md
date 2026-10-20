# 交付说明（可安装/可运行版本）

版本：**0.1.0**（便携包，无需安装器）

## 产物（两种形态）

### A. 自包含 app-image（推荐交付，不需要 JDK）

```
build/dist/app-image/Chat/Chat.exe     双击即启动（自带裁剪运行时）
build/dist/app-image/Chat/runtime/      jlink 裁剪的 JDK 运行时（java.base/desktop/logging/... 10 个模块）
build/dist/app-image/Chat/app/app.jar   应用
build/dist/app-image/Chat/app/native/   Rust host 动态库
build/dist/Chat-app-0.1.0.zip           上述整个目录的压缩包
```

构建：`gradlew :apps:desktop:appImageZip --offline --max-workers=1`

**已验证**：
- 用包内运行时跑冒烟 → `PASS platform slice`；
- 直接启动 `Chat.exe`，7 秒后进程仍在（窗口起来了）。

### B. 便携包（需要本机 JDK 21）

```
build/dist/Chat-0.1.0.zip          交付压缩包（约 49 MB）
build/dist/Chat-0.1.0/
  app.jar                          全部运行期 classpath（含 shared/* 与 desktop 模块）
  native/chat_ffi.dll              Rust host 动态库（本机库解析的目标）
  run.bat                          启动脚本（java -Djna.library.path=native -jar app.jar）
  README.txt                       运行前提与数据位置
```

## 构建

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
.\gradlew.bat :apps:desktop:portableZip --offline --max-workers=1
# 产物：build/dist/Chat-0.1.0.zip
# 需要换版本号：加 -PchatVersion=0.1.1
```

## 运行前提

1. **JDK 21**（`java -version` 能输出 21.x）；
2. Windows x64；
3. 首次运行会在用户目录下创建加密数据库（口令由密钥槽封装）。

## 已验证（打包产物本身，不是源码构建）

从 `build/dist/Chat-0.1.0/` 用打包出来的 `app.jar` + `native/` 直接运行冒烟：

```powershell
java -Djna.library.path=<dir>\native -cp <dir>\app.jar chat.desktop.PlatformSmokeKt
# 输出: PASS platform slice: notifications honest-unavailable, lifecycle transitions, single exit
```

GUI 启动：双击 `run.bat`（或在同目录执行）。

## 为什么没有 MSI

Compose 的 `nativeDistributions` 在本机离线环境下会在 `downloadWix` 任务上失败（WiX 需要联网下载），
因此交付形态选择**免安装目录 + zip**：不依赖 WiX、不写注册表、删目录即卸载。
需要 MSI 时：装好 WiX 3.14 并允许联网，然后在 `apps/desktop/build.gradle.kts` 的
`nativeDistributions` 里打开 `TargetFormat.Msi`（当前该块被移除，避免离线构建失败）。

## 已知限制（交付时必须告知使用者）

- 仅 Windows x64 验证过；macOS/Linux 打包未做。
- 需要本机 JDK 21（未内置 jlink 运行时）。
- 插件无沙箱（同 JVM 加载执行），`trusted` 只是确认，不是安全边界。
- 传输仅回环/LAN 验证过，未做真实中继/NAT 穿透。
- 系统通知在无托盘环境会明确提示不可用（不是静默失败）。

## 两种形态怎么选

| 场景 | 用哪个 |
|---|---|
| 发给非技术用户试用 | **A. app-image（Chat.exe）** |
| 自己开发调试 / CI 冒烟 | B. 便携包（换 jar 方便） |
| 需要系统级安装、开始菜单、卸载项 | 都需要 MSI（本机离线不可构建，见下） |

## 交付物级回归：tools/verify-package.ps1

打包出来的 app-image（`build/dist/app-image/Chat`）到底还能不能用？一条命令回答：

```powershell
powershell -File tools\verify-package.ps1                 # 构建 + 结构断言 + 四个冒烟
powershell -File tools\verify-package.ps1 -SkipBuild      # 只验已有产物（秒级）
powershell -File tools\verify-package.ps1 -SkipBuild -Only PlatformSmoke,P0Smoke
```

阶段：

1. **build**：`gradlew.bat :apps:desktop:appImageWithNative --offline --max-workers=1`（`-SkipBuild` 可跳过）；
2. **structure**：`Chat.exe`、`app/app.jar`、`app/native/chat_ffi.dll`、`runtime/bin/java.exe` 必须存在（缺哪个报哪个）；
3. **四个冒烟**：用**包内的** `runtime/bin/java.exe` 跑 `app/app.jar` 里的 `chat.desktop.{PlatformSmokeKt,P0SmokeKt,LocalStoreSmokeKt,IdentitySmokeKt}`，
   各自独立临时工作目录、`-Djna.library.path=<Chat>/app/native`、**180s 超时**；
4. **汇总**：控制台 PASS/FAIL 表 + 每步耗时；报告 `build/verify-package-<ts>.md`，日志 `build/verify-package-logs/<ts>/<stage>.log`。

约定：任一步失败**不中断**其余步骤，最终退出码非 0；输出**纯 ASCII**（PowerShell 5.1 中文代码页会破坏 UTF-8）。

实测（2026-10-20）：`-SkipBuild` → structure PASS + 四个冒烟全 PASS，**overall PASS / 退出码 0**。
构建阶段当日因并行改动 `shared/ui/.../PreviewChatScreen.kt`（`onOpenAbout` 参数不存在）失败——**脚本如实报告，没有放宽断言**。

## 交付物回归接入总回归 + GUI 启动存活检查（2026-10-20）

### 一条命令覆盖到交付物

`tools/verify-all.ps1` 新增 **package** 阶段（默认开启，`-SkipPackage` 可跳过）：内部调用
`tools/verify-package.ps1 -SkipBuild`，并把它的 PASS/FAIL 与**报告路径**并入总表（`detail` 列）。
失败不中断其它阶段；任一步失败整体退出码非 0。

### GUI 启动存活检查

新增 `tools/check-gui-launch.ps1`（`verify-package` 的第 5 步，`-SkipGui` 可跳过）：

1. 先清残留 `Chat` 进程（幂等，避免旧进程假装 PASS）；
2. 启动 `build/dist/app-image/Chat/Chat.exe`；
3. **等 8 秒**后断言进程仍在；在则 PASS 并强制结束，退出则 FAIL 并打印退出码；
4. **无交互桌面会话**（`UserInteractive=false` 或 `SESSIONNAME` 为空/`Services`）→ 输出 **SKIP** 并说明，绝不误判成 PASS/FAIL。

退出码约定：`0` PASS、`1` FAIL（含启动失败与提前退出）、`3` SKIP（无桌面）。

为什么需要它：jar 冒烟只能证明“类能跑”，证明不了**打包后的 exe 能起来并活着**（缺少 DLL/runtime 镜像/错误的 JVM 参数都会在这里暴露）。

### 实测（2026-10-20）

`verify-all -SkipAndroid -SkipGradle` → `disk/rust-check/verify-project` PASS、`package` PASS（detail 给出 verify-package 报告路径）、**overall PASS、退出码 0**；
`verify-package -SkipBuild` → structure + 四个冒烟 + `gui PASS (8.7s)`、**overall PASS、退出码 0**。


## 0.1.0 已知限制（交付时必须告知）

| 限制 | 影响 | 现状 |
| --- | --- | --- |
| 插件无沙箱 | 同 JVM 加载执行；trusted 只是确认，不是安全边界 | 未做 |
| 传输仅回环/LAN | 无真实中继/NAT 穿透 | 未做 |
| 无 MSI 安装器 | 只有 app-image（免安装目录） | 离线无法构建 WiX |
| 双设备互发（脚本级已通） | 界面缺「加对方为节点成员」按钮，需走脚本/API | 脚本 PASS |
| KeyPackage 私钥内存态 | 重开身份对象需重新导出 | 未做落库 |
| 无托盘环境无系统通知 | 已明确提示不可用并回退应用内提示 | 诚实降级 |

### 关于「双设备」的准确说法

- 单机闭环已自动化验证：tools/verify-package-e2e.ps1 证明打包产物能提交到节点并被 accepted，也能 syncOnce 拉回条目（fetched=2 applied=1）。
- 双设备（A 与 B）：tools/demo-two-devices.ps1 已能完成 建群 -> 登记节点 -> 把对方加为节点成员（seq=1） -> 对方凭 Welcome 入群；但两处节点规则尚未打通：
  1. 邀请候选（application + control_metadata）被节点判 INVALID_CANDIDATE；
  2. 非建群方读群日志仍 PERMISSION_DENIED。
- 因此目前不建议对外宣称 两台设备可直接互发；可行的手工路径是用 .kp 文件加节点投递（邀请体可离线传递），或等上述两条规则修好。

## 0.1.0 发布基线（2026-10-20 冻结）

| 项 | 值 |
| --- | --- |
| commit | `78d079a` |
| Rust 测试 | **1141 passed / 0 failed** |
| Gradle | 11 桌面任务 + Android `assembleDebug` 全 PASS |
| 一键回归 | `tools/verify-all.ps1` **7 阶段全 PASS**（disk / rust-check / verify-project / gradle / android 115.1s / package 16.9s / two-device 6s） |
| 回归报告 | `build/verify-all-20261020-213824.md` |
| 交付包 | `build/dist/Chat-app-0.1.0.zip`（81.79 MB，SHA256 `996E799E…23C7`） |
| 免 JDK 启动器 | `build/dist/app-image/Chat/Chat.exe`（SHA256 `644E1672…5BD36`） |
| 便携包（需 JDK 21） | `build/dist/Chat-0.1.0.zip`（49.17 MB） |

### 这一基线证明了什么

1. **能构建**：Rust 全量测试、11 个桌面冒烟、Android APK、自包含 app-image；
2. **能跑**：包内运行时跑 4 个冒烟、`Chat.exe` 启动 8 秒存活；
3. **能发**：打包产物向真实节点提交并被 `accepted`；
4. **能收**：`syncOnce` 拉回条目并落库（`fetched=2 applied=1`）；
5. **能对话**：双设备 A/B 全链路（建群→登记→加成员→邀请→发送→入群→拉取）；
6. **有闸门**：任何一项退化都会让 `verify-all` 变红。

### 这一基线**没有**证明什么

- TLS 两端可用（节点 `--tls-cert/--tls-key` 真监听；客户端**严格 pinning + 指纹集合轮换**，系统根不参与；信任可在 **FFI 进程级**一次性配置；**应用界面尚未接线**）
- 跨机器/跨网段/NAT 可用（**不同时在线已可送达**：三进程错时投递 PASS）（**已证明不依赖回环接口**：LAN 地址 e2e PASS；但仍是同一台机器，无中继/NAT 穿透/TLS，明文 HTTP 仅限可信网络）；
- 插件安全（权限门只是门槛，不是隔离；无签名）；
- OS 级凭据保护（当前 DPAPI 文件封装 + 演示口令）；
- 长期运行稳定性（没有 soak 测试）；
- 非 Windows 平台。

## 0.1.x 第二基线（2026-10-20 冻结，10 阶段全绿）

| 项 | 值 |
| --- | --- |
| commit | `4d566cd` |
| Rust 测试 | **545 passed / 0 failed**（chat-crypto / chat-storage / chat-core / chat-ffi 四包，99 suites；工作区合计约 1160） |
| Gradle | 13 个桌面任务 PASS |
| Android | `assembleDebug` PASS（98.4s） |
| 一键回归 | `tools/verify-all.ps1` **10 阶段全 PASS**（disk / rust-check / verify-project / gradle 32.7s / android 98.4s / package 16.9s / two-device 6.3s / lan 4.7s / tls 3.7s / offline 5.4s） |
| 回归报告 | `build/verify-all-20261020-235645.md` |
| TLS 端到端 | 打包产物经 FFI 指纹连 https：正确指纹 accepted+applied；错误指纹被拒 |
| 错误分层展示 | 统一 `ErrorBanner`（4 个面板已收敛）；TLS 类给可执行指引；可重试性取自冻结码表 |
| 错误可诊断 | `NodeClientError.detail` 已改 `Cow`，TLS 失败能看到 `PIN_MISMATCH`/`TLS_TRUST_REQUIRED` 等真实原因；**发送链路也通过错误链带出**（顶层短语不变） |
| 交付包 | `build/dist/Chat-app-0.1.0.zip`（81.79 MB，SHA256 `996E799E…23C7`） |
| 免 JDK 启动器 | `Chat.exe`（SHA256 `644E1672…5BD36`） |

### 与上一基线相比新增的证明

1. **LAN**：不依赖回环接口（192.168.x 上发消息 accepted + 同步 applied）；
2. **TLS**：节点真监听 https，客户端严格 pinning（只认 SHA-256 指纹，系统根不参与）；
3. **错时投递**：发送时接收方离线，四进程顺序验证，稍后上线 applied=1；
4. **跨进程入群**：KeyPackage 私钥加密落库，drop 进程重开仍能入群。

### 仍未证明（下一批目标）

- 真正跨机器/NAT/中继服务（同机 LAN ≠ 公网可达）；
- 证书轮换：库层已支持指纹集合（新旧同时信任），但 FFI/应用尚未透传指纹；到期告警未做；
- 后台消息通知、安装器/自动更新、soak 稳定性；
- 插件签名信任链（权限门已有，签名没有）。

## 0.1.x 第三基线（2026-10-21 冻结，11 阶段全绿）

| 项 | 值 |
| --- | --- |
| commit | `8343543` |
| Rust | `cargo check --workspace --locked` PASS；`chat-core`+`chat-ffi` **72 suites / 389 passed**；`chat-node-client` 8 suites（含 7 例 TLS） |
| Gradle | 13 个桌面任务 PASS |
| Android | `assembleDebug` PASS（97.6s） |
| 一键回归 | `tools/verify-all.ps1` **11 阶段全 PASS**（含 lan / tls / offline / tls-e2e） |
| 回归报告 | `build/verify-all-20261021-010454.md` |
| 交付包 | `Chat-app-0.1.0.zip` 81.79 MB（SHA256 `996E799E…23C7`） |
| 免 JDK 启动器 | `Chat.exe`（SHA256 `644E1672…5BD36`） |

### 本基线相比上一基线新增的证明

1. **TLS 端到端**：打包产物经 FFI 指纹连 https 节点 → accepted + applied；**错误指纹被拒**；
2. **错时投递**：四进程顺序，发送时对方离线、上线后 applied=1；
3. **跨进程入群**：KeyPackage 私钥加密落库（migration 0014）；
4. **错误可诊断 + 分层展示**：`Cow` 详情 + 统一 `ErrorBanner`（7 处），可重试性取自冻结码表。

### 仍未证明（需要外部条件或下一批）

- 真正跨机器 / NAT / 中继服务；
- 证书轮换自动化与到期告警；
- 后台通知、安装器/自动更新、soak；
- 插件签名信任链（权限门已有）。

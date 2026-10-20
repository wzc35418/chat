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


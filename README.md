# chat：跨平台加密聊天项目

Kotlin / Compose Multiplatform 界面、Rust 内核、iroh 与 OpenMLS。已建立正式工程模块，当前提供一个可运行的 P0 双设备加密通信原型。

- [P0 运行方式、链路与验证范围](docs/P0.md)
- [P0-03 SQLite/OpenMLS 崩溃恢复局部实验](docs/P0_PERSISTENCE.md)
- [P0 节点接受事务与未知结果恢复实验](docs/P0_NODE_ACCEPTANCE.md)
- [项目模块入口](docs/PROJECT_GUIDE.md)
- [依赖版本和安装记录](docs/DEPENDENCIES.md)
- [完整架构设计](chat-architecture/完整架构设计.md)
- [完整 P0 验收清单](docs/implementation/P0_CHECKLIST.md)
- [DeepSeek 前端与轻业务任务书](docs/DEEPSEEK_WORK_PLAN.md)
- [消息 Markdown 规格（展示与输入）](docs/design/MESSAGE_MARKDOWN.md)

## 代码仓库

- GitHub（私有）：<https://github.com/wzc35418/chat>
- 发布脚本：[tools/publish-github.ps1](tools/publish-github.ps1)。仓库不包含 `.tools/` 工具链、`target/`、`build/`、本地配置与数据库（见 `.gitignore`）。
- 仓库默认 `main` 分支；每轮开发完成后按实际验证结果提交并推送。

## Windows 运行

在项目根目录打开 PowerShell：

```powershell
.\start-chat.cmd
# 或在 PowerShell 中直接调用：
. .\tools\activate.ps1
.\tools\run-desktop.ps1
```

Windows 下也可以双击根目录 `start-chat.cmd`，它会自动切到项目目录、激活现有工具环境并启动 P0 桌面应用。首次编译可能需要一些时间；失败时窗口会保留错误信息。

界面中连接 Alice / Bob，选择发送设备并发送消息。两个临时设备通过真实的本机 iroh/QUIC 传输 MLS 密文，接收方验证并解密后显示结果。窗口关闭会释放原生会话；本原型不保存历史。

```powershell
# 全链路验证、Android arm64 APK、界面截图和验证报告。
.\tools\verify-p0.ps1

# 独立 Rust 最小往返。
cargo run --locked --offline -p chat-core --bin chat-p0

# Android 调试包。
.\gradlew.bat :apps:android:assembleDebug --offline
```

Android APK 位于 `apps/android/build/outputs/apk/debug/android-debug.apk`。真机安装和操作说明见 P0 文档。

## 构建与安装

首次安装或恢复环境执行 `tools/install-windows.ps1`，随后激活环境。已有依赖检查仍可通过 `tools/verify-windows.ps1` 运行。精确版本保留在 Cargo/Gradle 配置和锁文件中，工具与缓存位于 `.tools/`，不修改永久 PATH。

原生库与 Kotlin 绑定通过 Gradle 任务自动生成；`clean` 后可直接重新构建。日常命令使用已下载缓存；新增依赖缺少缓存时需要联网解析，再保存锁文件。

## 当前边界

这是 P0 技术验证，不是完整聊天产品。当前界面中的两设备位于同一进程并通过回环 UDP 通信；另有独立文件 SQLite 实验验证发送意图、准备任务、MLS 与固定密文 outbox 的原子事务和进程崩溃恢复，独立节点库验证接受日志/投递任务原子性与未知结果重试。正式协调节点、离线信箱、生产持久化链路、加密数据库和可信插件宿主仍未实现。其他模块保留工程边界和接口；`chat-node` 明确提示未实现并退出。

完整 P0 仍需 Android/Linux 实际运行、跨物理设备互操作、完整崩溃矩阵、系统密钥保护及自托管 NAT/relay 验证。局部测试通过不代表完整 P0 通过。

## 构建前磁盘检查（2026-10-19 事故后固化）

**事故**：D: 盘写满（free=0）时，写入工具"先截断再写"导致 `crates/node-services/src/group.rs` 尾部丢失约 200 行；同时出现 `link.exe` LNK1318 与 `os error 112`。

**规则**：
1. 大规模编辑或整仓构建前先跑 `pwsh tools/check-disk.ps1`（可用 < 6 GB 时会以退出码 2 失败）。
2. 需要清理时用 `pwsh tools/check-disk.ps1 -Clean`（删 `target/debug/incremental` 与 `deps/.fingerprint`，二者都可安全重建）。
3. 见到 `os error 112` 或 `link.exe` LNK1318，**先查磁盘**，不要当代码 bug 排查。
4. 长文件（>1500 行的 Kotlin/文档/源码）一律只用定点 edit 或末尾追加，不要整文件重写。


## 快速开始（2026-10-19）

前置：Windows x64、项目内工具链（`. .\tools\activate.ps1`）、D: 至少 6 GB 空闲。

```powershell
cd D:\AgentProject\chat
. .\tools\activate.ps1
powershell -ExecutionPolicy Bypass -File tools\check-disk.ps1     # 磁盘不足会以退出码 2 失败

# 一键：建演示种子 -> 起本地节点 -> 轮询 /v1/health -> 打印粘贴用配置
powershell -ExecutionPolicy Bypass -File tools\demo.ps1
# 另开一个窗口：
.\gradlew.bat :apps:desktop:previewApp --offline --max-workers=1
```

- 节点默认监听 `http://127.0.0.1:8080`；`demo.ps1 -Stop` 停止，`-Preview` 顺带起预览。
- 人工测试步骤见 [docs/manual-test-guide.md](docs/manual-test-guide.md)（身份 → 恢复文件 → 发送 → 附件 → 群治理 → 同步 → 插件 P4 → 通知）。
- 全量回归：`cargo test --offline --workspace` + `gradlew :apps:desktop:presentationChecks ... pluginJvmSmoke`（见指南第 12 节）。

## 全量回归：一键脚本（tools/verify-all.ps1）

```powershell
powershell -File tools\verify-all.ps1                    # 磁盘 + Rust + 项目结构 + Gradle 11 任务 + Android
powershell -File tools\verify-all.ps1 -SkipAndroid -SkipGradle   # 只跑检查/结构（约 1-2 分钟）
powershell -File tools\verify-all.ps1 -TargetDir C:\chat-target  # 把 cargo target 放到 C: 盘
```

- 阶段顺序：`disk`（不通过直接 abort，退出码 2）→ `rust-check`（`cargo check --offline --locked --workspace --all-targets`）→ `verify-project`（`node tools/verify-project.cjs`）→ `gradle`（11 个桌面回归任务，`--offline --max-workers=1`）→ `android`（`:apps:android:assembleDebug`）。
- 任一阶段失败**不会中断**其余阶段：最终退出码为 1，报告里能看到所有阶段的真实状态。
- 报告写到 `build/verify-all-<timestamp>.md`，每阶段日志在 `build/verify-logs/<timestamp>/<stage>.log`；控制台打印 PASS/FAIL 表。
- 输出**纯 ASCII**：Windows PowerShell 5.1 在中文代码页下会破坏 UTF-8 控制台文本，因此脚本刻意不输出非 ASCII 字符。
- 脚本会先 dot-source `tools/activate.ps1`（cargo/JDK/Android SDK/MSVC 环境），因此不依赖调用方 shell 已激活。


## 当前状态（2026-10-20，0.1.0 基线）

| 项 | 值 |
| --- | --- |
| 版本 / 基线 commit | 0.1.x / `f8907ba`（**11 阶段全绿**） |
| Rust 测试 | **1141 passed / 0 failed**（14 个 crate） |
| 桌面冒烟 | **11 个 Gradle 任务**（含 pluginSmoke / pluginJvmSmoke 沙箱断言） |
| Android | `assembleDebug` PASS |
| 一键回归 | `tools/verify-all.ps1` **11 阶段全 PASS**（+ lan / tls / offline / tls-e2e） |
| 交付物 | `build/dist/Chat-app-0.1.0.zip`（免 JDK，双击 `Chat.exe`） |
| 双设备互通 | `tools/demo-two-devices.ps1` 全 PASS |
| 错时投递 | `tools/verify-offline-delivery.ps1` 全 PASS（发送时对方离线，上线后 applied=1） |
| 加密传输 | 节点 TLS 监听 + 客户端严格 pinning（指纹集合，可轮换），界面可配置 |
| 插件安全 | 权限门 + 签名闸门（默认要求签名）+ 信任列表 |

**能做什么**：建身份、加密存储、群治理、附件、插件（带权限门）、发消息到真实节点、从节点拉回消息、两台设备互通。

**还不能做什么**：跨机器/跨网段（无中继）、插件安全隔离（权限门不是沙箱）、OS 凭据库、后台通知、安装器/自动更新。

细节见 [SHIPPING.md](docs/SHIPPING.md)（基线与限制）、[manual-test-guide.md](docs/manual-test-guide.md)（15 节手工测试）、[COVERAGE_SNAPSHOT.md](docs/implementation/COVERAGE_SNAPSHOT.md)（模块覆盖与缺口）。

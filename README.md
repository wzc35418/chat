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

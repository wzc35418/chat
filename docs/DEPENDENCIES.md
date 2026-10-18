# 开发依赖与安装记录

正式工程状态更新：源码模块现已接入 Cargo/Gradle 工作区，Kotlin 界面、Rust、OpenMLS 和 iroh 的真实 P0 最小链路已建立。当前运行、Android APK 与验证证据见 [P0 报告](P0.md)。本文件以下保留环境安装阶段的版本、来源和基础验证记录。

记录日期：2026-10-18（Pacific/Auckland）。依据：[完整架构设计](../chat-architecture/完整架构设计.md)、[MVP](../chat-architecture/docs/MVP.md)。

本次为 Windows x64 主机准备开发环境和可重复的依赖验证工程。项目原来只有大纲，尚无聊天实现。安装依赖、通过编译与完成大纲中的 P0 真机互操作是不同的交付阶段。

## 1. 安装位置和工具版本

工具默认保存在项目 `.tools/` 内，通过 `. .\tools\activate.ps1` 启用。该脚本只更改当前 PowerShell 进程的环境，不写入用户或系统的永久 PATH。

| 工具 | 本次版本 / 选择 | 用途与位置 |
|---|---|---|
| Git | 复用现有 2.55.0.windows.3 | 版本管理；`C:\Program Files\Git\cmd` |
| JDK | Eclipse Temurin 21.0.12.1+1 LTS | Kotlin/JVM、Gradle、Compose 桌面运行与后续 jpackage；`.tools/jdk/` |
| Gradle | 8.14.3 | 项目附带 Wrapper；独立启动工具在 `.tools/gradle/` |
| Rust / Cargo | 1.99.1，MSVC x64 | 内核和节点开发；`.tools/cargo/`、`.tools/rustup/` |
| rustfmt / Clippy | 随 Rust 1.99.1 | 格式化和静态检查 |
| Rust Android 标准库 | `aarch64-linux-android` | 对应 Android 真机 arm64 |
| Visual Studio Build Tools | 复用现有 2026，18.10.1 | C/C++ 编译及 Windows Rust 链接，无需重复安装 |
| MSVC | 当前激活 14.51.36231 | 编译 SQLite、TLS 等原生依赖 |
| Windows SDK | 复用现有 10.0.26100.0 | Windows 系统库和链接工具 |
| CMake / Ninja | 复用 VS 附带 4.3.1-msvc1 / 1.13.2 | 原生依赖构建，由环境脚本加入当前 PATH |
| Android Command-line Tools | 19.0（13114758） | `.tools/android-sdk/cmdline-tools/19.0/` |
| Android SDK Platform | API 36 | 编译 Android 库；`.tools/android-sdk/platforms/android-36/` |
| Android Build Tools | 36.0.0 | Android 打包与字节码工具 |
| Android Platform Tools | 37.0.1（adb 1.0.41） | adb；SDK 自带 package.xml/source.properties 记录版本 |
| Android NDK | 28.2.13676358 | Rust → Android arm64 原生库编译 |
| cargo-ndk | 4.1.2 | 选择 NDK 编译器和 Android ABI |
| UniFFI 绑定生成工具 | 0.32.2 | 作为 workspace 内的 `uniffi-bindgen` 可执行目标构建，保证与库同版 |

本次安装先尝试了最新 Android 命令行包 23.0，其 sdkmanager 转接到新的 Android CLI 后没有正确传递旧包标识。为确保标准 SDK 安装命令可复现，实际使用固定的 19.0。23.0 下载留在本地缓存，不进入激活 PATH。19.0 读取新仓库 XML 时会出现版本提示，是否安装成功以目标包的 `source.properties` 和实际构建结果为准。

工具下载地址、校验摘要和 Rust 版本记录在 [toolchain-lock.json](../tools/toolchain-lock.json)。JDK、Gradle、rustup 下载校验 SHA-256；Android 命令行包校验 Google 官方仓库 XML 提供的 SHA-1；SDK/NDK 由 sdkmanager 安装。本机 SDK 路径写入不提交的 `local.properties`。

## 2. Kotlin / Compose / 插件依赖

版本入口：[gradle/libs.versions.toml](../gradle/libs.versions.toml)。Gradle 下载库到 `.tools/gradle-home/`，库不需要全局安装。

| 依赖 | 版本 | 用途 |
|---|---|---|
| Kotlin Multiplatform / Serialization 插件 | 2.2.20 | 跨平台 Kotlin 编译和序列化生成 |
| Kotlin Compose Compiler 插件 | 2.2.20 | 必须与 Kotlin 插件保持同版 |
| Compose Multiplatform | 1.9.3 | 共享 UI、Runtime、Foundation、Material 3、Desktop |
| Android Gradle Plugin | 8.11.1 | Android 库构建；配合 Gradle 8.14.3 / JDK 21 |
| kotlinx-coroutines-core | 1.10.2 | 协程和 Flow 状态流 |
| kotlinx-serialization-json | 1.9.0 | DTO、配置和插件清单序列化 |
| PF4J | 3.14.1 | 可信 JVM 插件的发现、类加载和生命周期 |
| JNA | 5.17.0 | UniFFI 生成的 Kotlin/JVM 绑定；桌面使用 JAR，Android 使用 AAR |
| androidx.activity:activity-compose | 1.8.2 | 正式 Android P0 Activity 与 Compose 入口 |

这组版本是本次选定并通过构建检查的基线，不自动追随最新版。Kotlin 2.2.20 的官方支持矩阵覆盖 Gradle 8.14 和 AGP 8.11.1，Compose 编译器与 Kotlin 同版。后续升级应整体检查插件兼容性。

验证模块使用 `commonMain` + `desktopMain` + `androidMain`。最低 Android API 暂定为 26，compileSdk 为 36；这是安装验证工程的起点，不代表原始大纲已冻结最低系统版本。

当前网络、SQLite 和 MLS 都在 Rust 一侧，因此没有另加 Kotlin Ktor、SQLDelight 或第二套数据库依赖。Android KeyStore、Windows 系统密钥保护属于后续平台适配；本次没有实现密钥存储。

## 3. Rust 依赖

直接版本固定在 [Cargo.toml](../Cargo.toml)，完整传递版本固定在 `Cargo.lock`。Rust 库下载到 `.tools/cargo/registry/`，在编译时链接到目标程序；它们不是常驻软件服务。

| 依赖 | 版本 | 对应大纲职责 |
|---|---|---|
| iroh | 1.3.0 | QUIC、设备连接、P2P 与 relay 连接能力 |
| openmls | 0.9.1 | MLS 消息与群加密协议 |
| openmls_rust_crypto | 0.6.0 | OpenMLS 的 RustCrypto 后端 |
| openmls_basic_credential | 0.6.0 | MLS 设备凭据基础类型 |
| openmls_sqlite_storage | 0.3.0 | MLS SQLite 持久化适配；已用于独立 P0-03 同事务崩溃实验，生产方案待决策 |
| rusqlite（bundled） | 0.37.0 | 本地/节点 SQLite；随库编译 SQLite C 源码 |
| uniffi | 0.32.2 | Rust → Kotlin 绑定；仅宿主生成工具启用 cli 特性 |
| tokio | 1.53.3 | 异步任务、定时、文件与同步 |
| axum（ws） | 0.8.9 | 应用节点 HTTP / WebSocket |
| reqwest（rustls/json/stream） | 0.13.5 | 客户端 HTTPS 与流式下载；禁用默认特性 |
| serde / serde_json | 1.0.229 / 1.0.154 | 数据模型和 JSON 配置 |
| thiserror / anyhow | 2.0.21 / 1.0.104 | 库错误类型和程序错误上下文 |
| tracing / tracing-subscriber | 0.1.44 / 0.3.23 | 结构化诊断与日志 |
| uuid / bytes | 1.28.0 / 1.12.1 | 标识符与字节缓冲 |
| zeroize | 1.9.1 | 敏感内存清理辅助 |
| chacha20poly1305 | 0.11.0 | 附件/敏感记录 AEAD 的实现候选 |
| ed25519-dalek | 3.0.0 | 设备凭据与应用层签名候选 |
| argon2 | 0.6.0 | 恢复文件口令派生候选 |
| sha2 / rand | 0.11.1 / 0.10.3 | 摘要与安全随机基础接口 |

OpenMLS SQLite storage 0.3.0 依赖 rusqlite `^0.37`。因此直接依赖使用 0.37.0，避免与最新 rusqlite 引入的不同 `libsqlite3-sys` 原生链接发生冲突。SQLite 已使用 bundled 特性，无需再安装 SQLite 数据库服务器。

Android 模块的 Gradle 锁校验使用 `LockMode.LENIENT`：当前 AGP AAR 元数据任务会省略只作为变体约束的 `kotlin-stdlib-common`，严格模式会把锁文件中的该条误判为未解析。版本仍由锁文件约束；已用不带 `--write-locks` 的 `checkDebugAarMetadata --rerun-tasks --offline` 验证。其他模块保持严格锁校验。

上面的密码学辅助库只是选定的实现依赖，附件格式、nonce 规则、身份规范及密码套件仍需要协议设计和测试。OpenMLS 的默认内存 provider 仅用于本次启动检查；生产 MLS 状态必须接入统一事务/恢复方案。安装 SQLite storage provider 不表示大纲要求的跨 MLS/outbox 原子事务已经实现。

## 4. 重复安装和日常使用

### Windows 安装

在根目录 PowerShell 中执行：

```powershell
.\tools\install-windows.ps1
. .\tools\activate.ps1
.\tools\verify-windows.ps1
```

需要联网访问 Adoptium/GitHub、Rust 官方下载站、crates.io、Google Android/Maven 仓库、Maven Central 和 Gradle 官方源。脚本固定工具版本，对已下载并校验的文件直接复用；Android SDK 安装会接受所需组件许可。C++ 编译器、CMake 和 Ninja 复用本机 Visual Studio。

新 Windows 主机如果缺少 MSVC，先从微软官方安装 Visual Studio Build Tools 的“使用 C++ 的桌面开发”工作负载，并包含 x64/x86 编译工具及 Windows SDK。本次机器已具备，无需安装完整 Visual Studio IDE。宿主绑定工具、节点 HTTP 库与 Android 原生库按目标/特性分别启用，Android 构建不携带绑定生成 CLI 或节点 Axum 依赖。

进入一个新终端后先运行 `. .\tools\activate.ps1`。项目不要求全局安装 Kotlin 编译器、Gradle 或 Rust。离线复查时可使用 `cargo check --workspace --locked --offline` 和 Gradle `--offline`，前提是所需目标/任务的缓存已经下载完整。

### 锁文件维护

- `rust-toolchain.toml` 固定 Rust、组件及 Android target。
- `Cargo.toml` 固定直接版本，`Cargo.lock` 固定传递依赖。
- `gradle/libs.versions.toml` 固定 Kotlin/JVM 直接版本。
- `gradle/wrapper/gradle-wrapper.properties` 固定 Gradle 及发行包 SHA-256。
- Kotlin 验证模块的 `gradle.lockfile` 固定构建所需配置的依赖结果，根目录 `settings-gradle.lockfile` 记录设置脚本的锁定结果。
- `tools/toolchain-lock.json` 固定下载源和校验摘要。
- `tools/rust-dependency-versions.json` 是本次仓库查询快照；**实际选版以 Cargo.toml / Cargo.lock 为准**，例如 rusqlite 已因兼容性选择较低版本。

日常构建不要加 `--write-locks`，也不要无目的地运行 `cargo update`。有意升级时修改版本入口，更新锁文件并重新运行验证。需要重写 Gradle 锁文件时显式运行 `tools/verify-windows.ps1 -RefreshLocks`。

`verify-windows.ps1` 按顺序生成 Windows DLL、Kotlin 绑定和 Android `.so`，再编译 Kotlin/Compose、生成带 arm64 原生库的验证 AAR、执行桌面 Kotlin → Rust 实际调用。请勿同时运行 Rust 链接与 FFI 加载检查，Windows 会阻止加载正被写入的 DLL。Gradle `clean` 会清掉生成绑定和原生库，之后重新运行完整验证脚本即可。

## 5. 其他平台、可选工具与未安装服务

| 项目 | 处理方式 |
|---|---|
| Android Studio / IntelliJ IDEA | 可选 IDE；命令行构建已具备，不影响在现有编辑器里开发 |
| Android 模拟器与系统镜像 | 未安装；大纲的验收目标是 arm64 真机，可通过 adb 连接 |
| Linux 构建机 | 需 Linux x64 主机/CI，安装 Git、JDK 21、Rust 1.99.1、C/C++ 编译器、CMake、Ninja、pkg-config，以及桌面运行所需图形库；本次没有创建 Linux 系统或执行 Linux 验收 |
| Linux 基线发行版 | 待通过真实打包测试后确定；不能用 Windows 编译成功推断 Linux/glibc 兼容性 |
| Docker / Docker Compose | 仅后续容器部署需要；MVP 可直接运行 Rust 节点，本次未安装 Docker Desktop |
| WSL | 系统已有 wsl 命令；本次不新增发行版、不更改虚拟化或系统启动配置 |
| WiX 等桌面安装包工具 | 后续选定 Windows MSI/EXE 分发方式时安装；目前依赖检查不生成产品安装包 |
| iroh relay 服务 | 可选自托管组件；iroh 依赖树含 relay 库，本次未启动公网中继，也未配置监听端口 |
| 应用节点 / HTTPS 反向代理 | 业务节点尚未实现；部署时再安装和配置服务、证书及持久目录 |
| Redis / Kafka / Kubernetes / PostgreSQL | 大纲明确不要求，未安装 |
| Wasmtime / Wasm 插件 / 推送网关 | 不在首版范围，未安装 |

Android arm64 原生库由 Windows 上的 NDK 交叉编译；Linux 桌面应用应在 Linux 上构建和验证。Gradle Wrapper 同时提供 `gradlew` 和 `gradlew.bat`。

## 6. 本次实际验证结果

最终检查时间：2026-10-18 02:24（Pacific/Auckland）。完整执行 `tools/verify-windows.ps1 -RefreshLocks`，退出码 0，输出 `All Windows-host dependency checks passed.`。

| 检查 | 实际结果 |
|---|---|
| JDK / Gradle / Rust / MSVC / CMake / Ninja | 已运行版本或路径检查，版本见第 1 节 |
| Android SDK / Build Tools / NDK | 所有所需包存在 `source.properties`；aapt2、NDK Clang 可运行 |
| adb | 1.0.41，Platform Tools 37.0.1，命令运行成功 |
| `cargo check --workspace --all-features --locked` | 通过，包含宿主 UniFFI CLI、iroh、OpenMLS、SQLite、HTTP/WS 与密码学依赖 |
| `dependency-smoke` | 运行成功；内存 SQLite 事务建表、插入、提交、查询；SQLite 实际版本 3.50.2；OpenMLS RustCrypto provider 初始化成功 |
| Windows 原生库与 Kotlin 绑定生成 | 通过；生成 `target/debug/chat_dependency_check.dll` 及 UniFFI Kotlin 源码 |
| Android arm64 交叉编译 | 通过；NDK API 26，`aarch64-linux-android`，输出复制到验证模块的 `build/generated/jniLibs/arm64-v8a/` |
| Android 原生文件头 | `llvm-readelf` 确认 ELF64 / DYN / AArch64 |
| Kotlin / Compose 桌面编译 | `compileKotlinDesktop` 通过，包含生成的 UniFFI 绑定 |
| Kotlin / Compose Android 编译与 AAR 打包 | `assembleDebug` 通过，包含生成绑定及 arm64 原生库 |
| 桌面 Kotlin → Rust 实际调用 | `verifyFfi` 通过，返回 `Rust/Kotlin FFI dependencies ready` |
| 依赖锁定 | Cargo 与 Gradle 锁文件已生成；所有直接版本都有明确配置入口 |
| 安装/验证脚本和文档 | PowerShell 语法检查、Markdown 本地链接检查通过 |

验证 AAR：`tools/kotlin-dependency-check/build/outputs/aar/kotlin-dependency-check-debug.aar`。已检查其内容包含 `AndroidManifest.xml`、`classes.jar`、`jni/arm64-v8a/libchat_dependency_check.so` 及构建所需的 iroh 原生库。它是依赖验证库，不是可安装聊天 APK。

**后续 P0 状态**：Windows x64 已另行通过 [客户端 OpenMLS/SQLite/outbox 事务与崩溃恢复局部实验](P0_PERSISTENCE.md) 和 [独立节点接受事务实验](P0_NODE_ACCEPTANCE.md)；其所用直接依赖已写入各 crate 的 Cargo.toml，实际解析版本以 Cargo.lock 为准。Android 真机加载/运行、Linux 编译/打包、跨端 MLS 消息互操作、正式加密存储与自托管节点/relay 网络演示尚未执行。依赖检查本身不替代这些验收。

## 7. 官方资料

- [Kotlin Multiplatform 版本兼容矩阵](https://kotlinlang.org/docs/multiplatform/multiplatform-compatibility-guide.html)
- [Compose 平台与编译器兼容说明](https://kotlinlang.org/docs/multiplatform/compose-compatibility-and-versioning.html)
- [Compose Multiplatform 1.9.3 发布记录](https://github.com/JetBrains/compose-multiplatform/releases/tag/v1.9.3)
- [Android SDK 与命令行工具](https://developer.android.com/studio)
- [Rust Windows 安装与 MSVC 前提](https://rust-lang.org/tools/install/)
- [iroh 文档](https://docs.rs/iroh/1.3.0/iroh/)
- [OpenMLS 项目与平台支持](https://github.com/openmls/openmls)
- [OpenMLS SQLite storage 0.3.0](https://crates.io/crates/openmls_sqlite_storage/0.3.0/dependencies)
- [UniFFI](https://mozilla.github.io/uniffi-rs/latest/)
- [PF4J 类加载说明](https://pf4j.org/doc/class-loading.html)

PF4J 管理可信插件的加载与生命周期，不提供恶意 JVM 插件的安全沙箱。原始设计中的权限和安全边界仍然适用。

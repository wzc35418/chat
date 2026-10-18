# Chat 跨平台加密聊天项目

Android / Windows / Linux；Kotlin 界面、Rust 内核与自托管节点、MLS/OpenMLS、iroh、SQLite、可信桌面插件。技术方案沿用 [原架构](docs/design/ARCHITECTURE.md)。

**当前状态：工程骨架 + 197 个函数级详细计划。尚未实现或编译聊天客户端/节点。** 原设计资料保留，所有新工程文件集中在本文件夹。

- [开始阅读详细开发计划](docs/implementation/README.md)
- [按函数查看计划](docs/implementation/FUNCTION_INDEX.md)
- [开发阶段和工作项](docs/implementation/TASKS.md)
- [技术验证清单](docs/implementation/P0_CHECKLIST.md)
- [项目文件清单](docs/PROJECT_TREE.md)
- [实际验证记录与编译限制](docs/VALIDATION.md)

## 本地结构检查

本项目提供 Node.js 24 可运行的结构/文档/JSON/SQLite 草案检查。它不编译 Kotlin/Rust，不测试 E2EE 或网络行为。

~~~text
node tools/verify-project.cjs
~~~

## 后续构建

Cargo 工作区当前仅含本地模块、不依赖第三方库；Cargo.lock 只描述这些本地包，加入真实库后须由 Cargo 更新并锁定。取得 Rust 工具链后先运行 cargo check --workspace --locked。Gradle 文件当前使用 base 插件登记模块，未启用 Kotlin/Compose/Android 编译插件；即使 Gradle 基础任务成功也不代表应用能编译。必须先完成 P0 工具链兼容矩阵，再补真实 wrapper、插件与精确依赖版本。

节点程序在骨架阶段以状态码 78 明确退出。deploy/compose.yaml 是待 P5 实现的部署草案；需要真实构建镜像且应先修订服务监听和 TLS 配置，当前不能部署聊天服务。两个 Kotlin 插件示例提供源代码/清单，尚未编译 JAR，也没有可运行宿主。

## 第一项工作

按 [P0_CHECKLIST](docs/implementation/P0_CHECKLIST.md) 记录三端环境，锁定 Rust/Gradle/JDK/Kotlin/Compose/AGP/NDK/UniFFI/OpenMLS/iroh 版本，然后验证 Android arm64、Windows x64、Linux x64 的 FFI 与 MLS 往返。原设计未授权的后续功能不进入 MVP。

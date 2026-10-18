# 依赖安装完成后的工作区整合

状态更新（2026-10-18）：下列工作区登记与构建插件整合已完成，原精确工具链及依赖验证模块均保留，锁文件通过实际构建更新。产品 P0 原型与剩余边界见 [P0 记录](../P0.md)。以下清单保留为整合依据。

1. 保留根目录的精确第三方依赖版本、Rust工具链、Kotlin/Compose/AGP组合、Gradle Wrapper、repositories及现有tools依赖验证模块。
2. Cargo workspace.members 加入 crates/protocol、domain、identity、crypto、storage、transport、sync、core、ffi、node-services、node-api 和 apps/node。不要移除 tools/rust-dependency-check。
3. workspace.dependencies 增加 chat-* 本地path条目，对照 scaffold-config/Cargo.toml.txt；保持原精确第三方依赖不变。业务包继承目标workspace.package元数据。
4. 安装验证任务结束且无其他Cargo写锁任务时，才统一重新生成/核查锁文件；不要用本次骨架Cargo.lock.txt覆盖真实Cargo.lock。
5. Gradle settings保留现有pluginManagement/dependencyResolutionManagement和tools模块，再追加源码模块include；原骨架base插件仅为登记占位，应按已选工具链逐项升级真实source sets。
6. 不把空版本目录覆盖到gradle/libs.versions.toml。保留安装工程选择的版本并按P0验证结果增补，不自动升级。
7. 按 scaffold-config/.gitignore.txt 补充敏感数据/签名钥/恢复文件忽略规则；保留已有.tools、缓存、local.properties和日志规则。
8. 保留根README的安装流程，完成整合后再合并项目总入口；本次入口在docs/PROJECT_GUIDE.md。

本清单描述后续代码接入；文件迁移本身已完成。先确认安装工程实际结果，不把目录存在当作编译/跨端/密码学验证成功。

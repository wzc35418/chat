# 项目入口与文件管理

正式项目根目录：D:/AgentProject/chat。

- [详细实施计划：17模块、197个函数](implementation/README.md)
- [函数索引](implementation/FUNCTION_INDEX.md)
- [分阶段工作项](implementation/TASKS.md)
- [DeepSeek 前端与轻业务任务书（底层进度交接）](DEEPSEEK_WORK_PLAN.md)
- [正在使用的依赖安装与验证记录](DEPENDENCIES.md)
- [迁移记录与同名配置说明](migration/README.md)
- [口令加好友与桌面双栏界面设计](design/CONTACT_INVITE_DESKTOP.md)
- [消息 Markdown 规格（展示与输入）](design/MESSAGE_MARKDOWN.md)

apps/、shared/、crates/、desktop/、plugins/ 是产品源码骨架；docs/implementation 是函数级计划；docs/design 是原设计的项目内归档；schemas/ 是配置草案；tests/ 是待实现验收场景；deploy/ 是后续部署草案。

.tools/、.gradle/、.kotlin/、target/、build/ 是已有工具与构建缓存，迁移未改动。tools/ 中已有安装/依赖验证脚本保持原样，新增 verify-project.cjs 只检查项目资料，且跳过依赖缓存。

根目录 Cargo.toml/Cargo.lock、Gradle配置/版本目录/Wrapper已整合正式 Rust/JVM/Android 模块，并保留原有依赖验证模块。当前最小实现位于 crypto/transport/core/ffi 和共享 Kotlin 界面；其余服务仍是边界与接口。运行和验证见 [P0 最小链路](P0.md)。源骨架的历史同名配置仍保存在 docs/migration/scaffold-config/*.txt，不用于活动构建。

chat-architecture/ 是原先已经存在的设计包，保持原位。docs/design/ 与它属于设计快照；后续实际实施以 docs/implementation 与根目录活动依赖配置为准。

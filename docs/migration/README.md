# 迁移记录

日期：2026-10-18。目标：D:/AgentProject/chat。

源项目165个文件已逐项检查：158个新增文件进入对应目录；7个同名配置保留目标版本，源骨架版本保存到 scaffold-config/ 并附 .txt 后缀。详细映射与SHA-256见 [manifest.json](manifest.json)。

保留的同名文件：.gitignore、README.md、Cargo.toml、Cargo.lock、settings.gradle.kts、build.gradle.kts、gradle/libs.versions.toml。它们由现有依赖工程继续使用。本次不更新锁文件、不重新注册模块、不运行安装/构建命令、不停止任何进程。

迁移后唯一日常工作位置为 D:/AgentProject/chat；原C盘生成目录核验后归档到原聊天work/archive/，不再作为活动工程。旧ZIP也一并归档，不在D盘项目中混放发布包或临时文件。

[项目总入口](../PROJECT_GUIDE.md) · [安装完成后的整合清单](INTEGRATION.md)

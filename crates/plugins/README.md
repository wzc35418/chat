# crates/plugins

插件宿主**纯逻辑层**（`chat-plugins`，edition 2024，`#![forbid(unsafe_code)]`）。不依赖 JVM/WASM 运行时、不解压 ZIP、不联网、不执行插件代码。

- `manifest`：PLUGINS-001 的清单 schema 解析/校验（ID、版本、SDK/平台、能力、扩展点、权限、入口类名）；
- `registry`：扩展点注册/注销/查询（重复注册幂等，集合冲突显式拒绝）；
- `host`：安装 → 启动 → 分发计划 → 完成校验 → 停止/卸载的闭环，含输入/输出上限、超时预算与 `operationId` 幂等；
- `storage`：每插件命名空间键值隔离（键/值/条目/字节配额，失败不产生部分写入）；
- `limits`：可收紧不可放大的资源预算；
- `error`：插件层稳定错误码（`CONTENT_TOO_LARGE` 复用冻结协议码，其余为插件新增码，登记在 `docs/implementation/plugins.md`）。

真正的 JAR 加载、信任确认、安装事务、安全模式发现、主题配置等 P4 宿主能力未实现。语义、预算、错误码登记与证据见 [可信桌面插件、配置包、SDK 与示例](../../docs/implementation/plugins.md) 的「Rust 纯逻辑插件宿主核心」。

验证命令：`. .\tools\activate.ps1; cargo test --offline -p chat-plugins`（当前 36 passed：lib 19 + `tests/host_lifecycle.rs` 17）。

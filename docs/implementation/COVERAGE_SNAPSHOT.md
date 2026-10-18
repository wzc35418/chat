# 实现进度快照（自动盘点）

时间：2026-10-18（第 16 轮目标批次结束时）。统计方式：`docs/implementation/*.md` 每模块的 `## MODULE-NNN` 函数小节数 + 第 3 行状态声明，人工核对；**证据以各模块文档的验收段与 `cargo test` 结果为准**。

## 总览

- 计划函数总数：**197**（17 模块）
- 已完整实现并通过自动化用例：**协议 14/14、领域 12/12**
- 主体实现（部分子项待接线）：**storage 15、crypto 15、core 12、identity 12/14、attachments 8、sync 8/14、node 16、api 10**
- 有 Rust 纯逻辑核心、宿主侧未实现：**plugins 13**
- 仅在设计中（无实现）：**transport 12、groups 12、group-actions 5、presentation 11、platform 10、bridge 7**（bridge 已有 2 个竖直切片：LocalStoreFfi / IdentityFfi + 发送/附件/事件方法）

## 分模块（函数数 / 状态）

| 模块 | 函数 | 状态 | 关键证据 |
|---|---|---|---|
| PROTOCOL | 14 | **全部实现** | `cargo test -p chat-protocol` 73 passed（含固定字节向量） |
| DOMAIN | 12 | **全部实现** | `cargo test -p chat-domain` 43 passed |
| STORAGE | 15 | 主体实现 | `cargo test -p chat-storage` 79 passed（迁移 0001~0006） |
| CRYPTO | 15 | 主体实现（004 同事务 provider、FileKey 持久化缺） | `cargo test -p chat-crypto` 40 passed |
| CORE | 12 | 主体实现（007 send 已接真节点、008/009/010 完成） | `cargo test -p chat-core` 81 passed |
| IDENTITY | 14 | 001~012 实现，013/014 恢复文件未做 | `cargo test -p chat-identity` 52 passed |
| ATTACHMENTS | 8 | 本机 + 端口 + 节点适配完成，进度位图/离线缓存缺 | `cargo test -p chat-core`（附件 20 用例） |
| SYNC | 14 | 003/004/007/011 + 拉取/追赶决策核心，outbox 批执行缺 | `cargo test -p chat-sync` 53 passed |
| NODE | 16 | 001~015 实现，016 TTL 回收缺 | `cargo test -p chat-node-services` 73 passed |
| API | 10 | 002/003/005/008 + health/限流，004 能力令牌缺 | `cargo test -p chat-node-api` 20 passed |
| PLUGINS | 13 | Rust 纯逻辑核心 36 用例，Kotlin 宿主未做 | `cargo test -p chat-plugins` 36 passed |
| BRIDGE | 7 | 2 个竖直切片 + 发送/附件/事件方法，通用编排未做 | `:apps:desktop:*Smoke` 4 条链路 |
| TRANSPORT | 12 | **未开始** | — |
| GROUPS / GROUP-ACTIONS | 12 / 5 | **未开始** | — |
| PRESENTATION | 11 | 投影与检查已实现（19 组 presentationChecks），用例层未成形 | `:apps:desktop:presentationChecks` |
| PLATFORM | 10 | **未开始** | — |

## 测试与验证总量（第 16 轮批次末）

- Rust：**623 个测试**（workspace 全绿）
- Kotlin/Compose：presentationChecks **19 组**、快照 19 张、竖直链路冒烟 **4 条**（本机存储 / 身份 / 身份持久化 / 附件）、Android APK 构建
- 文档自检：`tools/verify-project.cjs`（链接、锚点、结构、冻结码一致性）

## 下一批优先级（按依赖）

1. `transport` 模块（TRANSPORT-001~012）：目前网络层直接用 iroh 之外的最小实现，正式传输/中继未开始；
2. `groups` / `group-actions`：需要 CRYPTO-004 同事务 provider 与 MLS 成员操作串起来；
3. `platform`：托盘/前台服务/密钥槽（FileKey 持久化 ADR 依赖它）；
4. `presentation` 用例层与 `bridge` 通用编排：把已有竖直切片收敛成统一服务；
5. 插件 Kotlin 宿主（P4）与附件进度位图/离线缓存。

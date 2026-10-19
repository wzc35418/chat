# 实现进度快照（自动盘点）

时间：2026-10-18（第二批 32 轮结束时）。统计口径见 `docs/DEEPSEEK_WORK_PLAN.md` 的交付记录表；证据以各模块文档与 `cargo test` 结果为准。

## 测试与验证总量

- Rust：**875 个测试**（workspace 全绿，0 失败）
- Kotlin/Compose：presentationChecks 20 组、快照 19 张、竖直链路冒烟 **6 条**（本机存储 / 身份 / 身份持久化 / 附件 / 平台密钥槽 / DPAPI / 群）
- Android APK 构建通过；文档自检 `tools/verify-project.cjs` PASS

## 分模块状态

| 模块 | 状态 |
|---|---|
| PROTOCOL 14 | **全部实现**（含控制候选领域操作声明 55/56、通用提案值 9..=13） |
| DOMAIN 12 | **全部实现** |
| STORAGE 15 | 主体实现（迁移 0001~0009；附件密钥/进度/消息引用齐全） |
| CRYPTO 15 | 主体实现（MLS 群状态、成员操作、自移除提案、附件分块、密钥槽用途子密钥） |
| CORE 12 | 主体实现（发送编排、状态/事件、群治理全套、提案编排、同步 worker/runtime、附件编排） |
| IDENTITY 14 | 001~012 实现；013/014 恢复文件导出/导入未做 |
| ATTACHMENTS 8 | 本机导入导出、上传/下载、续传位图、离线缓存、回收（消息引用+宽限期） |
| SYNC 14 | 决策核心 + 拉取 worker + 常驻运行时；outbox 批执行（SYNC-002）未做 |
| NODE 16 | 001~016 实现（含能力令牌、TTL 回收、分块上限对齐） |
| API 10 | 002/003/004/005/008 + health/限流；其余设计项待核 |
| PLUGINS 13 | Rust 纯逻辑宿主核心；Kotlin 宿主（P4）未做 |
| BRIDGE 7 | LocalStore/Identity/Attachment/Group/Sync 五个切片 + 通用编排未成形 |
| TRANSPORT 12 | 纯逻辑核心 + 真实回环 iroh + LAN/中继配置；经中继实跑与穿透未验证 |
| GROUPS 12 | 建群/快照/角色/禁言/移除/改名/转让/退出/解散/接受/丢弃/提案链路 |
| GROUP-ACTIONS 5 | 部分并入 GROUPS 路径；独立模块未成形 |
| PRESENTATION 11 | 投影与 20 组检查 + 用例层骨架；完整用例未成形 |
| PLATFORM 10 | 001/002（DPAPI/口令槽）+ 004/005/006（文件选择）+ 007~010 未做 |

## 下一阶段优先级（建议）

1. **TRANSPORT 经中继实跑** + 直连穿透验证（当前只验证回环与不可达场景）；
2. **SYNC-002 outbox 批执行**（常驻循环已有，但待办队列批执行未实现）；
3. **PLATFORM-007~010**（通知/生命周期/桌面退出）+ 平台密钥槽接 OS 凭据库（DPAPI 已可用，需在非 Windows 有明确回退策略）；
4. **插件 Kotlin 宿主（P4）**：JAR 加载、安装事务、信任确认、命令/消息操作；
5. **PRESENTATION 用例层收口**：把投影/检查收敛成完整 UseCases + 界面接线；
6. 群治理剩余：禁言/群名随提交传播、角色变更的节点元数据对齐（DOMAIN-004 候选与节点语义一致）。

## 2026-10-19 快照刷新（第二批 32 轮结束后）

- **Rust 测试**：workspace **964 通过 / 0 失败**（较本轮批开始时的 577 增加 387）。
- **Kotlin/Compose**：`presentationChecks` **22 组**（新增身份恢复提示、附件卡片空列表断言）；竖直冒烟 **10 条**（本机存储 / 身份 / 身份持久化 / 附件 / 平台密钥槽 / DPAPI / 群 / 平台通知 / 插件 / 预览）。
- **构建**：Gradle 桌面 10 任务绿、Android APK 绿、`tools/verify-project.cjs` PASS、`tools/check-disk.ps1` 可用。
- **迁移**：storage 0001–0011、node-services 0001–0008。

### 与上一版快照相比已完成的关键能力

| 能力 | 状态 |
|---|---|
| 附件跨设备解密 | 描述符走受 MLS 加密的协议字段 57（v2 尾部）+ **MLS exporter** 派生包装密钥；全新库可独立解密 |
| 附件元数据兼容 | 字段 57 v1 前缀逐字节不变、v2 尾部追加 display_name；未登记版本显式拒绝 |
| 附件回收 | （本轮在做）两阶段 GC + 崩溃恢复 |
| 自移除 | MLS 禁止自移除提交 → Remove 提案 + 对端打包提交（端到端可用，需上层转发） |
| 群策略同步 | 角色/禁言/群名随控制提交同步（先授权再合并、失败零写入）；改名/禁言会**自动准备提交** |
| 入群邀请 | `accept_group_invite` 元数据一致性校验 + 零写入拒绝 + 凭据目录 |
| 成员凭据 | 协议字段 59/60；同步 apply 不再把 invite/remove 一律跳过 |
| 凭据目录 | 迁移 0011；`resolve_author_account` 真实映射 + `unknown_authors` 计数 |
| 身份恢复 | argon2id+KCV+AEAD 导出/导入（不含主密钥），FFI + 界面导出/导入 |
| 插件宿主 | Rust 逻辑 + FFI + 持久化（迁移 0010）+ Kotlin 桥 + 面板 + `pluginSmoke`；**仍不执行插件代码** |
| 能力令牌 | 强制模式（附件块 + 可选读端点）、撤销表 + 授权策略（self_only）、client 自适应取令牌 |
| 同步 | 拉取 worker + apply 进 MLS（游标只在 apply 后前进、重放幂等）+ 常驻运行时（阶段 0 outbox → 阶段 1 pull）+ outbox 批执行 |
| 平台 | DPAPI 密钥槽、系统通知（无托盘显式不可用）、生命周期状态机、文件选择 |
| 传输 | 纯逻辑核心 + 真实回环 iroh + LAN 绑定与真实网卡互连验证；**经中继/跨网段仍未验证** |

### 仍未覆盖（下一批优先级）

1. **插件代码执行（P4）**：JVM 插件加载、信任确认、安装事务、命令/消息操作接线。
2. **传输经中继实跑 + 跨网段/NAT**：需要 relay 或第二台设备。
3. **PRESENTATION 完整用例层**：群/附件/同步/插件已有个别用例，尚未收敛成界面唯一入口。
4. **平台 OS 凭据库**（Credential Manager/keychain）与后台收消息通知接线。
5. **两阶段附件 GC 的 outbox/job 引用**（本轮在做）与强杀恢复演练。
6. **群成员凭据真实性**：字段 59/60 只承载文本，身份绑定仍依赖 MLS + 凭据目录交叉校验。


## 本批次（32 轮）收尾结论（2026-10-19）

- **Rust 测试**：577 → **977**（+400，0 失败）
- **Gradle 任务**：10 → **12 个**（新增 p0Smoke/p0Snapshot/previewSnapshot 纳入全量回归），全绿
- **竖直冒烟**：3 → **10 条**（本机存储 / 身份 / 身份持久化 / 附件 / 密钥槽 / DPAPI / 群 / 平台通知 / 插件 / 预览）
- **文档**：DEEPSEEK_WORK_PLAN R-50~R-134 全记录；17 模块文档逐轮更新；P0_CHECKLIST 增局部实验盘点；COVERAGE_SNAPSHOT 两次刷新；新增 `tools/check-disk.ps1`

### 本批完成的主要能力（相对批次开始）

1. 附件：跨设备解密（协议字段 57 + MLS exporter）、显示名、元数据版本兼容、两阶段回收 + 常驻运行时 GC
2. 群：转让/退出/解散、自移除提案全链路、角色/禁言/群名随提交同步、提案投递与提交侧权限、入群邀请接受、群列表 + 清单摘要
3. 同步：拉取 worker 应用进 MLS（游标只在 apply 后前进、重放幂等）、常驻运行时（outbox → GC → pull）、outbox 批执行、凭据目录真实作者映射
4. 协议：领域操作声明 55/56、附件 metadata 57、成员凭据 59/60、通用提案值 9..=13（全部只增不改，固定向量持续通过）
5. 节点：能力令牌（签发/强制/撤销/授权策略）、TTL 回收、过期公告与密钥包清理、apps/node CLI 接线
6. 身份：恢复文件（argon2id+KCV+AEAD）导出/导入、FFI 与界面
7. 平台：DPAPI 密钥槽、系统通知（诚实不可用）、生命周期、文件选择
8. 插件：Rust 宿主核心 + FFI + 持久化 + Kotlin 桥 + 面板 + 冒烟（**仍不执行插件代码**）
9. 传输：LAN 绑定与真实网卡互连验证（**经中继/跨网段仍未验证**）

### 仍未完成（下一批 backlog，按优先级）

1. **插件代码执行（P4）**：JVM 插件加载、信任确认、安装事务、命令/消息操作 —— 最大缺口
2. **传输经中继实跑 + 跨网段/NAT + 连接恢复** —— 需 relay 或第二台设备
3. **PRESENTATION 完整用例层**：把分散用例收敛成界面唯一入口
4. **平台 OS 凭据库 + 后台收消息通知接线**
5. **多节点撤销同步** 与撤销记录保留期清理
6. **凭据目录与真实身份账户交叉校验**（当前映射仍是确定性派生）


## 2026-10-19 第 51 轮补充（P4 与传输策略层）

- **PLUGINS**：Rust 包结构校验（`inspect_plugin_package`，9 个稳定码）+ FFI + **P4 JVM 宿主真加载执行**（trusted 门、有界执行、unload 清理）+ 面板全链路 + `pluginJvmSmoke`（现场编译插件 JAR 执行）→ PLUGINS 从"只到宿主逻辑"推进到"**能真跑插件代码**（无沙箱，需用户确认）"。
- **TRANSPORT**：新增 `reconnect.rs` 策略层（RetryPolicy + 确定性抖动 + 状态机），**真实重连仍未接入 Session**；经中继/跨网段仍未验证。
- **测试**：Rust 994；Gradle 13 个任务（含 p0Smoke/p0Snapshot/previewSnapshot/pluginSmoke/pluginJvmSmoke）。
- **事故记录**：两次"脚本/整文件写"导致源码截断（`node-services/src/group.rs`、`transport/src/session.rs`），均已 git 恢复；此后所有任务都加了硬性规则（只新增文件 + 单行插入 + 定点 edit）。


## 最终基线（2026-10-19，第 48 轮收尾）

| 指标 | 数值 |
|---|---|
| Rust 测试 | **994 passed / 0 failed** |
| Gradle 任务 | **14 个**（含 p0Smoke/p0Snapshot/previewSnapshot/pluginSmoke/pluginJvmSmoke） |
| Android | assembleDebug 成功 |
| 迁移 | storage 0001–0012、node-services 0001–0008 |
| 核对 | `tools/verify-project.cjs` PASS、`tools/check-disk.ps1` OK |

### 距"功能完整（A 口径）"还差什么

1. **插件沙箱与安装事务**：当前同 JVM 无沙箱（trusted 仅确认），缺 JAR 安装事务/版本升级/权限清单执行
2. **传输真实重连 + 经中继 + 跨网段**：只有策略层（RetryPolicy/抖动/状态机），Session 未接入；中继与 NAT 未验证
3. **PRESENTATION 用例层收敛**：界面仍以面板/样例为主，未收敛为单一用例入口
4. **OS 凭据库 + 后台收消息通知接线**
5. 多节点撤销同步、凭据目录与真实身份账户交叉校验

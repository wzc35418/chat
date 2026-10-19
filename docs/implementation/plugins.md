# 可信桌面插件、配置包、SDK 与示例

阶段：P4。状态：Kotlin 宿主生命周期（JAR 加载/安装事务/信任确认/主题配置，PLUGINS-001~013 的宿主侧）尚未实现；Rust **纯逻辑宿主核心**已实现于 [crates/plugins](../../crates/plugins)（清单 schema 校验、扩展点注册/分发、每插件私有命名空间存储，见下方「Rust 纯逻辑插件宿主核心」章节）。Kotlin 目标源码：[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)。

MVP 只有声明式配置与用户明确选择可信的 JVM 插件。SDK 权限是正常插件约定，ClassLoader 不提供恶意代码沙箱。插件安装升级下次启动生效；无后台全群监听或自动发送。包清单 schema 已建，两个示例仍需 P4 打包与宿主集成。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [PLUGINS-001 inspect_plugin_package](plugins.md#plugins-001)
- [PLUGINS-002 install_trusted_plugin](plugins.md#plugins-002)
- [PLUGINS-003 discover_plugins](plugins.md#plugins-003)
- [PLUGINS-004 start_plugin](plugins.md#plugins-004)
- [PLUGINS-005 register_plugin_command](plugins.md#plugins-005)
- [PLUGINS-006 register_message_action](plugins.md#plugins-006)
- [PLUGINS-007 selected_content](plugins.md#plugins-007)
- [PLUGINS-008 propose_plugin_send](plugins.md#plugins-008)
- [PLUGINS-009 render_plugin_settings](plugins.md#plugins-009)
- [PLUGINS-010 stop_plugin](plugins.md#plugins-010)
- [PLUGINS-011 disable_plugin](plugins.md#plugins-011)
- [PLUGINS-012 remove_plugin](plugins.md#plugins-012)
- [PLUGINS-013 load_theme_config](plugins.md#plugins-013)

## Rust 纯逻辑插件宿主核心（chat-plugins，本轮实现）

本轮只实现可独立验证的**纯逻辑层**：不引入 JVM/WASM 运行时，不解压 ZIP，不联网，无 `unsafe`，不执行任何插件代码。crate 为 `crates/plugins`（包名 `chat-plugins`，edition 2024，`#![forbid(unsafe_code)]`），是 workspace 成员。

### 覆盖范围与文档函数的关系

| 本轮交付 | API | 对应文档契约 |
|---|---|---|
| 清单解析/校验（插件 ID、版本、SDK/平台、所需能力、可注册扩展点、权限声明、入口类名） | `chat_plugins::parse_manifest` | PLUGINS-001 处理步骤 3「schema 验清单」（ZIP 读取/路径穿越/解压炸弹/签名核对未实现） |
| 扩展点注册/注销/按扩展点查询 | `ExtensionPointRegistry`、`PluginHost::start/stop` | PLUGINS-002/003 的注册表子步骤（包复制、原子安装、目录扫描、ClassLoader 启动未实现） |
| 调用分发（输入/输出上限、超时预算、幂等 operationId） | `PluginHost::dispatch` / `finish_dispatch` | PLUGINS-002/003 的调用分发子步骤；命令/消息操作的真实定义与执行分别在 PLUGINS-005/006 与 P4 宿主，未实现 |
| 每插件命名空间键值隔离 | `PluginHost::data_*`、`PluginStorage` | PLUGINS-004 运行上下文与 PLUGINS-009 的插件私有偏好存储 |

真正的 JAR 加载、信任模式选择、安装事务（staging/原子目录/重启生效）、安全模式发现、主题配置、UserActionToken、命令/消息操作定义、发送提案与设置表单渲染**仍未实现**，因此本文件中 PLUGINS-001~013 的「完成验收」勾选保持未勾。

### 清单 schema（`schemaVersion: 1`）

```json
{
  "schemaVersion": 1,
  "id": "notes.v1",
  "version": "1.2.3",
  "sdkVersion": 1,
  "platform": "desktop",
  "capabilities": ["PrivateStorage", "Command"],
  "permissions": ["UserSelectedContent"],
  "extensionPoints": ["Command"],
  "entryClass": "com.example.notes.NotesPlugin"
}
```

- **ID**：1–64 字节 ASCII，2–4 段点分；每段 1–32 字节，首字符小写字母，其余 `[a-z0-9-]` 且不以 `-` 结尾；保留首段 `core`/`sys`/`chat` 拒绝。
- **version**：恰好三段十进制，无前导零，每段 ≤ 65535。
- **sdkVersion**：必须等于宿主 `CURRENT_SDK_VERSION = 1`，否则 `SDK_INCOMPATIBLE`；`platform` 只接受 `desktop`，其它为 `UNSUPPORTED_PLATFORM`。
- **capabilities**：`PrivateStorage`/`Command`/`MessageAction`/`SettingsPanel`；数量受 `maxCapabilities`（默认 16）限制，重复/未知显式拒绝。
- **permissions**：`UserSelectedContent`/`ComposeDraft`（最多 4 个）；仅声明，不是沙箱。
- **extensionPoints**：`Command`/`MessageAction`/`SettingsPanel`；每个扩展点必须同时声明其所需能力，否则 `CAPABILITY_NOT_DECLARED`；未知扩展点 `UNKNOWN_EXTENSION_POINT`，重复声明拒绝。
- **entryClass**：1–256 字节 ASCII 点分 Java 类名，仅 `[A-Za-z0-9_.$]`，拒绝 `/`、`\`、`..`、首尾 `.`。
- JSON 使用 `deny_unknown_fields`：未知字段与重复字段都拒绝；清单总长受 `maxManifestBytes`（默认 64 KiB）限制。

### 生命周期、幂等与预算

| 操作 | 语义 | 失败码 |
|---|---|---|
| `install_json` / `install` | 解析并安装；同一 ID 再次安装显式拒绝（本切片不做升级） | `PLUGIN_ID_CONFLICT`、`INVALID_PLUGIN_PACKAGE`、`CONTENT_TOO_LARGE` |
| `start` | 按清单注册扩展点并签发 `PluginRuntime`；重复启动幂等，不重复注册；未安装拒绝 | `PLUGIN_NOT_INSTALLED` |
| `dispatch` | 校验运行状态、该插件是否注册该扩展点、输入 ≤ `maxInputBytes`、截止时间未到；返回 `DispatchPlan`（实际执行由宿主负责）；同一 `operationId` 的相同请求幂等重放，不同请求冲突 | `PLUGIN_NOT_RUNNING`、`PLUGIN_NOT_REGISTERED`、`CONTENT_TOO_LARGE`、`PLUGIN_TASK_TIMEOUT`、`PLUGIN_OPERATION_CONFLICT` |
| `finish_dispatch` | 校验输出 ≤ `maxOutputBytes`、未超过生效截止时间；相同输出重复完成幂等，不同输出冲突；未开始/已被清理的 operation 拒绝 | `CONTENT_TOO_LARGE`、`PLUGIN_TASK_TIMEOUT`、`PLUGIN_OPERATION_CONFLICT`、`PLUGIN_OPERATION_UNKNOWN` |
| `stop` | 撤销全部注册项并清空该插件在途 operation；可重复，未安装拒绝 | `PLUGIN_NOT_INSTALLED` |
| `uninstall` | 停止后按选择保留/删除私有数据；重复卸载幂等 | `PLUGIN_NOT_INSTALLED`（仅 stop） |

- **超时预算**：请求截止时间早于等于当前时间 → `PLUGIN_TASK_TIMEOUT`；截止时间超出 `maxTimeoutMs` 时**只收紧不放宽**（生效截止时间 = now + maxTimeoutMs）。
- **幂等缓存**是有界的：默认 64 条，超过后按 FIFO 淘汰最旧 operation；`stop`/注销会清空该插件在途 operation。
- **私有存储隔离**：所有读写都以宿主签发的 `PluginRuntime` 绑定 ID 作为分区；同一键在不同插件下互不可见；键 ≤128 字节、值 ≤8 KiB、每插件 ≤256 条 / ≤64 KiB；超限失败**不产生部分写入**。
- 所有预算由 `PluginLimits` 配置，可小于默认值但不得超过硬上限（1 MiB / 60 秒 / 1024 条），否则 `INVALID_PLUGIN_LIMITS`。

### 插件层稳定错误码（登记）

新增稳定码必须在协议层登记后才能进入 `chat_protocol::ErrorCode` 冻结码表；本表即插件模块的登记处。

| 码 | 触发 | 可重试 | 与 PROTOCOL-014 关系 |
|---|---|---|---|
| `INVALID_PLUGIN_PACKAGE` | 清单 JSON/字段/ID/入口类名非法 | 否 | 插件新增码，未进冻结表 |
| `PLUGIN_ID_CONFLICT` | 同 ID 重复安装 | 否 | 插件新增码 |
| `SDK_INCOMPATIBLE` | sdkVersion 不匹配 | 否 | 插件新增码 |
| `UNSUPPORTED_PLATFORM` | 平台不是 desktop | 否 | 插件新增码 |
| `INVALID_PLUGIN_VERSION` | 版本字符串非法 | 否 | 插件新增码 |
| `UNKNOWN_EXTENSION_POINT` | 清单声明未知扩展点 | 否 | 插件新增码 |
| `UNKNOWN_CAPABILITY` | 清单声明未知能力 | 否 | 插件新增码 |
| `UNKNOWN_PERMISSION` | 清单声明未知权限 | 否 | 插件新增码 |
| `CAPABILITY_NOT_DECLARED` | 扩展点缺所需能力 / 使用私有存储未声明能力 | 否 | 插件新增码 |
| `PLUGIN_NOT_INSTALLED` | 未安装即启动/停止 | 否 | 插件新增码 |
| `PLUGIN_NOT_RUNNING` | 停止后仍用旧句柄调用 | 否 | 插件新增码 |
| `PLUGIN_NOT_REGISTERED` | 未注册该扩展点（含注销后调用） | 否 | 插件新增码 |
| `PLUGIN_STATE_CONFLICT` | 同插件注册集合冲突 | 否 | 插件新增码 |
| `PLUGIN_OPERATION_CONFLICT` | 同 operationId 绑定不同请求/输出 | 否 | 插件新增码 |
| `PLUGIN_OPERATION_UNKNOWN` | operation 未开始或已清理 | 否 | 插件新增码 |
| `PLUGIN_TASK_TIMEOUT` | 截止时间已到/已超时 | 否 | 插件新增码 |
| `CONTENT_TOO_LARGE` | 清单/输入/输出/存储值超限 | 否 | **复用已冻结码**（`ErrorCode::ContentTooLarge`，`frozen_code()` 可映射） |
| `INVALID_STORAGE_KEY` | 存储键为空/超长/字符非法 | 否 | 插件新增码 |
| `PLUGIN_STORAGE_QUOTA_EXCEEDED` | 私有存储条目/字节超配额 | 否 | 插件新增码 |
| `INVALID_PLUGIN_LIMITS` | 宿主预算为 0 或超硬上限 | 否 | 插件新增码 |

映射约定：插件错误一律不可自动重试；跨 FFI 暴露时，已有冻结码的 `CONTENT_TOO_LARGE` 可直接进入 `chat_protocol::map_error`，其余在协议层登记前映射为 `INTERNAL_FAILURE` 并保留本层码做诊断，禁止伪装成网络类可重试错误。

### 验证证据

`cargo test --offline -p chat-plugins`（2026-10-18）→ **36 passed / 0 failed**（lib 19 + `tests/host_lifecycle.rs` 17），`cargo clippy --offline -p chat-plugins --all-targets` 0 警告。

- 清单：`manifest::tests::*`（非法 ID/版本、未知/重复字段、未知能力/权限/扩展点、缺能力、SDK/平台不兼容、入口类名、数量上限）。
- 注册表：`registry::tests::repeated_registration_is_idempotent`、`conflicting_registration_set_is_rejected`、`unregister_is_idempotent_and_removes_all_points`、`query_returns_sorted_plugins_per_point`。
- 生命周期：`host_lifecycle::start_registers_points_and_repeated_start_is_idempotent`、`stop_revokes_registrations_and_stale_runtime_is_rejected`、`uninstall_is_idempotent_and_can_keep_or_remove_data`、`start_and_stop_require_installed_plugin`。
- 分发与边界：`dispatch_returns_bounded_plan_and_rejects_unregistered_points`、`input_and_output_sizes_are_bounded`、`timeout_budget_is_expired_and_clamped`、`operation_id_replay_is_idempotent_and_conflicts_are_rejected`、`stop_clears_in_flight_operations`、`a_plugin_cannot_finish_another_plugins_operation`。
- 隔离与配额：`storage_is_isolated_per_plugin`、`storage_requires_declared_capability`、`storage_keys_values_and_quotas_are_enforced`、`storage::tests::quota_failure_does_not_create_partial_state`。
- 错误码：`error::tests::codes_are_unique_and_stable`、`only_content_too_large_has_a_frozen_protocol_code`。

<a id="plugins-001"></a>

## PLUGINS-001 · inspect_plugin_package

~~~kotlin
suspend fun inspectPluginPackage(source: FileToken): PluginInspection
~~~

**职责与输出**：给用户展示包来源、摘要、权限和兼容性，尚不执行代码。

**输入与前置条件**：用户选择单一本地包；ZIP 条目数/解压量/压缩比有预算。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 有界读包。
2. 拒绝绝对路径/../符号链接/重复路径。
3. schema 验清单。
4. 验 jar 摘要和可选签名。
5. 核对 SDK/平台。
6. 输出可信模式提示。

**持久化、并发与副作用**：只读暂存；不初始化 ClassLoader，不运行入口。

**失败处理**：INVALID_PLUGIN_PACKAGE，ZIP_BOMB，SDK_INCOMPATIBLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLATFORM-006 open_file_token](platform.md#platform-006)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 路径穿越/解压炸弹拒绝。
- [ ] 签名存在不被描述成安全保证。

**实现状态**：部分实现（仅 Rust 纯逻辑子集：清单 schema 解析/校验，见上方「Rust 纯逻辑插件宿主核心」，证据 `cargo test --offline -p chat-plugins` → 36 passed，`manifest::tests::*`）。ZIP 有界读取、绝对路径/`../`/符号链接/重复条目拒绝、解压炸弹预算、jar 摘要与可选签名、SDK/平台核对提示均未实现，因此两项验收未勾选。

<a id="plugins-002"></a>

## PLUGINS-002 · install_trusted_plugin

~~~kotlin
suspend fun installTrustedPlugin(inspection: PluginInspection, choice: TrustedModeChoice): InstallTicket
~~~

**职责与输出**：复制到版本化插件目录并记录下次重启启用。

**输入与前置条件**：用户明确选择“具有应用级权限”；inspection 摘要与实际包匹配。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 重新核对来源摘要。
2. 检查重复 ID/升级策略。
3. 写 staging。
4. 原子完成包目录。
5. 保存 pending install。
6. 返回需重启。

**持久化、并发与副作用**：文件/配置两阶段恢复标记；不在安装动作执行 JAR。

**失败处理**：TRUST_CONFIRMATION_REQUIRED，PLUGIN_ID_CONFLICT，FILE_IO_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLUGINS-001 inspect_plugin_package](plugins.md#plugins-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 包在检查后替换拒绝。
- [ ] 安装中崩溃不启用半包。

**实现状态**：部分实现（Rust 纯逻辑子集：重复插件 ID 显式拒绝 `PLUGIN_ID_CONFLICT`；扩展点集合重复注册幂等、冲突 `PLUGIN_STATE_CONFLICT`；卸载幂等，证据见上方章节与 `host_lifecycle::install_rejects_duplicates_and_invalid_manifests`、`registry::tests::repeated_registration_is_idempotent`）。staging 复制、原子完成包目录、pending install、重启生效与来源摘要复核未实现，验收项未勾选。

<a id="plugins-003"></a>

## PLUGINS-003 · discover_plugins

~~~kotlin
suspend fun discoverPlugins(safeMode: Boolean): PluginCatalog
~~~

**职责与输出**：安全启动跳过第三方代码，正常启动列兼容启用包。

**输入与前置条件**：宿主版本已锁定；只扫描应用插件目录。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取安装状态。
2. 检查完整性和禁用标志。
3. safeMode 跳过执行。
4. 检查入口兼容。
5. 输出懒加载目录。

**持久化、并发与副作用**：启动只读，不为每插件常驻新线程池。

**失败处理**：PLUGIN_CORRUPTED，SDK_INCOMPATIBLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 安全模式下恶意/崩溃插件入口不会调用。
- [ ] 不兼容保留可移除记录。

**实现状态**：部分实现（Rust 纯逻辑子集：安装后按扩展点查询 `ExtensionPointRegistry::query`、启动时按清单注册；证据 `host_lifecycle::start_registers_points_and_repeated_start_is_idempotent`）。安装目录扫描、禁用标记、safeMode 跳过执行、入口兼容检查、懒加载目录与可移除记录未实现，验收项未勾选。

<a id="plugins-004"></a>

## PLUGINS-004 · start_plugin

~~~kotlin
suspend fun startPlugin(id: PluginId): PluginRuntime
~~~

**职责与输出**：按首次命令/操作懒加载并注册受控资源。

**输入与前置条件**：包已用户信任、启用且兼容；不能在 UI 线程运行未知初始化。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 加载隔离依赖 ClassLoader。
2. 创建插件作用域/上下文。
3. 调用 start。
4. 登记全部 Registration。
5. 失败清理并记录故障。

**持久化、并发与副作用**：宿主所有权跟踪；JVM 恶意线程不能保证强制停止。

**失败处理**：PLUGIN_START_FAILED，SDK_INCOMPATIBLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLUGINS-005 register_plugin_command](plugins.md#plugins-005)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 初始化异常无残留菜单。
- [ ] 重复 start 不重复注册。
- [ ] 共享依赖冲突可诊断。

**实现状态**：部分实现（Rust 纯逻辑子集：`PluginHost::start` 重复启动幂等且不重复注册；`PluginRuntime` 绑定插件 ID；私有命名空间存储隔离与配额，证据 `start_registers_points_and_repeated_start_is_idempotent`、`storage_is_isolated_per_plugin`、`storage_keys_values_and_quotas_are_enforced`）。JAR ClassLoader 加载、start 回调、Registration 所有权追踪、初始化异常清理、共享依赖冲突诊断未实现，验收项未勾选。

<a id="plugins-005"></a>

## PLUGINS-005 · register_plugin_command

~~~kotlin
fun registerPluginCommand(owner: PluginId, definition: CommandDefinition): Registration
~~~

**职责与输出**：登记唯一命令并返回幂等撤销对象。

**输入与前置条件**：命令命名空间归 owner，描述/参数有界。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对声明能力。
2. 校验命令名和重复项。
3. 登记 owner。
4. 包装执行上下文。
5. 返回 dispose。

**持久化、并发与副作用**：注册表线程安全；stop 按 owner 全量清理。

**失败处理**：DUPLICATE_COMMAND，CAPABILITY_NOT_DECLARED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 同名跨插件按 namespace 区分。
- [ ] dispose 两次无异常。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="plugins-006"></a>

## PLUGINS-006 · register_message_action

~~~kotlin
fun registerMessageAction(owner: PluginId, action: MessageActionDefinition): Registration
~~~

**职责与输出**：登记用户选择消息后可见操作。

**输入与前置条件**：SDK 不暴露全库读取，操作需要 UserActionToken。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证描述/能力。
2. 登记 handler。
3. 执行时签发短期选择 token。
4. 由宿主附所选会话上下文。

**持久化、并发与副作用**：资源归 owner；失活 token 不可后台延长。

**失败处理**：INVALID_ACTION，CAPABILITY_NOT_DECLARED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 没有选中内容时不可执行。
- [ ] stop 后菜单消失。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="plugins-007"></a>

## PLUGINS-007 · selected_content

~~~kotlin
suspend fun selectedContent(owner: PluginId, token: UserActionToken): SelectedContent
~~~

**职责与输出**：返回本次用户明确选择的最小明文内容。

**输入与前置条件**：token 绑定 plugin/会话/selection/期限/动作；用户已触发。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证 token。
2. 核对 owner 和选择未失效。
3. 取得受限内容。
4. 按预算返回副本。
5. 记录脱敏访问诊断。

**持久化、并发与副作用**：只读；不返回历史分页 API；可信插件仍具进程权限。

**失败处理**：USER_ACTION_EXPIRED，PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 跨插件复用 token 拒绝。
- [ ] 切会话后旧 token 无效。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="plugins-008"></a>

## PLUGINS-008 · propose_plugin_send

~~~kotlin
suspend fun proposePluginSend(owner: PluginId, token: UserActionToken, content: MessageContent): SendProposal
~~~

**职责与输出**：填编辑区并标来源，用户确认后走正常 send。

**输入与前置条件**：有效用户动作与 draft.propose 能力；内容通过普通限制。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对 token/会话。
2. 校验类型/大小。
3. 设置 MessageOrigin.Plugin。
4. 交 presentation 编辑区。
5. 等待显式确认。

**持久化、并发与副作用**：不创建直接发送 outbox；确认时用正常 core 权限和 operationId。

**失败处理**：USER_ACTION_EXPIRED，CONTENT_TOO_LARGE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-010 validate_extension](protocol.md#protocol-010)、[PRESENTATION-004 submit_composer](presentation.md#presentation-004)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 被禁言插件不能经 SDK 发言。
- [ ] 一提案不引发另一个自动回复循环。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="plugins-009"></a>

## PLUGINS-009 · render_plugin_settings

~~~kotlin
fun renderPluginSettings(schema: SettingsSchema, values: PluginValues): SettingsForm
~~~

**职责与输出**：宿主渲染有限表单，插件不直接挂 Compose ABI。

**输入与前置条件**：schema 有控件类型/长度/数量限制；仅插件私有偏好。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证 schema。
2. 映射宿主控件。
3. 安全显示描述。
4. 变更校验后保存插件私有配置。
5. 不暴露核心 DB。

**持久化、并发与副作用**：独立配置事务；不把群共享配置当成本机权限授权。

**失败处理**：INVALID_SETTINGS_SCHEMA。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 未知控件拒绝。
- [ ] 敏感字段不日志。
- [ ] 插件A不能读B偏好。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="plugins-010"></a>

## PLUGINS-010 · stop_plugin

~~~kotlin
suspend fun stopPlugin(id: PluginId, deadline: Duration): PluginStopReport
~~~

**职责与输出**：撤销注册项、合作式取消任务并记录未配合资源。

**输入与前置条件**：插件可能 start 半途失败；stop 可重复。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 标 stopping。
2. 调用插件 stop。
3. 取消插件 scope。
4. dispose 全部命令/操作。
5. 清空 token。
6. 记录泄漏/异常。
7. 建议重启。

**持久化、并发与副作用**：不宣称卸载全部 JVM 类或终止恶意线程；超时不冻结 UI。

**失败处理**：PLUGIN_STOP_FAILED，PLUGIN_TASK_TIMEOUT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 正常示例 stop 后无菜单/任务。
- [ ] 异常 stop 仍由宿主清理注册表。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="plugins-011"></a>

## PLUGINS-011 · disable_plugin

~~~kotlin
suspend fun disablePlugin(id: PluginId): PluginState
~~~

**职责与输出**：保存禁用并立即清注册，完整停用重启保证。

**输入与前置条件**：用户显式禁用；不会删除插件私有数据除非明确移除。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 保存 disabled。
2. 停止活动实例。
3. 标 restart_required。
4. 刷新目录。
5. 保留故障记录。

**持久化、并发与副作用**：配置先耐久，stop 失败下次启动仍不加载。

**失败处理**：PLUGIN_STOP_FAILED，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLUGINS-010 stop_plugin](plugins.md#plugins-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 崩溃后仍 disabled。
- [ ] 安全启动可移除坏包。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="plugins-012"></a>

## PLUGINS-012 · remove_plugin

~~~kotlin
suspend fun removePlugin(id: PluginId, removeData: Boolean): RemovalTicket
~~~

**职责与输出**：停止后标记删除包，JAR占用时下次启动完成。

**输入与前置条件**：用户明确移除；removeData 单独选择。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 耐久标 pending removal。
2. 停止实例。
3. 尝试受控目录删除。
4. 锁定文件延后。
5. 按选择清私有配置。

**持久化、并发与副作用**：删除 journal，路径必须属于插件安装根；不会删除核心消息库。

**失败处理**：PLUGIN_FILES_IN_USE，FILE_IO_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLUGINS-010 stop_plugin](plugins.md#plugins-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] Windows JAR 锁定显示待重启。
- [ ] 重复移除幂等。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="plugins-013"></a>

## PLUGINS-013 · load_theme_config

~~~kotlin
suspend fun loadThemeConfig(source: FileToken): ThemeConfig
~~~

**职责与输出**：只读取声明式主题和快捷键，不加载可执行代码。

**输入与前置条件**：支持 schema 版本和键白名单；文件体积有上限。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 有界读取 JSON。
2. 检查颜色/字体/快捷键范围。
3. 拒绝外部脚本/任意资源路径。
4. 预览。
5. 用户应用后保存。

**持久化、并发与副作用**：主题配置独立保存；错误保留当前主题。

**失败处理**：INVALID_THEME_CONFIG，SHORTCUT_CONFLICT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLATFORM-006 open_file_token](platform.md#platform-006)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 配置包不能包含 JAR 执行。
- [ ] 颜色非法回滚。
- [ ] 快捷键冲突可见。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

## 插件宿主接 FFI（PluginsFfi，2026-10-18，Windows x64）

背景：`chat-plugins` 的纯逻辑宿主（清单校验/注册分发/命名空间隔离/幂等，36 个测试）此前 Kotlin
完全用不到。本轮把它接到 FFI。

代码：[crates/ffi/src/plugins_ffi.rs](../../crates/ffi/src/plugins_ffi.rs)（新）+ lib.rs 一行注册；测试：
[crates/ffi/tests/plugins_ffi.rs](../../crates/ffi/tests/plugins_ffi.rs)（7 个，**纯内存**）。契约细节见
[bridge.md 的“插件宿主 FFI”](bridge.md)。

### 公开面（Kotlin 可见）

~~~text
PluginsFfi::new() -> Arc<PluginsFfi>          // 纯内存，每个实例一个独立宿主状态
manifest_validate(manifest_json) -> ManifestViewFfi
install(manifest_json) -> String               // 返回插件 ID
start(plugin_id) -> ()
dispatch(plugin_id, extension_point, operation_id, input, now_unix_ms, deadline_unix_ms)
    -> DispatchPlanViewFfi                     // 只出计划，不执行插件代码
finish_dispatch(plugin_id, operation_id, output, now_unix_ms) -> ()
stop(plugin_id) -> bool      uninstall(plugin_id, remove_data) -> bool
storage_get(plugin_id, key) -> Vec<u8>?      storage_set(plugin_id, key, value) -> ()
list_plugins() -> Vec<PluginInfoFfi>          is_open() -> bool      shutdown()
~~~

### 关键语义

1. **纯内存、不执行插件代码**：`dispatch` 只产出受预算约束的分发计划，`finish_dispatch` 只校验
   输出大小/截止时间/幂等；JAR/WASM/JVM 执行属后续宿主阶段。
2. **错误码原样透出**：`FfiError::Store { code }` 就是 `chat-plugins` 的稳定码
   （`PLUGIN_ID_CONFLICT` / `PLUGIN_NOT_RUNNING` / `PLUGIN_NOT_REGISTERED` /
   `CAPABILITY_NOT_DECLARED` / `CONTENT_TOO_LARGE` / `PLUGIN_OPERATION_CONFLICT` /
   `PLUGIN_OPERATION_UNKNOWN` / `UNKNOWN_EXTENSION_POINT` / `CORE_CLOSED` …）。
3. **运行时句柄由本对象持有**：`start` 之后才允许 dispatch/finish/存储访问；
   `stop`/`uninstall`/`shutdown` 丢弃句柄。
4. **存储隔离与能力门**：读写都要声明 `PrivateStorage`；每个插件只看自己的命名空间。

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi --test plugins_ffi   # 7 passed
cargo test --offline -p chat-ffi                       # 53 passed（既有 46 继续通过）
~~~

### 证据

- `plugins_ffi::install_start_dispatch_finish_stop_uninstall`：校验不安装 → 安装 → 启动（running）→
  dispatch 计划（input_bytes/deadline/timeout 正确）→ finish（同输出幂等）→ 停止后 dispatch →
  `PLUGIN_NOT_RUNNING` → 卸载清空。
- `plugins_ffi::duplicate_install_conflicts` → `PLUGIN_ID_CONFLICT`。
- `plugins_ffi::storage_requires_the_declared_capability` → `CAPABILITY_NOT_DECLARED`。
- `plugins_ffi::storage_is_isolated_between_plugins`：同名键互不可见；卸载其一（保留数据）不影响另一个。
- `plugins_ffi::oversized_input_and_output_are_rejected` → `CONTENT_TOO_LARGE`。
- `plugins_ffi::dispatch_is_idempotent_and_conflicts_are_explicit`：同 operation 同输入幂等；
  不同输入 → `PLUGIN_OPERATION_CONFLICT`；未知 operation → `PLUGIN_OPERATION_UNKNOWN`；
  未注册扩展点 → `PLUGIN_NOT_REGISTERED`；未知扩展点名 → `UNKNOWN_EXTENSION_POINT`。
- `plugins_ffi::shutdown_is_idempotent_and_closes_the_object`：关闭后 `CORE_CLOSED`、再关无副作用。

### 已知限制

1. **不加载/不执行插件代码**（没有 JAR 加载、WASM/JVM 运行时、信任确认与安装事务）。
2. **纯内存**：宿主状态不落库（清单/私有存储持久化未接线）。
3. **一次 start 一个句柄**：重启需先 stop/uninstall。
4. **超时只是计划预算**：`dispatch` 只收紧截止时间，不启动计时器。
5. 未知扩展点名 → `UNKNOWN_EXTENSION_POINT`；能力/权限文本未知 → `INVALID_PLUGIN_PACKAGE`。
### 持久化模式（PluginsFfi::open，2026-10-18 续）

~~~text
PluginsFfi::open(database_path, master_key_hex) -> Arc<PluginsFfi>   // 持久化模式
PluginsFfi::new() -> Arc<PluginsFfi>                                  // 纯内存（行为不变）
skipped_on_load() -> u32                                              // 打开时跳过的损坏清单行数
~~~

- 依赖 storage 迁移 **0010_plugins**：`plugins(id, manifest_json, state, installed_at_ms, updated_at_ms)`
  + `plugin_kv(plugin_id, key, value, updated_at_ms)`（值**密封**存储，`SealPurpose::PluginKv`）；
- 打开时把库里已安装的清单重新装回宿主；**运行状态不恢复**（重开后在 stopped，需上层再 start）；
- 生命周期与存储都会落库：install → `installed`；start → `running`；stop → `stopped`；
  uninstall(remove_data=true) → 删行（`plugin_kv` 随外键级联清空）；
  uninstall(remove_data=false) → 行标记 `uninstalled`（**KV 保留**，重装后仍可读）；
- 持久化模式下 `storage_get/`storage_set` 直接读写库（能力门仍要求已安装 + 声明 `PrivateStorage`），
  因此**重开后即使没 start 也能读回 KV**；配额由 storage 侧执行（单值 64 KiB / 512 条 / 1 MiB 总量），
  越界映射成 `PLUGIN_STORAGE_QUOTA_EXCEEDED` 且不写入；
- 清单损坏的行在打开时**跳过并计数**（`skipped_on_load`），不静默丢弃；
- `shutdown` 会释放库句柄（独占写者），因此同一个库可以重新 `open`。

证据：[crates/ffi/tests/plugins_ffi_persist.rs](../../crates/ffi/tests/plugins_ffi_persist.rs)（4 个，真临时加密库）：
`install_and_kv_survive_a_reopen`（安装→写 KV→shutdown→重开仍列出、KV 可读、running 不恢复、可再 start 覆盖写）、
`uninstall_with_or_without_data_removal`（false 保留 KV 供重装读取；true 连行一起删、KV 清空）、
`quota_exceeded_is_rejected_and_not_written`（64 KiB+1 → `PLUGIN_STORAGE_QUOTA_EXCEEDED` 且读回为空）、
`corrupted_manifest_rows_are_skipped_and_counted`（损坏行 skip=1，其它插件照常可用）。

持久化模式的已知限制：① 运行状态不恢复（要靠上层重新 start）；② `delete_data=false` 会留下
`uninstalled` 行（KV 占配额直到 true 卸载或重装后清理）；③ 配额是 storage 侧的硬边界，
与纯内存模式宿主自己的配额数值可能不同；④ 一次最多 256 行插件（`MAX_PLUGIN_ROWS`）。
## 插件包结构校验（PLUGINS-PKG，2026-10-18）

真加载插件前先做**不执行代码**的结构校验：新增 [crates/plugins/src/package.rs](../../crates/plugins/src/package.rs)。

### 公开面

~~~text
inspect_plugin_package(bytes, limits) -> PackageInspection
list_package_entries(bytes) -> Vec<PackageEntryView>     // 只读 central directory，不读内容
PackageInspection { plugin_id, plugin_version, entry_class, entry_file, spi_file,
                    entry_count, compressed_bytes, uncompressed_bytes }
const MAX_PACKAGE_BYTES = 8 MiB;  MAX_UNCOMPRESSED_BYTES = 64 MiB;  MAX_ENTRY_COUNT = 4096
const MANIFEST_FILE = "manifest.json";  SPI_DIRECTORY = "META-INF/services/"
~~~

### 校验规则（全部无第三方依赖、纯字节解析）

1. **ZIP 结构**：从尾部找 EOCD（允许 ≤64KiB comment，且 comment 长度必须正好收尾），
   再逐条读 central directory（签名/偏移/长度越界都显式拒绝）；
2. **条目名**：非空、≤512 字节、非绝对路径、无反斜杠、无控制字符、无 `..` 段、无 `:`，且**无重名**；
3. **规模**：包 ≤8 MiB、解压总量 ≤64 MiB、条目 ≤4096（解压总量按 central directory 的**声明值**累计）；
4. **清单**：`manifest.json` 必须存在、是 UTF-8、且能通过 [`parse_manifest`] 的完整清单校验；
5. **入口类**：清单 `entryClass` 必须有对应 `com/example/Plugin.class` 条目；
6. **SPI**：`META-INF/services/<entryClass>` 必须存在，去注释/空行后**每一行都必须等于 entryClass**。

### 稳定码

| 情形 | code |
|---|---|
| ZIP 结构损坏/截断/假 EOCD | `PLUGIN_PACKAGE_CORRUPTED` |
| 包/解压总量/条目数超限 | `PLUGIN_PACKAGE_TOO_LARGE` |
| 没有 `manifest.json` | `PLUGIN_PACKAGE_MISSING_MANIFEST` |
| 入口类缺 `.class` | `PLUGIN_PACKAGE_MISSING_ENTRY` |
| SPI 缺失或实现名不符 | `PLUGIN_PACKAGE_SPI_MISMATCH` |
| 路径穿越/非法条目名 | `PLUGIN_PACKAGE_UNSAFE_PATH` |
| 条目重名 | `PLUGIN_PACKAGE_DUPLICATE_ENTRY` |
| 清单/SPI 用了非 stored 压缩 | `PLUGIN_PACKAGE_UNSUPPORTED_COMPRESSION` |
| 清单/SPI 不是 UTF-8 | `PLUGIN_PACKAGE_NOT_TEXT` |
| 清单内容不合法 | `INVALID_PLUGIN_PACKAGE`（复用清单层码） |

### 证据

[crates/plugins/tests/package.rs](../../crates/plugins/tests/package.rs)（10 个，测试内构造最小 ZIP）：
合法包通过并报告字段；缺清单 / 缺入口类 / SPI 不符 / 缺 SPI / 路径穿越（`../`、绝对路径、`C:/`）/
重名 / 输入 >8 MiB / 条目 >4096 / 声明解压 >64 MiB / 非 stored 清单 / 非 ZIP / 截断 EOCD /
清单非法 / 清单非 UTF-8 / 超长条目名 —— 各自断言对应稳定码。

复现：`cargo test --offline -p chat-plugins --test package` → 10 passed；
`cargo test --offline -p chat-plugins` → 46 passed（既有 36 继续通过）。

### 已知限制

1. **不做 inflate**：只读 stored（method 0）条目的内容，因此清单与 SPI 必须是 stored；
   其它条目可以是 deflate（我们只看尺寸声明，不读内容）。真正的 inflate 与代码加载属于后续阶段。
2. **不校验 CRC**：不读内容的条目无法验 CRC（清单/SPI 读了内容但也未验 CRC）——CRC 只用于传输完整性，
   安全边界是"结构 + 清单 + SPI 一致"，不是加密校验。
3. **不做签名校验**：包签名/信任链不在本轮（P4 的信任确认另行设计）。
4. **ZIP64/多卷/加密 ZIP 未支持**：相关标记会落成 `PLUGIN_PACKAGE_CORRUPTED` / `TOO_LARGE`。
## 插件安装事务（INSTALL，2026-10-18）

真加载插件前的第二步：把"校验过的包"原子地装进宿主。新增 [crates/plugins/src/install.rs](../../crates/plugins/src/install.rs)。

### 公开面

~~~text
InstallTransaction::begin(package_bytes, limits) -> Result<InstallTransaction>
transaction.inspection() / manifest() / limits() / is_staged() / is_finished()
transaction.stage() -> Result<()>            // 幂等；宿主仍未改变
transaction.commit(&mut PluginHost) -> Result<InstallOutcome>
transaction.rollback()                       // 放弃；宿主不受影响
InstallOutcome::Installed { plugin_id, version }
InstallOutcome::Upgraded  { plugin_id, from, to }
~~~

### 版本语义（commit 时判定）

| 情形 | 结果 |
|---|---|
| 未安装 | `InstallOutcome::Installed` |
| 包版本 **高于**已装 | `InstallOutcome::Upgraded { from, to }`（运行中的插件升级后**仍然运行**） |
| 包版本 **等于**已装 | `PLUGIN_STATE_CONFLICT`（本切片不做重装） |
| 包版本 **低于**已装 | `PLUGIN_VERSION_DOWNGRADE`（新稳定码，不做降级） |
| 未 stage / 已结束 | `PLUGIN_PACKAGE_CORRUPTED`（事务状态自洽检查） |

### 原子性

- `begin` 失败（包损坏/缺清单/缺入口/SPI 不符/超限）→ **事务对象根本不存在**，宿主零写入；
- `stage` 只改事务内部状态，宿主零写入；
- `commit` 先停旧运行时（注销扩展点）→ 换清单 → 需要时重新 `start`；
  任一步失败都会把宿主恢复成提交前的状态（旧清单 + 是否运行的运行态），**绝不留下新旧混合**；
- `rollback` 后不能再提交（避免"回滚了却偷偷装进去"）。

### 证据

[crates/plugins/tests/install.rs](../../crates/plugins/tests/install.rs)（8 个，测试内构造最小 ZIP）：
正常安装（暂存阶段宿主干净、提交后可见、事务结束后不可二次提交）；升级返回 from/to；
同版本 → `PLUGIN_STATE_CONFLICT` 且版本不变；降级 → `PLUGIN_VERSION_DOWNGRADE` 且版本不变；
commit 前 rollback → 宿主无插件、注册表无注册项且不能再提交；坏包（缺清单/非 ZIP/SPI 不符）不进事务；
未 stage 直接 commit → 显式失败且宿主干净；运行中的插件升级后仍然运行且扩展点已重新注册。

复现：`cargo test --offline -p chat-plugins --test install` → 8 passed；
`cargo test --offline -p chat-plugins` → **54 passed**（既有 46 继续通过）。

### 已知限制

1. **不做重装**：同版本冲突而不是覆盖安装（需要显式卸载后重装）。
2. **不做签名/信任确认**：包校验只覆盖结构与清单一致性（见上一节的限制）。
3. **升级不迁移私有数据**：storage 命名空间按 plugin id 保留，版本升级不清理/不迁移 KV。
4. **宿主状态在内存**：事务只作用于 `PluginHost`；落库安装事务（与 0010 迁移的插件表联动）留给 FFI 侧后续任务。



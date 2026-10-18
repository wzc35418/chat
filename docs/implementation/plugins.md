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


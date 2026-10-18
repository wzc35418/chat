# 可信桌面插件、配置包、SDK 与示例

阶段：P4。状态：函数级设计，尚未实现。目标源码：[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)。

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

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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


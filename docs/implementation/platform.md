# 密钥保护、文件授权和三端生命周期

阶段：P0 → P2。状态：函数级设计，尚未实现。目标源码：[shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt](../../shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt)。

这里的函数签名为 Kotlin 契约。具体 Android 实现放 apps/android，桌面放 desktop/platform。Windows 系统保护、Android Keystore、Linux Secret Service 的具体库与恢复策略通过 P0 ADR 验证；不可用时锁定或采用用户明确设定的口令保护。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [PLATFORM-001 wrap_storage_key](platform.md#platform-001)
- [PLATFORM-002 unwrap_storage_key](platform.md#platform-002)
- [PLATFORM-003 delete_key_alias](platform.md#platform-003)
- [PLATFORM-004 choose_import_file](platform.md#platform-004)
- [PLATFORM-005 choose_export_file](platform.md#platform-005)
- [PLATFORM-006 open_file_token](platform.md#platform-006)
- [PLATFORM-007 show_notification](platform.md#platform-007)
- [PLATFORM-008 cancel_notification](platform.md#platform-008)
- [PLATFORM-009 handle_platform_lifecycle](platform.md#platform-009)
- [PLATFORM-010 desktop_exit](platform.md#platform-010)

<a id="platform-001"></a>

## PLATFORM-001 · wrap_storage_key

~~~kotlin
suspend fun wrapStorageKey(alias: String, secret: SecretBuffer): WrappedSecret
~~~

**职责与输出**：使用系统保护封装随机存储主密钥，返回不含明文的持久包。

**输入与前置条件**：别名命名空间/版本明确；系统服务可用；敏感缓冲有限寿命。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验 alias。
2. 查询/创建平台保护槽。
3. 调用系统封装。
4. 记录版本/平台元数据。
5. 清理临时明文。

**持久化、并发与副作用**：外部密钥槽与 DB 不天然原子；通过创建意图和孤儿槽清理协调。

**失败处理**：KEY_PROTECTION_UNAVAILABLE，USER_AUTH_REQUIRED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] Linux 无服务不能落明文。
- [ ] Android 锁屏授权。
- [ ] Windows 不同用户不可解包。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="platform-002"></a>

## PLATFORM-002 · unwrap_storage_key

~~~kotlin
suspend fun unwrapStorageKey(alias: String, wrapped: WrappedSecret): SecretBuffer
~~~

**职责与输出**：在可信内核启动路径恢复存储主密钥。

**输入与前置条件**：用户/平台授权完成，格式与平台版本匹配。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证元数据。
2. 请求系统授权。
3. 解包。
4. 检查长度/用途。
5. 交受控缓冲给 storage。
6. 失败清理。

**持久化、并发与副作用**：不通过 UI/插件 API 返回秘密；ByteArray 不承诺自动零化。

**失败处理**：STORAGE_LOCKED，KEY_INVALIDATED，USER_AUTH_CANCELLED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 系统密钥失效返回恢复引导。
- [ ] 错误 alias 不尝试明文 fallback。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="platform-003"></a>

## PLATFORM-003 · delete_key_alias

~~~kotlin
suspend fun deleteKeyAlias(alias: String): Unit
~~~

**职责与输出**：显式删除无引用/用户确认的保护槽。

**输入与前置条件**：不能因临时启动失败直接删除仍引用的唯一密钥。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对允许命名空间。
2. 检查本地引用和删除授权。
3. 调用系统删除。
4. 记录无秘密结果。

**持久化、并发与副作用**：外部副作用有可重试删除日志；重复删除幂等。

**失败处理**：KEY_STILL_REFERENCED，PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 误删在用 key 阻止。
- [ ] 槽已不存在重复成功。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="platform-004"></a>

## PLATFORM-004 · choose_import_file

~~~kotlin
suspend fun chooseImportFile(): PlatformFileToken?
~~~

**职责与输出**：系统选择器获得单文件读授权，取消返回 null。

**输入与前置条件**：用户显式操作；Android URI 不当作 Windows 路径。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 打开选择器。
2. 取得系统授权。
3. 登记短期 token/读模式/生命周期。
4. 返回 opaque token。

**持久化、并发与副作用**：平台 token 注册表，不写任意路径到插件可读配置。

**失败处理**：FILE_PICKER_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 用户取消无任务。
- [ ] Android provider 虚拟流可读。
- [ ] token 过期拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="platform-005"></a>

## PLATFORM-005 · choose_export_file

~~~kotlin
suspend fun chooseExportFile(suggestedName: String): PlatformFileToken?
~~~

**职责与输出**：取得用户指定目标的写授权。

**输入与前置条件**：建议文件名仅展示且清除路径分隔符；不覆盖未授权文件。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 清洗名称。
2. 调用保存选择器。
3. 明确覆盖行为。
4. 登记受限写 token。
5. 返回授权。

**持久化、并发与副作用**：选择本身不写内容；取消返回 null。

**失败处理**：FILE_PICKER_UNAVAILABLE，PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 恶意附件名 ../ 不影响实际目录。
- [ ] 覆盖由系统选择决定。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="platform-006"></a>

## PLATFORM-006 · open_file_token

~~~kotlin
suspend fun openFileToken(token: PlatformFileToken, mode: AccessMode): ControlledStream
~~~

**职责与输出**：把有效 token 换成一次受控流/句柄供 core 使用。

**输入与前置条件**：token 由当前平台进程签发，mode 不超过授予模式。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 查 token 表。
2. 验证期限/调用者/读写范围。
3. 取得 URI/系统句柄。
4. 返回有限流接口。
5. 关闭释放授权。

**持久化、并发与副作用**：流操作不跨 UI 线程；FFI 只传受控回调/句柄。

**失败处理**：INVALID_FILE_TOKEN，FILE_PERMISSION_REVOKED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 伪造 opaqueId 不打开路径。
- [ ] 读 token 不可写。
- [ ] core close 释放流。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="platform-007"></a>

## PLATFORM-007 · show_notification

~~~kotlin
suspend fun showNotification(notification: LocalNotification): Unit
~~~

**职责与输出**：显示不含正文的默认通知，尊重系统授权。

**输入与前置条件**：MVP Android 仅前台/恢复后可用，不假设后台必达。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查通知许可。
2. 按隐私设置构造标题。
3. 限制标识/正文。
4. 调用平台 API。
5. 记录投递结果。

**持久化、并发与副作用**：外部通知失败不影响消息事务。

**失败处理**：NOTIFICATION_PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 锁屏默认不泄漏消息正文。
- [ ] 拒绝权限不阻止同步。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="platform-008"></a>

## PLATFORM-008 · cancel_notification

~~~kotlin
suspend fun cancelNotification(id: String): Unit
~~~

**职责与输出**：幂等撤销本应用通知。

**输入与前置条件**：id 属于本应用命名空间。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验 ID。
2. 调用系统取消。
3. 清理本地显示状态。

**持久化、并发与副作用**：无核心数据库写权限。

**失败处理**：INVALID_NOTIFICATION_ID。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 通知不存在仍成功。
- [ ] 不能操作其他应用通知。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="platform-009"></a>

## PLATFORM-009 · handle_platform_lifecycle

~~~kotlin
suspend fun handlePlatformLifecycle(event: PlatformLifecycleEvent): Unit
~~~

**职责与输出**：将 Android 前后台/桌面睡眠唤醒映射给 sync。

**输入与前置条件**：应用生命周期 scope 可用；事件可重复。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 前后台映射。
2. 睡眠前保存工作。
3. 唤醒丢弃旧连接。
4. 重新读取网络 hint。
5. 触发增量同步。

**持久化、并发与副作用**：不依赖系统一定给予后台退出回调。

**失败处理**：CORE_CLOSED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[SYNC-012 set_lifecycle](sync.md#sync-012)、[SYNC-013 set_connectivity](sync.md#sync-013)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] Android 直接杀进程仍依靠恢复。
- [ ] Windows 睡眠后不复用旧 socket。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="platform-010"></a>

## PLATFORM-010 · desktop_exit

~~~kotlin
suspend fun desktopExit(reason: ExitReason): ExitReport
~~~

**职责与输出**：区分关窗口托盘常驻与显式退出。

**输入与前置条件**：用户托盘/菜单操作；平台托盘能力明确。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 关闭窗口转托盘。
2. 显式退出停止插件。
3. 等待 core close。
4. 释放托盘/文件句柄。
5. 退出进程。

**持久化、并发与副作用**：只在当前事务耐久完成后结束；超过截止提示状态已保存。

**失败处理**：SHUTDOWN_TIMEOUT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-002 close_core](core.md#core-002)、[PLUGINS-010 stop_plugin](plugins.md#plugins-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 点击关闭不停止同步。
- [ ] 显式退出无遗留后台网络线程。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。


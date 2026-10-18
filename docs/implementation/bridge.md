# FFI、回调、Flow 包装与资源生命周期

阶段：P0 → P2。状态：函数级设计，尚未实现。目标源码：[crates/ffi/src/bridge.rs](../../crates/ffi/src/bridge.rs)。

真实生成绑定必须来自选定框架，P0 前不伪造 ABI。Kotlin 包装代码目标位于 shared/core-bridge/src/jvmMain 及 Android 对应 source set；Flow 只负责 UI 通知。Rust 原生方法不导出 Kotlin coroutine 或 Flow；秘密不作为普通 UI DTO。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [BRIDGE-001 ffi_create_core](bridge.md#bridge-001)
- [BRIDGE-002 ffi_dispatch](bridge.md#bridge-002)
- [BRIDGE-003 ffi_subscribe](bridge.md#bridge-003)
- [BRIDGE-004 release_ffi_subscription](bridge.md#bridge-004)
- [BRIDGE-005 stop_event_bridge](bridge.md#bridge-005)
- [BRIDGE-006 bridge_event_flow](bridge.md#bridge-006)
- [BRIDGE-007 bridge_suspend_call](bridge.md#bridge-007)

<a id="bridge-001"></a>

## BRIDGE-001 · ffi_create_core

~~~text
ffi_create_core(options: FfiOptions, platform: PlatformBridge) -> Result<FfiCoreHandle>
~~~

**职责与输出**：建立有生命周期计数的原生 core 对象。

**输入与前置条件**：绑定框架和 panic 规则已 P0 验证；平台回调注册完整。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验版本/ABI。
2. 映射 options。
3. 调用 start。
4. 登记受控句柄。
5. 返回简单 DTO 和初始化状态。

**持久化、并发与副作用**：句柄引用计数；创建失败释放 callback；不返回裸数据库指针。

**失败处理**：ABI_MISMATCH，STORAGE_LOCKED，CORE_START_FAILED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-001 start_core](core.md#core-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] Windows/Linux/Android 真机创建/释放。
- [ ] 错误回调不泄漏句柄。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="bridge-002"></a>

## BRIDGE-002 · ffi_dispatch

~~~text
ffi_dispatch(handle: FfiCoreHandle, request: FfiRequest) -> Result<FfiResponse>
~~~

**职责与输出**：将有界业务请求映射到内核，不暴露协议内部对象。

**输入与前置条件**：handle 有效，类型/字节/超时受限。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证句柄。
2. 解析生成 DTO。
3. 进入异步任务。
4. 调用对应 core 服务。
5. 脱敏映射错误。
6. 返回稳定 DTO。

**持久化、并发与副作用**：不能持 FFI 锁等待 UI 回调；操作成功以持久化为准。

**失败处理**：CORE_CLOSED，INVALID_FFI_ARGUMENT，OPERATION_CANCELLED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-007 send_message](core.md#core-007)、[PROTOCOL-014 map_error](protocol.md#protocol-014)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重复 send 保持 clientOperationId。
- [ ] UI 取消后已提交意图可查。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="bridge-003"></a>

## BRIDGE-003 · ffi_subscribe

~~~text
ffi_subscribe(handle: FfiCoreHandle, callback: EventCallback) -> Result<FfiSubscription>
~~~

**职责与输出**：建立可释放事件通道。

**输入与前置条件**：回调跨线程规则已核实，UI 主线程由 Kotlin 自行切换。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 注册有界订阅。
2. 原生线程只投递 DTO。
3. 检测回调失败。
4. 溢出标 resync。
5. 保存订阅生命周期。

**持久化、并发与副作用**：引用与 core 关联；关闭后禁止回调已释放对象。

**失败处理**：CALLBACK_FAILED，CORE_CLOSED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-003 observe_events](core.md#core-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 快速订阅/释放竞态无 use-after-free。
- [ ] 慢消费者有 resync。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="bridge-004"></a>

## BRIDGE-004 · release_ffi_subscription

~~~text
release_ffi_subscription(subscription: FfiSubscription) -> Result<()>
~~~

**职责与输出**：幂等解除回调与待发送队列。

**输入与前置条件**：允许重复释放，不能从 callback 内死锁等待自己。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 标 closed。
2. 停止新回调。
3. 处理在途回调计数。
4. 丢弃临时通知。
5. 释放引用。

**持久化、并发与副作用**：不删除持久消息；从 callback 重入时采用延迟清理。

**失败处理**：INVALID_HANDLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] Compose 页面反复打开/关闭无回调增长。
- [ ] release 后无新事件。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="bridge-005"></a>

## BRIDGE-005 · stop_event_bridge

~~~text
stop_event_bridge(core: FfiCoreHandle) -> Result<()>
~~~

**职责与输出**：关闭 core 的全部订阅并等待有界完成。

**输入与前置条件**：core 已 draining；不能无限等恶意/挂起回调。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 快照订阅集合。
2. 逐个取消。
3. 截止时间内 drain。
4. 作废回调引用。
5. 报告未配合回调。

**持久化、并发与副作用**：不回滚 core 已提交事务；平台线程不能被原生 join 死锁。

**失败处理**：CALLBACK_SHUTDOWN_TIMEOUT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[BRIDGE-004 release_ffi_subscription](bridge.md#bridge-004)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 关闭时 callback 触发查询得到 CLOSED 而非死锁。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="bridge-006"></a>

## BRIDGE-006 · bridge_event_flow

~~~text
bridge_event_flow(native: NativeCore) -> Flow<CoreEvent>
~~~

**职责与输出**：Kotlin callbackFlow 包装与 awaitClose 释放。

**输入与前置条件**：选定 coroutine 版本已锁定；调用方遵守页面 scope。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 注册 FFI 回调。
2. 安全映射 DTO。
3. 投递有界 channel。
4. 溢出发 resync。
5. awaitClose 释放订阅。

**持久化、并发与副作用**：Kotlin 仅缓存 UI 状态；回调不在原生线程操作 Compose。

**失败处理**：CORE_CLOSED，INVALID_NATIVE_EVENT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[BRIDGE-003 ffi_subscribe](bridge.md#bridge-003)、[BRIDGE-004 release_ffi_subscription](bridge.md#bridge-004)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] collect 取消释放一次。
- [ ] 重收集查询持久状态补丢失通知。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="bridge-007"></a>

## BRIDGE-007 · bridge_suspend_call

~~~text
bridge_suspend_call(operation: BusinessRequest) -> BusinessResult
~~~

**职责与输出**：将 Rust async 业务调用包装为 Kotlin suspend。

**输入与前置条件**：每个变更请求保留稳定 operationId；回调/任务取消语义明确。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 切换 IO dispatcher。
2. 映射 DTO 防御复制。
3. 调用生成绑定。
4. 取消时只发取消 hint。
5. 映射稳定错误。
6. 必要时查询状态。

**持久化、并发与副作用**：取消 coroutine 不代表撤回已提交消息；原生任务有自己的所有权。

**失败处理**：OPERATION_CANCELLED，CORE_CLOSED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[BRIDGE-002 ffi_dispatch](bridge.md#bridge-002)、[CORE-010 get_draft_status](core.md#core-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 暂停页面导致取消后重开仍看到已排队草稿。
- [ ] 无 UI 阻塞。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。


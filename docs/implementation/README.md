> 迁移说明：本目录的函数计划来自原工程骨架。D:/AgentProject/chat 已有独立的依赖安装/验证工程；工具链和精确依赖以 ../DEPENDENCIES.md、根目录实际配置为准。迁移未更新正在使用的构建配置和锁文件，新增业务模块暂未登记到活动工作区。

# 实施计划：从架构到函数

日期：2026-10-18。产品暂名 chat。原始设计来自同日 linux-windows-p2p 的 chat-architecture 包，现已完整复制至 [原设计](../design/README.md)。

本次交付建立目录、工作区、模块源码位置、少量 DTO/接口/插件示例、配置与 schema 草案，并把设计展开为 **197 个函数契约**。这是一份开始实施的工程与详细计划，不是可安装聊天程序。加密、存储、网络、界面和节点处理器均未实现。

## 建议阅读顺序

1. [开始开发的 P0 清单](P0_CHECKLIST.md)：先验证工具链、MLS 与事务，不提前扩大功能。
2. [类型字典](TYPES.md)：ID、意图、尝试、认证结果、状态与权限窗口。
3. [统一事务与并发](TRANSACTIONS.md)：T1/T2/T3/T4、pending Commit 与取消竞争。
4. [关键函数调用链](FLOWS.md)：发送、接收、控制冲突、撤销、附件和插件。
5. [分阶段工作项](TASKS.md)：依赖、产出、验收与下一步。
6. [函数总索引](FUNCTION_INDEX.md)：按模块进入逐函数设计。
7. [需求覆盖表](COVERAGE.md)：原架构/MVP 与函数计划的对应关系。

## 模块计划

|模块|函数数|阶段|源码位置|
|---|---:|---|---|
|[标识符、线协议、编码与验证](protocol.md)|14|P0 → P1|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|
|[领域规则、群权限与状态机](domain.md)|12|P1 → P3|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|
|[账户、联系人、双设备与恢复](identity.md)|14|P1 → P2|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|
|[MLS、单群串行状态与附件密码学](crypto.md)|12|P0 → P3|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|
|[SQLite、统一事务、数据表与崩溃恢复](storage.md)|15|P0 → P2|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|
|[设备直连、中继与节点客户端](transport.md)|12|P0 → P1|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|
|[可靠发送、信箱、日志补齐与生命周期](sync.md)|14|P1 → P3|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|
|[客户端内核与 Kotlin 业务服务映射](core.md)|12|P1 → P2|[crates/core/src/service.rs](../../crates/core/src/service.rs)|
|[小群创建、邀请与治理用例](groups.md)|12|P3|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|
|[文件导入、分块上传、下载和导出](attachments.md)|8|P2|[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)|
|[节点服务、排序日志、密文信箱与文件](node.md)|16|P1 → P5|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|
|[节点 HTTP、鉴权、限流与运行入口](api.md)|10|P1 → P5|[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)|
|[FFI、回调、Flow 包装与资源生命周期](bridge.md)|7|P0 → P2|[crates/ffi/src/bridge.rs](../../crates/ffi/src/bridge.rs)|
|[密钥保护、文件授权和三端生命周期](platform.md)|10|P0 → P2|[shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt](../../shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt)|
|[应用用例、页面状态与 Compose 界面](presentation.md)|11|P2 → P4|[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)|
|[可信桌面插件、配置包、SDK 与示例](plugins.md)|13|P4|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|
|[逐项群操作的参数与授权分支](group-actions.md)|5|P3|[crates/core/src/group_actions.rs](../../crates/core/src/group_actions.rs)|

## 代码和数据边界

另见 [依赖方向与注入端口](DEPENDENCIES.md) 和 [数据模型说明](DATA_MODEL.md)。

## 执行规则

每次只做一个能验证的纵向工作项：先列输入/状态不变量，再实现最小真实路径，接真实库，运行相应用例，最后更新本计划与验证记录。未实现功能显式失败；不使用“模拟加密成功”“模拟节点接受”充当产品完成度。P0 可以有明确隔离的测试双，但不得接入发布路径。

将函数标为完成需要：源码实现、错误路径、持久化边界、必要故障用例、目标平台验证证据。UI 原型或空接口不满足完成条件。尚未由原设计指定的产品名、许可证、最低 Android、Linux 基线和依赖精确版本见 [待决事项](../adr/0001-bootstrap.md)；不得把暂名或草案参数当成最终发布承诺。

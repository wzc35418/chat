# 应用用例、页面状态与 Compose 界面

阶段：P2 → P4。状态：函数级设计，尚未实现。目标源码：[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)。

UI → presentation → core-api → bridge → Rust。状态只由业务查询与事件更新，不把乐观展示当作正式接受。所有 use case 的取消、生命周期和用户操作 ID 需要明确；界面具体组件放 shared/ui。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [PRESENTATION-001 bootstrap_application](presentation.md#presentation-001)
- [PRESENTATION-002 load_conversation_list](presentation.md#presentation-002)
- [PRESENTATION-003 load_message_page](presentation.md#presentation-003)
- [PRESENTATION-004 submit_composer](presentation.md#presentation-004)
- [PRESENTATION-005 retry_from_ui](presentation.md#presentation-005)
- [PRESENTATION-006 cancel_from_ui](presentation.md#presentation-006)
- [PRESENTATION-007 submit_group_action](presentation.md#presentation-007)
- [PRESENTATION-008 handle_core_event](presentation.md#presentation-008)
- [PRESENTATION-009 render_message_content](presentation.md#presentation-009)
- [PRESENTATION-010 present_error](presentation.md#presentation-010)
- [PRESENTATION-011 build_adaptive_layout](presentation.md#presentation-011)

<a id="presentation-001"></a>

## PRESENTATION-001 · bootstrap_application

~~~kotlin
suspend fun bootstrapApplication(): StartupUiState
~~~

**职责与输出**：返回 Welcome/Unlock/Ready/RecoveryRequired 页面状态。

**输入与前置条件**：平台依赖准备完毕；不在 Compose 重组中反复启动 core。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 调用 bridge start。
2. 判断账户/锁状态。
3. 订阅事件。
4. 拉会话第一页。
5. 映射可操作错误。

**持久化、并发与副作用**：页面 scope 持有订阅；失败清理；不建第二份数据库。

**失败处理**：STORAGE_LOCKED，STATE_CORRUPTED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[BRIDGE-001 ffi_create_core](bridge.md#bridge-001)、[IDENTITY-002 current_account](identity.md#identity-002)、[BRIDGE-006 bridge_event_flow](bridge.md#bridge-006)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重组不重复建账户。
- [ ] 损坏显示恢复页不空白登录页。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="presentation-002"></a>

## PRESENTATION-002 · load_conversation_list

~~~kotlin
suspend fun loadConversationList(cursor: Cursor?): ConversationListState
~~~

**职责与输出**：展示会话摘要、待发计数和同步状态。

**输入与前置条件**：维护页面请求代次，防旧响应覆盖新页。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 调用 core 列表。
2. 合并稳定 ConversationId。
3. 附同步状态。
4. 更新 loading/error/nextCursor。

**持久化、并发与副作用**：UI 内存状态；离页取消查询。

**失败处理**：INVALID_CURSOR，STORAGE_LOCKED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-005 list_conversations](core.md#core-005)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 网络离线仍可用。
- [ ] 连续翻页不重复会话。

**实现状态**：局部实现（2026-10-18）。`display/ConversationDisplayMerger.kt` 提供按稳定 ID 去重、分页追加/前置/reset、迟到结果按代次忽略的纯合并（DS-08）；`display/PagingPresentation.kt` 把列表状态映射为“加载中／还有更多／没有更多／失败重试”的文案、可用动作与语义播报（DS-11），加载中不给可点动作。预览用固定样例经合并器跑成功/失败/到底（`preview/PreviewPagingSample.kt`）。正式 `ConversationService` 列表查询与离线读缓存尚未接线，因此本函数复选框不勾选。

<a id="presentation-003"></a>

## PRESENTATION-003 · load_message_page

~~~kotlin
suspend fun loadMessagePage(conversation: ConversationId, before: Cursor?): MessageListState
~~~

**职责与输出**：按稳定身份合并消息与本地草稿投影。

**输入与前置条件**：当前页面会话固定，页面 limit 合理。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 记录请求代次。
2. 查询 core。
3. 按 MessageId/DraftId 关联去重。
4. 保留滚动锚。
5. 更新 nextCursor。

**持久化、并发与副作用**：UI 仅保留有界页面缓存；内容锁定时清理内存。

**失败处理**：PERMISSION_DENIED，INVALID_CURSOR。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-006 list_messages](core.md#core-006)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 切会话后旧异步响应不覆盖。
- [ ] 草稿正式接受后不显示两条。

**实现状态**：局部实现（2026-10-18）。`display/ConversationDisplayMerger.kt` 的 `applyMessagePage` 按请求代次与游标校验，切会话或重置后迟到的旧页返回 `ignored`（DS-08）；`display/PagingPresentation.kt` 提供“加载更早/加载中/失败重试/没有更多”的展示映射（DS-11）。草稿与正式消息按 `DraftId`/`MessageId` 关联去重、滚动锚点保持仍需正式查询接口，因此本函数复选框不勾选。

<a id="presentation-004"></a>

## PRESENTATION-004 · submit_composer

~~~kotlin
suspend fun submitComposer(state: ComposerState, operationId: String): ComposerResult
~~~

**职责与输出**：发送成功持久化后清编辑区并展示 QUEUED。

**输入与前置条件**：operationId 在一次用户意图期间固定，插件草稿必须用户确认。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验 UI 必填。
2. 冻结输入快照。
3. 构造 SendRequest。
4. 调用 core send。
5. 成功后清当前对应输入。
6. 失败保留草稿和原因。

**持久化、并发与副作用**：UI 防重复加 core 幂等；返回 QUEUED 不显示 DELIVERED。

**失败处理**：PERMISSION_DENIED，CONTENT_TOO_LARGE，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-007 send_message](core.md#core-007)、[BRIDGE-007 bridge_suspend_call](bridge.md#bridge-007)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 连点一次提交。
- [ ] 提交后继续输入新字不被迟到响应清掉。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="presentation-005"></a>

## PRESENTATION-005 · retry_from_ui

~~~kotlin
suspend fun retryFromUi(draft: DraftId): DraftUiState
~~~

**职责与输出**：显示可重试动作并重新查询实际状态。

**输入与前置条件**：操作来自用户；并发点击合并。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 禁用重复按钮。
2. 调用 retry。
3. 查询最新状态。
4. 按错误给重新联网/取消/权限提示。
5. 恢复按钮。

**持久化、并发与副作用**：UI 不直接操作 outbox 或重加密。

**失败处理**：RETRY_NOT_ALLOWED，PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-008 retry_draft](core.md#core-008)、[CORE-010 get_draft_status](core.md#core-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 已接受消息不再重发。
- [ ] 被禁言不展示无限重试按钮。

**实现状态**：局部实现（2026-10-18）。`display/DraftSendPresentation.kt` 的 `presentDraft` 对“节点已接受／已送达／结果待对账／可重试失败／不可重试失败”给出不同文案与动作：已接受只显示“已接受”并注明不等于送达、不提供重试；`PERMISSION_DENIED` 等不可重试错误只提供查看详情与取消；`DraftInteraction.beginRetry` 合并重复点击。重试必须由用户动作触发（`autoResendAllowed` 恒为 false）。正式 `retry_draft`/`get_draft_status` 接线与“已接受后不再重发”的底层对账仍未完成，因此本函数复选框不勾选。

<a id="presentation-006"></a>

## PRESENTATION-006 · cancel_from_ui

~~~kotlin
suspend fun cancelFromUi(draft: DraftId): CancelUiState
~~~

**职责与输出**：显示取消成功/已经接受/结果待对账。

**输入与前置条件**：文案明确取消不保证撤回在途消息。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 调用取消。
2. 按三态结果更新。
3. 未知状态保持查询。
4. 监听 DraftChanged。

**持久化、并发与副作用**：不乐观删除在途消息；以 core 结果为准。

**失败处理**：ACCEPTANCE_UNKNOWN。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-009 cancel_queued](core.md#core-009)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 取消与接受竞争 UI 最终仍显示正式消息。

**实现状态**：局部实现（2026-10-18）。`display/DraftSendPresentation.kt` 中取消不乐观删除消息：请求取消后显示“正在取消”并说明“不保证撤回已在途的消息”；已拿到节点接受回执时显示“取消未生效：已被接受”并保留消息；已接受/已送达不提供取消动作。`DraftInteraction.beginCancel` 合并重复点击。正式 `cancel_queued` 三态结果与 `DraftChanged` 事件尚未接线，因此本函数复选框不勾选。

<a id="presentation-007"></a>

## PRESENTATION-007 · submit_group_action

~~~kotlin
suspend fun submitGroupAction(snapshot: GroupSnapshot, op: GroupOperation, operationId: String): GroupActionUiState
~~~

**职责与输出**：管理操作展示 pending，确认后才改成员列表。

**输入与前置条件**：按钮权限来自 snapshot 仅用于提示；core 重新校验。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 冻结 expected head。
2. 用户确认敏感动作。
3. 调用 propose。
4. 显示 ticket 状态。
5. 正式 GroupChanged 后重查。

**持久化、并发与副作用**：UI 不直接修改正式 role/muted；旧 head 展示刷新提示。

**失败处理**：STALE_CONTROL_HEAD，PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[GROUPS-003 propose_group_operation](groups.md#groups-003)、[GROUPS-007 operation_status](groups.md#groups-007)、[GROUPS-002 group_snapshot](groups.md#groups-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 管理员权限被收回后旧页面按钮仍不能成功。
- [ ] 节点离线只显示待处理。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="presentation-008"></a>

## PRESENTATION-008 · handle_core_event

~~~kotlin
suspend fun handleCoreEvent(event: CoreEvent): Unit
~~~

**职责与输出**：将变更提示转换为合并重查任务。

**输入与前置条件**：事件可能重复/丢失，不能当作完整消息数据。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 按 conversation/draft 合并。
2. 只重查当前可见页。
3. ResyncRequired 全局重查。
4. 身份警告显示核验动作。

**持久化、并发与副作用**：UI 调度有界；避免每事件全库扫描。

**失败处理**：CORE_CLOSED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CORE-010 get_draft_status](core.md#core-010)、[CORE-005 list_conversations](core.md#core-005)、[CORE-006 list_messages](core.md#core-006)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 1000 个重复事件只有限查询。
- [ ] 溢出后最终 UI 与数据库一致。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="presentation-009"></a>

## PRESENTATION-009 · render_message_content

~~~kotlin
fun renderMessageContent(view: MessageView): RenderModel
~~~

**职责与输出**：得到文本/附件/扩展回退等安全显示模型。

**输入与前置条件**：消息来自已验证持久视图；插件原始 HTML/脚本不可直接执行。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 按类型分派。
2. 文字安全显示。
3. 未知扩展用 fallback。
4. 附件按钮引用 id。
5. 正式状态按 evidence 展示。

**持久化、并发与副作用**：纯展示；不因渲染触发自动发送/下载代码。

**失败处理**：UNSUPPORTED_RENDER_TYPE 使用文本回退。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-010 validate_extension](protocol.md#protocol-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 未知插件能读文字。
- [ ] 恶意 payload 不执行脚本。
- [ ] 未确认预传不进入正式列表。

**实现状态**：局部实现。`shared/presentation/.../display/MessageRenderModel.kt` 已提供基于 `MessageContent` 的只读文本/附件占位/扩展 fallback 模型与纯函数测试；`display/MarkdownModel.kt`（2026-10-18，DS-12）提供文本消息的安全 Markdown 子集解析：标题/列表/引用/代码块/粗体/斜体/删除线/行内代码/链接，**不解析 HTML**、链接只接受 http/https/mailto（其他协议降级为不可点击并给原因）、输入长度与块/节点/嵌套/代码行数/链接长度均有上限并可标记截断，`markdownPlainTextOrFallback` 保证空内容有可见回退，`applyMarkdownCommand` 提供选区包裹的输入命令。界面 `shared/ui/.../MarkdownText.kt` 只绘制该模型，消息卡片与样例发送框（含格式工具栏与预览）已接入。正式 `MessageView` 查询接口尚未接入，因此本函数复选框不勾选。

<a id="presentation-010"></a>

## PRESENTATION-010 · present_error

~~~kotlin
fun presentError(error: CoreFailure): UserActionableError
~~~

**职责与输出**：错误码映射中文原因和可选操作。

**输入与前置条件**：error 已脱敏，不展示内部堆栈。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 查稳定错误字典。
2. 映射重连/解锁/核验/取消。
3. 仅 retryable 提供重试。
4. 附无秘密 operationId 供诊断。

**持久化、并发与副作用**：纯函数；不把权限错误说成网络问题。

**失败处理**：未知码显示安全通用信息并保留诊断码。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-014 map_error](protocol.md#protocol-014)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] STALE_EPOCH 显示正在同步。
- [ ] LOG_CONFLICT 提示群同步暂停。

**实现状态**：局部实现（2026-10-18 扩展）。`shared/presentation/.../display/ErrorPresentation.kt` 现覆盖 PROTOCOL-014 冻结的 **全部 41 个错误码**（`FROZEN_ERROR_CODES` 与 `crates/protocol/src/error.rs` 的 `ErrorCode::ALL` 对齐），每个码都有稳定中文标题、说明、动作与诊断码；未知码仍走安全通用文案并保留脱敏诊断标识。同时把“手动重试”与“自动重试”拆开：`retryable` 由底层 `CoreFailure.retryable` 决定，新增 `autoRetryAllowed` 只按冻结规则（NETWORK_UNAVAILABLE、NODE_UNAVAILABLE、RATE_LIMITED、STALE_CONTROL_HEAD、STALE_EPOCH、MISSING_LOG_ENTRIES、STORAGE_LOCKED 七个码）为 true，签名/密文/权限/损坏类永不允许自动重试。验证在 `PresentationChecks.kt` 的 `checkFrozenErrorCoverage`（41 码全覆盖、7 码自动重试、STORAGE_FAILURE 手动可重试但不得自动重试、设备上限文案与产品口径一致）。正式 core 错误事件与页面接线尚未完成，因此本函数复选框不勾选。

<a id="presentation-011"></a>

## PRESENTATION-011 · build_adaptive_layout

~~~kotlin
fun buildAdaptiveLayout(window: WindowSize, selection: NavigationState): LayoutModel
~~~

**职责与输出**：窄屏列表/会话导航与宽屏双栏共享用例。

**输入与前置条件**：选中会话稳定 ID；布局变化不重启核心。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 根据可用宽度选择结构。
2. 保留导航/滚动状态。
3. 挂载页面 scope。
4. 只释放页面订阅不关闭 core。

**持久化、并发与副作用**：纯布局决策；Compose 不直接持原生对象。

**失败处理**：无运行时错误；非法导航回退会话列表。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 旋转/窗口缩放消息不重发。
- [ ] 桌面双栏和 Android 同一会话状态。

**实现状态**：局部实现（2026-10-18）。`shared/presentation/.../display/AdaptiveLayout.kt` 提供纯布局决策：宽屏（≥900 dp）双栏、窄屏二选一、返回按钮、侧栏宽度分档、紧凑宽度（<720 dp）、非法/失效选择回退会话列表（`navigationFallback`）、尺寸变化保留选择、浮层不改变结构。P0 页面与预览界面均改为调用该函数，预览的双栏判定沿用同一阈值。验证在 `apps/desktop/.../PresentationChecks.kt` 的 DS-09 组。页面 scope 挂载/释放、滚动锚点保持与“旋转不重发”仍需正式 `ConversationService` 与 bridge 事件接线后才能验收，因此本函数复选框不勾选。


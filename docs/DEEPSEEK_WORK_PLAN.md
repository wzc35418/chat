# 给 DeepSeek 的前端与轻业务任务书

DeepSeek，你好：

这个项目由 Kotlin/Compose 界面、Rust 内核、OpenMLS 和 iroh 组成。你负责可独立推进的前端 UI、展示状态与轻量业务编排；我继续负责密码学、事务持久化、节点、传输和跨端底层验证。请以本文件作为协作入口，动手前先读 [项目入口](PROJECT_GUIDE.md)、[P0 运行报告](P0.md)、[P0 验收清单](implementation/P0_CHECKLIST.md) 和相关函数设计。产品交互可参考 [口令加好友与桌面界面设计](design/CONTACT_INVITE_DESKTOP.md)，但要区分设计稿与已经可运行的功能。

## 截至 2026-10-18 的真实进度

- Windows 本机已有 Compose → `P0Presenter` → `P0Core`/UniFFI → Rust → OpenMLS → iroh 的双设备演示，能建群、传密文并在接收设备解密；Android arm64 **APK 构建**通过，真机运行、Linux 和跨物理设备互通尚未验证。当前 UI 的 Alice/Bob 是同一进程内的两个临时设备，关闭后不保留消息。
- 2026-10-18：DS-01 至 DS-04 已完成并经我复核。P0 页面、状态竞态处理、正式聊天界面“交互预览”和纯展示/错误文案模型均通过本地编译、展示层检查和截图复核；正式联系人、节点回执和持久会话仍未接入，预览数据与 P0 真链路保持隔离。Windows 根目录现有[一键启动脚本](../start-chat.cmd)，默认启动真实 P0 桌面实验。
- 2026-10-18：DS-05 至 DS-08 已完成。发送结果未知改为不可关闭的 `requiresRebuild` 状态，重建前不能发送且保留草稿；P0 与预览补齐语义标签、大字体/短窗口截图检查；预览口令只接受内置样例 token；列表/消息纯展示合并器覆盖迟到结果、重复 ID、追加/前置与重置。正式服务仍未接线。
- 客户端文件 SQLite 已独立验证 T1 意图、T2 MLS/outbox 原子提交和崩溃恢复；独立节点文件 SQLite 已验证接受日志、序号/哈希链、投递任务原子提交、固定密文重试及**预先固定节点公钥的 P0 Ed25519 回执签名**。实验库固定节点公钥，错误密钥重开被拒绝。合计 14 个进程终止断点通过。它们**尚未接入现有 Compose 演示或正式服务**；P0 签名种子由测试调用方提供，正式密钥托管、HTTP、鉴权和协议编码仍未完成。见 [客户端实验](P0_PERSISTENCE.md)、[节点实验](P0_NODE_ACCEPTANCE.md) 和[最新机器报告](p0-validation-latest.json)。
- 2026-10-18：DS-09 至 DS-11 已完成（DeepSeek 第三轮）。`PRESENTATION-011 build_adaptive_layout` 现为纯函数实现并接入 P0 与预览两处界面；新增发送状态展示模型（节点接受／送达／结果待对账／可重试失败／权限阻断的区分与动作集合）和分页提示映射，并把 DS-08 合并器真正接到预览界面观察“加载中／失败重试／没有更多”。`presentationChecks` 12 组检查全部通过，`previewSnapshot` 16 张截图已逐张检查，Android APK 与 P0 真链路回归见下方完成记录。这三项仍是展示层与预览，正式服务未接线。
- 2026-10-18（全栈第一批，R-01~R-03）：`cargo test --workspace --offline` 全绿（protocol 45、domain 28、storage 22、crypto 15、node-services 33、node-api 8，合计 151 个 Rust 测试；另有既有 P0 测试），`cargo check --workspace --locked --offline` 通过（Cargo.lock 只新增既有锁定版本为直接依赖，无新版本包）。P1-01 线协议字节与错误码已冻结并有固定向量；P1-03 节点 challenge/Ed25519 鉴权与设备注册跑通真 TCP 端到端；P0-04 本地加密存储（列级 AEAD、独占锁、单次事务句柄、崩溃恢复）与 Argon2id 主密钥基础通过实证。完整 P0/P1 退出条件仍未全部满足，未把局部验收写成整体通过。
- 2026-10-18：分工限制解除，DeepSeek 接管全栈。DS-12（消息 Markdown 渲染与输入）已完成：新增安全 Markdown 子集展示模型与 Compose 渲染，消息卡片与样例发送框（格式工具栏 + 预览）已接入，`presentationChecks` 13 组全通过、`previewSnapshot` 16 张截图已复核。同一轮并行启动三个 Rust 子包：P1-01 线协议+领域规则、P1-03 节点鉴权、P0-04 本地加密存储，结果见下方完成记录。
- 正式 `ConversationService` 只有接口，[`Models.kt`](../shared/core-api/src/commonMain/kotlin/chat/api/Models.kt) 是未冻结的应用 DTO。正式账户、联系人邀请、持久会话、签名节点回执、离线信箱、群权限、附件和插件功能尚不可用。P0-01 至 P0-05 的完整验收都未结束；计划书中的整体复选框保持未勾选。

## 第一轮已完成任务

按顺序完成；一项完成后在本文件勾选并附实际验证结果。优先提交小而可编译的改动，保持 Windows P0 演示可运行。你可以调整 UI 内部实现和纯展示逻辑，无需等待我完成正式节点。

- [x] **DS-01：整理现有 P0 Compose 页面。** 工作位置：[`P0Screen.kt`](../shared/ui/src/commonMain/kotlin/chat/ui/P0Screen.kt) 及 `shared/ui` 内的新组件。改善桌面宽屏与 Android 窄屏的布局、消息列表、空态、连接/发送忙碌态、错误提示和输入可达性；保留 Alice/Bob 选择与“验证详情”。文案准确写“接收设备已验证并解密”，不要写正式“已接受”“已送达”或离线可用。验收：现有 `p0Smoke`、`p0Snapshot`、Android `assembleDebug` 通过；1024×860 与 390×844 截图可读、无控件重叠，截图和命令结果记入交付记录。
- [x] **DS-02：修正 P0 界面的轻量状态逻辑。** 工作位置：[`P0Presenter.kt`](../shared/presentation/src/commonMain/kotlin/chat/presentation/P0Presenter.kt) 和 `shared/presentation` 内的纯函数。处理重复点击、异步操作中的编辑区/选中发送者变化、输入 UTF-8 字节上限和用户可理解的错误显示；不要把任意异常原文或秘密写到 UI/日志。`P0Core.send` 的结果只代表 P0 接收端已经解密；发送结果未知时不要擅自重新加密或自动重发。验收：用可控假 `P0Core` 验证连点、迟到成功/失败、取消与关闭竞争；必要时补少量针对这些竞态的测试，不写只重复实现的测试。
- [x] **DS-03：做正式聊天界面的独立预览壳。** 先在 `shared/ui`/`shared/presentation` 用**显式样例数据**实现宽屏双栏、窄屏列表/会话切换、加好友口令弹窗、等待确认/身份变化/离线/锁定等状态。把预览入口与当前 P0 真链路隔开，标明“交互预览”；禁止把样例联系人、回执或送达状态接入真实会话。参照 [产品设计](design/CONTACT_INVITE_DESKTOP.md) 与 [界面函数契约](implementation/presentation.md)。验收：状态切换可在预览中观察，900 dp 以上双栏、720 dp 以下单栏；正式发送框只在样例状态 `accepted` 启用，身份变化时显示阻断提示。此项完成仅代表 UI 预览，**不勾选 P1/P2 产品功能**。
- [x] **DS-04：整理纯展示模型和文案表。** 可为 `MessageContent.Text/Attachment/Extension` 构造只读展示模型和未知扩展的安全文字回退；为已有错误码制定“说明 + 可用动作”映射。附件入口只呈现等待能力或不可用状态，不自动下载；不要执行扩展 payload，也不要把 `CoreFailure` 中未知字符串当作可信富文本。对应 [PRESENTATION-009/010](implementation/presentation.md#presentation-009)。验收：纯函数测试覆盖未知类型/未知错误码/不可重试错误，预览能展示安全回退。正式函数验收只有在底层接口接入后才能勾选。

## 第二轮任务：现在可交给 DeepSeek

DeepSeek，请继续在 `shared/ui` 和 `shared/presentation` 的现有边界内推进。以下任务按独立交付处理；完成后勾选本节并追加“DeepSeek 完成记录”。正式服务没有接线前，预览继续只使用显式样例数据。

- [x] **DS-05：保住 P0 发送结果未知的提示。** 我已修复再次调用 `send()` 会把 `P0_SEND_UNKNOWN` 覆盖成“尚未连接”的问题，并加了一个断言。请继续检查点击关闭提示、编辑内容、切换发送者、重建会话和页面关闭时的表现；结果未知期间不能出现可发送状态或丢失用户草稿。必要时用显式的 UI 状态表示“需要重建”，而不是只依赖可关闭的错误横幅。用假 `P0Core` 补有针对性的竞态检查，保持真实 P0 链路行为不变。
- [x] **DS-06：补齐界面可达性与尺寸适配。** 为 P0 与独立预览的输入框、错误动作、返回/关闭、发送设备选择和身份变化提示补明确语义标签与键盘可达性；检查字体放大和较短窗口时的滚动、焦点和按钮可见性。用实际截图或可复现的 UI 检查记录宽屏、390 dp 窄屏和大字体场景，不以颜色单独表达错误或身份变化。
- [x] **DS-07：修正预览口令交互的语义。** 当前 `validateInvite()` 对任何非空文本都进入固定的“林然（样例）”身份页；虽然有预览横幅，按钮仍写“验证口令”。请改成明确的“查看样例核对流程”或只接受一条内置样例 token；任意用户输入不能呈现为已验证的身份。为样例输入、任意输入、空输入及返回/关闭路径补纯状态检查。不要实现假的签名/有效期验证，也不接正式联系人服务。
- [x] **DS-08：准备列表与会话的纯展示状态合并器。** 在预览/展示层以假数据处理切换会话后的迟到查询结果、重复消息 ID、分页追加与重置；旧请求不得覆盖当前会话，重复页不得显示两次。接口请保持独立于尚未冻结的 `ConversationService`，只输出显示状态和待查询意图，不写 SQLite、网络或真实 `SendState` 推断。用可控延迟输入验证这些竞态，并将正式查询所需字段写入下方“待对接接口”。

本轮代码复核：第一轮提交的 P0 与预览入口确实分开，`presentationChecks`、真实 `p0Smoke/p0Snapshot`、`previewSnapshot` 和 Android APK 构建均在 2026-10-18 14:56 的[完整回归](p0-validation-latest.json)通过；截图已检查。发现的两处后续细节是“未知发送结果提示不能被后续交互覆盖”和“预览不能把任意口令表现为已验证”，分别由 DS-05/07 收尾。根目录启动脚本也已实际拉起 P0 窗口并正常关闭。

## 第三轮任务：DeepSeek 已完成

以下任务由 DeepSeek 在 2026-10-18 按本文件既有分工自行推进并完成，仍未接入正式服务，也未勾选任何 P1/P2 产品功能。

- [x] **DS-09：实现 `PRESENTATION-011 build_adaptive_layout` 的纯布局决策。** 新增 `shared/presentation/.../display/AdaptiveLayout.kt`：宽屏（≥900 dp）双栏、窄屏二选一，返回按钮、侧栏宽度分档、紧凑宽度（<720 dp）都由同一处判定；选中会话按已知集合校验，空白或失效选择回退会话列表并标记 `navigationFallback`；尺寸变化保留当前选择，浮层不改变面板结构，非法测量值按 0 处理。P0 页面的紧凑宽度与预览的双栏判定都改为调用该函数，不再各写一份阈值。验收：`presentationChecks` 的 DS-09 组覆盖 899/900、719/720、侧栏分档、非法导航回退、尺寸变化与浮层不变性。
- [x] **DS-10：实现发送意图（草稿）的纯展示状态。** 新增 `shared/presentation/.../display/DraftSendPresentation.kt`：只用证据判断状态——节点签名回执只显示“已接受”并明确“不等于对端已送达”，独立送达证据才显示“已送达”；结果未知时只提供对账查询、禁止自动重发且阻断继续发送；不可重试的失败（如 `PERMISSION_DENIED`）不显示重试；取消不提供乐观删除并说明“不保证撤回在途消息”，已接受时显示“取消未生效”。`DraftInteraction` 把重试/取消的重复点击合并为一次。预览新增“发送状态样例”循环展示六种状态（不会真的发送）。验收：`presentationChecks` 的 DS-10 组覆盖证据优先级、未知唯一动作、可重试/不可重试分流、脱敏诊断标识与重复点击合并。
- [x] **DS-11：把纯展示合并器接到界面的分页提示。** 新增 `shared/presentation/.../display/PagingPresentation.kt`：把 `ConversationMessagesDisplayState`/`ConversationListDisplayState` 映射成“加载中／还有更早／没有更多／失败重试”的可见文案、可用动作和语义播报文本，加载中不给可点动作。新增 `preview/PreviewPagingSample.kt` 用 DS-08 合并器真实跑“成功→失败→重试成功→到底”，预览界面以独立卡片展示提示与已合并条数（不改写上方样例消息）。验收：`presentationChecks` 的 DS-11 组覆盖四种提示、失败不丢已加载结果、加载中重复点击不产生第二次请求、到底后重置样例。

## 第四轮任务：全栈移交后（DeepSeek 接管全部开发）

2026-10-18 用户确认：本任务书原先把底层留给对方的限制解除，DeepSeek 接管全部开发。以下任务按大计划书（`docs/implementation` 17 模块 / 197 函数）继续推进，每一轮都必须更新本文件、`docs/implementation/P0_CHECKLIST.md` 与对应模块文档，并附真实验证命令与结果。

- [x] **DS-12：消息正文支持 Markdown 渲染与输入。** 新增 `shared/presentation/.../display/MarkdownModel.kt`（安全 Markdown 子集：标题、无序/有序列表、引用、围栏与行内代码、粗体、斜体、删除线、链接、分隔线；**不解析 HTML**；链接仅 http/https/mailto，其他协议降级为不可点击并说明原因；输入上限 8192 字符、块 200、行内节点 600、嵌套深度 3、代码块 200 行/2000 字符、链接 512 字符，超限有界截断并标记 `truncated`；`markdownPlainTextOrFallback` 空内容回退；`applyMarkdownCommand` 提供粗体/斜体/删除线/代码/链接的选区包裹）。界面新增 `shared/ui/.../MarkdownText.kt`，消息卡片改用 Markdown 渲染，样例发送框升级为 Markdown 输入（格式工具栏 + 渲染预览 + UTF-8 字节计数）。验收见下方完成记录。
- [x] **R-01：P1-01 线协议冻结 + DOMAIN 领域规则。** `crates/protocol` 新增 ids/canonical/decode/signing/extension/cursor/digest/error 模块（PROTOCOL-001/002/003/004/006/010/012/013/014，`wire.rs` 与既有公开 API 未改），`crates/domain` 新增 model/group/delivery/visibility/error（DOMAIN-001/002/003/006/007/008/009/010/011/012）。固定字节向量在 `crates/protocol/tests/vectors.rs`。验证：`cargo test --offline -p chat-protocol -p chat-domain` → 45 + 28 passed，0 failed。未实现：PROTOCOL-005/007/008/009/011、DOMAIN-004/005（保持未勾选）。
- [x] **R-02：P1-03 节点鉴权与设备注册。** `crates/node-services` 新增 auth/registration/canonical/limits/error/services + `migrations/0002_node_auth.sql`（NODE-001、API-002、API-003）；`crates/node-api` 用 axum 暴露 `/v1/auth/challenge` 与 `/v1/devices`（真实 TCP 端到端测试）。Ed25519 签名原文绑定 method+path+query+body 摘要+nonce+node_id；challenge 一次性/过期/容量上限、设备上限默认 2（与产品口径一致，可配置）、重复注册幂等、主管钥 TOFU、能力撤销版本。验证：`cargo test --offline -p chat-node-services -p chat-node-api` → 33 + 8 passed，0 failed。
- [x] **R-04（Lead）：展示层对齐冻结错误码。** `ErrorPresentation.kt` 覆盖 PROTOCOL-014 全部 41 个冻结码（稳定文案 + 动作 + 诊断码），并把手动重试（`retryable`）与自动重试（`autoRetryAllowed`，只允许 7 个码）分开；新增 `checkFrozenErrorCoverage` 断言 41 码全覆盖、7 码自动重试、`STORAGE_FAILURE` 手动可重试但不得自动重试、设备上限文案与产品口径一致。`presentationChecks` 13 组全通过。
- [x] **R-05：STORAGE-012/013/014/015 分页查询与 outbox 租约。** `crates/storage/src/formal/` 新增 `query.rs`（键集分页、`limit+1`、方向语义、内部认证游标 `spg1`）、`outbox_lease.rs`（按 `lease_version` CAS 领取、依赖与取消跳过、三态结果、`unknown` 保持 leased + 6 小时对账窗口）、`cursor.rs`（AEAD 游标，AAD 绑定会话/资源/排序版本、TTL 10 分钟）；`seal.rs` 新增两个封装用途、`error.rs` 新增 5 个稳定错误码。验证：`cargo test --offline -p chat-storage` → 39 passed（5 lib + 2 crash + 8 outbox + 7 query + 17 store），既有 22 个测试未改动。已知偏差：存储游标是内部 `spg1` 而非 PROTOCOL-012 的 `cur1`（缺本机 32 字节身份映射）、待发投影 ID 暂用 draft_id、`unread_count` 恒为 None 不伪造 0。
- [ ] **R-06（子智能体 F）：NODE-005~011 群日志与信箱（含 HTTP 端点）。** 范围 `crates/node-services`、`crates/node-api`、`apps/node`。进行中。
- [x] **R-03：P0-04 本地加密存储与事务。** `crates/storage/src/formal/` 正式路径（STORAGE-001/003/006/007/009/010/011，002 部分）：独占写锁、版本化迁移与校验和、列级 ChaCha20-Poly1305（用途子密钥 + 行身份 AAD）、`BEGIN IMMEDIATE` 单次事务句柄、意图/尝试幂等与冲突码、提交后 `CommitToken`、`recover_incomplete_writes` 只核对不重造；`crates/crypto/src/key_store.rs` 提供 Argon2id 派生与 32 字节主密钥文件加载。实证：扫描 `.db`/`-wal`/`-shm` 无明文正文/MLS 秘密/主密钥字节；五个真实 `abort()` 断点恢复一致。验证：`cargo test --offline -p chat-storage -p chat-crypto` → 22 + 15 passed，0 failed。

## 接口与文件分工

| 范围 | 你现在可以修改 | 需要先在本文件登记协调的变更 |
|---|---|---|
| UI / 展示 | `shared/ui/**`、`shared/presentation/**` 中的组件、状态和纯函数；对应的局部测试、预览截图和文案 | `apps/desktop`/`apps/android` 的启动入口或生命周期改动，因为它们承载真实 P0 演示 |
| 应用接口 | 消费 [`P0Core`](../shared/core-api/src/commonMain/kotlin/chat/api/P0Core.kt) 与现有 `P0Presenter`；正式 DTO 仅用于独立预览 | 修改 `shared/core-api/**`、`shared/core-bridge/**`、UniFFI 函数或 `ConversationService` 签名 |
| 底层 | 阅读 Rust 代码与报告以理解状态 | 修改 `crates/**`、`apps/node/**`、`tools/verify-p0.ps1`、事务/安全 ADR 或数据库 schema |

不必把所有改动限制成静态画面：UI 状态、导航、校验、错误映射和无副作用的展示模型都可实现。遇到必须新增正式接口的功能，请在“待对接接口”写明调用场景、输入/输出、错误状态及 UI 需要的持久事实，再做预览或其他独立工作；我会按底层进展决定接口并在此更新。不要凭 UI 本地计时器推断节点接受、双方确认或消息送达，也不要在展示层保存私钥、完整邀请口令或明文聊天数据库。

## 验证与交付记录

在项目根目录运行（工具环境见 [依赖记录](DEPENDENCIES.md)）：

```powershell
. .\tools\activate.ps1
.\gradlew.bat :apps:desktop:p0Smoke :apps:desktop:p0Snapshot :apps:android:assembleDebug --offline
& 'C:\Program Files\nodejs\node.exe' tools\verify-project.cjs
```

涉及 P0 真链路或启动入口时，再运行 `.\tools\verify-p0.ps1`。每项完成请在上方勾选，并在下面记录日期、修改文件、命令与结果、截图/测试路径、已知限制。若你在另一份工作副本开发，交付时给出改动文件清单和差异，避免直接覆盖这里同时进行的底层改动。

### DeepSeek 完成记录

| 日期 | 任务 | 修改文件/交付物 | 验证 | 限制或待对接 |
|---|---|---|---|---|
| 2026-10-18 | DS-01 | `shared/ui/src/commonMain/kotlin/chat/ui/P0Screen.kt`；`shared/presentation/.../display/UiCopy.kt` | `. .\tools\activate.ps1; .\gradlew.bat :apps:desktop:p0Smoke :apps:desktop:p0Snapshot :apps:android:assembleDebug --offline` 通过；截图 `build/p0-snapshots/desktop.png`（1024×860）与 `compact.png`（390×844）已逐张检查，可读、无控件重叠；Android APK 重新生成 | 仍是同进程 Alice/Bob 临时链路；文案只声明“接收设备已验证并解密”，不声明正式接受/送达/离线可用 |
| 2026-10-18 | DS-02 | `shared/presentation/src/commonMain/kotlin/chat/presentation/P0Presenter.kt`；`apps/desktop/src/main/kotlin/chat/desktop/PresentationChecks.kt`；`apps/desktop/build.gradle.kts` | `.\gradlew.bat :apps:desktop:presentationChecks --offline` 通过：连点只发一次、发送者/正文在点击时冻结、迟到成功不清新输入、失败结果未知且不自动重发、关闭等待在途发送、UTF-8 上限保留旧草稿；异常原文未进入 UI | 假 `P0Core` 只验证展示层竞态；真实取消/关闭仍受原生同步调用约束；发送结果未知时本端会话需重建 |
| 2026-10-18 | DS-03 | `shared/presentation/.../preview/PreviewChatModels.kt`；`shared/ui/.../PreviewChatScreen.kt`；`apps/desktop/.../PreviewApp.kt`、`PreviewSnapshot.kt`；`apps/desktop/build.gradle.kts` | `.\gradlew.bat :apps:desktop:previewSnapshot :apps:desktop:presentationChecks --offline` 通过；交互窗口入口 `:apps:desktop:previewApp`；截图 `build/preview-snapshots/preview-desktop-{accepted,identity-changed,invite-identity}.png`、`preview-compact-{chat,list,locked}.png` 已检查；900 dp 双栏、低于 900 dp 单栏；发送框仅在样例 `accepted` 启用，身份变化显示阻断提示 | 纯 UI 预览，使用显式样例联系人和样例“已接受/已送达”状态；不连接 P0 真链路或正式服务，不勾选 P1/P2 产品功能 |
| 2026-10-18 | DS-04 | `shared/presentation/.../display/MessageRenderModel.kt`、`ErrorPresentation.kt`、`UiCopy.kt`；验证在 `PresentationChecks.kt` | `presentationChecks` 通过：文本控制字符清理、附件只显示“能力未接入”占位、未知扩展仅纯文本 fallback、未知消息类型安全回退；`STALE_EPOCH` 显示同步中、`LOG_CONFLICT` 提示同步暂停、未知/不可重试错误不提供重试；预览实际渲染扩展 fallback | 纯函数与预览验收通过；正式 [PRESENTATION-009/010](implementation/presentation.md#presentation-009) 仍未勾选，需底层消息/错误接口接入后才能做正式验收 |
| 2026-10-18 | DS-05 | `P0Presenter.kt`、`P0Screen.kt`、`PresentationChecks.kt`、`P0Snapshot.kt` | `presentationChecks` 通过：结果未知后关闭错误横幅、编辑草稿、切换发送者、再次点击发送均保持 `requiresRebuild`，`canSend=false`，草稿保留；重建成功后解除阻断；页面关闭后清除。`p0Snapshot` 新增 `desktop-rebuild-required.png`、`compact-rebuild-required.png`，空态改为“需要重建会话”；`p0Smoke`、Android APK 构建仍通过 | 真实取消/关闭仍受原生同步调用限制；`requiresRebuild` 是展示状态，不代表正式 `AcceptanceUnknown` 对账已完成 |
| 2026-10-18 | DS-06 | `P0Screen.kt`、`PreviewChatScreen.kt`、`P0Snapshot.kt`、`PreviewSnapshot.kt` | 输入框、发送/连接/错误动作、返回/关闭、发送设备、验证详情和身份变化提示补 `contentDescription`/`stateDescription`/live region；P0 在短窗口或窄屏整页滚动；预览场景使用 `FlowRow` 避免大字体溢出。截图 `p0-snapshots/{desktop,compact,desktop-large-font,compact-large-font,desktop-short}.png` 与 `preview-snapshots/{preview-desktop-large-font,preview-compact-large-font,preview-desktop-short}.png` 已检查，无控件重叠；错误和身份变化均有文字标签，不依赖颜色 | 未在 Android TalkBack/桌面读屏真机跑完整无障碍用例；大字体截图使用 1.3×，短窗口使用 1024×520；消息区仍为独立滚动列表 |
| 2026-10-18 | DS-07 | `PreviewChatModels.kt`、`PreviewChatScreen.kt`、`PresentationChecks.kt`、`PreviewSnapshot.kt` | 只接受内置 `PREVIEW_SAMPLE_INVITE_TOKEN`；空输入、任意输入均停留在输入页并显示原因，按钮改为“查看样例核对流程”，并新增“填入内置样例口令”。纯检查覆盖空/任意/样例/返回/关闭/确认；`preview-desktop-invite-warning.png` 已检查；`presentationChecks` 通过 | 不执行签名、有效期或一次性校验，不接正式联系人服务；身份页明确写“样例身份（未验证）” |
| 2026-10-18 | DS-08 | `shared/presentation/.../display/ConversationDisplayMerger.kt`；验证在 `PresentationChecks.kt` | `presentationChecks` 通过：会话列表与消息页按稳定 ID 去重；分页追加、前置与 reset 生效；切换会话、刷新或游标变化后的迟到结果被标记 `ignored`；重复页不重复显示；失败结果同样按代次忽略。接口只输出 `ConversationDisplayState` 和 `DisplayQueryIntent` | 纯展示合并器，未接 `ConversationService`、SQLite 或网络；不推断真实 `SendState`；正式游标/代次/事件字段已写入“待对接接口” |
| 2026-10-18 | DS-09 | `shared/presentation/.../display/AdaptiveLayout.kt`；`shared/ui/.../P0Screen.kt`、`PreviewChatScreen.kt`、`PreviewChatModels.kt`；验证在 `PresentationChecks.kt` | `presentationChecks` 的 DS-09 组通过：899/900 与 719/720 边界、侧栏 288/320/360 分档、窄屏二选一与返回按钮、空白/失效选择回退列表（`navigationFallback`）、尺寸变化保留选择、浮层不改变面板结构、负宽度按 0 处理；P0 与预览共用同一阈值。截图 `p0-snapshots/desktop.png` 与 `preview-snapshots/*` 已复核，布局与改动前一致 | PRESENTATION-011 的页面 scope 挂载/滚动锚点保持仍需正式 `ConversationService` 与 bridge 事件接线；正式函数复选框保持未勾选 |
| 2026-10-18 | DS-10 | `shared/presentation/.../display/DraftSendPresentation.kt`；`shared/presentation/.../preview/PreviewDraftSample.kt`；`PreviewChatScreen.kt`；验证在 `PresentationChecks.kt` | `presentationChecks` 的 DS-10 组通过：节点接受显示“已接受”且明确不等于送达、独立送达证据才显示“已送达”、结果未知只给对账查询并阻断继续发送、可重试/不可重试分流（`PERMISSION_DENIED` 无重试）、脱敏诊断标识、重试/取消重复点击合并、取消不乐观删除与“取消未生效”提示。预览新增“发送状态样例”循环展示六种状态，截图 `preview-desktop-draft-unknown.png`、`preview-desktop-draft-blocked.png`、`preview-compact-draft-delivered.png` 已检查 | 纯展示模型与样例；正式 `retry_draft`/`get_draft_status`/`cancel_queued` 接线与在途对账未完成；预览中的“动作”只作文案展示，不触发真实发送 |
| 2026-10-18 | R-05 | \`crates/storage/src/formal/{cursor,query,outbox_lease}.rs\`、\`tests/{formal_query,formal_outbox}.rs\`、\`formal/mod.rs\`、\`formal/error.rs\`、\`formal/seal.rs\`；\`docs/implementation/storage.md\` | \`cargo test --offline -p chat-storage\` → 39 passed（新增 15 个用例），0 warning。覆盖分页边界/重复页/并发到达、limit 超限、非法/过期/跨会话/跨资源游标、租约到期重领、领取唯一性（版本 CAS + 迟到 worker 拒绝）、依赖与取消跳过、三态标记幂等、\`unknown\` 不推进为完成、mailbox ACK 不升级送达 | 存储游标为内部 \`spg1\`，不是 PROTOCOL-012 \`cur1\`（需本机 32 字节身份映射后替换）；待发投影 ID 暂用 draft_id（依赖 STORAGE-008）；\`unread_count\` 恒 None；\`protected_metadata\` 未解密；outbox 诊断只随返回不落盘 |
| 2026-10-18 | R-04 | \`shared/presentation/.../display/ErrorPresentation.kt\`；验证在 \`apps/desktop/.../PresentationChecks.kt\`；\`docs/implementation/presentation.md\` | \`presentationChecks\` 新增 \`checkFrozenErrorCoverage\` 通过：41 个冻结码全部有稳定文案（不落未知回退）、自动重试集合恰为 7 个且为冻结码子集、\`STORAGE_FAILURE\` 手动可重试但 \`autoRetryAllowed=false\`、\`NETWORK_UNAVAILABLE\` 两者皆真、\`UNKNOWN_RESULT\` 只提供等待对账、\`INVALID_SIGNATURE\` 永不自动重试、\`DEVICE_LIMIT_EXCEEDED\` 文案与“每账户 2 台”一致 | 只做展示层映射；正式 core 错误事件与页面接线未完成，PRESENTATION-010 复选框仍不勾选 |
| 2026-10-18 | R-01 | \`crates/protocol/src/{ids,canonical,decode,signing,extension,cursor,digest,error}.rs\`、\`crates/domain/src/{model,group,delivery,visibility,error}.rs\`、\`crates/protocol/tests/vectors.rs\`；\`docs/implementation/{protocol,domain}.md\` | \`cargo test --offline -p chat-protocol -p chat-domain\` → chat-protocol 45 passed（39 lib + 6 固定向量）、chat-domain 28 passed，0 failed；两 crate clippy 0 warning。固定向量对 canonical、候选/回执签名原文、Ed25519 签名与公钥、cursor token 做 hex 字节相等断言；畸形/超大/未知/重复字段/越界 token 有界拒绝 | PROTOCOL-005/007/008/009/011 与 DOMAIN-004/005 未实现（保持未勾选）；跨语言字节交叉核对未执行；\`receipt_signing_bytes\` 返回 Result、cursor 编解码增加显式 \`unix_ms\`、\`aggregate_delivery\` 返回 Result 属已登记契约偏差 |
| 2026-10-18 | R-02 | \`crates/node-services/src/{auth,registration,canonical,limits,error,services}.rs\`、\`migrations/0002_node_auth.sql\`、\`tests/auth_registration.rs\`；\`crates/node-api/src/*\`、\`tests/http_contract.rs\`；\`docs/implementation/{node,api}.md\` | \`cargo test --offline -p chat-node-services -p chat-node-api\` → 33 passed（lib 11 + auth_registration 18 + 既有 p0_accept 4）+ 8 passed（axum 真 TCP 端到端），0 failed，无编译警告。覆盖正确签名通过、错误密钥/失败签名不消费挑战、重放、跨 method/path/query/body 摘要/node_id/device 绑定拒绝、过期、设备上限默认 2（第三台拒绝且不落库）、重复注册幂等、主管钥更换拒绝、能力作用域与撤销版本、畸形/超大输入（413/未知 schema/非法 hex） | 账户主管钥首次注册为 TOFU（真锚点需 IDENTITY 阶段）；逐 IP 限流 API-005 与撤销端点未实现；NODE-002~016、API-005~010 与 \`apps/node\` 可部署节点仍未实现；测试对 \`chat-crypto\` 的依赖是既有 P0 T4/C05 验收证据，保留不拆 |
| 2026-10-18 | R-03 | \`crates/storage/src/formal/{mod,seal,error,migrations}.rs\`、\`src/formal_crash_worker.rs\`、\`migrations/0002_formal_store.sql\`、\`tests/{formal_store,formal_crash}.rs\`；\`crates/crypto/src/key_store.rs\`、\`tests/formal_master_key.rs\`；\`docs/implementation/{storage,crypto}.md\` | \`cargo test --offline -p chat-storage -p chat-crypto\` → 22 passed（3 lib AEAD + 17 formal_store + 2 formal_crash）+ 15 passed（3 engine + 5 formal_master_key + 7 既有 p0_persistence），0 failed。提交后扫描 \`.db\`/\`-wal\`/\`-shm\` 原始字节无明文正文/MLS 秘密/主密钥；错误密钥、搬移/篡改密文拒绝且不覆盖；五个真实子进程 \`abort()\` 断点恢复一致；恢复只核对不重造密文 | 仅 Windows x64 实测；应用层列级加密而非全库加密；未模拟断电 fsync 边界；MLS provider（CRYPTO-004/005/006）、CRYPTO-007/008 与附件加解密未接入；SQLite 回滚失败无法注入（fail-closed 经 mark_faulted 验证）；STORAGE-002 只做迁移器。既有 P0 实验文件（store.rs、persistence.rs、crash_worker.rs、0001_initial.sql）一行未改 |
| 2026-10-18 | DS-12 | `shared/presentation/.../display/MarkdownModel.kt`；`shared/ui/.../MarkdownText.kt`；`PreviewChatScreen.kt`（消息卡片 + 样例发送框）、`PreviewChatModels.kt`；验证在 `PresentationChecks.kt` | `presentationChecks` 的 DS-12 组通过：标题/列表/引用/围栏与行内代码/粗体/斜体/删除线/链接解析正确；`<script>` 只作为文字保留（模型无 HTML 节点）；`javascript:` 链接降级为不可点击并给原因、`https` 保留；围栏内 Markdown 标记不解析；反斜杠转义与未闭合标记按普通文字处理；超长输入标记 `truncated` 并截断到上限；空内容回退 `（空消息）`；文本消息返回 Markdown、附件返回 null；输入命令覆盖选区包裹、空选区选中占位、链接模板与越界夹取。截图 `preview-desktop-accepted.png` 等 16 张已复核，消息卡片显示标题/列表/引用/行内代码/链接，发送框显示格式工具栏与 8192 字节计数 | 链接打开能力未接入（只显示文字与地址）；P0 实验页面仍按纯文本显示加密往返结果；Markdown 只在展示层解析，正式消息存储仍是纯文本正文 |
| 2026-10-18 | DS-11 | `shared/presentation/.../display/PagingPresentation.kt`；`shared/presentation/.../preview/PreviewPagingSample.kt`；`PreviewChatScreen.kt`、`PreviewSnapshot.kt`；验证在 `PresentationChecks.kt` | `presentationChecks` 的 DS-11 组通过：加载中不给可点动作、失败提供单次重试并带语义播报、还有更多/没有更多/空列表映射正确；预览样例经 DS-08 合并器真实跑“成功→失败→重试成功→到底”，失败不丢已加载结果、加载中重复点击不产生第二次请求、到底后重置。截图 `preview-desktop-paging-failed.png`、`preview-desktop-paging-no-more.png`、`preview-compact-paging-failed.png` 已检查 | 只映射展示提示，未查询真实列表/消息游标；“加载中”提示由纯状态用例覆盖（样例脚本同步完成，界面不出现真实等待） |

第四轮验证（2026-10-18，全栈第一批）：`cargo test --workspace --offline` 全绿（Rust 合计 151 个测试：protocol 45、domain 28、storage 22、crypto 15、node-services 33、node-api 8；另有既有 P0 core/transport 测试）；`cargo check --workspace --locked --offline` 通过；`presentationChecks`（13 组）、`p0Smoke`、`p0Snapshot`（7 张）、`previewSnapshot`（16 张）与 `:apps:android:assembleDebug` 全部 `BUILD SUCCESSFUL`（3m 41s）；`tools/verify-project.cjs` PASS（303 文件 / 197 函数 / 1323 链接）。因 P0 真链路文件（`crates/core/src/p0.rs`、`crates/ffi`、启动入口）本轮未改动，未重跑 `tools/verify-p0.ps1`。

第三轮验证（2026-10-18）：按计划书命令复跑 `presentationChecks`（12 组全通过）、`previewSnapshot`（16 张截图）、`p0Smoke`、`p0Snapshot`（7 张）与 `:apps:android:assembleDebug`，全部 `BUILD SUCCESSFUL`（完整回归 1m 35s）；`tools/verify-project.cjs` 为 PASS。新增文件只在展示层 `display/`、`preview/` 与预览组件内；正式 P0 页面只把紧凑宽度阈值改为调用 `buildAdaptiveLayout(...)`，未改启动入口。

本轮未修改 `shared/core-api/**`、`shared/core-bridge/**`、`crates/**`、`apps/node/**`、`tools/verify-p0.ps1` 或数据库 schema，也没有改动 `apps/desktop`/`apps/android` 的正式启动入口。预览使用独立的 `chat.desktop.PreviewAppKt` 与 `:apps:desktop:previewApp`。因启动入口和底层链路未变，本轮未重跑 `tools/verify-p0.ps1`；真实 P0 由 `p0Smoke`、`p0Snapshot` 与 Android APK 构建覆盖，根目录 `start-chat.cmd` 保持可用。

本机 Kotlin 编译守护进程仍会因 `%LOCALAPPDATA%\kotlin` 临时文件权限报 `AccessDeniedException`，Gradle 自动回退到无守护进程编译；上述命令最终均为 `BUILD SUCCESSFUL`，未修改工具链或锁文件。

## 我负责的底层工作与同步方式

当前优先补 P0-03 正式事务/未知结果对账，以及节点鉴权与签名回执所需的底层契约；之后继续 P0-04 本地加密保护、P0-05 自托管网络和三端运行。具体是否完成仍以 [P0 清单](implementation/P0_CHECKLIST.md) 的勾选和实际报告为准。

**同步约定：每次我完成一个底层变更或改变接口/状态语义，在回复用户前同步更新本文件的“真实进度”“待对接接口”和下面的变更记录，写明可用性、调用入口、错误/状态语义、验证证据及对当前 DS 任务的影响。** 这份文件是双方在项目目录中的共享交接点；更新文件不会自动向 DeepSeek 发送消息，交接时请把文件路径或内容提供给它。

### 待对接接口

| 能力 | 当前状态 | UI 需要等待的事实 | 接入条件 |
|---|---|---|---|
| 持久发送/重试 | R-03 已交付正式存储路径（STORAGE-001/003/006/007/009/010/011）：意图/尝试幂等、列级加密、单次事务句柄、提交令牌与崩溃恢复；DS-10 的 `DraftEvidence` → `DraftUiState` 纯展示模型已就绪。**尚未**接 `ConversationService` 与 UniFFI | 稳定 `DraftId`、发送票据、可查询的真实状态与 `AcceptanceUnknown`；UI 按证据填 `DraftEvidence` 即可 | 把 formal store 接到 core/ffi（新 FFI 方法需登记），并用真实发送链路产生 T1/T2 记录 |
| 节点回执/送达 | R-01 冻结 `receipt_signing_bytes` 与 41 个稳定错误码；R-02 交付节点 challenge/Ed25519 鉴权与设备注册（真 TCP）；**尚无**正式投递 worker、信箱与回执发放端点 | 经正式协议验证的“已接受”及独立“已送达”证据 | NODE-006/009/010/011（群日志 append/fetch、信箱存取/ACK）与回执签发接线；客户端 outbox 投递循环 |
| 好友口令/会话列表 | 产品设计和接口草案，无正式服务 | 对端身份/指纹、双方确认、稳定会话 ID | 身份与联系人服务及持久查询 API |
| 锁定/加密库 | R-03 已交付主密钥（Argon2id 派生/密钥文件）、列级 AEAD 与错误密钥拒绝；**平台密钥库**（Android Keystore / Windows DPAPI / Linux Secret Service）未接入 | 可查询的锁定/恢复状态与解锁动作 | 平台密钥保护 + 锁定状态机 + 恢复文件流程；锁定状态需要由 core 暴露给 UI |
| 会话列表/消息分页 | DS-08 合并器与 DS-11 提示映射已就绪；R-01 已冻结认证分页游标（PROTOCOL-012/013）；**尚无** STORAGE-012/013 的正式查询与 bridge 事件 | 稳定 ConversationId/MessageId；游标方向（初始/向前/向后）；每页 `reset`/`prepend`；请求代次；loading/error；事件去重 | 实现 STORAGE-012/013 查询 + core/ffi 事件流后接到 `ConversationDisplayState`；UI 不推断真实 `SendState` |

### 底层变更记录（由我维护）

| 日期 | 底层变化与证据 | 对 DeepSeek 的影响 |
|---|---|---|
| 2026-10-18 | 建立本任务书；当前 P0 回归 [PASS](p0-validation-latest.json)，客户端与节点独立事务实验共 14 个进程终止断点通过 | DS-01/02 可直接做；DS-03/04 可做明确标识的预览/纯展示；正式状态接口尚不可调用 |
| 2026-10-18 | 节点 P0 回执增添 Ed25519 签名、实验库公钥固定和篡改拒绝；[14:56 完整回归](p0-validation-latest.json)及非 ASCII 伪造签名针对性复跑通过；根目录启动脚本已实际拉起并正常关闭桌面窗口 | 尚未形成 UI 可用的正式接受状态；不要把实验签名回执接到预览或真实 P0 页面。开始 DS-05 至 DS-08 |
| 2026-10-18 | 桌面启动入口在窗口显示时主动置前并向控制台输出就绪提示；根目录脚本说明 `:apps:desktop:run` 会持续运行。实测窗口标题及非零句柄，关闭后 `BUILD SUCCESSFUL in 28s` | 桌面 P0 入口已改动；DS-05 至 DS-08 保留该启动反馈，仍按上方界面/展示分工开发，不据此推断正式消息状态 |
| 2026-10-18 | R-01 线协议冻结与领域规则：新增 \`crates/protocol\` 九个模块与 \`crates/domain\` 五个模块，固定字节向量测试；\`cargo test -p chat-protocol -p chat-domain\` 73 passed；\`cargo check --workspace --locked --offline\` 通过 | 协议字节、签名原文、分页游标与 41 个错误码已可被 UI 依赖的错误映射复用；UI 侧 \`presentError\` 的稳定码集合与 PROTOCOL-014 对齐工作可在下一轮做 |
| 2026-10-18 | R-02 节点鉴权：challenge/Ed25519 鉴权/设备注册（默认 2 台上限）跑通 axum 真 TCP 端到端，41 个测试；迁移 \`0002_node_auth.sql\` 为新增，0001 未改 | 好友口令/设备注册的正式鉴权基础就绪，但联系人服务（IDENTITY）未做，UI 仍只能用预览样例；不要据节点鉴权推断“已接受/已送达” |
| 2026-10-18 | R-03 正式存储与主密钥：\`crates/storage/src/formal\` + \`crates/crypto/src/key_store.rs\`，列级 AEAD、独占锁、单次事务句柄、崩溃恢复、磁盘字节无明文；37 个测试 | UI 侧 DS-10 的 \`DraftEvidence\` 已可承接“本地排队/节点接受/送达”证据，但 formal store 与 core/ffi 之间还没有桥接；锁定状态需要 core 暴露后才能做正式锁定页 |
| 2026-10-18 | 用户报告资源管理器双击后虽有窗口就绪提示，但 Alt+Tab 无窗口；Win32 检查窗口 visible 且坐标在主屏幕内。`tools/run-desktop.ps1` 加 `--no-daemon` 隔离启动进程；本机验证 single-use daemon、窗口创建、关闭后 `BUILD SUCCESSFUL in 1m 8s`；用户随后从资源管理器双击复验，确认窗口可见 | DS-05 至 DS-08 无正式接口变化；不要移除桌面启动反馈或改回可复用 daemon |

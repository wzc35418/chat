# 设备直连、中继与节点客户端

阶段：P0 → P1。状态：Rust **纯逻辑传输核心**已实现于 [crates/transport](../../crates/transport/src)（会话建立、连接状态机、帧编解码/分片重组、确定性重试退避，见下方「Rust 纯逻辑传输核心」章节）；P0 仅 loopback 的 iroh/QUIC 实验仍在 [network.rs](../../crates/transport/src/network.rs)。真正的直连/穿透/中继与节点客户端（TRANSPORT-006~012）尚未实现。目标源码：[crates/transport/src/network.rs](../../crates/transport/src/network.rs)。

网络层传送相同不可变协议帧，不负责判断已送达。节点公钥与 HTTPS 身份均验证；用户配置明确替换公共发现/中继。连接句柄不可给 UI 或插件。认证细节在 node-api 的函数计划中定义。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [TRANSPORT-001 start_endpoint](transport.md#transport-001)
- [TRANSPORT-002 verify_discovery_record](transport.md#transport-002)
- [TRANSPORT-003 connect_peer](transport.md#transport-003)
- [TRANSPORT-004 send_peer_frame](transport.md#transport-004)
- [TRANSPORT-005 read_peer_frame](transport.md#transport-005)
- [TRANSPORT-006 append_candidate](transport.md#transport-006)
- [TRANSPORT-007 fetch_group_entries](transport.md#transport-007)
- [TRANSPORT-008 put_mailbox](transport.md#transport-008)
- [TRANSPORT-009 pull_mailbox](transport.md#transport-009)
- [TRANSPORT-010 ack_mailbox](transport.md#transport-010)
- [TRANSPORT-011 subscribe_node_hints](transport.md#transport-011)
- [TRANSPORT-012 shutdown_transport](transport.md#transport-012)

## Rust 纯逻辑传输核心（chat-transport，本轮实现）

本轮只实现**不需要真实网络**的四块：会话建立、连接状态机、帧编解码/分片重组、重试退避。不引入新的网络依赖（iroh 仍只用于既有 P0 loopback 实验）、不联网、不 sleep、无 `unsafe`。crate 为 `crates/transport`（包名 `chat-transport`）。

### 覆盖范围与文档函数的关系

| 本轮交付（任务 ID） | API | 与文档契约的关系 |
|---|---|---|
| TRANSPORT-001 会话建立 | `TransportSession::open` / `open_session` | `start_endpoint` 的纯逻辑子集：端点/设备标识、协商结果、初始连接态、会话预算；真实端点启动、密钥装载、运行时与入站队列未实现 |
| TRANSPORT-002 连接状态机 | `ConnectionStateMachine` / `next_state` / `TRANSITIONS` | `connect_peer` 的连接生命周期子集：只做合法迁移与幂等判定，不发起网络 |
| TRANSPORT-003 帧编解码 | `FrameCodec` / `FragmentAssembler` | `send_peer_frame`/`read_peer_frame` 的帧边界子集：长度前缀、分片、重组与拒绝；ACK/队列/背压未实现 |
| TRANSPORT-004 重试退避 | `RetryPolicy` / `RetryTracker` | `connect_peer` 的 `ConnectBudget` 与全局「网络类错误按持久退避」策略；不执行等待 |

TRANSPORT-002（verify_discovery_record）与 TRANSPORT-006~012（节点 append/fetch/mailbox/WS/shutdown）仍未实现；P0 的 loopback 端到端实验不等于这些函数完成。

### 会话建立（TRANSPORT-001）

- 输入：本机/对端 `EndpointId`（32 字节，非零且互不相同）、本机/对端 `DeviceId`（非零且互不相同）、可选 `SessionCapabilities`（PROTOCOL-011 的协商输出）、建立时间、`SessionLimits`。
- 校验顺序：预算 → 标识 → 互斥关系 → 协商结果。协商结果必须：版本受支持、包含必需能力 `e2ee.v1`/`mls.v1`、协商的 `max_frame_bytes` 不小于会话上限（**只能收紧、不能放大**）；否则 `CAPABILITY_MISMATCH`。
- 初始状态固定 `Disconnected`；未协商（`negotiated: None`）时允许建立会话对象，但 `Connect` 事件返回 `CAPABILITY_NOT_NEGOTIATED`。
- 会话预算：帧 ≤64 KiB、入站/出站队列 1–4096、重连次数 1–100、握手超时 1–60 s；非法配置显式拒绝。

### 连接状态机（TRANSPORT-002）

状态：`Disconnected`、`Connecting`、`Connected`、`Degraded`、`Closed`；事件：`Connect`、`Connected`、`Degrade`、`Recover`、`Disconnect`、`Close`。冻结迁移表（19 条合法迁移，自环为幂等重放）：

| 当前状态 | 合法事件 → 目标 |
|---|---|
| Disconnected | Connect → Connecting；Disconnect → Disconnected；Close → Closed |
| Connecting | Connect → Connecting；Connected → Connected；Disconnect → Disconnected；Close → Closed |
| Connected | Connect → Connected；Connected → Connected；Degrade → Degraded；Recover → Connected；Disconnect → Disconnected；Close → Closed |
| Degraded | Degrade → Degraded；Connected → Connected；Recover → Connected；Disconnect → Disconnected；Close → Closed |
| Closed | Close → Closed（其余事件一律拒绝） |

表外迁移返回 `INVALID_TRANSITION` 且状态不变；`TransitionTableCheck::verify()` 可在宿主启动时自检表一致性（状态/事件/合法/非法计数为 5/6/19/11）。

### 帧编解码与分片重组（TRANSPORT-003）

冻结线格式（每片一条记录）：

```text
[u32 BE wire_len][u32 BE frame_id][u16 BE index][u16 BE count][u32 BE total_len][payload]
```

- `wire_len = 12 + payload_len`（不含自身 4 字节）；解码先核对前缀与实际长度，再解析字段。
- 分片大小固定为配置的 `max_fragment_bytes`（协商结果的一部分）：`count = ceil(total_len / max_fragment_bytes)`，非末尾分片长度必须等于该值；非规范切分直接 `MALFORMED_FRAME`。
- 上限：帧 1–64 KiB、分片 1–`max_frame_bytes`、分片数 1–1024，且 `max_fragments_per_frame` 必须足够切分最大帧（否则 `INVALID_FRAME_LIMITS`）。所有长度先检查后分配：声明超大总长/前缀不会触发大内存分配。
- 重组严格有序：索引必须从 0 连续递增。重复分片 `DUPLICATE_FRAGMENT`、乱序 `OUT_OF_ORDER_FRAGMENT`、旧帧 `STALE_FRAME`、未收齐换帧或总长不一致 `INCOMPLETE_FRAME`；`reset()` 丢弃在途状态。

### 重试退避（TRANSPORT-004）

- 基础延迟 `min(base × 2^(n-1), max)`；`max_delay_ms` 受 PROTOCOL-014 的 5 分钟上限约束（`MAX_RETRY_AFTER_MS`）。
- jitter：`[0, jitter_max_ms]` 内的确定性偏移（splitmix64 混淆，仅用于打散同步重试，**不是安全随机**），最终延迟仍封顶；固定 seed 使测试完全可复现。
- 预算：连续失败超过 `max_attempts`（默认 8，上限 100）返回 `RetryOutcome::Stop { BudgetExhausted }`，不无限重试；成功后可 `reset()`；`next_attempt_at_ms` 饱和。
- 本模块不 sleep：只返回延迟与下一次尝试时间，由宿主执行等待。

### 传输层稳定错误码（登记）

| 码 | 触发 | 可重试 | 与 PROTOCOL-014 关系 |
|---|---|---|---|
| `INVALID_TRANSPORT_CONFIG` | 会话/端点/设备/预算非法 | 否 | 本层新增码，未进冻结表 |
| `INVALID_TRANSITION` | 状态机表外迁移 | 否 | 本层新增码 |
| `CAPABILITY_MISMATCH` | 协商版本/必需能力/帧上限不兼容 | 否 | **复用** `PROTOCOL_UNSUPPORTED`（`frozen_code()` 可映射） |
| `CAPABILITY_NOT_NEGOTIATED` | 未协商即连接 | 否 | 本层新增码 |
| `MALFORMED_FRAME` | 帧结构/长度/规范切分非法 | 否 | **复用**冻结码 `MALFORMED_FRAME` |
| `CONTENT_TOO_LARGE` | 帧/分片/记录超限 | 否 | **复用**冻结码 `CONTENT_TOO_LARGE` |
| `INVALID_FRAME_LIMITS` | 帧预算非法 | 否 | 本层新增码 |
| `DUPLICATE_FRAGMENT` | 分片重复 | 否 | 本层新增码 |
| `OUT_OF_ORDER_FRAGMENT` | 分片索引跳跃 | 否 | 本层新增码 |
| `STALE_FRAME` | 旧帧迟到 | 否 | 本层新增码 |
| `INCOMPLETE_FRAME` | 未收齐换帧/总长不一致 | 否 | 本层新增码 |
| `INVALID_RETRY_POLICY` | 退避参数非法 | 否 | 本层新增码 |

映射约定：本层错误一律不可自动重试；跨 FFI 暴露时冻结码可直接进入 `chat_protocol::map_error`，新增码在协议层登记前映射为 `INTERNAL_FAILURE` 并保留本层码做诊断。

### 验证证据

`cargo test --offline -p chat-transport`（2026-10-18）→ **33 passed / 0 failed**（lib 25 + `tests/transport_core.rs` 8），`cargo clippy --offline -p chat-transport --all-targets` 0 警告。

- 状态机：`state::tests::transition_table_matches_the_full_matrix`（5×6 逐格）、`repeated_events_are_idempotent`、`closed_is_terminal_except_repeated_close`、`illegal_transitions_are_explicit`，以及 `transport_core::transition_matrix_is_complete_and_unique`。
- 会话：`session::tests::opens_session_in_disconnected_state`、`rejects_invalid_session_inputs`、`session_limits_are_bounded`、`negotiated_frame_limit_cannot_be_enlarged`、`session_lifecycle_uses_the_state_machine`、`transport_core::sessions_cannot_connect_without_negotiation_or_enlarge_limits`。
- 帧：`frame::tests::round_trip_single_and_multi_fragment`、`wire_format_round_trip_and_byte_vectors`、`frame_and_fragment_boundaries_are_enforced`、`malformed_wire_is_rejected_before_allocation`、`assembler_rejects_duplicate_out_of_order_stale_and_incomplete`、`random_wire_input_never_panics`、`transport_core::frame_*_rejects_*`。
- 退避：`retry::tests::exponential_base_delay_is_capped`、`jitter_is_bounded_deterministic_and_capped`、`budget_exhaustion_stops_retrying`、`next_attempt_time_is_saturating`、`invalid_policies_are_rejected`、`transport_core::retry_sequence_is_deterministic_and_bounded`。
- 端到端纯逻辑闭环：`transport_core::session_frame_retry_flow_is_closed_loop`（建立会话 → 连接 → 分片/重组 → 降级/恢复 → 退避预算 → 关闭）。

<a id="transport-001"></a>

## TRANSPORT-001 · start_endpoint

~~~text
start_endpoint(config: TransportConfig, device_key: DeviceKeyRef) -> Result<EndpointHandle>
~~~

**职责与输出**：启动 iroh 端点并返回受控句柄。

**输入与前置条件**：P0 验证目标 ABI；密钥已保护；公共 fallback 默认显式策略。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证配置。
2. 装载设备端点密钥。
3. 建立运行时。
4. 注册有界入站队列。
5. 记录脱敏端点状态。

**持久化、并发与副作用**：启动可取消；失败关闭已创建任务；不写消息状态。

**失败处理**：INVALID_CONFIG，ENDPOINT_START_FAILED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 关闭公共服务后仍使用自托管。
- [ ] 端口冲突无遗留任务。

**实现状态**：部分实现（Rust 纯逻辑子集：`TransportSession::open`/`open_session` 校验端点/设备标识、协商结果、初始状态与会话预算，见上方章节；证据 `cargo test --offline -p chat-transport` → 33 passed，`session::tests::*`、`transport_core::sessions_cannot_connect_without_negotiation_or_enlarge_limits`）。iroh 端点启动、设备密钥装载、运行时/任务取消、有界入站队列与端口冲突清理未实现（`network.rs` 只有 P0 loopback 实验，不构成本函数验收），两项验收未勾选。

<a id="transport-002"></a>

## TRANSPORT-002 · verify_discovery_record

~~~text
verify_discovery_record(record: SignedDiscoveryRecord, credential: VerifiedCredential, cached_version: u64) -> Result<VerifiedPeerAddress>
~~~

**职责与输出**：验证设备地址版本、防旧记录回滚与期限。

**输入与前置条件**：记录携带设备签名，地址/URL受策略限制。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证签名。
2. 核对端点绑定。
3. 检查期限。
4. 比较单调版本。
5. 剔除未授权地址。
6. 输出已验证地址。

**持久化、并发与副作用**：只在验证后更新地址缓存；不能顺便更新信任根。

**失败处理**：INVALID_SIGNATURE，DISCOVERY_ROLLBACK，RECORD_EXPIRED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重放旧版本拒绝。
- [ ] 伪造局域网地址不能绑定他人设备。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-003"></a>

## TRANSPORT-003 · connect_peer

~~~text
connect_peer(peer: VerifiedPeerAddress, budget: ConnectBudget) -> Result<Connection>
~~~

**职责与输出**：按受支持路径尝试直连/穿透/中继。

**输入与前置条件**：peer 已验证；尝试共享总超时，禁止无限并发探测。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 尝试已知局域网/直接地址。
2. 进行库支持的穿透。
3. 失败按配置使用中继。
4. 验证对端 EndpointId。
5. 输出路径诊断。

**持久化、并发与副作用**：网络异步；取消关闭所有竞赛连接；不持 DB tx。

**失败处理**：NETWORK_UNAVAILABLE，PEER_UNREACHABLE，ENDPOINT_MISMATCH。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[TRANSPORT-002 verify_discovery_record](transport.md#transport-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 对称 NAT 回退。
- [ ] 中继不可用明确错误。
- [ ] 取消无后台重试泄漏。

**实现状态**：部分实现（Rust 纯逻辑子集：连接状态机 `ConnectionStateMachine`/`next_state` 与确定性重试退避 `RetryPolicy`/`RetryTracker`，见上方章节；证据 `state::tests::transition_table_matches_the_full_matrix`、`retry::tests::budget_exhaustion_stops_retrying`、`transport_core::session_frame_retry_flow_is_closed_loop`）。局域网/穿透/中继路径、超时总预算、取消关闭竞赛连接与 `EndpointId` 验证未实现，三项验收未勾选。

<a id="transport-004"></a>

## TRANSPORT-004 · send_peer_frame

~~~text
send_peer_frame(connection: Connection, frame: ImmutableFrame) -> Result<TransportAck>
~~~

**职责与输出**：传输确认只表示字节接收。

**输入与前置条件**：帧已完成规范编码/签名，长度符合双方协商。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验长度。
2. 写入帧和边界。
3. 等待有限 ACK。
4. 验证连接关联。
5. 返回纯传输证据。

**持久化、并发与副作用**：不改变 ACCEPTED/DELIVERED；网络重试不能改密文。

**失败处理**：TRANSPORT_TIMEOUT，CONNECTION_CLOSED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-011 negotiate_capabilities](protocol.md#protocol-011)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] ACK 丢失后重发相同 bytes。
- [ ] P2P 待确认消息不触发正式插件行为。

**实现状态**：部分实现（Rust 纯逻辑子集：帧长度/分片/线格式编码与上限检查，见上方章节；证据 `frame::tests::wire_format_round_trip_and_byte_vectors`、`frame_and_fragment_boundaries_are_enforced`、`transport_core::frame_decode_rejects_oversize_without_allocation`）。连接关联校验、有限 ACK 等待、超时错误与同字节重发（重试策略已就绪）未实现，两项验收未勾选。

<a id="transport-005"></a>

## TRANSPORT-005 · read_peer_frame

~~~text
read_peer_frame(connection: Connection, limits: FrameLimits) -> Result<UnverifiedFrame>
~~~

**职责与输出**：输出有界未认证帧交给核心验证。

**输入与前置条件**：连接身份只证明端点，不代表应用事件授权。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读长度头。
2. 预拒绝超限。
3. 按预算读取。
4. 有界解析。
5. 投递入站队列。
6. 队列满施加背压。

**持久化、并发与副作用**：不写正式 inbox 去重；断开时保留已耐久队列由上层处理。

**失败处理**：MALFORMED_FRAME，CONTENT_TOO_LARGE，BACKPRESSURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-003 decode_bounded](protocol.md#protocol-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 超大长度声明不分配巨内存。
- [ ] 伪造 EventId 不被提前去重。

**实现状态**：部分实现（Rust 纯逻辑子集：有界解码 `FrameCodec::decode_wire` 与严格有序分片重组 `FragmentAssembler`，长度先检查后分配；证据 `frame::tests::malformed_wire_is_rejected_before_allocation`、`assembler_rejects_duplicate_out_of_order_stale_and_incomplete`、`random_wire_input_never_panics`）。连接读循环、入站队列投递与背压未实现；内层去重仍归 protocol/sync（伪造 EventId 不会被本层提前去重），两项验收未勾选。

<a id="transport-006"></a>

## TRANSPORT-006 · append_candidate

~~~text
append_candidate(node: VerifiedNode, candidate: ImmutableCandidate) -> Result<AppendOutcome>
~~~

**职责与输出**：返回正式签名回执、明确拒绝或结果未知。

**输入与前置条件**：固定 SubmissionId/候选字节；设备请求鉴权有效。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 规范请求并签名。
2. 带限时发送。
3. 有界读响应。
4. 校验回执绑定。
5. 超时返回 Unknown 而非 Rejected。

**持久化、并发与副作用**：无 DB tx；本地 worker 负责耐久记录结果。

**失败处理**：NODE_UNAVAILABLE，STALE_EPOCH，STALE_CONTROL_HEAD，ACCEPTANCE_UNKNOWN。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-007 verify_log_entry](protocol.md#protocol-007)。客户端用注入 RequestSigner 生成签名；authenticate_request 是节点端验签函数，transport 不调用服务器实现。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [ ] 响应丢失按同 ID 重新 append 得原结果。
- [ ] 篡改回执拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-007"></a>

## TRANSPORT-007 · fetch_group_entries

~~~text
fetch_group_entries(node: VerifiedNode, group: GroupId, after: u64, limit: u32) -> Result<UnverifiedLogPage>
~~~

**职责与输出**：有界拉取连续日志材料，正式验证归 sync。

**输入与前置条件**：节点授权设备属于允许可见范围。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 签名请求。
2. 限制条数/响应字节。
3. 读取页。
4. 核对群与起点。
5. 返回未验证条目。

**持久化、并发与副作用**：不提前推进本地 verified cursor。

**失败处理**：NODE_UNAVAILABLE，HISTORY_NOT_AUTHORIZED，MALFORMED_RESPONSE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-003 decode_bounded](protocol.md#protocol-003)。客户端用注入 RequestSigner 生成签名；authenticate_request 是节点端验签函数，transport 不调用服务器实现。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [ ] 节点跳号返回给 sync 检测。
- [ ] 巨响应截断为错误。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-008"></a>

## TRANSPORT-008 · put_mailbox

~~~text
put_mailbox(item: AuthorizedMailboxItem) -> Result<MailboxReceipt>
~~~

**职责与输出**：返回节点持久信箱 ACK，不能显示已送达。

**输入与前置条件**：目标设备/能力/TTL明确，payload 只含密文与必要路由。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 签名请求。
2. 写目标条目。
3. 验证 item 摘要回执。
4. 输出 MailboxStored 类型。

**持久化、并发与副作用**：网络重试同 item ID 与同摘要。

**失败处理**：INVALID_CAPABILITY，QUOTA_EXCEEDED，NODE_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或注入端口。客户端用注入 RequestSigner 生成签名；authenticate_request 是节点端验签函数，transport 不调用服务器实现。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [ ] 过期能力拒绝。
- [ ] ACK 后收件人离线仍不是 delivered。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-009"></a>

## TRANSPORT-009 · pull_mailbox

~~~text
pull_mailbox(cursor: Option<MailboxCursor>, limit: u32) -> Result<MailboxPage>
~~~

**职责与输出**：获取当前设备信箱条目，保持稳定翻页位置。

**输入与前置条件**：鉴权设备固定；cursor 不跨设备。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验游标。
2. 签名请求。
3. 有界读取。
4. 核对 target device。
5. 返回条目与游标候选。

**持久化、并发与副作用**：获取成功不删除条目、不推进正式群游标。

**失败处理**：INVALID_CURSOR，DEVICE_REVOKED，NODE_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-013 decode_page_cursor](protocol.md#protocol-013)。客户端用注入 RequestSigner 生成签名；authenticate_request 是节点端验签函数，transport 不调用服务器实现。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [ ] WS 丢失后仍完整拉取。
- [ ] 重复页上层幂等处理。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-010"></a>

## TRANSPORT-010 · ack_mailbox

~~~text
ack_mailbox(ids: Vec<MailboxItemId>, proof: LocalPersistProof) -> Result<()>
~~~

**职责与输出**：本地验证并耐久落盘后确认信箱消费。

**输入与前置条件**：proof 不由传输 ACK 生成；条目均归当前设备。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 限制批次。
2. 绑定 IDs 和设备签名。
3. 发送 ACK。
4. 成功记录可重试完成状态。

**持久化、并发与副作用**：与本地已提交消费记录配合；ACK 失败可重复发送。

**失败处理**：PERMISSION_DENIED，NODE_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或注入端口。客户端用注入 RequestSigner 生成签名；authenticate_request 是节点端验签函数，transport 不调用服务器实现。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [ ] 先 crash 再 ACK 仍能恢复。
- [ ] 他设备 item 不可确认。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-011"></a>

## TRANSPORT-011 · subscribe_node_hints

~~~text
subscribe_node_hints(token: OneTimeWsToken) -> Result<HintStream>
~~~

**职责与输出**：仅提供有更新提示，丢失提示由游标同步兜底。

**输入与前置条件**：短期一次性 WS token，不把长期秘密放 URL。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 升级连接。
2. 消费一次性 token。
3. 创建有界队列。
4. 合并重复提示。
5. 断线触发重新拉取。

**持久化、并发与副作用**：提示非权威存储；溢出触发 resync 标记。

**失败处理**：WS_AUTH_EXPIRED，NETWORK_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 提示重复/乱序不重复消息。
- [ ] token 重用拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-012"></a>

## TRANSPORT-012 · shutdown_transport

~~~text
shutdown_transport(handle: EndpointHandle, deadline: Duration) -> Result<ShutdownReport>
~~~

**职责与输出**：关闭端点、WS 和网络任务并报告超时。

**输入与前置条件**：允许重复调用，不能再接收新任务。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 标记 draining。
2. 停止连接建立。
3. 取消流。
4. 等待有界完成。
5. 释放密钥引用和运行时。

**持久化、并发与副作用**：未完成可靠任务由 outbox 下次恢复。

**失败处理**：SHUTDOWN_TIMEOUT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重复 shutdown 不崩溃。
- [ ] 睡眠恢复用新连接。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。


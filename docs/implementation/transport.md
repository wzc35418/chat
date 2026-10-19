# 设备直连、中继与节点客户端

阶段：P0 → P1。状态：Rust **纯逻辑传输核心 + 对端收发管线 + 真实 iroh 端点（默认回环，可显式 `allow_lan`/中继）**已实现于 [crates/transport](../../crates/transport/src)（会话建立、连接状态机、帧编解码/分片重组、确定性重试退避、可注入 wire 端口的发送/接收管线与 Pull 计划映射，以及回环/LAN 的真实 iroh 端点/连接、显式中继配置、配置快照与 wire 适配，见下方「Rust 纯逻辑传输核心」章节）；P0 仅 loopback 的 iroh/QUIC 实验仍在 [network.rs](../../crates/transport/src/network.rs)。对称 NAT 穿透、自托管中继、发现记录验证与节点客户端（TRANSPORT-002、006、008~012）尚未实现。目标源码：[crates/transport/src/network.rs](../../crates/transport/src/network.rs)。

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

### 对端收发管线（TRANSPORT-005~007，本轮）

在上面的纯逻辑核心之上，用**可注入 wire 端口**实现对端收发管线；仍不引入真实 IO、不 sleep、无 `unsafe`。

| 本轮交付（任务 ID） | API | 与文档契约的关系 |
|---|---|---|
| TRANSPORT-005 send_peer_frame | `send::send_peer_frame`、`SendBudget`、`SendOutcome` | 文档 TRANSPORT-004 `send_peer_frame` 的发送路径子集：编码→分片→逐片写 `WireSink`，带预算与可重试/致命判定；ACK 等待与连接关联未实现 |
| TRANSPORT-006 receive_peer_frame | `receive::receive_peer_frame`、`PeerFrameReceiver`、`ReceiveBudget` | 文档 TRANSPORT-005 `read_peer_frame` 的接收路径子集：逐片喂重组器并绑定会话/端点；入站队列与背压未实现 |
| TRANSPORT-007 Pull 映射层 | `pull_map::plan_pull_requests`、`PullRange`、`ResponseValidator` | 文档 TRANSPORT-007 `fetch_group_entries` 的请求计划与响应校验子集（分页/序号绑定/去重）；HTTP、请求签名与页正文解析未实现 |

**wire 端口**（`wire.rs`）：`WireSink::write_record`（写一条长度前缀分片记录，`queued_records()` 提供队列深度）与 `WireSource::next_record`（逐条读取，`peer_endpoint()` 提供连接认证的对端端点）。端口失败只有 `WireFailure::Unavailable`（可重试）与 `WireFailure::Closed`（致命）两类；真实实现由 QUIC/WS 适配器提供，测试用内存队列。

**发送（TRANSPORT-005）**：

- 会话必须 `Connected`/`Degraded`（否则 `SESSION_NOT_USABLE`）；负载为空 `MALFORMED_FRAME`，超过会话帧上限 `CONTENT_TOO_LARGE`。
- 分片大小 = min(`SendBudget::max_fragment_bytes`, 会话帧上限)；`max_records_per_send` 必须能覆盖最大帧，否则 `SEND_BUDGET_EXCEEDED`；发送前检查 `queued_records + fragments ≤ max_queue_depth`，超限显式拒绝、不排队等待。
- 可重试 wire 失败交给 `RetryTracker`：返回 `RetryableFailure { delay_ms, next_attempt_at_ms, sent_records }`；重试预算耗尽转 `FatalFailure { RetryBudgetExhausted }`；`WireFailure::Closed` 直接致命且不消耗预算；成功写完后 `retry.reset()`。
- 部分写入语义：已写记录数在结果中返回；上层必须按连接级失败重建会话（对端重组器随之重置），不得在旧连接上从中间分片续传。

**接收（TRANSPORT-006）**：

- `PeerFrameReceiver` 创建时绑定会话（本机/对端端点与设备）并持有与发送方一致的 `FrameLimits`（分片大小必须协商一致；超过会话帧上限的配置显式拒绝）。
- 每次调用先校验：会话可用、接收器绑定同一会话、`WireSource::peer_endpoint()` 等于会话对端端点（返回 `None` 同样拒绝，绝不把未认证来源当会话）→ `SESSION_MISMATCH`。
- 单次最多消费 `ReceiveBudget::max_records_per_call` 条记录；未收齐返回 `Pending { budget_exhausted }`，无数据 `Pending`，来源失败 `Unavailable`/`Closed`。
- 乱序/重复/旧帧/未收齐/超限沿用帧层错误码（`OUT_OF_ORDER_FRAGMENT`/`DUPLICATE_FRAGMENT`/`STALE_FRAME`/`INCOMPLETE_FRAME`/`CONTENT_TOO_LARGE`/`MALFORMED_FRAME`）。

**Pull 映射（TRANSPORT-007）**：

- 为保持 transport 在 sync 之下的依赖方向，**不依赖 `chat-sync`**：本地 `PullRange { from, to }` 与 sync 的 `PullAction::Pull { from, to }` 字段一一对应，由 core 层转换。
- `plan_pull_requests(range, page_items, base_frame_id, max_requests)`：闭区间按 `page_items`（1–500）分页；请求 0 起编号，帧号 = `base_frame_id + request_index`（溢出拒绝）；请求数不得超过 `max_requests`（≤256）；非法范围/分页/数量 `PULL_PLAN_INVALID`。
- 计划有确定性摘要（SHA-256，绑定范围/分页/帧号），相同输入同摘要，可用于幂等去重。
- `ResponseValidator` 要求按序响应：请求索引越界/乱序/范围不符/条目数不符 `RESPONSE_MISMATCH`，重复响应 `DUPLICATE_RESPONSE`；失败不改变校验器状态。
- 请求正文编码属于 node-api 职责（本层不含 HTTP）。

### 真实 iroh 端点与连接（TRANSPORT-002/006，本轮）

在纯逻辑核心之上接入真实 iroh 端点：默认只绑定回环，**显式 `allow_lan` 才允许非回环绑定**；显式 `relay_urls` 可启用自定义中继（空 = 禁用中继）；地址发现、portmapper 与 NetReport 始终关闭。帧线格式、预算与错误码不变。

| 本轮交付（任务 ID） | API | 与文档契约的关系 |
|---|---|---|
| TRANSPORT-002 start_endpoint | `endpoint::start_endpoint`/`TransportEndpoint`（`EndpointConfig`、`EndpointSnapshot`、`node_id`/`endpoint_id`、`connect`、`accept`、`accept_loop`、`close`） | 文档 TRANSPORT-001 `start_endpoint` 的回环/LAN 实现：真实 iroh Endpoint、可选固定 secret key、显式中继列表、接受循环；公共 relay/发现路径与端口冲突恢复未实现 |
| 连接建立 | `PeerConnection`（`open_stream`/`accept_stream`/`close`） | 文档 TRANSPORT-003 `connect_peer` 的连接建立子集：回环直连、连接超时、断开重连；NAT 穿透/中继/发现记录验证未实现 |
| TRANSPORT-006 wire 适配 | `wire_io::WireStream`（`write_record`/`read_record`/`send_frame`/`receive_frame`/`split`）、`IrohWireSink`/`IrohWireSource` | 文档 TRANSPORT-004/005 的真实 IO 子集：同一分片线格式在 iroh 双向流上收发；同步 `WireSink`/`WireSource` 通过有界通道 + 后台读写任务桥接 |

- **端点**：`EndpointConfig { bind_addr, alpn, secret_key, allow_lan, relay_urls }`；`allow_lan` 默认 `false`，非回环绑定（含 `0.0.0.0`）未开启时 `INVALID_TRANSPORT_CONFIG`；ALPN 1–255 字节；`secret_key` 可选且固定时节点 ID 可复现（`EndpointId` 即 iroh 公钥）。`connect` 校验对端 `EndpointId` 与握手结果一致（`SESSION_MISMATCH`），不可达/超时 `PEER_UNREACHABLE`；`accept` 与接受循环超时 `TRANSPORT_TIMEOUT`；接受循环有界队列（1–256），句柄 drop 取消任务。
- **LAN 绑定**：`allow_lan=true` 允许非回环 bind。tests/iroh_endpoint.rs 的 `lan_endpoint_real_address_interconnect` 会从本机取一个真实非回环 IPv4（UDP `connect` 选源地址，不发包），A 绑定该 `IP:0` 并断言 `snapshot.local_addr` 是真实非回环地址、`allow_lan` 生效、`relay_enabled=false`；同一地址未开 `allow_lan` 时必须启动失败。通配绑定的 `address()` 显式返回 `ENDPOINT_START_FAILED`（不可直接 dial）。
- **中继配置**：`relay_urls` 为空 → `RelayMode::Disabled`；非空 → `RelayMode::Custom(RelayMap)`。每个 URL 必须是 https，或仅本机回环的 http（`127.0.0.0/8`、`::1`、`localhost`）；拒绝凭据、片段与不可解析 URL；数量 ≤ 8（`MAX_RELAY_URLS`）；规范化（补尾 `/`）并去重保序。中继只是可选路径，合法但不可达的中继不影响启动与回环直连。
- **配置快照**：`TransportEndpoint::snapshot()` 返回只读 `EndpointSnapshot { node_id, bind_addr, local_addr, allow_lan, alpn, relay_urls, relay_enabled, discovery_enabled(恒 false), secret_key_configured }`，不含密钥材料。
- **IO 超时**：所有等待显式校验为 1ms–30s（`validate_timeout`），0 或超上限直接拒绝；测试另有整体 30s 上限。
- **异步记录 API**：`write_record`/`read_record` 读写 `[u32 wire_len][分片元数据][payload]`；读取先读 1 字节区分「流正常结束」与「截断」，记录长度先检查（`12..=65548`）再分配；`send_frame` 复用 `encode_frame_for_send` 的预算/分片规则，写超时按重试策略返回 `RetryableFailure`/`FatalFailure`；`receive_frame` 复用 `PeerFrameReceiver::push_record` 的有界解码与严格有序重组。
- **同步端口桥接**：`WireStream::split(queue)` 拆出后台写任务/读任务与有界通道（1–256），`IrohWireSink` 实现 `WireSink`（队列满 → `Unavailable`，连接错误 → `Closed`），`IrohWireSource` 实现 `WireSource`（`peer_endpoint` 为握手认证的对端 ID）；`IrohWireSink::flush(timeout)` 等待队列写完。既有 `send_peer_frame`/`receive_peer_frame` 可直接运行在真实连接上。
- **真实回环/LAN 用例**（全部 1–5s 单步超时 + 整体 30s 上限，实测约 3.5s）：两端点交换多分片帧并重组、单帧连续收发、超限帧发送前 `CONTENT_TOO_LARGE`、同步端口桥接（含队列预算 `SEND_BUDGET_EXCEEDED`）、断开后 `Closed` 与新连接重连继续收发、不可达对端 `PEER_UNREACHABLE` 且 10s 内返回、无入站时 `accept` `TRANSPORT_TIMEOUT`、配置校验与重复关闭；快照与输入一致、非回环未开 `allow_lan` 拒绝、开 `allow_lan` 后绑定成功、非法/超量中继启动前拒绝、合法但不可达中继下的回环直连收发；**真实非回环地址直连**：A 绑定本机非回环 IPv4 且开 `allow_lan`，B 首选回环、本机不可路由时回退绑定 `0.0.0.0`（打印 note），经该真实地址交换多分片帧并拒绝超限；本机取不到非回环 IPv4 或回环自连被防火墙拦截时 skip 并打印原因（详见证据）。

- **本轮限制**：中继只支持显式自定义列表（无默认/生产 relay、无 relay 故障切换与自托管 relay 端到端验证）；地址发现始终关闭；LAN 已用**真实非回环 IPv4** 完成单机双向互连（A 绑该地址、B 在严格回环源不可路由时回退 `0.0.0.0`），但未做多机/跨网段/对称 NAT 验证；严格「B 回环源 → 非回环目的」拓扑在本机 Windows 不可路由（测试打印 note 后回退），无可用非回环地址或回环自连被防火墙拦截时整段 skip；中继可用性只保证「不影响本地直连」，未验证经中继的实际收发。

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
| `SESSION_NOT_USABLE` | 会话未连接/已关闭却收发 | 否 | 本层新增码 |
| `SESSION_MISMATCH` | 跨会话复用接收器或来源端点不匹配/未认证 | 否 | 本层新增码 |
| `SEND_BUDGET_EXCEEDED` | 发送队列深度/记录数预算超限 | 否 | 本层新增码 |
| `PULL_PLAN_INVALID` | 拉取范围/分页/请求数/帧号非法 | 否 | 本层新增码 |
| `DUPLICATE_RESPONSE` | 同一请求索引的响应重复 | 否 | 本层新增码 |
| `RESPONSE_MISMATCH` | 响应索引/范围/条目数与计划不符 | 否 | 本层新增码 |
| `ENDPOINT_START_FAILED` | iroh 端点绑定/启动失败 | 否 | 本层新增码 |
| `PEER_UNREACHABLE` | 对端不可达/连接被拒绝/握手失败 | 否（由显式退避策略决定是否重试） | 本层新增码 |
| `TRANSPORT_TIMEOUT` | IO/握手/等待超时 | 否（由显式退避策略决定是否重试） | 本层新增码 |
| `CONNECTION_CLOSED` | 连接/流已关闭 | 否 | 本层新增码 |

映射约定：本层错误一律不可自动重试；跨 FFI 暴露时冻结码可直接进入 `chat_protocol::map_error`，新增码在协议层登记前映射为 `INTERNAL_FAILURE` 并保留本层码做诊断。

### 验证证据

`cargo test --offline -p chat-transport`（2026-10-19）→ **68 passed / 0 failed**（lib 43 + `tests/iroh_endpoint.rs` 11 真实 iroh（含真实非回环地址互连）+ `tests/peer_pipeline.rs` 6 + `tests/transport_core.rs` 8），`cargo clippy --offline -p chat-transport --all-targets` 0 警告。

- 状态机：`state::tests::transition_table_matches_the_full_matrix`（5×6 逐格）、`repeated_events_are_idempotent`、`closed_is_terminal_except_repeated_close`、`illegal_transitions_are_explicit`，以及 `transport_core::transition_matrix_is_complete_and_unique`。
- 会话：`session::tests::opens_session_in_disconnected_state`、`rejects_invalid_session_inputs`、`session_limits_are_bounded`、`negotiated_frame_limit_cannot_be_enlarged`、`session_lifecycle_uses_the_state_machine`、`transport_core::sessions_cannot_connect_without_negotiation_or_enlarge_limits`。
- 帧：`frame::tests::round_trip_single_and_multi_fragment`、`wire_format_round_trip_and_byte_vectors`、`frame_and_fragment_boundaries_are_enforced`、`malformed_wire_is_rejected_before_allocation`、`assembler_rejects_duplicate_out_of_order_stale_and_incomplete`、`random_wire_input_never_panics`、`transport_core::frame_*_rejects_*`。
- 退避：`retry::tests::exponential_base_delay_is_capped`、`jitter_is_bounded_deterministic_and_capped`、`budget_exhaustion_stops_retrying`、`next_attempt_time_is_saturating`、`invalid_policies_are_rejected`、`transport_core::retry_sequence_is_deterministic_and_bounded`。
- 端到端纯逻辑闭环：`transport_core::session_frame_retry_flow_is_closed_loop`（建立会话 → 连接 → 分片/重组 → 降级/恢复 → 退避预算 → 关闭）。
- 收发管线：`peer_pipeline::send_receive_round_trip_across_fragments`（分片-重组往返、连续帧）、`injected_loss_duplication_and_reordering_are_rejected`（丢片→乱序、重复、尾片丢失换帧→未收齐）、`send_budgets_and_retry_budget_are_explicit`（队列/记录预算、可重试→预算耗尽→致命、Closed 不消耗预算）、`cross_session_and_unauthenticated_sources_are_rejected`（跨会话/未认证/未连接）、`pull_plan_maps_paginated_ranges_to_frame_requests`（分页映射、帧号绑定、摘要稳定、响应去重与校验）、`pull_plan_boundaries_are_explicit`（单条/整除/最大页/非法范围）。
- 真实 iroh 回环（`tests/iroh_endpoint.rs`，单步 1–5s、整体 30s 超时）：`endpoints_exchange_fragmented_frames_and_reject_oversize`（固定密钥节点 ID、连接/接受、3 片帧重组、连续帧、超限发送前拒绝）、`sync_ports_bridge_over_real_connection_and_enforce_queue_budget`（同步端口桥接 + 队列预算）、`disconnect_then_reconnect`（断开 `Closed` 后新连接继续）、`unreachable_peer_fails_with_timeout_instead_of_hanging`（`PEER_UNREACHABLE` 且不挂死、accept `TRANSPORT_TIMEOUT`）、`start_endpoint_validates_configuration`（非回环/空 ALPN/0 超时/接受队列预算/重复关闭）、`snapshot_matches_effective_configuration`（快照字段与输入一致、中继规范化、密钥不暴露）、`lan_binding_requires_allow_lan`（非回环未开拒绝/开启后绑定/通配地址不可 dial）、`relay_configuration_is_validated_at_startup`（非法协议/非回环 http/凭据/不可解析拒绝，合法不可达中继可启动）、`unreachable_relay_does_not_break_loopback_connection`（带不可达中继仍完成回环收发）、`lan_endpoint_real_address_interconnect`（本机真实非回环 IPv4 绑定 + snapshot 断言 + B 回环/回退 0.0.0.0 的真实互连 + 多分片帧 + 超限拒绝；无地址/不可达时 skip 并打印原因）、`non_loopback_bind_rejected_without_allow_lan`（非回环地址未开 allow_lan 校验与启动均拒绝）；端点单元用例 `endpoint::tests::*`（配置、LAN、中继 URL 与超时校验）。
- 管线单元用例：`send::tests::sends_single_and_multi_fragment_frames`、`wire_failures_map_to_retry_or_fatal`、`queue_and_record_budgets_are_enforced`；`receive::tests::reassembles_frames_from_sent_records`、`partial_data_returns_pending_and_budget_flag`、`rejects_out_of_order_duplicate_and_stale_records`、`rejects_cross_session_and_unauthenticated_sources`、`source_failures_map_to_unavailable_or_closed`；`pull_map::tests::paginates_inclusive_ranges`、`plan_digest_is_deterministic_and_bound`（固定摘要向量）、`responses_are_validated_in_order_without_duplicates`、`rejects_invalid_ranges_pages_and_request_counts`。

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

**实现状态**：部分实现（Rust 纯逻辑子集：`TransportSession::open`/`open_session` 校验端点/设备标识、协商结果、初始状态与会话预算；真实回环端点 `TransportEndpoint::start`/`start_endpoint`：iroh Endpoint 启动、可选 32 字节 secret key、`node_id`/`endpoint_id` 导出、`close`，见上方「真实 iroh 端点与连接」；证据 `session::tests::*`、`transport_core::sessions_cannot_connect_without_negotiation_or_enlarge_limits`、`endpoint::tests::*`、`iroh_endpoint::start_endpoint_validates_configuration`、`iroh_endpoint::endpoints_exchange_fragmented_frames_and_reject_oversize`）。公共 relay/发现已按配置关闭，但未做真实自托管 relay 端到端验证；`DeviceKeyRef` 到端点密钥的装载策略、运行时/任务取消、有界入站队列与端口冲突清理未实现，两项验收未勾选。

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

**实现状态**：部分实现（Rust 纯逻辑子集：连接状态机 `ConnectionStateMachine`/`next_state` 与确定性重试退避 `RetryPolicy`/`RetryTracker`；真实回环连接建立 `TransportEndpoint::connect`/`accept`/`accept_loop` + `PeerConnection`：连接/接受超时（1ms–30s）、对端 `EndpointId` 与握手结果校验、断开后重连，见上方「真实 iroh 端点与连接」；证据 `state::tests::transition_table_matches_the_full_matrix`、`retry::tests::budget_exhaustion_stops_retrying`、`transport_core::session_frame_retry_flow_is_closed_loop`、`iroh_endpoint::unreachable_peer_fails_with_timeout_instead_of_hanging`、`disconnect_then_reconnect`）。局域网/对称 NAT 穿透、自托管中继回退、发现记录验证、取消关闭竞赛连接与路径诊断未实现，三项验收未勾选。

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

**实现状态**：部分实现（Rust 纯逻辑子集：帧长度/分片/线格式编码与上限检查 + 对端发送管线 `send_peer_frame`：编码→分片→逐片写 `WireSink`、发送队列/记录预算、可重试/致命判定与退避，见上方章节；证据 `send::tests::sends_single_and_multi_fragment_frames`、`wire_failures_map_to_retry_or_fatal`、`queue_and_record_budgets_are_enforced`、`peer_pipeline::send_receive_round_trip_across_fragments`、`send_budgets_and_retry_budget_are_explicit`）。真实 iroh 连接上的同一发送路径由 `WireStream::send_frame`/`sync_ports_bridge...` 覆盖（`iroh_endpoint::endpoints_exchange_fragmented_frames_and_reject_oversize`、`sync_ports_bridge_over_real_connection_and_enforce_queue_budget`）。连接关联校验、有限 ACK 等待、超时错误与同字节重发（重试策略已就绪）未实现，两项验收未勾选。

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

**实现状态**：部分实现（Rust 纯逻辑子集：有界解码 `FrameCodec::decode_wire`、严格有序分片重组 `FragmentAssembler`，以及接收管线 `receive_peer_frame`：逐片喂入、会话/端点绑定、跨会话/未认证拒绝、单次记录预算，见上方章节；证据 `receive::tests::reassembles_frames_from_sent_records`、`partial_data_returns_pending_and_budget_flag`、`rejects_out_of_order_duplicate_and_stale_records`、`rejects_cross_session_and_unauthenticated_sources`、`source_failures_map_to_unavailable_or_closed`、`peer_pipeline::cross_session_and_unauthenticated_sources_are_rejected`）。真实 iroh 连接上的接收路径由 `WireStream::receive_frame` 与 `IrohWireSource`（有界通道桥接）覆盖（证据同上）。连接读循环/入站队列的完整背压语义未实现；内层去重仍归 protocol/sync（伪造 EventId 不会被本层提前去重），两项验收未勾选。

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

**实现状态**：部分实现（Rust 纯逻辑子集：Pull 计划映射与响应校验层，见上方「对端收发管线」；证据 `pull_map::tests::paginates_inclusive_ranges`、`plan_digest_is_deterministic_and_bound`、`responses_are_validated_in_order_without_duplicates`、`peer_pipeline::pull_plan_maps_paginated_ranges_to_frame_requests`、`pull_plan_boundaries_are_explicit`）。真实节点请求签名、HTTP 传输、页正文解析与响应字节上限截断未实现（跳号与巨响应检测需节点响应解码后才能落地），两项验收未勾选。

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

## TRANSPORT-009 · 连接恢复策略（2026-10-19，Windows x64）

连接恢复的纯逻辑策略层：只计算退避与调度，不持有连接、不依赖会话对象、不联网、不 sleep。

代码：[crates/transport/src/reconnect.rs](../../crates/transport/src/reconnect.rs)；
测试：[crates/transport/tests/reconnect_policy.rs](../../crates/transport/tests/reconnect_policy.rs)（4 个）+ 模块内 2 个单元测试。

### 公开面

    pub struct RetryPolicy { max_attempts: u32, base_delay_ms: u64, max_delay_ms: u64, jitter_permille: u32 }
        DEFAULT = { 5, 500, 30_000, 200 }
        validate() / base_delay_for(attempt) / jitter_cap_for(attempt) / jitter_for(attempt) / delay_for(attempt)

    pub struct ReconnectAttempts        // 尝试计数 + 调度状态机（不持有连接）
        new(policy) / policy() / state() / on_disconnected(code, now_ms) / ready_at(now_ms)
        next_attempt(now_ms) -> Result<ReconnectSchedule, ReconnectError> / record_success()
    pub struct ReconnectState { attempts: u32, last_error_code: Option<&'static str>, next_attempt_ms: u64 }
    pub struct ReconnectSchedule { attempt: u32, delay_ms: u64, next_attempt_ms: u64 }
    pub enum ReconnectError { InvalidPolicy { detail }, BudgetExhausted { attempts } }
        code() -> INVALID_RECONNECT_POLICY | RECONNECT_BUDGET_EXHAUSTED

### 语义

1. 指数退避 + 封顶：基础延迟 = min(base × 2^(n-1), max_delay_ms)；max_delay_ms 受 300000ms 硬上限约束，极端 attempt 饱和到封顶。
2. 确定性抖动：抖动上限 = base × jitter_permille / 1000，实际值 = splitmix64(attempt) 对上限+1 取模；不读系统时间，同策略同 attempt 永远相同；permille = 0 表示无抖动。
3. 预算显式耗尽：第 max_attempts + 1 次 next_attempt 返回 RECONNECT_BUDGET_EXHAUSTED，状态写入 last_error_code 与 next_attempt_ms；被拒绝的那次不消耗预算。
4. 断线只排期：on_disconnected(code, now_ms) 不消耗预算，只写错误码与下次尝试时间；ready_at(now_ms) 供宿主判断是否到点。
5. 成功复位：record_success() 把计数与错误清零，下一次断线重新从第 1 次算。

### 证据（cargo test --offline -p chat-transport，74 passed）

- jitter_is_deterministic_and_stays_within_the_permille_band：每个 attempt 的抖动 ≤ 上限、上限等于 base×permille/1000、反复取值相同（跨新策略实例也相同）、最终延迟落在 [base, base+cap] 且 ≤ 封顶；permille = 0 无抖动，1000 仍合法。
- backoff_doubles_and_caps_at_max_delay：1000/2000/4000/8000 → 封顶 10000，u32::MAX 不溢出；max_delay_ms 超过 300000 被拒绝。
- out_of_range_permille_and_attempts_are_rejected：permille = 1001、max_attempts = 0、base = 0、cap < base 全部返回 INVALID_RECONNECT_POLICY。
- exhaustion_is_explicit_and_success_resets_the_budget：断线不耗预算、ready_at 语义、三次预算内成功（延迟 100/200/400）、第四次显式 RECONNECT_BUDGET_EXHAUSTED 且计数仍为 3、record_success 后复位。
- 模块内单元测试：策略校验边界、状态机在预算内爬升与耗尽语义。

### 已知限制

1. 只做策略，不做拨号：真实重连（重建 iroh 连接、重放未确认帧、对端去重）仍由宿主/后续任务承担。
2. 抖动不是安全随机：splitmix64 只用于打散同时重试，不能用于密钥/令牌。
3. 没有全局重试预算：每个 ReconnectAttempts 实例独立计数，多会话并发时的总预算由宿主聚合。

## TRANSPORT-009 · 真实重连接入 Session（2026-10-19，Windows x64）

策略层（`reconnect.rs`）已有确定性退避与预算；本轮把它**接入会话**：断线检测、重连计划、
断线期间发送显式失败、重连后保留出站序号与重放窗口，并按序重放。仍不持有 socket：真实拨号由宿主完成。

代码：[crates/transport/src/session.rs](../../crates/transport/src/session.rs)（纯追加）、
[crates/transport/src/error.rs](../../crates/transport/src/error.rs)（两个新变体）；
测试：[crates/transport/tests/reconnect_session.rs](../../crates/transport/tests/reconnect_session.rs)（4 个，含真回环 iroh）。

### 公开面（`TransportSession` 追加方法）

    on_disconnected(code, now_ms) -> ReconnectState        // 状态机到 Disconnected + 记录错误码与退避
    reconnect_plan(now_ms) -> Result<ReconnectSchedule, ReconnectError>   // 预算内给出调度，超预算 RECONNECT_BUDGET_EXHAUSTED
    mark_reconnected() -> Result<ConnectionState>          // 复位 attempts，保留出站序号与重放窗口
    reconnect_state() -> ReconnectState                    // attempts / last_error_code / next_attempt_ms
    ensure_sendable() -> Result<()>                        // 断线时 SESSION_DISCONNECTED
    allocate_outgoing_frame() -> Result<u64>               // 分配序号并登记重放窗口
    queue_frame_for_replay(seq) -> Result<()> / replay_frames() -> Vec<u64> / acknowledge_replay(seq) -> bool
    replay_window() -> usize / set_replay_window(window) -> Result<()>   // 默认 16，上限 256

新增错误变体（`TransportError`）：`SESSION_DISCONNECTED`、`REPLAY_WINDOW_FULL{limit}`。

### 语义

1. 断线检测：`on_disconnected` 把状态机推进到 `Disconnected`（幂等），记录稳定错误码并算好下次尝试时间；不消耗重连预算。
2. 重连计划：`reconnect_plan` 消耗一次尝试并返回 `{attempt, delay_ms, next_attempt_ms}`；超过 `max_attempts` 返回 `ReconnectError::BudgetExhausted`（码 `RECONNECT_BUDGET_EXHAUSTED`），被拒绝的那次不计数。
3. 断线期间发送必须显式失败：`ensure_sendable()` 返回 `SESSION_DISCONNECTED`（既有发送管线的 `SESSION_NOT_USABLE` 语义不变，两者互补）。
4. 重连成功：`mark_reconnected()` 走 Connect/Connected 并复位 attempts，出站序号 `next_seq()` 与 `replay_frames()` 原样保留；重放顺序即入队顺序，对端确认后 `acknowledge_replay` 移出窗口。
5. 窗口有界：默认 16（`DEFAULT_REPLAY_WINDOW`），`set_replay_window` 允许 1..=256；满时 `REPLAY_WINDOW_FULL`，且失败不会推进序号。

### 证据（cargo test --offline -p chat-transport，78 passed）

- disconnect_blocks_sending_and_reconnect_preserves_seq_and_replay_window：序号 0/1 入窗 → 断线后 ensure_sendable 返回 SESSION_DISCONNECTED 且序号/窗口不变 → reconnect_plan(attempt 1) → mark_reconnected 复位 attempts 并保留 next_seq=2、replay=[0,1] → 按序重放并逐个确认 → 重连后新序号 2 不断档。
- replay_window_is_bounded_and_reports_window_full：窗口设为 2 后第三帧 REPLAY_WINDOW_FULL 且序号不前进，确认一帧后恢复；窗口 0 与 257 被拒绝。
- reconnect_budget_exhaustion_is_explicit_after_the_default_policy：默认预算 5 次内给出调度，第 6 次显式 RECONNECT_BUDGET_EXHAUSTED，attempts 仍为 5，状态带码与下次尝试时间。
- real_loopback_disconnect_then_reconnect_continues_sending（真回环 iroh）：未协商连接被拒（既有语义）→ 建连并发送 seq 0 → 关闭连接后 detected + ensure_sendable 失败 → 计划重连、重建连接、mark_reconnected → 序号仍为 1、窗口仍含 0 → 发送 seq 1 成功、窗口剩 [1]。

### 已知限制

1. `reconnect_plan` 只算调度，不重建连接、不重放帧；宿主必须先真实拨号成功再 `mark_reconnected`，再按 `replay_frames()` 逐帧重发。
2. 重放窗口只存序号，不缓存帧内容（发送管线按帧号重发）。
3. 会话层去重仍由帧层（STALE_FRAME / 分片重组）负责；本层不新增对端 seq 去重。


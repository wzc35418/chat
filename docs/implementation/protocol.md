# 标识符、线协议、编码与验证

阶段：P0 → P1。状态：PROTOCOL-001~014 全部 14 个函数已实现并通过自动化用例；P1 增补控制候选可选领域操作声明（字段 55/56，向后兼容旧字节）。源码：[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)（P0 帧，公开 API 未改动）+ 模块 [ids.rs](../../crates/protocol/src/ids.rs)、[canonical.rs](../../crates/protocol/src/canonical.rs)、[decode.rs](../../crates/protocol/src/decode.rs)、[signing.rs](../../crates/protocol/src/signing.rs)、[verify.rs](../../crates/protocol/src/verify.rs)、[event.rs](../../crates/protocol/src/event.rs)、[extension.rs](../../crates/protocol/src/extension.rs)、[capability.rs](../../crates/protocol/src/capability.rs)、[cursor.rs](../../crates/protocol/src/cursor.rs)、[digest.rs](../../crates/protocol/src/digest.rs)、[error.rs](../../crates/protocol/src/error.rs)。

本层只处理有界字节、规范编码和验签输入，不查询 UI。所有公钥、摘要、签名算法和线编码由 P0 决策锁定；下列签名是设计契约，不是可编译 Rust。Result 的错误包含稳定 code、retryable 与 operationId。签名前的域分隔必须覆盖协议主版本、消息类别和用途。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [PROTOCOL-001 parse_id](protocol.md#protocol-001)
- [PROTOCOL-002 encode_canonical](protocol.md#protocol-002)
- [PROTOCOL-003 decode_bounded](protocol.md#protocol-003)
- [PROTOCOL-004 candidate_signing_bytes](protocol.md#protocol-004)
- [PROTOCOL-005 verify_candidate](protocol.md#protocol-005)
- [PROTOCOL-006 receipt_signing_bytes](protocol.md#protocol-006)
- [PROTOCOL-007 verify_log_entry](protocol.md#protocol-007)
- [PROTOCOL-008 encode_application_event](protocol.md#protocol-008)
- [PROTOCOL-009 verify_inner_event](protocol.md#protocol-009)
- [PROTOCOL-010 validate_extension](protocol.md#protocol-010)
- [PROTOCOL-011 negotiate_capabilities](protocol.md#protocol-011)
- [PROTOCOL-012 encode_page_cursor](protocol.md#protocol-012)
- [PROTOCOL-013 decode_page_cursor](protocol.md#protocol-013)
- [PROTOCOL-014 map_error](protocol.md#protocol-014)

## P1-01 已冻结的字节与错误码

本节记录本轮冻结的具体字节格式（对应 [WIRE_FORMAT.md](../protocol/WIRE_FORMAT.md) 要求的 P0/P1-01 冻结项）。修改任何一项都必须走协议版本流程，不能只改测试期望值。

- **ID 文本**：`<prefix>:1:<32 字节小写 hex>`；前缀 acc/dev/grp/conv/sub/evt/msg/draft/op/obj/node；文本上限 96 字节。大写 hex、前导零版本、空白、非 ASCII、长度不符一律 `INVALID_ID`；类型前缀不符同样拒绝（AccountId 不能代作 DeviceId）。
- **规范编码**：`"CPC" major:u8 minor:u8 body`。body tag：0x01 u64（8 字节大端）、0x02 bytes（u32 大端长度）、0x03 text（u32 大端 UTF-8 长度）、0x04 none、0x05 some、0x06 list（u32 大端数量）、0x07 struct（u16 大端字段数 + 每项 u16 字段号 + 值，字段号严格升序）。编码期按字段号重排，重复/未知字段号显式拒绝。预算：总长 64 KiB、深度 8、集合 512、文本 8 KiB。
- **字段号**：1 schema_version、2/3 协议主/次版本、4 submission_id、5 group_id、6 author_device_id、7 kind、8 control_head、9 policy_version、10 mls_epoch、11 payload_digest、12/13 control 操作与目标摘要、14 receipt seq、15 previous_log_hash、16 candidate_digest、17–26 cursor（account/device/conversation/resource/sort_version/sort_key/seq/tie/issued_at/expires_at）、27–32 extension（namespace/schema_version/operation/object/payload/fallback）、33 节点回执签名、34 日志条目哈希、35 event_id、36 author_account_id、37 内容种类、38 文本、39 扩展内容、40 附件 file_id、41/42 回复会话/消息、43 origin、44 事件会话、45 展示时间、46–50 策略操作（account/role/muted/devices/name_digest）、51–54 会话能力（features/max_frame/max_message/max_ciphertext）、55/56 控制候选可选领域操作声明（domain_operation / domain_target_digest）。字段号只增不改，33–56 为 P1 补充。
- **签名原文**：`域字符串 || 规范编码`。候选作者域 `chat/p1/candidate-author-signature/v1`；节点回执域 `chat/p1/coordinator-receipt-signature/v1`。候选原文编码 kind（消息类别）、协议主/次版本与全部路由/控制字段，排除 `author_signature` 自身；回执原文绑定 group/seq/previous_log_hash/candidate_digest。
- **通用提案值（NODECAP，PROTOCOL-004）**：`ControlOperation` 在 1..=8 之后新增 9..=13 五个**提案**取值
  （`9 proposal:membership`、`10 proposal:role_policy`、`11 proposal:settings`、`12 proposal:key_rotation`、
  `13 proposal:custom`），提案类型编码在操作值本身（不新增字段、不改 1..=8 的数值），因此既有固定向量逐字节不变。
  提案必须**不**附领域声明（语义来源唯一）；节点可用 `decode_proposal_kind` 从候选规范编码解出 `ProposalKind` 做
  按类型的审计/分类。字段 42 仍是控制操作。
- **控制候选领域操作声明（可选，PROTOCOL-004）**：控制候选可携带字段 55/56（领域操作白名单 `1 set_role`、`2 transfer_ownership`、`3 leave_self`、`4 dissolve`；目标摘要恰 32 字节且非全零）。两字段必须成对出现；**旧候选不写这两个字段，签名原文字节与既有固定向量完全一致，解码为 `None`**。应用候选携带声明、未知枚举值、字段只出现一个、摘要长度错误/全零、或与公开控制操作组合不合法（如 `rename + transfer_ownership`）一律 `INVALID_CANDIDATE`。该字段让 `SelfUpdate` 类 MLS 提交能显式声明真正的领域语义（改角色/转让群主/退出/解散）。
- **分页游标**：`cur1.<12 字节系统随机 nonce hex>.<ChaCha20-Poly1305 密文+16 字节 tag hex>`；AAD 固定 `chat/p1/page-cursor/v1`；明文是 scope/position/issued_at/expires_at 的规范编码；TTL 10 分钟，允许 60 秒时钟超前；token 上限 1024 字节。`encode_page_cursor`/`decode_page_cursor` 因过期检查必须有可信时间，契约签名上补了显式 `unix_ms` 参数。
- **日志条目**：`AcceptedLogEntry = VerifiedCandidate + group_sequence + previous_log_hash + coordinator_signature`；条目哈希 `log_hash = SHA-256(chat/p1/log-entry-hash/v1 || 规范编码{group, seq, previous_log_hash, candidate_digest, coordinator_signature})`。回执签名原文沿用 PROTOCOL-006。下一跳 `previous_log_hash` 必须等于上一条 `log_hash`；创世条目引用全零头。
- **应用内层事件**：16 个冻结字段（schema、协议主/次、会话、群、event_id、account、device、内容种类、文本/附件/扩展三种可选内容、回复会话/消息、origin、展示时间）；内容种类 1=文本、2=附件、3=扩展；文本按 UTF-8 字节 ≤8 KiB，完整事件 ≤64 KiB；扩展走 PROTOCOL-010；`SecretBytes` 析构清零。去重键 `event_digest = SHA-256(chat/p1/application-event/v1 || 事件字节)`，与外层候选摘要无关。
- **能力协商**：schema=1；双方各声明协议版本（≤8）、能力名（≤32 个，每个 ≤64 字节规范名）、三项限额（≥1 且 ≤64 KiB）。必需能力 `e2ee.v1`/`mls.v1` 缺失即拒绝；未知能力被忽略但不参与会话；版本取双方共同支持的最高版本；限额逐项取较小值。会话指纹 `SHA-256(chat/p1/capability-session/v1 || 规范编码{version, features, limits})`。
- **错误码**：`chat_protocol::ErrorCode` 共 43 个冻结 ASCII 码（本轮按计划文档补入 `INVALID_POLICY_TRANSITION`、`INVALID_CONTROL_RESULT`，未发明新码）；`PublicError` 只含 code、retryable、限幅后的 retry_after_ms（≤300000）、operationId 与静态 detail_key。只有网络/节点/限流/存储忙/需同步补齐类可自动重试；未知错误映射 `INTERNAL_FAILURE` 且不可重试；内部诊断绝不进入公共错误。

<a id="protocol-001"></a>

## PROTOCOL-001 · parse_id

~~~text
parse_id(kind: IdKind, input: &str) -> Result<TypedId>
~~~

**职责与输出**：返回独立强类型 ID；输入错误不能构造内部合法值。

**输入与前置条件**：外部字符串不可信，ASCII 编码、版本和长度取决于冻结规范。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 先限长。
2. 解析类型前缀和版本。
3. 解码并验证字节数。
4. 检查规范形式并构造对应 ID。

**持久化、并发与副作用**：纯函数；不读取网络或数据库。

**失败处理**：INVALID_ID，PROTOCOL_UNSUPPORTED；不可自动改写非法输入。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 超长/空串/错误类型前缀。
- [x] 同字节非规范编码拒绝。
- [x] AccountId 不能代作 DeviceId。

**实现状态**：已实现（`crates/protocol/src/ids.rs`）。证据：`cargo test --offline -p chat-protocol -p chat-domain`（2026-10-18，chat-protocol 45 passed）中的 `ids::tests::rejects_non_canonical_encodings`（空串/超长/大写 hex/前导零版本/尾随字节/空白/非法字符）、`ids::tests::canonical_round_trip_for_every_kind`、`ids::tests::account_text_is_not_a_device_id`。

<a id="protocol-002"></a>

## PROTOCOL-002 · encode_canonical

~~~text
encode_canonical(value: WireValue, version: ProtocolVersion) -> Result<CanonicalBytes>
~~~

**职责与输出**：返回所有语言一致的签名原文，不含签名自身。

**输入与前置条件**：类型已通过字段验证，编码格式须 P0 冻结。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查版本。
2. 按规范字段序和整数范围编码。
3. 明确可选值表示。
4. 拒绝重复/未知安全字段。
5. 返回有界不可变字节。

**持久化、并发与副作用**：纯函数；不允许直接签任意 JSON 字符串。

**失败处理**：PROTOCOL_UNSUPPORTED，CONTENT_TOO_LARGE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-001 parse_id](protocol.md#protocol-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 字段排列变化产出同字节。
- [ ] 整数边界跨语言向量一致。（Rust 侧固定向量已冻结到 `tests/vectors.rs`；与 Kotlin/独立实现的逐字节交叉核对未执行，未勾选）

**实现状态**：已实现（`canonical.rs`）。证据：`canonical::tests::field_order_does_not_change_bytes`、`canonical::tests::integer_encoding_is_fixed_width_big_endian`（0/1/u64::MAX 固定大端向量）、`canonical::tests::rejects_unsupported_version_duplicate_and_unknown_fields`、`canonical::tests::rejects_oversized_payloads`、`canonical::tests::optional_values_have_explicit_representation`、`tests/vectors.rs::canonical_encoding_matches_frozen_vector`（固定 hex 向量 + 字段乱序同字节）。

<a id="protocol-003"></a>

## PROTOCOL-003 · decode_bounded

~~~text
decode_bounded(bytes: &[u8], limits: DecodeLimits) -> Result<WireFrame>
~~~

**职责与输出**：只返回结构合法但尚未认证的帧。

**输入与前置条件**：先有帧字节上限、最大深度、集合数量和解压预算。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查总长度。
2. 有界解析。
3. 拒绝重复字段和溢出。
4. 校验必需字段。
5. 标注 Unverified 类型。

**持久化、并发与副作用**：纯解析；成功不代表可信作者。

**失败处理**：MALFORMED_FRAME，CONTENT_TOO_LARGE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-001 parse_id](protocol.md#protocol-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 截断/深嵌套/巨量数组。
- [x] 随机畸形输入不得越界或耗尽内存。

**实现状态**：已实现（`decode.rs`，`DecodeLimits` 先于分配生效）。证据：`decode::tests::rejects_truncation_trailing_and_unsupported_version`（每个截断位置）、`decode::tests::rejects_duplicate_unsorted_and_unknown_fields`、`decode::tests::rejects_double_excessive_counts_and_depth`（u32::MAX 计数/超深/超长/非 UTF-8）、`decode::tests::random_malformed_input_never_panics`（0..256 字节确定性伪随机畸形输入不 panic）、`decode::tests::total_length_limit_is_enforced_before_parsing`。

<a id="protocol-004"></a>

## PROTOCOL-004 · candidate_signing_bytes

~~~text
candidate_signing_bytes(candidate: UnsignedCandidate) -> Result<CanonicalBytes>
~~~

**职责与输出**：生成作者签名覆盖内容。

**输入与前置条件**：候选包括 submission/group/device/kind/head/policy/epoch 与密文。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 计算 payload 摘要。
2. 加入用途域。
3. 规范编码全部路由和控制字段。
4. 排除 author_signature 自身。

**持久化、并发与副作用**：纯函数；不持有私钥。

**失败处理**：INVALID_CANDIDATE，CONTENT_TOO_LARGE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 替换目标群/epoch/控制元数据任一字段导致验签失败。
- [x] 可选领域操作声明向后兼容（旧字节解码 `None`）且新字节通过白名单/长度/kind 组合校验。

**NODECAP 补充（通用提案值）**：`ControlOperation` 新增 9..=13 五个提案取值与 `ProposalKind`（5 个分类，
`as_u64/as_str/from_u64` 全部冻结），并提供 `is_proposal()`、`proposal_kind()`、`audit_label()` 与
`decode_proposal_kind(body)`（只读公开字段，供节点按提案类型审计）。校验：提案值**不得**同时携带领域声明
（`INVALID_CANDIDATE`），提案值参与签名原文（改提案值必然改签名）。证据：`proposal_values_round_trip_and_carry_a_stable_audit_label`、
`proposal_kind_is_encoded_and_decodable_for_audit`、`proposals_must_not_carry_a_domain_declaration`，且
`tests/vectors.rs` 的 12 个固定向量继续逐字节通过（新增取值只出现在新字节里）。

**实现状态**：已实现 `candidate_signing_bytes` 与可选领域操作声明（`signing.rs`）。证据：`cargo test --offline -p chat-protocol`（2026-10-19，**78 passed**：lib 66 + tests/vectors 12；既有 73 全部继续通过）。`signing::tests::every_signed_field_change_breaks_verification`（group/device/submission/head/policy/epoch/payload/kind/控制操作/目标摘要共 10 类变异都用原候选签名验签失败）、`domain_declaration_is_signed_and_any_change_breaks_verification`（声明字段/目标摘要/移除声明都会让原签名失效）、`domain_declaration_requires_control_kind_and_compatible_operation`（应用候选携带声明、公开操作与领域操作不匹配、全零目标摘要都 → `INVALID_CANDIDATE`；5 组合法组合通过）、`domain_declaration_is_omitted_for_old_bytes_and_decoded_from_new_bytes`（旧字节无 55/56 字段且解码 `None`；新字节成对解码；单字段/未知枚举/31 字节摘要/全零/非结构体 → `INVALID_CANDIDATE`；55/56 已登记 `KNOWN_FIELD_IDS`）、`tests/vectors.rs::candidate_signing_bytes_match_frozen_vector`（既有固定 716 hex 向量不变）、`tests/vectors.rs::domain_declaration_vectors_are_frozen`（self_update+transfer_ownership / leave_self / dissolve 三条新固定向量）、`domain_declaration_round_trips_and_old_bytes_stay_none`、`ed25519_signature_is_deterministic_and_verifies`。注意：完整 `verify_candidate`（PROTOCOL-005）见下节；下游 core/group_node 与 node-services 的文本元数据尚未映射新声明（后续集成任务）。

<a id="protocol-005"></a>

## PROTOCOL-005 · verify_candidate

~~~text
verify_candidate(candidate: SignedCandidate, credential: VerifiedCredential, context: GroupContext) -> Result<VerifiedCandidate>
~~~

**职责与输出**：返回外层验签通过的候选；MLS 内层验证仍必需。

**输入与前置条件**：凭据已验证且设备未撤销，节点与群上下文明确。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查设备和账户绑定。
2. 核对版本/大小/群。
3. 验证作者签名。
4. 核对 control 元数据是否单操作。
5. 输出带验证标记对象。

**持久化、并发与副作用**：纯验证；不修改正式群状态。

**失败处理**：INVALID_SIGNATURE，DEVICE_REVOKED，PROTOCOL_UNSUPPORTED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-004 candidate_signing_bytes](protocol.md#protocol-004)。凭据已由上层校验；protocol 不依赖 identity 服务。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [x] 伪造 author_device。
- [x] 重放到其他群。
- [x] 内层伪造仍需后续拒绝。

**实现状态**：已实现（`verify.rs::verify_candidate`，输出 `VerifiedCandidate`；构造入口只有本函数）。证据：`cargo test --offline -p chat-protocol -p chat-domain`（2026-10-18，chat-protocol 73 passed、chat-domain 43 passed；2026-10-19 protocol 更新为 78 passed）中的 `verify::tests::valid_candidate_verifies_with_bindings`、`revoked_device_and_wrong_device_are_rejected`（伪造/借用设备 → `INVALID_SIGNATURE`；撤销 → `DEVICE_REVOKED`）、`signature_from_another_key_or_tampered_fields_is_rejected`（换钥与篡改密文/提交/群/设备）、`cross_group_and_stale_context_are_rejected`（跨群 → `INVALID_SIGNATURE`；head/policy 陈旧 → `STALE_CONTROL_HEAD`；epoch 陈旧 → `STALE_EPOCH`；不支持版本 → `PROTOCOL_UNSUPPORTED`）、`application_candidate_cannot_smuggle_control_metadata`（`INVALID_CANDIDATE`）；内层伪造由 PROTOCOL-009 用例 `event::tests::inner_event_binds_sender_and_outer_candidate`、`cross_group_inner_event_is_rejected` 拒绝。契约偏差：head/policy/epoch 陈旧使用冻结的 `STALE_CONTROL_HEAD`/`STALE_EPOCH`（契约失败表只列三个码），因为陈旧不是伪造，调用方应先同步；凭据/设备的账户绑定由上层 `VerifiedCredential` 保证，本层检查撤销、设备一致与群/状态绑定。

<a id="protocol-006"></a>

## PROTOCOL-006 · receipt_signing_bytes

~~~text
receipt_signing_bytes(candidate_hash: Digest, seq: u64, previous: Digest, group: GroupId) -> CanonicalBytes
~~~

**职责与输出**：产生节点回执的唯一签名输入。

**输入与前置条件**：seq 从 1 开始，group 与候选一致。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 加入节点回执域。
2. 编码候选全文摘要。
3. 绑定 seq/previous/group。
4. 返回原文字节。

**持久化、并发与副作用**：纯函数；seq 不加入已经生成的 MLS 正文。

**失败处理**：INVALID_SEQUENCE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 交换候选或前序摘要导致签名失效。
- [x] 跨群回执拒绝。

**实现状态**：已实现（`signing.rs`）。证据：`tests/vectors.rs::receipt_signing_bytes_match_frozen_vector`（固定向量）、`signing::tests::receipt_changes_each_binding`（candidate_hash/seq/previous/group 任一改变字节即变，且与候选原文域不同）。`seq=0` 返回 `INVALID_SEQUENCE`：契约签名写作 `CanonicalBytes`，但失败处理要求显式错误，因此返回 `Result<CanonicalBytes>`。

<a id="protocol-007"></a>

## PROTOCOL-007 · verify_log_entry

~~~text
verify_log_entry(entry: AcceptedLogEntry, expected: VerifiedHead, node_key: PinnedNodeKey) -> Result<VerifiedLogEntry>
~~~

**职责与输出**：返回连续且节点签名合法的日志条目。

**输入与前置条件**：节点公钥已固定；expected 为本机已验证头。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证作者候选。
2. 计算候选摘要。
3. 验证节点回执。
4. 核对 seq=expected+1 和 previous_hash。
5. 校验群绑定。

**持久化、并发与副作用**：纯函数；后续业务与 MLS 校验成功才提交游标。

**失败处理**：MISSING_LOG_ENTRIES，LOG_CONFLICT，INVALID_SIGNATURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-005 verify_candidate](protocol.md#protocol-005)、[PROTOCOL-006 receipt_signing_bytes](protocol.md#protocol-006)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 跳号触发补齐。
- [x] 相同 seq 不同摘要暂停。
- [x] 换节点公钥不得静默信任。

**实现状态**：已实现（`verify.rs::verify_log_entry` + `log_entry_hash`）。证据：`verify::tests::log_chain_accepts_contiguous_entries_only`（创世 seq=1 接受、第二条引用首条链尾接受、跳号 → `MISSING_LOG_ENTRIES`、重复序号/错误前序 → `LOG_CONFLICT`）、`wrong_node_key_and_tampered_receipt_are_rejected`（换节点公钥或篡改回执/候选摘要 → `INVALID_SIGNATURE`）、`cross_group_and_stale_log_heads_are_rejected`（跨群 → `INVALID_SIGNATURE`；head/epoch 陈旧 → `STALE_*`；创世非零前序 → `LOG_CONFLICT`）、`log_entry_hash_is_deterministic_and_field_sensitive`、固定向量 `tests/vectors.rs::log_entry_hash_and_receipt_signature_match_frozen_vectors`。实现说明：`AcceptedLogEntry.candidate` 是 `VerifiedCandidate`（PROTOCOL-005 输出），作者验签是类型前置条件；本函数只验证节点回执、序号与前序哈希链，并返回新 `log_hash`，不推断 control_head/epoch 的更新规则。

<a id="protocol-008"></a>

## PROTOCOL-008 · encode_application_event

~~~text
encode_application_event(event: PlainApplicationEvent) -> Result<SecretBytes>
~~~

**职责与输出**：生成 MLS 内层事件字节，绑定事件 ID 和作者设备。

**输入与前置条件**：content/reply/origin 已验证，时间仅用于显示。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 加入 group/event/account/device。
2. 编码内容类型。
3. 校验回复目标上下文。
4. 计算完整序列化大小。
5. 返回敏感缓冲区。

**持久化、并发与副作用**：纯函数；调用方负责使用后清理。

**失败处理**：CONTENT_TOO_LARGE，INVALID_REPLY。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 64 KiB 边界按 UTF-8 完整字节计算。
- [x] 跨会话引用拒绝。

**实现状态**：已实现（`event.rs::encode_application_event`，返回析构清零的 `SecretBytes`）。证据：`event::tests::text_event_round_trips_deterministically`、`extension_and_attachment_content_round_trip`、`reply_must_stay_in_the_same_conversation`（跨会话/零消息 ID → `INVALID_REPLY`）、`malformed_or_oversized_events_are_bounded`（文本 >8 KiB 与完整事件 >64 KiB → `CONTENT_TOO_LARGE`；恰好 8 KiB 接受；空文本/零 ID → `INVALID_EVENT_BINDING`）、固定向量 `tests/vectors.rs::application_event_matches_frozen_vector`。字节边界按 UTF-8 完整字节计算（`String::len`）。

<a id="protocol-009"></a>

## PROTOCOL-009 · verify_inner_event

~~~text
verify_inner_event(event: DecryptedEvent, sender: MlsSender, outer: VerifiedCandidate) -> Result<AuthenticatedEvent>
~~~

**职责与输出**：取得可以用于可信去重的 EventId。

**输入与前置条件**：MLS 解密认证已成功；sender 来自 MLS 凭据。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对 group/account/device。
2. 验证内外事件类别匹配。
3. 验证 origin 不是授权替代。
4. 检查内容范围。
5. 返回认证事件。

**持久化、并发与副作用**：纯函数；外层 message ID 不能提前进入永久去重表。

**失败处理**：INVALID_CREDENTIAL，INVALID_EVENT_BINDING。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-003 decode_bounded](protocol.md#protocol-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 伪造外层 ID 不吞掉真消息。
- [x] MLS sender 与外层设备不一致拒绝。

**实现状态**：已实现（`event.rs::verify_inner_event`，另有解码步骤 `decode_application_event`）。证据：`event::tests::inner_event_binds_sender_and_outer_candidate`（sender 与外层作者不一致 → `INVALID_CREDENTIAL`；控制候选冒充应用候选 → `INVALID_EVENT_BINDING`；去重键为内层域分隔摘要）、`forged_outer_candidate_does_not_change_inner_dedupe_key`（换外层候选摘要不改变内层去重键，伪造外层 ID 不会吞掉真消息）、`cross_group_inner_event_is_rejected`、`tampered_or_non_canonical_decrypted_bytes_are_rejected`（魔数/尾随字节/截断 → `INVALID_EVENT_BINDING`）。契约偏差：完整事件超过 64 KiB 时返回 `CONTENT_TOO_LARGE`（冻结码），其余绑定/结构失败统一 `INVALID_EVENT_BINDING`。

<a id="protocol-010"></a>

## PROTOCOL-010 · validate_extension

~~~text
validate_extension(content: ExtensionContent, limits: ContentLimits) -> Result<ValidatedExtension>
~~~

**职责与输出**：返回 namespace/version/operation/object/payload/fallback 齐全的内容。

**输入与前置条件**：完整消息仍受 64 KiB 上限；未知功能不能修改核心权限。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查命名空间。
2. 检查 schemaVersion。
3. 校验 operation/object ID。
4. 限制 payload 与回退文本。
5. 保留未知 payload。

**持久化、并发与副作用**：纯函数；未知内容可显示回退，未知控制操作必须拒绝。

**失败处理**：INVALID_EXTENSION，CONTENT_TOO_LARGE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-001 parse_id](protocol.md#protocol-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 未安装插件仍显示 fallback。
- [x] 空回退/非法 namespace。
- [ ] 重复 operation 后续幂等。（本层只保证同一 content 重复校验结果逐字节相同；按 operation_id 的幂等应用属于插件宿主，未在本 crate 实现，未勾选）

**实现状态**：已实现（`extension.rs`）。证据：`extension::tests::unknown_namespace_is_preserved_with_fallback`（未知命名空间 payload 原样保留、fallback 保留）、`extension::tests::rejects_invalid_namespace_and_fallback`（空/大写/空段/保留 core 命名空间/空回退/schemaVersion=0）、`extension::tests::oversized_payload_and_fallback_are_bounded`、`extension::tests::repeated_validation_is_byte_identical`。核心/系统命名空间被拒绝：未知控制操作必须走 MLS 控制事件。

<a id="protocol-011"></a>

## PROTOCOL-011 · negotiate_capabilities

~~~text
negotiate_capabilities(local: Capabilities, remote: Capabilities) -> Result<SessionCapabilities>
~~~

**职责与输出**：返回双方兼容主版本、必需能力交集和更小限额。

**输入与前置条件**：本地安全必需能力不可被远端关闭。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证远端声明。
2. 选择支持版本。
3. 逐项检查必需能力。
4. 按 min 合并限额。
5. 固定会话协商结果。

**持久化、并发与副作用**：连接级不可变状态；不修改身份信任。

**失败处理**：PROTOCOL_UNSUPPORTED，REQUIRED_CAPABILITY_MISSING。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 远端不支持 E2EE 必须失败。
- [x] 限额更低时严格采用较低值。

**实现状态**：已实现（`capability.rs::negotiate_capabilities`）。证据：`capability::tests::negotiates_required_features_and_smaller_limits`（限额逐项取较小值、未声明能力不进入会话）、`remote_missing_e2ee_is_rejected`（远端缺 `e2ee.v1` → `REQUIRED_CAPABILITY_MISSING`；本机缺必需能力 → `PROTOCOL_UNSUPPORTED`）、`no_common_version_is_rejected`（无共同版本 → `PROTOCOL_UNSUPPORTED`；未知版本被忽略而非放大）、`unknown_features_are_ignored_but_bounded`（未知能力忽略并计数；>32 个、>64 字节、大写、重复都拒绝）、`malformed_declarations_are_rejected`（schemaVersion≠1、空/重复/超量版本、主版本 0、0 或超过 64 KiB 的限额）、`session_fingerprint_is_deterministic_and_limit_sensitive`、固定向量 `tests/vectors.rs::session_fingerprint_matches_frozen_vector`。

<a id="protocol-012"></a>

## PROTOCOL-012 · encode_page_cursor

~~~text
encode_page_cursor(scope: CursorScope, position: CursorPosition, key: CursorKey) -> Result<OpaqueCursor>
~~~

**职责与输出**：返回认证的有界游标，不暴露未加密本地索引。

**输入与前置条件**：scope 包含账户/设备/会话/排序版本。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 规范编码 scope 和 position。
2. 认证封装。
3. 加入版本和过期策略。
4. 输出不透明 token。

**持久化、并发与副作用**：纯函数；算法依 P0 锁定。

**失败处理**：INVALID_CURSOR_SCOPE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 换会话/换设备游标拒绝。
- [x] 篡改位置拒绝。

**实现状态**：已实现（`cursor.rs`：ChaCha20-Poly1305 + 系统随机 nonce + 用途域 AAD，位置不入明文暴露）。证据：`cursor::tests::cross_scope_cursors_are_rejected`、`cursor::tests::tampered_position_and_ciphertext_are_rejected`（翻转密文 hex → `INVALID_CURSOR`；大写 hex 拒绝）、`cursor::tests::round_trip_and_no_plaintext_leak`、`tests/vectors.rs::cursor_token_matches_frozen_vector`（固定 token 向量可回解为固定位置）。契约签名补了显式 `issued_at_unix_ms`：过期检查需要可信时间来源，纯函数不能读系统钟。

<a id="protocol-013"></a>

## PROTOCOL-013 · decode_page_cursor

~~~text
decode_page_cursor(token: OpaqueCursor, expected: CursorScope, key: CursorKey) -> Result<CursorPosition>
~~~

**职责与输出**：取得合法位置，禁止跨范围复用。

**输入与前置条件**：限制 token 长度后再解码。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证认证标签。
2. 检查格式版本。
3. 匹配 expected。
4. 检查过期与位置范围。
5. 返回位置。

**持久化、并发与副作用**：纯函数；错误不能默认跳到最新页。

**失败处理**：INVALID_CURSOR，CURSOR_EXPIRED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-003 decode_bounded](protocol.md#protocol-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 截断 token。
- [x] 旧排序版本明确要求重查。
- [x] 跨账户拒绝。

**实现状态**：已实现（`cursor.rs`）。证据：`cursor::tests::expired_and_truncated_tokens_fail_closed`（截断/垃圾 token/错误密钥/过期）、`cursor::tests::stale_sort_version_requires_requery`（`INVALID_CURSOR`，调用方必须重查）、`cursor::tests::cross_scope_cursors_are_rejected`（账户/设备/会话/资源）、`cursor::tests::position_bounds_are_enforced`、`tests/vectors.rs::cursor_token_matches_frozen_vector`。失败绝不退化为「跳到最新页」。契约签名补了显式 `now_unix_ms`。

<a id="protocol-014"></a>

## PROTOCOL-014 · map_error

~~~text
map_error(error: InternalError, operation: Option<OperationId>) -> PublicError
~~~

**职责与输出**：输出稳定脱敏错误结构。

**输入与前置条件**：内部错误可能包含路径或输入明文，不可原样显示。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 按分类映射 code。
2. 确定 retryable。
3. 限幅 retryAfter。
4. 附 operationId。
5. 删除敏感 cause。

**持久化、并发与副作用**：纯函数；详细诊断也须脱敏。

**失败处理**：未知错误映射 INTERNAL_FAILURE，默认不可无限重试。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 私钥/正文不能出现在错误。
- [x] 存储损坏不标普通网络重试。

**实现状态**：已实现（`error.rs`）。证据：`error::tests::every_code_is_unique_and_non_empty`（41 码唯一）、`error::tests::unknown_error_maps_to_internal_failure_without_leaking_detail`（内部 detail 含伪密钥/正文也不出现在 `PublicError` 的 Display/Debug）、`error::tests::retry_after_is_clamped_and_operation_id_is_kept`（限幅 300000/默认 1000/最小 1）、`error::tests::storage_corruption_is_not_a_network_retry`（`STORAGE_FAILURE`/`STATE_CORRUPTED` 不可重试，`STORAGE_LOCKED` 可重试）、`error::tests::network_error_is_retryable`。领域错误经 `InternalError::Classified` 保留具体码。


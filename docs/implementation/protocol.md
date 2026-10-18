# 标识符、线协议、编码与验证

阶段：P0 → P1。状态：PROTOCOL-001/002/003/004/006/010/012/013/014 已实现并通过自动化用例；PROTOCOL-005/007/008/009/011 仍为设计（P1-01 范围不含）。源码：[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)（P0 帧，公开 API 未改动）+ 新模块 [ids.rs](../../crates/protocol/src/ids.rs)、[canonical.rs](../../crates/protocol/src/canonical.rs)、[decode.rs](../../crates/protocol/src/decode.rs)、[signing.rs](../../crates/protocol/src/signing.rs)、[extension.rs](../../crates/protocol/src/extension.rs)、[cursor.rs](../../crates/protocol/src/cursor.rs)、[error.rs](../../crates/protocol/src/error.rs)。

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
- **字段号**：1 schema_version、2/3 协议主/次版本、4 submission_id、5 group_id、6 author_device_id、7 kind、8 control_head、9 policy_version、10 mls_epoch、11 payload_digest、12/13 control 操作与目标摘要、14 receipt seq、15 previous_log_hash、16 candidate_digest、17–26 cursor（account/device/conversation/resource/sort_version/sort_key/seq/tie/issued_at/expires_at）、27–32 extension（namespace/schema_version/operation/object/payload/fallback）。
- **签名原文**：`域字符串 || 规范编码`。候选作者域 `chat/p1/candidate-author-signature/v1`；节点回执域 `chat/p1/coordinator-receipt-signature/v1`。候选原文编码 kind（消息类别）、协议主/次版本与全部路由/控制字段，排除 `author_signature` 自身；回执原文绑定 group/seq/previous_log_hash/candidate_digest。
- **分页游标**：`cur1.<12 字节系统随机 nonce hex>.<ChaCha20-Poly1305 密文+16 字节 tag hex>`；AAD 固定 `chat/p1/page-cursor/v1`；明文是 scope/position/issued_at/expires_at 的规范编码；TTL 10 分钟，允许 60 秒时钟超前；token 上限 1024 字节。`encode_page_cursor`/`decode_page_cursor` 因过期检查必须有可信时间，契约签名上补了显式 `unix_ms` 参数。
- **错误码**：`chat_protocol::ErrorCode` 共 41 个冻结 ASCII 码；`PublicError` 只含 code、retryable、限幅后的 retry_after_ms（≤300000）、operationId 与静态 detail_key。只有网络/节点/限流/存储忙/需同步补齐类可自动重试；未知错误映射 `INTERNAL_FAILURE` 且不可重试；内部诊断绝不进入公共错误。

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

**实现状态**：已实现 `candidate_signing_bytes`（`signing.rs`）。证据：`signing::tests::every_signed_field_change_breaks_verification`（group/device/submission/head/policy/epoch/payload/kind/控制操作/目标摘要共 10 类变异都用原候选签名验签失败）、`tests/vectors.rs::candidate_signing_bytes_match_frozen_vector`（固定 716 hex 字符向量）、`tests/vectors.rs::ed25519_signature_is_deterministic_and_verifies`（固定种子签名/公钥向量，篡改字段后验签失败）。注意：完整 `verify_candidate`（PROTOCOL-005，需要凭据与群上下文）未实现。

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

- [ ] 伪造 author_device。
- [ ] 重放到其他群。
- [ ] 内层伪造仍需后续拒绝。

**实现状态**：未实现（设计保留；P1-01 实现范围不含该函数，未开始）。

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

- [ ] 跳号触发补齐。
- [ ] 相同 seq 不同摘要暂停。
- [ ] 换节点公钥不得静默信任。

**实现状态**：未实现（设计保留；P1-01 实现范围不含该函数，未开始）。

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

- [ ] 64 KiB 边界按 UTF-8 完整字节计算。
- [ ] 跨会话引用拒绝。

**实现状态**：未实现（设计保留；P1-01 实现范围不含该函数，未开始）。

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

- [ ] 伪造外层 ID 不吞掉真消息。
- [ ] MLS sender 与外层设备不一致拒绝。

**实现状态**：未实现（设计保留；P1-01 实现范围不含该函数，未开始）。

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

- [ ] 远端不支持 E2EE 必须失败。
- [ ] 限额更低时严格采用较低值。

**实现状态**：未实现（设计保留；P1-01 实现范围不含该函数，未开始）。

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


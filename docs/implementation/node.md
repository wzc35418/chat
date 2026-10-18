# 节点服务、排序日志、密文信箱与文件

阶段：P1 → P5。状态：函数级设计。NODE-001 register_device 与 [api.md](api.md) 的 API-002/API-003 已在 [crates/node-services/src/services.rs](../../crates/node-services/src/services.rs) 与 [crates/node-api](../../crates/node-api) 落地并通过自动化测试（见各函数“实现状态”）。NODE-005~008 群登记/日志（[src/group.rs](../../crates/node-services/src/group.rs)）与 NODE-009~011 密文信箱（[src/mailbox.rs](../../crates/node-services/src/mailbox.rs)）已在同一轮落地；NODE-012~015 附件密文 blob 的上传/发布/读取（[src/blob.rs](../../crates/node-services/src/blob.rs)）也已落地，HTTP 端点见 [api.md](api.md)。NODE-006 的独立 P0 事务实验仍见 [节点接受报告](../P0_NODE_ACCEPTANCE.md)。NODE-002~004、NODE-016 仍未实现。

节点只持节点签名钥与用户公开凭据，不持用户解密钥。使用独立 SQLite 与密文文件目录；默认一个应用进程。设备/角色元数据可见是设计取舍。节点对 Commit 只能做公开元数据校验，最终加密结果由客户端验证。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [NODE-001 register_device](node.md#node-001)
- [NODE-002 publish_discovery](node.md#node-002)
- [NODE-003 publish_key_packages](node.md#node-003)
- [NODE-004 claim_key_package](node.md#node-004)
- [NODE-005 register_group](node.md#node-005)
- [NODE-006 append_group_entry](node.md#node-006)
- [NODE-007 fetch_group_log](node.md#node-007)
- [NODE-008 get_group_head](node.md#node-008)
- [NODE-009 store_mailbox_item](node.md#node-009)
- [NODE-010 read_mailbox_page](node.md#node-010)
- [NODE-011 ack_mailbox_items](node.md#node-011)
- [NODE-012 begin_blob_upload](node.md#node-012)
- [NODE-013 put_blob_chunk](node.md#node-013)
- [NODE-014 complete_blob_upload](node.md#node-014)
- [NODE-015 read_blob_chunk](node.md#node-015)
- [NODE-016 expire_node_data](node.md#node-016)

<a id="node-001"></a>

## NODE-001 · register_device

~~~text
register_device(request: RegisterDeviceRequest, invite: RegistrationCapability) -> Result<DeviceRegistration>
~~~

**职责与输出**：验证主管授权并幂等登记设备公开信息。

**输入与前置条件**：注册能力有效/作用域匹配，设备证明持有私钥。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证邀请和请求鉴权。
2. 验设备凭据/撤销版本。
3. 检查账户设备上限。
4. 按设备唯一键保存。
5. 消费注册能力。

**持久化、并发与副作用**：T4：设备登记/能力消费/request dedup 一事务；不收用户私钥。

**失败处理**：INVALID_CAPABILITY，INVALID_CREDENTIAL，DEVICE_LIMIT_EXCEEDED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)、[API-003 authenticate_request](api.md#api-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 重复注册同摘要同结果。
- [x] 冒充新设备第三台拒绝。

**实现状态**：已实现（P1-03）。

实现签名：`NodeService::register_device(&mut self, request: &RegisterDeviceRequest, proof: &VerifiedRequestProof, now: u64) -> NodeResult<DeviceRegistration>`。契约中的 `invite` 参数就是 `request.capability`；请求鉴权由 `proof` 保证，而 `VerifiedRequestProof` 只能由 `NodeService::verify_device_signature`（API-003）构造，字段私有，因此“持有证明”等价于“刚通过了一次一次性挑战鉴权”。时间一律显式传入，节点不使用本地计时器推断任何正式状态。

源码位置：[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)（`NodeService::register_device` 与 `NodeService::verify_device_signature`）、[registration.rs](../../crates/node-services/src/registration.rs)（凭据/能力/回执类型与签名原文）、[canonical.rs](../../crates/node-services/src/canonical.rs)（域分隔规范编码）、[auth.rs](../../crates/node-services/src/auth.rs)（一次性挑战）、[error.rs](../../crates/node-services/src/error.rs)（稳定错误码）、[limits.rs](../../crates/node-services/src/limits.rs)（全部上限）、[migrations/0002_node_auth.sql](../../crates/node-services/migrations/0002_node_auth.sql)（复用 0001 的 devices/auth_challenges/capability_revocations 并补节点身份、设备公钥、能力消费与回执表）；HTTP 适配在 [crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)。

已实现行为：

1. 注册需要账户主管钥签发的设备凭据与注册能力（能力只允许登记 account/device 匹配的一台设备，用途固定 `device_registration`，有期限上限与撤销版本）。
2. 设备必须在已签发的一次性挑战上用私钥签名，签名原文绑定 `node_id + device_id + 用途 + method + 规范 path + query + 正文摘要 + challenge nonce`；节点用凭据中的设备公钥验签，因此换方法/路径/查询/正文/节点、换设备或重放同一挑战都会显式失败。
3. T4 单事务写入 devices、device_public_keys、consumed_registration_capabilities、device_registrations，并返回节点签名回执；重复注册返回同一份持久回执（幂等），同 device_id 换凭据拒绝（INVALID_CREDENTIAL）。
4. 账户主管公钥在首次注册时固定（TOFU），此后不一致即 INVALID_CREDENTIAL；有效设备数达到上限即 DEVICE_LIMIT_EXCEEDED，不静默替换旧设备。

设备上限取值：`DEFAULT_MAX_DEVICES_PER_ACCOUNT = 2`，与产品口径一致（[ARCHITECTURE.md](../design/ARCHITECTURE.md)、[MVP.md](../design/MVP.md) 与 [TYPES.md](TYPES.md) 的“每账户最多 2 台设备”）。该值是 [NodeAuthPolicy](../../crates/node-services/src/services.rs) 的可配上限，测试用可配值覆盖其它取值（例如 cap=3 时拒绝第 4 台）；节点任何情况下都不隐式放宽，也不静默替换或截断旧设备。

验证证据（本地离线执行）：

- 命令：`cargo test --offline -p chat-node-services -p chat-node-api`
- 结果：全部通过（chat-node-services：11 个 lib 单元测试 + 18 个 `tests/auth_registration.rs` 集成测试 + 4 个既有 `tests/p0_accept.rs` 测试；chat-node-api：8 个 `tests/http_contract.rs` 真 TCP 端到端测试）。
- 测试依赖说明：`crates/node-services` 的 dev-dependency `chat-crypto` 只服务于既有 `tests/p0_accept.rs`，用真实 MLS 设备生成密文/Welcome，是 NODE-006 既有 P0 T4/C05 验收证据（[P0_NODE_ACCEPTANCE.md](../P0_NODE_ACCEPTANCE.md) 引用）；本轮新增的鉴权/注册测试只用 `ed25519-dalek` + `serde_json`，不依赖客户端加密 crate。
- 幂等：`duplicate_registration_is_idempotent_and_does_not_add_devices`、`valid_signature_registers_device_and_duplicate_is_idempotent`（HTTP）。
- 设备上限：`device_limit_rejects_registrations_beyond_the_cap`（默认策略下第三台即被拒绝且不落库；可配 cap=3 时拒绝第 4 台）、`device_limit_is_enforced_end_to_end`（HTTP 端到端同样覆盖默认 2 台与可配 3 台，均返回 409 `DEVICE_LIMIT_EXCEEDED`）。
- 其余用例：`valid_registration_returns_signed_receipt_and_persists_device`、`wrong_device_key_is_rejected_without_consuming_the_challenge`、`second_use_of_the_same_challenge_is_rejected`、`signature_is_bound_to_method_path_query_body_and_node`、`expired_challenge_is_rejected`、`capability_scope_expiry_and_purpose_are_enforced`、`expired_credential_is_rejected`、`supervisor_key_change_is_rejected`、`revoked_capability_version_is_rejected`（lib 单元测试）、`registered_device_cannot_register_for_another_account`、`malformed_and_oversized_inputs_are_rejected_with_bounded_errors`、`concurrent_challenge_issuance_stays_within_capacity`、`concurrent_reuse_of_one_challenge_succeeds_at_most_once`、`node_identity_is_pinned_across_restart`、`migration_checksum_drift_is_rejected`（lib 单元测试）。

已知限制（未勾选的验收项之外仍需补齐的能力）：

- 账户根信任：节点侧没有账户主管公钥的预注册，首次注册按 TOFU 固定；真正的账户根信任需要 IDENTITY 阶段提供可信锚点，当前实现只保证“后续设备必须由同一主管钥授权”。
- 逐设备/账户限流（API-005）已在 HTTP 层实现（[src/rate_limit.rs](../../crates/node-services/src/rate_limit.rs)，记录见 [api.md](api.md) 的 API-005）：固定窗口、进程内计数、设备/账户双桶、超限返回 429 `RATE_LIMITED` 并带 `retry_after`。**逐 IP 维度仍未实现**：节点从不读取 `X-Forwarded-For` 等代理头，伪造代理头因此无效，但同一 NAT 后的匿名请求共享一个全局匿名预算。此外仍有全局未消费挑战容量上限、单请求正文上限与单次清理批量上限。
- 注册能力撤销只读取 0001 的 `capability_revocations`（按 issuer_id=account_id + capability_id 匹配版本）；本阶段没有对外撤销端点，撤销记录需由后续阶段或运维写入。
- 挑战只持久化 nonce 的 SHA-256（沿用 0001 表结构），因此客户端必须回送 nonce；挑战不可跨重启重放，但节点重启后旧挑战仍会按过期时间被拒绝。
- 已登记设备更换凭据（撤销后重新授权）在 P1-03 明确拒绝（`INVALID_CREDENTIAL` / `DEVICE_REVOKED`），设备撤销与重授权流程属后续阶段。
- NODE-002 publish_discovery、NODE-003～NODE-004（密钥包）、NODE-016（TTL 回收）与 API-009～API-010 的其余部分（事件套接字、后台 GC、TLS 终止、管理接口）仍未实现。[apps/node](../../apps/node) 现在**真的会启动**（`run_node`：配置 → 迁移 → 绑定 → axum 全部端点 → Ctrl+C 优雅关闭；实现记录见 [api.md](api.md) 的 API-010），但仍是“单进程、单 SQLite 连接串行、进程内限流”的形态：没有多实例协调、没有 TLS（默认只允许回环监听）、没有后台回收 worker，因此还不是生产级可部署节点。

通过本函数用例不自动代表整个模块完成。

<a id="node-002"></a>

## NODE-002 · publish_discovery

~~~text
publish_discovery(record: SignedDiscoveryRecord, actor: AuthenticatedDevice) -> Result<()>
~~~

**职责与输出**：保存版本递增的设备签名地址记录。

**输入与前置条件**：actor=record.device；记录有效期有上限。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证签名/端点绑定。
2. 检查版本大于当前或同版同摘要。
3. 有界保存地址。
4. 记录过期时间。

**持久化、并发与副作用**：短事务 CAS 版本；拒绝回滚。

**失败处理**：DISCOVERY_ROLLBACK，PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[TRANSPORT-002 verify_discovery_record](transport.md#transport-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 相同版本不同地址拒绝。
- [ ] 过期记录不返回。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="node-003"></a>

## NODE-003 · publish_key_packages

~~~text
publish_key_packages(packages: Vec<PublicKeyPackage>, actor: AuthenticatedDevice) -> Result<PublishSummary>
~~~

**职责与输出**：保存设备公开包，不保存配套私有材料。

**输入与前置条件**：每设备库存/批次/字节受限；actor 自己的包。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 逐包验签和凭据。
2. 检查套件/期限。
3. 按摘要去重。
4. 限制库存。
5. 保存可领取状态。

**持久化、并发与副作用**：一批短事务，策略选择全成或全败并固定。

**失败处理**：INVALID_KEY_PACKAGE，QUOTA_EXCEEDED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CRYPTO-003 validate_key_package](crypto.md#crypto-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重复发布不增加库存。
- [ ] 过期包清理。
- [ ] 非本人包拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="node-004"></a>

## NODE-004 · claim_key_package

~~~text
claim_key_package(target: DeviceId, request_id: OperationId, actor: AuthenticatedDevice, scope: ClaimCapability) -> Result<ClaimedPackage>
~~~

**职责与输出**：原子取出一个一次性包；同请求返回同结果。

**输入与前置条件**：请求有联系人/群加入授权，不能匿名消耗库存。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证 scope。
2. 查 request dedup。
3. 选择未消费有效包。
4. 原子标 claimed_by。
5. 存请求结果。
6. 返回公开包。

**持久化、并发与副作用**：T4：消费状态与幂等结果同事务；后续失败不将包无条件重新投放。

**失败处理**：KEY_PACKAGE_UNAVAILABLE，INVALID_CAPABILITY。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 两请求并发不拿同包。
- [ ] 同 request ID 重试返回原包。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="node-005"></a>

## NODE-005 · register_group

~~~text
register_group(request: SignedGroupCreation, actor: AuthenticatedDevice) -> Result<GroupRegistration>
~~~

**职责与输出**：固定 GroupId/CoordinatorId 和初始授权设备集合。

**输入与前置条件**：actor 有创建能力；初始状态签名与设备凭据合法。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验创建候选。
2. 检查 group ID 未冲突。
3. 校验单一群主/容量。
4. 保存初始 head/policy/epoch。
5. 返回节点签名起点。

**持久化、并发与副作用**：T4：群/成员/head/幂等创建结果原子化。

**失败处理**：GROUP_ID_CONFLICT，PERMISSION_DENIED，INVALID_CREDENTIAL。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-005 verify_candidate](protocol.md#protocol-005)、[DOMAIN-003 validate_capacity](domain.md#domain-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 同 ID 同摘要重试返回原结果。
- [x] 同 ID 换 owner 拒绝。

**实现状态**：已实现（P1，群日志与信箱轮次）。

实现签名：`NodeService::register_group(&mut self, request: &SignedGroupCreation, actor: &AuthenticatedDevice, now: u64) -> NodeResult<GroupRegistration>`，源码 [crates/node-services/src/group.rs](../../crates/node-services/src/group.rs)。

已实现行为：

1. 请求形状与容量：`GroupCreation::validate_shape` 要求 GroupId 是协议规范文本（PROTOCOL-001）、owner 账户有界、初始控制头为 32 字节摘要、policy_version/mls_epoch 有界、成员数在节点策略 `GroupLogPolicy.max_members` 以内（超限 GROUP_CAPACITY_EXCEEDED）、设备互不重复且恰好一个 owner。
2. 权限：actor 必须是 owner 账户下的已登记 active 设备（未登记 INVALID_CREDENTIAL、已撤销 DEVICE_REVOKED、跨账户 PERMISSION_DENIED）；创建请求必须由该设备私钥签名（域分隔原文 `chat/p1/node-group-creation/v1`），验签失败 INVALID_SIGNATURE。
3. 初始成员必须是同一 owner 账户下的已登记 active 设备；跨账户成员 PERMISSION_DENIED，未登记 INVALID_CREDENTIAL。
4. T4 单事务写入 groups（coordinator_id 固定为本节点、last_seq=0、last_hash=全零）、group_control_heads（初始 head/policy/epoch 与公开成员快照）、group_members（joined_seq=0）与 group_registrations（幂等回执）；回执由节点签名（域分隔 `chat/p1/node-group-registration/v1`）。
5. 幂等：同 GroupId 同创建摘要（含签名）返回同一份持久回执；同 GroupId 相异内容（含换 owner）返回 GROUP_ID_CONFLICT，不覆盖已登记状态。

验证证据（`cargo test --offline -p chat-node-services -p chat-node-api` 全绿）：`register_group_is_idempotent_and_rejects_conflicts`（服务层：幂等、换内容冲突、跨账户 owner、跨账户成员）、`group_registration_and_append_are_idempotent_over_http`（真 TCP 端到端：登记回执签名可验证、重复登记同回执）。

已知限制：创建能力（API-004 validate_capability）未实现，本阶段以“owner 账户的已登记设备私钥签名”作为创建授权，尚无独立的一次性群创建能力令牌；控制头/epoch 的语义见 NODE-006 的说明。通过本函数用例不自动代表整个模块完成。

<a id="node-006"></a>

## NODE-006 · append_group_entry

~~~text
append_group_entry(group: GroupId, candidate: SignedCandidate, actor: AuthenticatedDevice) -> Result<AppendResult>
~~~

**职责与输出**：为合法候选持久分配唯一 seq 与签名回执。

**输入与前置条件**：传输鉴权/大小/速率通过；actor 是候选作者；节点固定协调该群。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验外层签名。
2. 在事务内查 SubmissionId。
3. 同摘要返回旧结果。
4. 核对 head/policy/epoch 和权限。
5. 分配 seq/hash。
6. 签回执。
7. 写日志/公开策略/去重/投递任务。
8. commit 后返回。

**持久化、并发与副作用**：T4：以上同 SQLite 写事务；网络 fanout 在提交后任务执行；节点不能验证 MLS 明文。

**失败处理**：SUBMISSION_ID_CONFLICT，STALE_EPOCH，STALE_CONTROL_HEAD，PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-005 verify_candidate](protocol.md#protocol-005)、[DOMAIN-001 validate_send_permission](domain.md#domain-001)、[DOMAIN-002 authorize_group_operation](domain.md#domain-002)、[PROTOCOL-006 receipt_signing_bytes](protocol.md#protocol-006)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 两管理员竞争只一个前置状态成功。
- [x] 超时重试得到同 seq。
- [x] 同 ID 不同字节拒绝。

**实现状态**：正式函数已实现（P1，群日志与信箱轮次）。实现签名：`NodeService::append_group_entry(&mut self, group: &str, candidate: &SignedCandidate, actor: &AuthenticatedDevice, now: u64) -> NodeResult<AppendResult>`，源码 [crates/node-services/src/group.rs](../../crates/node-services/src/group.rs)；另有 `#[doc(hidden)] append_group_entry_with_checkpoint` 只用于崩溃证据注入，生产语义完全相同。独立 [`p0_accept`](../../crates/node-services/src/p0_accept.rs) 仍是分离的 P0 实验（其 4 个既有测试继续通过），与本正式路径没有共享代码。

已实现行为：

1. 外层签名：候选转成 PROTOCOL-004 的 `UnsignedCandidate` 后用作者设备已登记公钥严格验签（`candidate_signing_bytes` + `verify_strict`），任何字段（群/设备/kind/expected_control_head/policy_version/mls_epoch/密文摘要/控制元数据）变化都会失败。路由 group、候选 group 与 actor 设备三者必须一致，否则 PERMISSION_DENIED。
2. 幂等：同一 (group, submission_id) 在事务内比对候选摘要；同摘要返回原持久回执（先复核回执签名与“回执-候选”绑定），相异摘要 SUBMISSION_ID_CONFLICT。`group_log` 的 `UNIQUE(group_id, submission_id)` 是该语义的数据库兜底。
3. 权限：actor 必须是该群当前有效成员（按 `group_members` 的 joined_seq/left_seq 窗口判定）；未加入或已退出的一律 PERMISSION_DENIED。
4. 公开元数据 CAS：policy_version 与 expected_control_head 必须等于节点当前值（否则 STALE_CONTROL_HEAD），mls_epoch 必须等于当前值（否则 STALE_EPOCH）。接受控制候选时节点只推进“节点可观察的控制链头”（= 该条日志的 log_hash）；policy_version/mls_epoch 不由节点猜测推进（见下方已知限制）。
5. seq 与哈希链：seq = last_seq + 1（溢出即 STORAGE_FAILURE），previous = 当前 last_hash，`log_hash = SHA-256(域 chat/p1/node-log-chain/v1 || group || seq || previous_hash || candidate_hash)`；回执用 PROTOCOL-006 `receipt_signing_bytes(candidate_hash, seq, previous_hash, group)` 由节点签名。
6. 原子性：日志行、控制头（仅控制候选）与 groups.last_seq/last_hash 全部在同一个 IMMEDIATE 事务内提交；事务之外没有任何第二轮写。

验证证据（`cargo test --offline -p chat-node-services -p chat-node-api` 全绿）：

- 幂等与相异摘要：`append_is_idempotent_and_rejects_divergent_bytes`、`group_registration_and_append_are_idempotent_over_http`（真 TCP 端到端 409 SUBMISSION_ID_CONFLICT）。
- 提交后结果丢失用同密文重试同结果：`append_is_idempotent_and_rejects_divergent_bytes`、`crash_after_commit_keeps_result_and_retry_is_idempotent`（子进程在提交后 abort，重启后重试拿回同一 seq/回执）。
- 并发 ≥8 路序号连续：`concurrent_appends_allocate_contiguous_sequence`（8 线程各自打开同一文件库并发追加，seq 结果恰为 1..=8，audit 结果为 8）。
- 原子提交、无半条日志：`crash_before_commit_rolls_back_the_whole_log_entry`（在“日志已写入未提交”和“提交前”两个检查点用 `std::process::abort()` 杀死子进程 [src/append_crash_worker.rs](../../crates/node-services/src/append_crash_worker.rs)；重启后 audit 为 0、head.seq 为 0、重试得到 seq 1）。
- 前置状态竞争：`stale_control_head_and_epoch_are_rejected`（两个管理员基于同一 expected_control_head，第二个 STALE_CONTROL_HEAD；epoch 不符 STALE_EPOCH）。
- 越权与畸形输入：`unauthorized_appends_are_rejected`（跨群/代签/篡改签名/超上限密文/未知群）、`append_requires_bound_authentication_and_matching_group`（缺鉴权头、换方法/路径/正文、路径与正文 group 不一致、挑战重放）。

已知限制（诚实的不完整状态，不是模拟成功）：

- 节点不解析 MLS 明文，也不做 MLS/DOMAIN 控制结果验证（PROTOCOL-005、DOMAIN-001/002 的控制语义尚未交付）。因此接受控制候选后，节点回执只声明“该条控制日志的位置”，不声明“MLS 状态已按此 commit 推进”。公开的 policy_version/mls_epoch 在本阶段被当作 CAS 常量：客户端应按注册时声明的值签名，直到“客户端验证过的控制应用路径”落地。这是明确的能力缺口，节点不会猜测 epoch += 1。
- 没有 fanout/投递任务（0001 的 node_jobs 未使用）：NODE-009 的投递由客户端显式调用信箱端点完成，节点不在事务内创建异步任务。
- 没有按设备/群的速率限制（API-005 未实现），只有策略上限（密文大小、分页条数）与 SQLite 事务串行化。
- 没有日志裁剪（NODE-016 未实现），因此 LOG_RANGE_PRUNED 路径尚不存在；`fetch_group_log` 对 after 大于 head 的情况返回明确空页。

通过本函数用例不自动代表整个模块完成。

<a id="node-007"></a>

## NODE-007 · fetch_group_log

~~~text
fetch_group_log(group: GroupId, after: u64, limit: u32, actor: AuthenticatedDevice) -> Result<LogPage>
~~~

**职责与输出**：返回可见日志与恢复控制材料，条数/字节双限。

**输入与前置条件**：actor 在允许的成员窗口；不能从 after=0 绕过加入历史策略。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 查成员窗口。
2. 限制内容范围。
3. 按 seq 返回。
4. 控制恢复材料走单独规则。
5. 返回签名 head 和分页位置。

**持久化、并发与副作用**：只读快照；旧内容清理后返回明确不可恢复缺口。

**失败处理**：HISTORY_NOT_AUTHORIZED，LOG_RANGE_PRUNED，GROUP_NOT_FOUND。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-012 enforce_join_visibility](domain.md#domain-012)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 新成员无法拉旧正文/附件。
- [x] 已退出设备无权读新条目。

**实现状态**：已实现（P1，群日志与信箱轮次）。实现签名：`NodeService::fetch_group_log(&mut self, group: &str, after: u64, limit: u32, actor: &AuthenticatedDevice) -> NodeResult<LogPage>`，源码 [crates/node-services/src/group.rs](../../crates/node-services/src/group.rs)。

已实现行为：

1. 只读快照：整个读取（成员窗口 + 日志页 + 控制头 + head）在同一个 deferred 事务内完成，返回的 head 与这一页来自同一快照。
2. 成员窗口：设备必须在该群有有效成员记录（按 joined_seq/left_seq 取最近一段），否则 HISTORY_NOT_AUTHORIZED；可见范围是 `[max(after+1, joined_seq), left_seq)`，因此后加入的设备读不到自己加入前的条目、已退出的设备读不到退出后的条目。
3. 分页：limit 必须在 `[1, GroupLogPolicy.max_page_entries]` 内（否则 INVALID_REQUEST），返回 `entries`、下一位置 `next_after` 与 `has_more`；after 大于 head 时返回明确空页而不是回到开头。
4. 双限：单条密文受 `GroupLogPolicy.max_candidate_ciphertext_bytes` 限制；单页条数受 `max_page_entries` 限制。
5. 逐条复核：读出的每一条都会重算 candidate 摘要、哈希链与回执签名，任何不一致都返回 STORAGE_FAILURE（不返回被篡改的日志）。
6. 返回签名 head（NODE-008 的同一份编码）与页码位置；控制恢复材料不走本端点。

验证证据：`log_paging_and_head_are_consistent`（分页两页、head 与页一致、after 越界空页、limit 0/超上限拒绝、非成员 HISTORY_NOT_AUTHORIZED/PERMISSION_DENIED、未知群 GROUP_NOT_FOUND）、`membership_window_limits_history_and_left_device_cannot_append`（真实文件库中把成员窗口改成 joined_seq=3 后只读到 seq 3；改成 joined=1/left=3 后只读到 seq 1,2 且追加被拒绝）、`log_and_head_endpoints_paginate_and_reject_bad_input`（HTTP 端到端分页与 limit/未知参数拒绝）。

已知限制：没有历史裁剪（NODE-016），因此不会返回 LOG_RANGE_PRUNED；没有按字节的分页预算，只有条数上限（单条密文上限远小于正文上限）。通过本函数用例不自动代表整个模块完成。

<a id="node-008"></a>

## NODE-008 · get_group_head

~~~text
get_group_head(group: GroupId, actor: AuthenticatedDevice) -> Result<SignedLogHead>
~~~

**职责与输出**：返回绑定 group/seq/hash/control_head/policy/epoch 的节点签名头。

**输入与前置条件**：设备有该群对账权限，签名域与 entry 回执区分。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取一致 head。
2. 规范编码。
3. 用节点签名钥签。
4. 返回头与节点 key id。

**持久化、并发与副作用**：只读快照；WS 提示不能替代此对账。

**失败处理**：PERMISSION_DENIED，GROUP_NOT_FOUND。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 替换 group/epoch 导致验签失败。
- [x] 节点重启同持久头。

**实现状态**：已实现（P1，群日志与信箱轮次）。实现签名：`NodeService::get_group_head(&mut self, group: &str, actor: &AuthenticatedDevice) -> NodeResult<SignedLogHead>`，源码 [crates/node-services/src/group.rs](../../crates/node-services/src/group.rs)。

已实现行为：

1. 只读一致快照：groups.last_seq/last_hash 与 group_control_heads 在同一事务内读取。
2. 规范编码 + 节点签名：head 绑定 `node_id + node_public_key + group_id + coordinator_id + seq + log_hash + control_head + policy_version + mls_epoch`（域分隔 `chat/p1/node-log-head/v1`，与条目回执的 PROTOCOL-006 域严格不同）。
3. 权限：actor 必须是该群当前有效成员，否则 PERMISSION_DENIED；未知群 GROUP_NOT_FOUND；句柄包含节点 key id（node_public_key），客户端据此固定节点身份。

验证证据：`head_survives_restart_and_binds_group_sequence_and_hash`（重启前后同一 head；篡改 group_id/seq/mls_epoch 都导致验签失败）、`log_paging_and_head_are_consistent`（head 端点与日志页 head 完全相等）、`stale_control_head_and_epoch_are_rejected`（控制候选被接受后 head.control_head 等于该条日志摘要，而 policy/epoch 不被节点猜测推进）。

已知限制：head 声明的是“节点自己的日志链位置与公开控制头”，不是 MLS 状态的证明；WS 提示（API-009）未实现，本端点是对账的唯一正式入口。通过本函数用例不自动代表整个模块完成。

<a id="node-009"></a>

## NODE-009 · store_mailbox_item

~~~text
store_mailbox_item(item: AuthorizedMailboxItem, actor: AuthenticatedDevice) -> Result<MailboxReceipt>
~~~

**职责与输出**：配额内持久存密文/Welcome 并返回存储证据。

**输入与前置条件**：目标设备有效；能力限定 sender/target/group/期限；payload 有界。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验能力和目标。
2. 核对 accepted entry 或 Welcome 授权关联。
3. 查 item dedup。
4. 预留配额。
5. 插入条目/TTL。
6. 签存储回执。

**持久化、并发与副作用**：T4：item/dedup/quota 原子提交；ACK 后故障恢复仍存在。

**失败处理**：INVALID_CAPABILITY，QUOTA_EXCEEDED，DEVICE_REVOKED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[API-004 validate_capability](api.md#api-004)、[PROTOCOL-005 verify_candidate](protocol.md#protocol-005)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 开放匿名垃圾投递拒绝。
- [x] 重复 item 不重占配额。
- [x] 节点重启后仍可拉。

**实现状态**：已实现（P1，群日志与信箱轮次）。实现签名：`NodeService::store_mailbox_item(&mut self, item: &MailboxItem, actor: &AuthenticatedDevice, now: u64) -> NodeResult<MailboxReceipt>`，源码 [crates/node-services/src/mailbox.rs](../../crates/node-services/src/mailbox.rs)。

已实现行为：

1. 只收密文：`ciphertext_envelope` 是客户端密文（小写十六进制），节点按 `MailboxPolicy.max_item_bytes` 有界解码，不解析明文、不保存文件密钥。
2. 授权关联（拒绝匿名/无关联投递）：actor 必须是已登记 active 设备；目标设备必须是该群当前有效成员；`source_seq` 必须指向一条已接受日志条目，且该条目作者正是发送设备本人。任一条件不满足即 PERMISSION_DENIED（未知条目、他设备条目、非成员目标都返回同一码，不泄漏存在性）。
3. 期限：`expires_at` 必须满足 now < expires_at ≤ now + MailboxPolicy.default_ttl_seconds（默认 30 天），否则 INVALID_REQUEST。
4. 幂等：同 item_id 同内容返回同一份持久回执；同 item_id 相异密文 ITEM_ID_CONFLICT。回执由节点签名（域分隔 `chat/p1/node-mailbox-receipt/v1`），并绑定 item/group/target/sender/source_seq/摘要/字节数/ordinal/时间。
5. 配额：T4 单事务内按未过期且未消费条目的字节合计核算该目标设备的持久占用，超出 `max_bytes_per_device` 即 QUOTA_EXCEEDED，不静默驱逐旧条目。
6. 稳定序：ordinal 在事务内分配（全局单调），分页按 ordinal 键集推进。

验证证据：`store_is_idempotent_and_rejects_divergent_payload`、`store_requires_authorized_entry_and_group_membership`（未知 source_seq、非成员目标、代别人条目、过期/超 TTL、伪造 actor）、`quota_is_enforced_and_released_by_ack`（配额硬上限与 ACK 释放）、`mailbox_survives_restart_and_ack_stays_consumed`（重启后仍可拉取）、`mailbox_store_read_and_ack_over_http`（真 TCP 端到端投递与签名回执）。

已知限制：API-004 validate_capability 未实现，因此没有独立的“一次性投递能力令牌”（例如把 Welcome 投给尚未入群的新设备所需的能力）；本阶段只支持“已入群成员 + 已接受条目”的授权关联，Welcome 场景明确未支持。通过本函数用例不自动代表整个模块完成。

<a id="node-010"></a>

## NODE-010 · read_mailbox_page

~~~text
read_mailbox_page(actor: AuthenticatedDevice, cursor: Option<MailboxCursor>, limit: u32) -> Result<MailboxPage>
~~~

**职责与输出**：仅当前设备拉自己的有界页面。

**输入与前置条件**：cursor 认证绑定 device/排序版本；不能用 ID 查询他人信箱。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验游标。
2. 按稳定 item 序列读取未消费有效条目。
3. 限制字节。
4. 生成新游标。
5. 附过期缺口提示。

**持久化、并发与副作用**：只读；读取不删除；分页位置不能因 TTL 删除而跳到别人的数据。

**失败处理**：INVALID_CURSOR，DEVICE_REVOKED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-013 decode_page_cursor](protocol.md#protocol-013)、[PROTOCOL-012 encode_page_cursor](protocol.md#protocol-012)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 跨设备游标拒绝。
- [x] 重复请求相同材料可重读。

**实现状态**：已实现（P1，群日志与信箱轮次）。实现签名：`NodeService::read_mailbox_page(&mut self, actor: &AuthenticatedDevice, cursor: Option<&str>, limit: u32, now: u64) -> NodeResult<MailboxPage>`，源码 [crates/node-services/src/mailbox.rs](../../crates/node-services/src/mailbox.rs)。

已实现行为：

1. 只作用于自己的信箱：查询目标恒为 actor 设备，客户端不能指定 device id（HTTP 层拒绝未知查询参数）。
2. 游标复用 PROTOCOL-012/013：`encode_page_cursor/decode_page_cursor` 的 scope 绑定 账户/设备/资源（Mailbox）/排序版本，位置是稳定键（ordinal），AEAD 密钥由节点签名种子派生（跨重启稳定、不外泄）。跨设备、伪造、过期游标一律 INVALID_CURSOR，绝不退化为跳到最新页。
3. 分页边界：limit 必须在 `[1, MailboxPolicy.max_page_items]` 内（否则 INVALID_REQUEST）；单页还受 `max_page_bytes` 预算约束，超出预算就分页而不是截断单条密文（策略校验保证单条能放进一页）。
4. 只读不删：读取不改变条目状态，因此同一游标重复请求得到同一批材料。
5. 过期缺口：只返回仍存储且未过期的条目，并返回 `expired_items`（本设备已过期未消费的条数）作为历史缺口提示。

验证证据：`read_page_paginates_and_rejects_foreign_or_forged_cursors`（两页推进、重复读同材料、跨设备游标、伪造游标、过期游标、limit 0/超上限）、`read_hides_expired_items_and_reports_the_gap`、`mailbox_store_read_and_ack_over_http`（HTTP 端到端 GET 分页与 400 INVALID_CURSOR）。

已知限制：游标放在 GET 查询串里，而 PROTOCOL-013 的 token 上限是 1024 字节，因此本轮把节点查询串上限从 512 放宽到 2048（`limits::MAX_QUERY_BYTES`），否则合法游标无法通过签名原文规范化；整段查询串仍参与签名，仍是明确的有界值。通过本函数用例不自动代表整个模块完成。

<a id="node-011"></a>

## NODE-011 · ack_mailbox_items

~~~text
ack_mailbox_items(actor: AuthenticatedDevice, ids: Vec<MailboxItemId>) -> Result<AckSummary>
~~~

**职责与输出**：幂等确认当前设备消费并释放条目配额。

**输入与前置条件**：请求签名绑定 ids；批量大小受限。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 去重 ids。
2. 核对归属。
3. 标 consumed 或删除可清理条目。
4. 释放精确配额。
5. 返回逐项结果。

**持久化、并发与副作用**：T4：消费状态与 quota 同事务；与客户端 E2E delivered 语义分离。

**失败处理**：PERMISSION_DENIED，INVALID_ACK_BATCH。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 重复 ACK 不把 quota 减为负。
- [x] 他设备 ACK 拒绝。

**实现状态**：已实现（P1，群日志与信箱轮次）。实现签名：`NodeService::ack_mailbox_items(&mut self, actor: &AuthenticatedDevice, ids: &[String], now: u64) -> NodeResult<AckSummary>`，源码 [crates/node-services/src/mailbox.rs](../../crates/node-services/src/mailbox.rs)。

已实现行为：

1. 批量上限：ids 非空且不超过 `MailboxPolicy.max_ack_batch`（否则 INVALID_ACK_BATCH）；请求内重复 id 去重且保留顺序。
2. 归属校验：逐个核对 target_device_id，未知条目与他设备条目都返回 PERMISSION_DENIED，不泄漏存在性；只要有一项不满足，整批在写任何消费状态之前就失败（原子拒绝，没有部分消费）。
3. 幂等：已消费的条目进入 `already_consumed` 且 `released_bytes` 为 0；只有真正从 stored 改为 consumed 的条目计入 `acknowledged` 与释放字节数，所以重复 ACK 不会把配额减成负数。
4. 结果：返回逐项状态与精确释放字节；消费状态与配额释放同事务（配额由未消费条目的字节合计派生，不存在独立计数器漂移）。

验证证据：`ack_is_idempotent_and_only_acks_own_items`（重复 ACK、他设备条目、未知条目、空批量、超批量、批量混入未知项无部分消费）、`quota_is_enforced_and_released_by_ack`、`mailbox_survives_restart_and_ack_stays_consumed`（重启后仍为已消费且不再释放）、`mailbox_store_read_and_ack_over_http`（HTTP 端到端 200 幂等与 403 越权）。

已知限制：ACK 只表示节点侧条目已消费，与客户端 E2E delivered 语义无关；没有单独的 TTL 回收任务（NODE-016），过期条目仍以 stored 状态保留在原表。通过本函数用例不自动代表整个模块完成。

<a id="node-012"></a>

## NODE-012 · begin_blob_upload

~~~text
begin_blob_upload(actor: AuthenticatedDevice, descriptor: PublicBlobDescriptor, operation: OperationId) -> Result<UploadSession>
~~~

**职责与输出**：创建只含密文规则的上传会话并预留配额。

**输入与前置条件**：公共 descriptor 不含文件密钥/明文摘要；实际密文字节有界。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查能力与限额。
2. request dedup。
3. 预留实际字节。
4. 创建随机 upload ID/期限。
5. 返回受限上传能力。

**持久化、并发与副作用**：T4：quota/session/dedup 同事务；临时文件清理有日志。

**失败处理**：QUOTA_EXCEEDED，CONTENT_TOO_LARGE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[API-004 validate_capability](api.md#api-004)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 并发上传不能超总配额。
- [x] 同 operation 重试同 session。

**实现状态**：已实现（W 包，附件 blob 轮次）。实现签名：`NodeService::begin_blob_upload(&mut self, descriptor: &PublicBlobDescriptor, operation_id: &str, actor: &AuthenticatedDevice, now: u64) -> NodeResult<UploadSession>`，源码 [crates/node-services/src/blob.rs](../../crates/node-services/src/blob.rs)。

已实现行为：

1. 描述符校验：`blob_id` 是 PROTOCOL-001 object 文本、`group_id` 是 group 文本；总密文（≤ 对象上限）、块大小（≤ 单块上限）、块数（≤ 块数上限）都必须落在 `BlobPolicy` 内，且三者必须自洽（前 n-1 块满块、最后一块非空不超块大小），否则 CONTENT_TOO_LARGE / INVALID_REQUEST。
2. 授权关联：actor 是该群当前有效成员，且 `source_seq` 指向一条作者正是 actor 本人的已接受日志条目；否则 PERMISSION_DENIED。
3. 幂等：`(owner_device_id, operation_id)` 唯一；同描述重试返回**同一份**会话（会话签名是确定性的），相异描述返回 BLOB_UPLOAD_CONFLICT。已发布对象或别的会话占用的 blob_id 也返回 BLOB_UPLOAD_CONFLICT。
4. 配额：T4 同一 IMMEDIATE 事务内按“在传预留（未过期 uploading 声明字节）+ 已发布占用”精确核算该设备字节，超出 `max_bytes_per_device` 即 QUOTA_EXCEEDED（并发 begin 也只能有一个赢）。
5. 会话：节点分配随机 `upload_id`、写入 1 小时（可配）`expires_at`，并返回**节点签名**的 `UploadSession`（域 `chat/p1/node-blob-session/v1`，绑定 blob/群/条目/块参数/期限/节点身份）。上传授权就是“该设备的 API-003 身份”，会话是条款回执而不是可转借的令牌。

验证证据（`cargo test --offline -p chat-node-services -p chat-node-api -p chat-node-client` 全绿）：`blob::upload_round_trip_returns_exact_ciphertext_chunks`、`blob::operation_dedup_returns_same_session_and_conflicts_on_divergence`、`blob::device_quota_is_enforced_across_uploads`、`blob::device_quota_is_enforced_across_uploads`（在传预留也计入配额）、`blob::descriptor_validation_rejects_malformed_shapes`、`blob::size_and_count_limits_are_enforced`、`blob::source_entry_and_membership_window_gate_uploads_and_reads`，HTTP/客户端侧 `blob_upload_and_read_round_trip_over_http`、`blob_http_error_statuses_are_stable`、客户端 `blob_flow::blob_upload_and_read_round_trip_over_http`。

已知限制：API-004 validate_capability 未实现，因此没有独立的上传能力令牌（授权=已登记设备 + 群成员 + 本人条目）；没有按设备/群的速率限制（API-005）；过期会话不会主动回收（NODE-016 未实现）。通过本函数用例不自动代表整个模块完成。

<a id="node-013"></a>

## NODE-013 · put_blob_chunk

~~~text
put_blob_chunk(upload: UploadId, index: u32, bytes: CipherChunk, actor: AuthenticatedDevice) -> Result<ChunkReceipt>
~~~

**职责与输出**：同 index/同摘要幂等，异摘要拒绝，不信任用户文件名。

**输入与前置条件**：会话有效，index/块大小/总密文大小受限。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对能力/owner。
2. 计算密文摘要。
3. 查旧 index。
4. 写受控临时块并同步。
5. 短事务登记。
6. 返回摘要回执。

**持久化、并发与副作用**：文件与 DB 使用 staged/committed 恢复标记；不能声称双系统天然原子。

**失败处理**：CHUNK_CONFLICT，UPLOAD_EXPIRED，CONTENT_TOO_LARGE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 同块重传不占双份。
- [ ] 写文件后 crash 启动可恢复/清理。

**实现状态**：已实现（W 包，附件 blob 轮次）。实现签名：`NodeService::put_blob_chunk(&mut self, upload_id: &str, index: u32, bytes: &[u8], actor: &AuthenticatedDevice, now: u64) -> NodeResult<ChunkReceipt>`，源码 [crates/node-services/src/blob.rs](../../crates/node-services/src/blob.rs)。

已实现行为：

1. 归属与生命周期：只有上传设备本人能写块（否则 PERMISSION_DENIED）；会话不存在 UPLOAD_NOT_FOUND、已过期或已结束 UPLOAD_EXPIRED（已完成会话的重复块仍走幂等分支）。
2. 幂等：同 `(upload_id, chunk_index)` 已存在时，同摘要同长度返回**同一份回执**（不占第二份），相异字节 CHUNK_CONFLICT。
3. 按序：下一个待写序号 = 已提交块数；index 大于它即 CHUNK_OUT_OF_ORDER，小于它但内容不同即 CHUNK_CONFLICT，越界（≥ 声明块数）INVALID_REQUEST。
4. 边界：单块超过策略上限或超过声明块大小 → CONTENT_TOO_LARGE；与声明边界不符（更短）→ MANIFEST_MISMATCH；空块 INVALID_REQUEST。
5. 原子性：块登记行（`blob_chunks`）与块内容（`blob_chunk_content`）在**同一个** IMMEDIATE 事务写入，节点没有独立文件系统路径，因此不存在“文件已写、DB 未登记”的双系统半写；`storage_ref` 是内容寻址引用（`sha256:<hex>`）。
6. 回执：节点签名的 `ChunkReceipt`（域 `chat/p1/node-blob-chunk-receipt/v1`，绑定 upload/序号/摘要/字节数/进度）。

验证证据：`blob::duplicate_chunk_is_idempotent_and_divergent_chunk_is_rejected`、`blob::chunks_must_arrive_in_order_and_index_is_bounded`、`blob::size_and_count_limits_are_enforced`、`blob::expired_upload_session_rejects_chunks_and_completion`、`blob::unknown_identifiers_and_foreign_uploads_are_rejected`，HTTP/客户端侧 `blob_http_error_statuses_are_stable`（409/413/400/410）、`blob_flow::out_of_order_and_missing_chunks_are_rejected`、`blob_flow::duplicate_chunk_is_idempotent_and_size_violation_is_rejected`。

已知限制：块内容与登记行同库同事务，因此“写文件后 crash 的恢复/清理”这条文件系统语义**不适用**（也没有踩点实验）——本实现刻意选择单库单事务而不是“文件 + DB”双系统；NODE-016 的临时物回收仍未实现。通过本函数用例不自动代表整个模块完成。

<a id="node-014"></a>

## NODE-014 · complete_blob_upload

~~~text
complete_blob_upload(upload: UploadId, manifest: CipherManifest, actor: AuthenticatedDevice) -> Result<BlobReceipt>
~~~

**职责与输出**：只发布块数/大小/摘要全部匹配的完整密文对象。

**输入与前置条件**：会话所有必须块耐久存在，manifest 不含用户解密密钥。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对 owner/能力。
2. 验证全部 chunk 摘要和边界。
3. 完成不可变对象。
4. 登记下载授权范围。
5. 转移配额状态。
6. 返回签回执。

**持久化、并发与副作用**：先文件同步再 DB 标 published；失败保持可恢复 uploading；重复 complete 同结果。

**失败处理**：MISSING_CHUNKS，MANIFEST_MISMATCH，UPLOAD_EXPIRED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 缺末块拒绝。
- [x] 重复完成不重占配额。
- [x] 中途重启不发布半对象。

**实现状态**：已实现（W 包，附件 blob 轮次）。实现签名：`NodeService::complete_blob_upload(&mut self, upload_id: &str, manifest: &CipherManifest, actor: &AuthenticatedDevice, now: u64) -> NodeResult<BlobReceipt>`，源码 [crates/node-services/src/blob.rs](../../crates/node-services/src/blob.rs)。

已实现行为：

1. 清单校验：块数与总字节必须**等于会话声明**（否则 MANIFEST_MISMATCH）；已提交块数必须等于声明块数（否则 MISSING_CHUNKS，含缺末块）。
2. 内容校验：逐块复核序号连续、存储摘要与块内容自洽（不一致按 STORAGE_FAILURE fail-closed），累计实际总字节（必须等于声明总字节），并对“按块序拼接的密文”重算 SHA-256 与 `manifest.ciphertext_digest` 比对，不符即 MANIFEST_MISMATCH。
3. 原子发布：`blob_objects`（state=published，含 group/source_seq/块数/字节数）、上传会话置 complete、节点签名回执写入 `blob_receipts` 在同一 IMMEDIATE 事务；任何一步失败都不会留下“已发布但无回执”或“半对象”。
4. 幂等：重复 complete 返回**同一份**持久回执（验签后返回），不重复占用配额；清单与已发布内容不一致则 MANIFEST_MISMATCH。
5. 配额：把本次上传的在传预留转成已发布占用，并再次检查 `used + reserved_after <= max_bytes_per_device`（fail-closed）。

验证证据：`blob::complete_rejects_missing_chunks_and_manifest_mismatch`、`blob::complete_is_idempotent_and_conflicting_retry_is_rejected`、`blob::upload_round_trip_returns_exact_ciphertext_chunks`、`blob::unpublished_blob_is_not_readable_and_owner_is_enforced`、`blob::size_and_count_limits_are_enforced`，HTTP/客户端侧 `blob_http_error_statuses_are_stable`（缺块/摘要/未发布/发布成功）、`blob_flow::digest_mismatch_is_rejected_and_unfinished_blob_is_not_readable`。

已知限制：只有“已发布/未发布”两态，没有 collecting（NODE-016 的回收态）；没有跨对象的去重（同内容不同 blob_id 会各占一份配额）。通过本函数用例不自动代表整个模块完成。

<a id="node-015"></a>

## NODE-015 · read_blob_chunk

~~~text
read_blob_chunk(blob: BlobId, index: u32, actor: AuthenticatedDevice, capability: DownloadCapability) -> Result<CipherChunk>
~~~

**职责与输出**：仅向受限持有人返回密文块。

**输入与前置条件**：授权绑定对象/设备/群消息窗口/期限，猜 BlobId 无效。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证能力与撤销状态。
2. 检查群历史范围。
3. 验证 index。
4. 有界读取。
5. 返回实际密文摘要。

**持久化、并发与副作用**：只读；不能因在群内直接获得全部旧附件。

**失败处理**：INVALID_CAPABILITY，HISTORY_NOT_AUTHORIZED，BLOB_NOT_FOUND。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[API-004 validate_capability](api.md#api-004)、[DOMAIN-012 enforce_join_visibility](domain.md#domain-012)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 新设备拿到旧 URL 仍拒绝。
- [ ] 过期/跨对象能力拒绝。

**实现状态**：已实现（W 包，附件 blob 轮次）。实现签名：`NodeService::read_blob_chunk(&mut self, blob_id: &str, index: u32, actor: &AuthenticatedDevice, now: u64) -> NodeResult<CipherChunk>`，源码 [crates/node-services/src/blob.rs](../../crates/node-services/src/blob.rs)。

已实现行为：

1. 只读发布对象：blob 不存在或未发布一律 BLOB_NOT_FOUND（未完成的上传读不到任何块），并拒绝“已发布但块数/字节数为 0”的损坏记录。
2. 下载授权按成员窗口：上传设备本人恒可读；其他设备必须是该群**当前**有效成员，且 `joined_seq <= blob.source_seq`——附件写入之后才加入的新成员（以及注册更晚的新设备，它们根本不在群成员表里）一律 HISTORY_NOT_AUTHORIZED。
3. 范围校验：index 必须小于块数（否则 INVALID_REQUEST）；读出后再复核存储摘要与长度，不一致按 STORAGE_FAILURE 失败，不返回可疑字节。
4. 只返回密文块本身（`CipherChunk { blob_id, chunk_index, chunk_count, cipher_hash, bytes }`）；HTTP 层以 `application/octet-stream` 返回原始字节，客户端按清单摘要自行校验。

验证证据：`blob::upload_round_trip_returns_exact_ciphertext_chunks`（上传者与老成员都能读、字节完全一致）、`blob::source_entry_and_membership_window_gate_uploads_and_reads`（把成员加入点挪到附件之后 → HISTORY_NOT_AUTHORIZED）、`blob::unpublished_blob_is_not_readable_and_owner_is_enforced`、`blob::size_and_count_limits_are_enforced`（越界序号），HTTP/客户端侧 `blob_upload_and_read_round_trip_over_http`、`blob_endpoints_require_bound_authentication`（群外设备 403）、`blob_flow::blob_upload_and_read_round_trip_over_http`（群外设备 HISTORY_NOT_AUTHORIZED）。

已知限制：API-004 的签名下载能力令牌（绑定对象/设备/群消息窗口/期限）**未实现**，本阶段以“成员窗口 + 发布状态”替代，因此“过期/跨对象能力拒绝”仍未勾选；也没有按能力的撤销检查与下载速率限制（API-005）。通过本函数用例不自动代表整个模块完成。

<a id="node-016"></a>

## NODE-016 · expire_node_data

~~~text
expire_node_data(now: Time, budget: GcBudget) -> Result<CleanupReport>
~~~

**职责与输出**：分策略回收过期信箱、上传临时物和不再引用密文，保留控制恢复日志。

**输入与前置条件**：30天为信箱默认 TTL；控制日志不是同一 TTL。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 分类型选择候选。
2. 标回收。
3. 修正配额。
4. 受控路径删除。
5. 保留控制链和必要幂等结果至重试窗口结束。

**持久化、并发与副作用**：短批事务+文件删除日志；禁止一条 TTL SQL 删除所有群日志。

**失败处理**：STORAGE_FAILURE，FILE_IO_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 消息过期不破坏现存群状态验证。
- [ ] dedup 过早删不能让旧尝试二次接受。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。


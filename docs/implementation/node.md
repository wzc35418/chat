# 节点服务、排序日志、密文信箱与文件

阶段：P1 → P5。状态：函数级设计。NODE-001 register_device 与 [api.md](api.md) 的 API-002/API-003 已在 [crates/node-services/src/services.rs](../../crates/node-services/src/services.rs) 与 [crates/node-api](../../crates/node-api) 落地并通过自动化测试（见各函数“实现状态”）；其余 NODE 函数仍未实现。NODE-006 的独立 P0 事务实验见 [节点接受报告](../P0_NODE_ACCEPTANCE.md)。

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
- 逐 IP / 逐设备限流（API-005）未实现；当前只有全局未消费挑战容量上限、单请求正文上限与单次清理批量上限。错误码 `RATE_LIMITED` 已预留，但本阶段没有任何路径返回它。
- 注册能力撤销只读取 0001 的 `capability_revocations`（按 issuer_id=account_id + capability_id 匹配版本）；本阶段没有对外撤销端点，撤销记录需由后续阶段或运维写入。
- 挑战只持久化 nonce 的 SHA-256（沿用 0001 表结构），因此客户端必须回送 nonce；挑战不可跨重启重放，但节点重启后旧挑战仍会按过期时间被拒绝。
- 已登记设备更换凭据（撤销后重新授权）在 P1-03 明确拒绝（`INVALID_CREDENTIAL` / `DEVICE_REVOKED`），设备撤销与重授权流程属后续阶段。
- NODE-002 publish_discovery、NODE-003～NODE-016、API-005～API-010（含 `run_node` 与事件套接字）仍未实现；[apps/node](../../apps/node) 仍是明确以 78 退出的骨架，不能被当作可部署节点。

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

- [ ] 同 ID 同摘要重试返回原结果。
- [ ] 同 ID 换 owner 拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 两管理员竞争只一个前置状态成功。
- [ ] 超时重试得到同 seq。
- [ ] 同 ID 不同字节拒绝。

**实现状态**：正式函数未开始。独立 [`p0_accept`](../../crates/node-services/src/p0_accept.rs) 已通过同 ID 同密文重试、换密文拒绝、8 并发连续 seq、3 个进程终止断点与固定公钥的 P0 Ed25519 回执验证；尚无鉴权、生产密钥托管、正式签名编码和 HTTP，故上方正式验收框仍未勾选。证据见 [节点实验报告](../P0_NODE_ACCEPTANCE.md)。

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

- [ ] 新成员无法拉旧正文/附件。
- [ ] 已退出设备无权读新条目。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 替换 group/epoch 导致验签失败。
- [ ] 节点重启同持久头。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 开放匿名垃圾投递拒绝。
- [ ] 重复 item 不重占配额。
- [ ] 节点重启后仍可拉。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 跨设备游标拒绝。
- [ ] 重复请求相同材料可重读。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 重复 ACK 不把 quota 减为负。
- [ ] 他设备 ACK 拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 并发上传不能超总配额。
- [ ] 同 operation 重试同 session。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 同块重传不占双份。
- [ ] 写文件后 crash 启动可恢复/清理。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 缺末块拒绝。
- [ ] 重复完成不重占配额。
- [ ] 中途重启不发布半对象。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 新设备拿到旧 URL 仍拒绝。
- [ ] 过期/跨对象能力拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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


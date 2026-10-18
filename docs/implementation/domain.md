# 领域规则、群权限与状态机

阶段：P1 → P3。状态：DOMAIN-001~012 全部 12 个函数已实现并通过自动化用例。源码：[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)（占位保留）+ 模块 [model.rs](../../crates/domain/src/model.rs)、[group.rs](../../crates/domain/src/group.rs)、[control.rs](../../crates/domain/src/control.rs)、[delivery.rs](../../crates/domain/src/delivery.rs)、[visibility.rs](../../crates/domain/src/visibility.rs)、[error.rs](../../crates/domain/src/error.rs)。

本层为可独立验证的纯规则。GroupSnapshot 必须来自已验证正式日志；作者时间不能参与权限排序。私聊也采用同样的节点确认模型；会话类别另行限制成员治理入口。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [DOMAIN-001 validate_send_permission](domain.md#domain-001)
- [DOMAIN-002 authorize_group_operation](domain.md#domain-002)
- [DOMAIN-003 validate_capacity](domain.md#domain-003)
- [DOMAIN-004 apply_policy_operation](domain.md#domain-004)
- [DOMAIN-005 verify_control_result](domain.md#domain-005)
- [DOMAIN-006 validate_owner_exit](domain.md#domain-006)
- [DOMAIN-007 validate_ownership_transfer](domain.md#domain-007)
- [DOMAIN-008 transition_send_state](domain.md#domain-008)
- [DOMAIN-009 aggregate_delivery](domain.md#domain-009)
- [DOMAIN-010 classify_retry](domain.md#domain-010)
- [DOMAIN-011 validate_reply_target](domain.md#domain-011)
- [DOMAIN-012 enforce_join_visibility](domain.md#domain-012)

## P1-01 实现边界与语义

- **快照不变量**：所有入口先校验「群主唯一且与 owner 字段一致、群主为有效成员、账户唯一、单账户设备唯一、设备不跨账户共享、加入窗口合法」；违反返回 `STATE_CORRUPTED` 并停止该群分支，不推断授权。
- **基线限额**：`GroupLimits::BASELINE` = 20 账户 / 40 设备 / 每账户 2 设备。`authorize_group_operation` 的邀请按基线校验；节点若配置更低限额，必须在节点侧再调用 `validate_capacity`（该函数签名不含 limits）。
- **治理入口**：只有 `ConversationKind::Group` 允许群治理；`Direct`/`SelfSync` 只允许 `Leave`/`Dissolve`。管理员只能治理普通成员（不能踢/降级/禁言管理员或群主、不能改角色、不能邀请管理员）；任何人不能修改自己的角色，群主降级必须走转让；任命群主只能通过 `TransferOwnership`。
- **策略迁移（DOMAIN-004）**：`apply_policy_operation` 先要求 `op.snapshot_head/policy_version` 等于当前快照（否则 `STALE_CONTROL_HEAD`），再按当前快照重新授权同一操作并与传入的授权结果逐项比对（角色迁移/owner_after/操作必须一致，否则 `INVALID_POLICY_TRANSITION`），随后只应用该单一操作、校验唯一群主不变量，最后 `policy_version + 1` 并派生新控制头；MLS epoch 不在策略层推进。冻结派生公式：`policy_operation_digest = SHA-256(chat/p1/policy-operation/v1 || 规范编码{操作种类, 目标账户/角色/禁言/设备/名称摘要})`；`control_head_after = SHA-256(chat/p1/control-head/v1 || 规范编码{前一控制头, 新策略版本, 操作种类, 操作摘要})`。候选快照 `ProposedPolicy` 不是正式快照。
- **控制结果（DOMAIN-005）**：`verify_control_result` 要求 `StagedCommit` 的 group 与本地一致、`epoch_before == before.mls_epoch`（否则 `LOG_CONFLICT`）、`epoch_after > epoch_before`（否则 `INVALID_CONTROL_RESULT`，不猜测固定 +1）；公开 `ControlMetadata` 的操作种类与目标摘要必须等于已授权操作的 `policy_operation_digest`；随后重新授权并应用出预期业务结果，实际 `added/removed` 凭据叶子集合必须与预期完全一致（移除账户必须覆盖其全部设备，纯角色操作不得夹带成员变化），否则 `INVALID_CONTROL_RESULT`。
- **错误码**：领域错误复用 `chat_protocol::ErrorCode`（PERMISSION_DENIED / DEVICE_REVOKED / INVALID_TARGET / GROUP_LIMIT_EXCEEDED / DEVICE_LIMIT_EXCEEDED / OWNER_TRANSFER_REQUIRED / NOT_MEMBER / INVALID_STATE_TRANSITION / INVALID_POLICY_TRANSITION / INVALID_CONTROL_RESULT / INVALID_RECEIPT_SCOPE / INVALID_REPLY / MESSAGE_NOT_VISIBLE / HISTORY_NOT_AUTHORIZED / STALE_CONTROL_HEAD / LOG_CONFLICT / STATE_CORRUPTED）；重试性完全遵循冻结码表（只有 `STALE_CONTROL_HEAD` 这类「先同步再重试」可重试，权限/验证/损坏/迁移失败不可重试）；经 `chat_protocol::map_error` 输出时通过 `InternalError::Classified` 保留具体码。
- **发送状态表**：`QUEUED + VerifiedAcceptedReceipt -> ACCEPTED`；`ACCEPTED + AuthenticatedReceipt -> DELIVERED`；`VerifiedRejection(终止错误) -> FAILED`；transport/mailbox ACK、本机超时、可重试错误一律 `INVALID_STATE_TRANSITION`；`ACCEPTED/DELIVERED` 不得回退；重复证据幂等。
- **重试分类**：`AcceptanceUnknown`/`UNKNOWN_RESULT -> QueryExisting`（保留 SubmissionId，禁止重新加密）；网络/节点/限流/存储忙 -> `ResendSameBytes`；陈旧 head/epoch/缺日志/日志冲突 -> `SyncThenRebuild`；权限/禁言/设备撤销/配额/限额 -> `WaitForUser`（不自动重试）；签名/损坏/不支持 -> `PermanentFailure`。
- **送达聚合**：只统计消息时点的 `RecipientSnapshot`；按 (账户, 设备) 去重、按账户 OR 聚合、排除发件人账户；发件人自己其他设备的回执不计入对方；超出快照的回执显式 `INVALID_RECEIPT_SCOPE`。
- **可见性**：`enforce_join_visibility` 只返回「正文/附件内容」范围（与 `MembershipWindow` 的交集），控制链恢复材料是另一条范围，必须由调用方单独授权。

<a id="domain-001"></a>

## DOMAIN-001 · validate_send_permission

~~~text
validate_send_permission(snapshot: GroupSnapshot, actor: Actor, size: usize) -> Result<AuthorizedSend>
~~~

**职责与输出**：返回发言授权及当前 head/epoch/policy 上下文。

**输入与前置条件**：actor 的账户与设备关系已验证。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查群未解散。
2. 设备在成员集合。
3. 账户有效且未禁言。
4. 核对大小。
5. 生成授权上下文。

**持久化、并发与副作用**：纯函数；节点与客户端都调用，节点结果不替代客户端验证。

**失败处理**：PERMISSION_DENIED，DEVICE_REVOKED，CONTENT_TOO_LARGE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 禁言后旧草稿不能越权。
- [x] 本人已撤销设备不能发送。

**实现状态**：已实现（`group.rs::validate_send_permission`）。证据：`cargo test --offline -p chat-protocol -p chat-domain`（2026-10-18，chat-domain 28 passed）中的 `muted_or_revoked_or_foreign_device_cannot_send`（禁言/已撤销设备/借用他人设备/非成员/撤销账户）、`send_permission_returns_context_and_enforces_size`（64 KiB 边界）、`dissolved_or_paused_group_rejects_send`、`snapshot_with_two_owners_is_rejected_before_any_permission`。每次调用都重新读取快照，禁言后旧草稿同样失败。

<a id="domain-002"></a>

## DOMAIN-002 · authorize_group_operation

~~~text
authorize_group_operation(snapshot: GroupSnapshot, actor: Actor, op: GroupOperation) -> Result<AuthorizedGroupOperation>
~~~

**职责与输出**：单一合法操作，不接受顺便改变其他权限。

**输入与前置条件**：snapshot 含角色、设备、加入位置、当前 control_head。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 按操作分支校验角色。
2. 验证目标成员。
3. 限制管理员只能治理普通成员。
4. 阻止任命群主。
5. 检查容量和解散状态。

**持久化、并发与副作用**：纯函数；不得先更新本地正式成员。

**失败处理**：PERMISSION_DENIED，INVALID_TARGET，GROUP_LIMIT_EXCEEDED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-003 validate_capacity](domain.md#domain-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 管理员踢管理员/群主拒绝。
- [x] 成员给自己升权拒绝。

**实现状态**：已实现（`group.rs::authorize_group_operation`）。证据：`admin_cannot_govern_admin_or_owner`（管理员踢/禁言同级或群主均拒绝，治理普通成员通过）、`admin_cannot_change_roles_and_nobody_can_self_promote`（成员/管理员自改角色拒绝，群主自降拒绝，管理员改角色拒绝，任命群主拒绝，群主降级管理员允许并给出 before/after）、`invite_rules_and_capacity`（邀请管理员/群主/已存在账户/无设备/重复设备/他人设备/每账户 3 设备）、`capacity_limit_blocks_the_twenty_first_account`、`owner_removal_and_self_removal_are_rejected`、`direct_and_self_sync_conversations_have_no_governance`、`dissolve_requires_owner_and_owner_leave_requires_transfer`。

<a id="domain-003"></a>

## DOMAIN-003 · validate_capacity

~~~text
validate_capacity(accounts: usize, devices: usize, per_account: usize, limits: GroupLimits) -> Result<()>
~~~

**职责与输出**：验证 20 账户/40 设备/每账户2设备基线。

**输入与前置条件**：实际节点可能配置更低上限。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 安全加法计算变化后规模。
2. 检查各维度。
3. 拒绝整数溢出。
4. 输出明确超限类型。

**持久化、并发与副作用**：纯函数；客户端提示与节点权威检查并用。

**失败处理**：GROUP_LIMIT_EXCEEDED，DEVICE_LIMIT_EXCEEDED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 19→20 接受、20→21拒绝。
- [x] 重复设备不重复计数。

**实现状态**：已实现（`group.rs::validate_capacity` + `Capacity::of_members`）。证据：`capacity_baseline_boundaries`（19/20 接受、21 拒绝、40/41 设备、每账户 2/3、更低配置生效、usize::MAX 加法溢出显式拒绝）、`duplicate_devices_are_counted_once`（重复设备只计 1；同一成员内重复设备同时被快照不变量拒绝）。

<a id="domain-004"></a>

## DOMAIN-004 · apply_policy_operation

~~~text
apply_policy_operation(before: GroupSnapshot, op: AuthorizedGroupOperation) -> Result<ProposedPolicy>
~~~

**职责与输出**：产出候选快照和预期设备增删差集。

**输入与前置条件**：op 必须来自当前 before 的授权结果。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 匹配 head/policy。
2. 只应用该操作。
3. 计算唯一群主不变量。
4. 计算设备集合差异。
5. 递增 policy_version。

**持久化、并发与副作用**：纯函数；不是正式快照，节点回执与 MLS 比对后提交。

**失败处理**：STALE_CONTROL_HEAD，INVALID_POLICY_TRANSITION。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-002 authorize_group_operation](domain.md#domain-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 加人操作夹带管理员角色拒绝。
- [x] 移除账户覆盖其全部设备。

**实现状态**：已实现（`control.rs::apply_policy_operation`）。证据：`cargo test --offline -p chat-protocol -p chat-domain`（2026-10-18，chat-protocol 73 passed、chat-domain 43 passed）中的 `control_rules.rs::owner_authorized_admin_invite_is_allowed_but_smuggled_authorization_is_not`（群主邀请管理员通过；管理员/普通成员伪造「邀请管理员」授权 → 重新授权 `PERMISSION_DENIED`）、`invite_application_updates_snapshot_and_device_diff`（policy_version+1、epoch 不变、控制头更新、设备增删差集、加入序号 = verified_sequence+1）、`removal_covers_every_device_and_keeps_single_owner`（移除账户差集包含其全部设备、唯一群主保持）、`tampered_authorization_is_rejected`（篡改角色迁移/owner_after → `INVALID_POLICY_TRANSITION`）、`stale_head_or_policy_is_rejected_before_any_mutation`（`STALE_CONTROL_HEAD`）、`role_mute_rename_and_transfer_apply_exactly`（转让后唯一群主、原群主降级且不退出）、`leave_and_dissolve_apply_exactly`（群主伪造 Leave → `OWNER_TRANSFER_REQUIRED`；解散置 dissolved）、`frozen_policy_digest_and_control_head_vectors`（固定 hex 向量）。

<a id="domain-005"></a>

## DOMAIN-005 · verify_control_result

~~~text
verify_control_result(before: GroupSnapshot, metadata: ControlMetadata, mls: StagedCommit) -> Result<VerifiedControlResult>
~~~

**职责与输出**：确认公开业务变更与实际 MLS 设备成员变更相同。

**输入与前置条件**：metadata 外层签名合法；MLS Commit 已验证但未提交。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 重新校验原作者权限。
2. 生成预期业务快照。
3. 比对实际 added/removed 叶子和凭据。
4. 核对 epoch 变化。
5. 检查纯角色操作不夹带成员变化。

**持久化、并发与副作用**：纯函数；任一失败不提交 MLS/策略/游标。

**失败处理**：INVALID_CONTROL_RESULT，LOG_CONFLICT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-004 apply_policy_operation](domain.md#domain-004)。凭据集合须先经过共享纯验证器；domain 不访问账户数据库。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [x] 管理员 Invite 夹带 Remove Owner 拒绝。
- [x] 账户所有设备移除才通过。

**实现状态**：已实现（`control.rs::verify_control_result`）。证据：`control_rules.rs::invite_commit_matches_business_change`（epoch/head/操作摘要一致时通过，added 叶子与邀请设备一致）、`commit_cannot_smuggle_extra_member_changes`（Invite 的 Commit 夹带移除群主设备 → `INVALID_CONTROL_RESULT`；纯禁言操作夹带新增叶子同样拒绝；空变化通过）、`account_removal_requires_every_device`（只移除一台设备拒绝，移除账户全部设备通过）、`metadata_must_match_operation`（操作种类或目标摘要不符 → `INVALID_CONTROL_RESULT`）、`wrong_group_or_epoch_is_a_log_conflict`（跨群/epoch_before 不符 → `LOG_CONFLICT`；epoch 不前进 → `INVALID_CONTROL_RESULT`）、`stale_authorization_and_bad_actor_are_rejected`（陈旧 head → `STALE_CONTROL_HEAD`；设备撤销 → `DEVICE_REVOKED`）、`control_failures_map_to_frozen_codes_without_retry`（码表映射与重试性）。实现说明：`StagedCommit` 携带 MLS 的 `epoch_before/epoch_after`、已授权操作与实际 added/removed 凭据叶子（账户+设备）；本层只核对，不合并 MLS 状态，也不断言 epoch 固定 +1。

<a id="domain-006"></a>

## DOMAIN-006 · validate_owner_exit

~~~text
validate_owner_exit(snapshot: GroupSnapshot, actor: AccountId) -> Result<ExitMode>
~~~

**职责与输出**：返回成员可退出或群主须转让/解散。

**输入与前置条件**：群未解散，actor 是当前成员。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 查角色。
2. 普通成员返回可退出。
3. 群主返回需要明确转让或解散。
4. 空群情况不自动推断授权。

**持久化、并发与副作用**：纯函数。

**失败处理**：OWNER_TRANSFER_REQUIRED，NOT_MEMBER。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 群主不能因最后一人而隐式越过显式解散确认。

**实现状态**：已实现（`group.rs::validate_owner_exit`，返回 `ExitMode`）。证据：`owner_exit_requires_transfer_or_dissolve`（普通成员/管理员 → Leave；群主仍有人 → OwnerMustTransfer；群主是最后一人 → OwnerMustDissolve；非成员 → NOT_MEMBER；已解散 → 显式失败）。群主 `Leave` 在授权层返回 `OWNER_TRANSFER_REQUIRED`。

<a id="domain-007"></a>

## DOMAIN-007 · validate_ownership_transfer

~~~text
validate_ownership_transfer(snapshot: GroupSnapshot, actor: Actor, target: AccountId) -> Result<AuthorizedGroupOperation>
~~~

**职责与输出**：原群主降为普通成员、目标成为唯一群主的单一操作。

**输入与前置条件**：目标是当前活跃成员且至少有一台有效设备。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证 actor 是群主。
2. 阻止自转让。
3. 验证目标。
4. 生成明确 before/after 角色变化。
5. 确保结果唯一群主。

**持久化、并发与副作用**：纯函数；转让不自动让原群主退出。

**失败处理**：PERMISSION_DENIED，INVALID_TARGET。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-002 authorize_group_operation](domain.md#domain-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 转让给已移除账户拒绝。
- [x] 旧群主重试需重新校验权限。

**实现状态**：已实现（`group.rs::validate_ownership_transfer`）。证据：`ownership_transfer_is_single_explicit_operation`（owner_after=目标，两条明确 before/after 角色迁移）、`transfer_rejects_non_owner_self_removed_and_deviceless_targets`（非群主/自转让/已离开/无设备/被禁言但有效可接任）、`former_owner_retry_is_re_evaluated_against_new_snapshot`（新快照下旧群主重试 PERMISSION_DENIED，新群主可转让）。转让不让原群主自动退出。

<a id="domain-008"></a>

## DOMAIN-008 · transition_send_state

~~~text
transition_send_state(current: SendState, evidence: DeliveryEvidence) -> Result<SendState>
~~~

**职责与输出**：只依据证据推进 QUEUED/ACCEPTED/DELIVERED/FAILED。

**输入与前置条件**：正式接受需要经过验证的节点回执，送达需要端到端持久化回执。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验证据种类。
2. 排除 transport/mailbox ACK。
3. 按状态表推进。
4. 允许重复证据幂等。
5. 已接受后不被超时降为失败。

**持久化、并发与副作用**：纯函数；FAILED 的可重试原因另记不伪装新消息。

**失败处理**：INVALID_STATE_TRANSITION。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 信箱 ACK 不置 DELIVERED。
- [x] 先到送达证据仍须验证正式接受关联。

**实现状态**：已实现（`delivery.rs::transition_send_state`）。证据：`send_state_advances_only_on_verified_evidence`、`ack_and_timeout_evidence_cannot_advance_state`（TransportAck/MailboxStored/LocalTimeout 在 QUEUED 与 ACCEPTED 都显式拒绝；QUEUED + 送达证据拒绝）、`only_terminal_verified_rejection_fails_an_attempt`（只有终止错误可置 FAILED；可重试错误不能转正式失败；ACCEPTED 后不得拒绝；seq=0 畸形回执拒绝）。本机超时永不推断正式失败。

<a id="domain-009"></a>

## DOMAIN-009 · aggregate_delivery

~~~text
aggregate_delivery(message: MessageIdentity, receipts: List<VerifiedReceipt>, members: RecipientSnapshot) -> DeliverySummary
~~~

**职责与输出**：返回每设备/每账户明细和对方已送达账户数。

**输入与前置条件**：只统计该消息当时获准收件设备，排除本人账户。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 按 message/device 去重。
2. 校验接收权限时点。
3. 按账户 OR 聚合。
4. 排除发件人。
5. 计算计数和缺失列表。

**持久化、并发与副作用**：纯函数；不能仅取回执条数。

**失败处理**：INVALID_RECEIPT_SCOPE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 两台 B 设备只计1账户。
- [x] A 第二台不算对方。
- [x] 撤销后伪造回执拒绝。

**实现状态**：已实现（`delivery.rs::aggregate_delivery`）。证据：`delivery_aggregates_per_account_without_counting_self_devices`（两台 B 设备 → 1 账户/2 设备；重复回执去重；A 第二台设备回执被排除且不计入对方）、`delivery_rejects_out_of_scope_receipts`（非收件账户/不可见设备/其他消息/重复收件账户均 INVALID_RECEIPT_SCOPE）。

<a id="domain-010"></a>

## DOMAIN-010 · classify_retry

~~~text
classify_retry(error: CoreError, state: OperationState) -> RetryDecision
~~~

**职责与输出**：区分原字节重发、先同步、等待用户和永久失败。

**输入与前置条件**：已生成密文的尝试与草稿身份分离。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 网络错误退避。
2. 旧 head/epoch 转同步。
3. 签名/损坏停止。
4. 权限错误终止。
5. 结果未知先按原 ID 查询。

**持久化、并发与副作用**：纯函数；不实际 sleep 或加密。

**失败处理**：UNKNOWN_RESULT 必须保留同 SubmissionId。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 超时后节点已接受不重新加密。
- [x] 禁言不无限重试。

**实现状态**：已实现（`delivery.rs::classify_retry`）。证据：`retry_classification_keeps_submission_or_rebuilds`（网络 → ResendSameBytes 同 SubmissionId；AcceptanceUnknown/UNKNOWN_RESULT → QueryExisting 且不自动重试、不重新加密；陈旧 epoch/缺日志 → SyncThenRebuild 需先同步且换新 SubmissionId；已接受后网络错误 → PermanentFailure）、`muted_member_is_not_retried_forever`（PERMISSION_DENIED → WaitForUser，不自动重试；签名/状态损坏 → PermanentFailure）。

<a id="domain-011"></a>

## DOMAIN-011 · validate_reply_target

~~~text
validate_reply_target(conversation: ConversationId, target: MessageRef) -> Result<ReplyReference>
~~~

**职责与输出**：绑定合法同会话回复引用，允许正文已过期。

**输入与前置条件**：引用目标身份和会话须可信。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对 conversation。
2. 限制引用字段与大小。
3. 保留 ID。
4. 正文不可用输出占位引用。

**持久化、并发与副作用**：纯函数；不隐式下载全部历史。

**失败处理**：INVALID_REPLY，MESSAGE_NOT_VISIBLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 跨群消息引用拒绝。
- [x] 删除/过期内容可显示原消息不可用。

**实现状态**：已实现（`visibility.rs::validate_reply_target`）。证据：`reply_targets_must_share_conversation`（跨会话 INVALID_REPLY；过期/删除 → content_unavailable 占位；不可见 → MESSAGE_NOT_VISIBLE）。引用字段大小由强类型 ID 固定，不额外下载历史。

<a id="domain-012"></a>

## DOMAIN-012 · enforce_join_visibility

~~~text
enforce_join_visibility(member: MembershipWindow, requested: SequenceRange) -> Result<SequenceRange>
~~~

**职责与输出**：返回内容可见范围，控制链验证材料另有范围。

**输入与前置条件**：默认加入后历史，不能因成员身份开放所有旧附件。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 计算加入/退出窗口。
2. 限制内容序号。
3. 将恢复控制链与可见正文分开。
4. 对附件采用同一授权边界。

**持久化、并发与副作用**：纯函数；节点日志和 blob 下载复用。

**失败处理**：HISTORY_NOT_AUTHORIZED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 新设备只能读加入后消息。
- [ ] 合法恢复材料不包含旧正文密钥。（本层只返回正文/附件可见范围；控制链恢复材料与附件密钥由 crypto/attachments 层实现，本轮未实现，未勾选）

**实现状态**：已实现（`visibility.rs::enforce_join_visibility`）。证据：`join_visibility_hides_history_before_joining`（joined=11 时 1..100 → 11..100；1..10 → HISTORY_NOT_AUTHORIZED；边界 11..11 通过）、`left_members_do_not_see_later_content`（joined=5/left=9 → 5..8；9..100 拒绝；畸形窗口/请求显式失败）。附件复用同一序号授权边界，控制链恢复范围由调用方另行授权。


# 小群创建、邀请与治理用例

阶段：P3。状态：函数级设计；GROUPS-001 建群与 GROUPS-002 快照已实现（真实 OpenMLS + 正式加密库），成员治理的角色变更（`set_role`）与禁言（`set_muted`）也已实现（DOMAIN-002/004 策略层 + CRYPTO-005 控制提交）；移除成员（`remove_member`）与改名（`rename_group`）也已实现（同一条 DOMAIN-002/004 + CRYPTO-005 流程）；控制提交的**接受合并（`accept_pending_control`）与丢弃回滚（`discard_pending_control`）**也已接线（CRYPTO-007/008）；上述能力已通过 `GroupFfi` 暴露给 Kotlin（`group_create`/`snapshot`/`set_role`/`set_muted`/`remove_member`/`rename`/`accept_pending`/`discard_pending`）；对端条目处理（`group_process_peer_entry`，CRYPTO-006）与 Welcome 加入（`group_join_from_welcome`，CRYPTO-009）也已实现；其余治理函数未开始。目标源码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)。

本章对应 GroupService 的完整业务入口。所有管理操作都是待确认票据；UI 读取正式快照与 pending 列表分别显示。邀请加入分为业务接受、KeyPackage 获取、正式 Commit、Welcome 投递四步。重加入需要当前成员明确授权，不直接从节点读取群密钥。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [GROUPS-001 create_group](groups.md#groups-001)
- [GROUPS-002 group_snapshot](groups.md#groups-002)
- [GROUPS-003 propose_group_operation](groups.md#groups-003)
- [GROUPS-004 accept_group_invite](groups.md#groups-004)
- [GROUPS-005 decline_group_invite](groups.md#groups-005)
- [GROUPS-006 prepare_group_operation](groups.md#groups-006)
- [GROUPS-007 operation_status](groups.md#groups-007)
- [GROUPS-008 remove_account_devices](groups.md#groups-008)
- [GROUPS-009 rename_group](groups.md#groups-009)
- [GROUPS-010 transfer_ownership](groups.md#groups-010)
- [GROUPS-011 leave_group](groups.md#groups-011)
- [GROUPS-012 dissolve_group](groups.md#groups-012)
- [GN 包：控制提交经节点回执再接受](groups.md#group-node)

<a id="groups-001"></a>

## GROUPS-001 · create_group

~~~text
create_group(request: CreateGroupRequest) -> Result<GroupCreationTicket>
~~~

**职责与输出**：建立固定协调节点的新群和本地创建任务。

**输入与前置条件**：群名有界、账户解锁、节点已验证；首批成员仍须接受邀请。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 分配 GroupId/operationId。
2. 验证自己设备凭据。
3. 保存创建意图。
4. 构建初始 MLS。
5. 向节点幂等创建。
6. 验证回执并落正式快照。

**持久化、并发与副作用**：T1 意图→T2 MLS准备→远端创建→T3确认；不持 DB 网络 await。

**失败处理**：NODE_UNAVAILABLE，GROUP_LIMIT_EXCEEDED，OPERATION_ID_CONFLICT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CRYPTO-001 create_crypto_group](crypto.md#crypto-001)、[NODE-005 register_group](node.md#node-005)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 创建响应丢失用同 group/operation 复用。（未实现：本切片不联网、没有节点创建回执；幂等复用属 NODE-005 register_group 接入后的工作）
- [x] 初始只有自己获准设备：`groups_snapshot::create_group_then_snapshot_matches`（正式成员只有创建者与引导成员；其余成员只出现在 `pending_members` 与 pending Add 提交里）。

**实现状态**：部分实现（本地建群切片，Windows x64 实测）。`groups::create_group(device, bootstrap_peer, store, request)`：
名称/凭据/成员上限/自我加法/重复成员/策略版本在**任何 MLS 或存储写入之前**校验；用 P0 引导
（`MlsDevice::create_and_invite`）建立含创建者与首个成员的真实 MLS 群；其余成员用 CRYPTO-005
`prepare_control(AddMembers)` 生成**真实 Commit + Welcome**，pending 随群状态落库（epoch 不推进）；
群快照（名称/成员/角色/策略版本/创建时间）写入正式库；返回 `GroupView{group_id,name,epoch,policy_version,role,created_at_ms,mls_group_key,state_version,members,pending_members,pending}`。
证据：`cargo test --offline -p chat-core --test groups_snapshot`（6 passed）。限制：引导成员需要**同进程设备句柄**
（crypto 仅有双人引导路径，CRYPTO-001 create_crypto_group 未实现）；不联网、不建节点群（NODE-005 未接）；
群元数据暂存在 `mls_group_state` 的独立命名空间（见实现记录）。

<a id="groups-002"></a>

## GROUPS-002 · group_snapshot

~~~text
group_snapshot(group: GroupId) -> Result<GroupSnapshotView>
~~~

**职责与输出**：返回正式成员/角色/head/policy/epoch/seq及 pending 提示。

**输入与前置条件**：当前账户可见，库未锁定。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取已验证快照。
2. 关联 pending operations。
3. 附 coordinator/故障状态。
4. 剔除秘密和未确认成员。

**持久化、并发与副作用**：只读一致快照；pending 不混入正式 members。

**失败处理**：GROUP_NOT_FOUND，PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 节点离线仍返回最后正式状态：`groups_snapshot::snapshot_survives_reopen`（关闭库再重开同一库文件后，快照的名称/epoch/成员/pending/state_version 与建群结果完全一致；整个过程不联网）。
- [x] 移除任务尚未确认清晰可见：`groups_snapshot::create_group_then_snapshot_matches`（pending 成员只出现在 `pending_members`，正式 `members` 只含已确认成员；pending 投影给出 base/target epoch、added/removed 与 commit 摘要）。

**实现状态**：部分实现（本地快照切片，Windows x64 实测）。`groups::group_snapshot(store, group_id)`：
校验 PROTOCOL-001 群文本（否则 `INVALID_ID`）→ 读持久快照记录（不存在 → `INVALID_TARGET`）→
解码并自校验（版本/名称/成员上限/角色码/凭据去重，不一致 → `STATE_CORRUPTED`）→ 用
`MlsDevice::import_from_store` 恢复真实 MLS 状态取 **epoch 与 pending 的权威值**（恢复失败 → `STATE_CORRUPTED`）。
证据：`groups_snapshot::unknown_and_malformed_group_ids_are_rejected`、`corrupted_mls_state_is_reported_as_corrupted`、
`snapshot_survives_reopen`。限制：没有群列表 API（必须给出 group_id）；head/seq/coordinator/故障状态
需要节点同步（NODE-006/007）后才能填，本切片不臆造，故 `GroupView` 暂不含这些字段。

<a id="groups-003"></a>

## GROUPS-003 · propose_group_operation

~~~text
propose_group_operation(group: GroupId, expected: ControlHead, op: GroupOperation, operation: OperationId) -> Result<GroupOperationTicket>
~~~

**职责与输出**：持久化单一管理意图，不直接改变正式群。

**输入与前置条件**：调用方提供 UI 看到的 expected head；actor 取内核身份，不能从插件伪造。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 群串行检查 head。
2. 重新授权。
3. 查 operation 幂等。
4. 保存 pending。
5. 需要对方接受则发邀请。
6. 否则排队 prepare-control。

**持久化、并发与副作用**：T1 意图；之后 T2 pending Commit；T3 正式确认。

**失败处理**：STALE_CONTROL_HEAD，PERMISSION_DENIED，OPERATION_ID_CONFLICT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-002 authorize_group_operation](domain.md#domain-002)、[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 旧 UI 任免按最新角色重新校验：`groups_governance::non_owner_cannot_govern` / `changing_the_owner_or_self_is_rejected`（每次治理都从**持久快照**重建 DOMAIN-001 快照并重新授权，普通成员/自我改角色/改群主全部 `PERMISSION_DENIED`）。
- [ ] 同 ID 不同 op 拒绝。（未实现：本切片没有 operation id 的幂等票据，只有具体的 `set_role`/`set_muted` 直接入口）

**实现状态**：部分实现。**角色变更（`set_role`）与禁言（`set_muted`）已实现**，见文末“成员治理实现记录”：
DOMAIN-002 授权 + DOMAIN-004 计算 `policy_version`/`control_head`，角色变更再经 CRYPTO-005
`prepare_control(SelfUpdate)` 生成真实控制提交；通用“治理意图票据”（expected head CAS、operation id 幂等、
邀请投递）仍未实现。

<a id="groups-004"></a>

## GROUPS-004 · accept_group_invite

~~~text
accept_group_invite(invite: InviteId) -> Result<GroupJoinTicket>
~~~

**职责与输出**：持久记录接受并准备一次性 KeyPackage。

**输入与前置条件**：邀请签名/目标/期限合法，用户显式接受。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对 inviter 和群上下文。
2. 检查未拒绝/过期。
3. 生成包。
4. 排队接受响应。
5. 等待节点正式加入和 Welcome。

**持久化、并发与副作用**：T1 接受/包/响应任务同事务或经验证 T2；不提前显示已加入。

**失败处理**：INVITE_EXPIRED，INVITE_NOT_AUTHORIZED，GROUP_LIMIT_EXCEEDED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[CRYPTO-002 generate_key_package](crypto.md#crypto-002)、[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 接受后断网恢复。（未实现：没有 invitation ticket / 重试队列）
- [x] 重复接受同 ticket。（本切片的对偶形态已覆盖：同一设备重复**加入**由 crypto 的“群已初始化”显式拒绝，快照不变——`groups_peer::duplicate_join_is_rejected_and_snapshot_is_unchanged`）
- [x] 无 Welcome 不假称可解密。（`groups_peer::welcome_join_rejects_wrong_binding` / `join_rejects_declared_owner_outside_the_group`：绑定不符/群主不在成员里都显式拒绝且**不写快照**，绝不假装已加入）

**实现状态**：部分实现（**Welcome 加入侧**已实现）。`groups::group_join_from_welcome(device, store, name, welcome,
acceptance, local_credential, owner_credential)`：调 CRYPTO-009 `join_from_welcome` 校验外层绑定
（群 ID/epoch/成员集合）→ 真实成员与 epoch 写快照（名称由调用方给，成员/epoch 取真实 MLS 值，群主来自已验证
邀请元数据并核对确为成员）→ 同一群重复加入 `INVALID_STATE_TRANSITION`。证据：`cargo test --offline -p chat-core --test groups_peer`。
待实现：邀请票据（accept/decline）、KeyPackage 生成与投递、节点正式加入回执（SYNC 侧）。

<a id="groups-005"></a>

## GROUPS-005 · decline_group_invite

~~~text
decline_group_invite(invite: InviteId) -> Result<()>
~~~

**职责与输出**：拒绝邀请并发送幂等拒绝结果。

**输入与前置条件**：邀请归当前账户，尚未正式加入。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证状态。
2. 持久 declined。
3. 排队拒绝通知。
4. 清理未发布 KeyPackage 引用。

**持久化、并发与副作用**：短事务；已加入需走 leave，不能以 decline 移除。

**失败处理**：ALREADY_JOINED，INVITE_NOT_FOUND。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重复 decline 幂等。
- [ ] 接受与拒绝竞争只一个最终状态。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="groups-006"></a>

## GROUPS-006 · prepare_group_operation

~~~text
prepare_group_operation(ticket: OperationTicket) -> Result<PendingControl>
~~~

**职责与输出**：把已满足前置条件的意图转为签名控制候选。

**输入与前置条件**：当前群 head/epoch 已同步；新增成员已接受邀请。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 重新检查角色。
2. 验证成员 KeyPackage。
3. 计算 expected policy。
4. 生成 pending Commit。
5. 保存不可变候选/outbox。

**持久化、并发与副作用**：T2：pending MLS/业务差异/候选/任务一事务。

**失败处理**：STALE_EPOCH，PERMISSION_DENIED，KEY_PACKAGE_EXPIRED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-002 authorize_group_operation](domain.md#domain-002)、[CRYPTO-005 prepare_control](crypto.md#crypto-005)、[STORAGE-007 persist_prepared_attempt](storage.md#storage-007)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 接受邀请不保证管理员权限仍有效。
- [ ] 过期包先重新获取。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="groups-007"></a>

## GROUPS-007 · operation_status

~~~text
operation_status(ticket: OperationTicket) -> Result<GroupOperationStatus>
~~~

**职责与输出**：返回 AwaitingAcceptance/Preparing/Submitted/Accepted/Rejected/NeedsUser 等。

**输入与前置条件**：ticket 属于可见会话。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 查询意图/尝试/正式日志。
2. 附可重试原因。
3. 将跨设备撤销子任务展开。
4. 输出 pending 与最终结果。

**持久化、并发与副作用**：只读；网络发送成功不等于生效。

**失败处理**：OPERATION_NOT_FOUND，PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 旧 head 重建显示同业务意图新尝试。（未实现：没有 operation ticket 与 expected head CAS）
- [x] 节点离线永不 Accepted。（本轮**不会**把本地接受当成节点接受：`SendStepView`/`GroupView` 的状态来自持久快照，节点回执仍未接线；见“控制提交接受与合并实现记录”的语义说明）

**实现状态**：部分实现（状态投影侧）。`group_snapshot` 如实返回 `pending`（base/target epoch、added/removed、提交摘要）
与 `last_control_cleared`（最近一次被丢弃的提交：操作/摘要/原因码/说明/时间），**不把本地接受伪装成节点接受**；
`accept_pending_control` / `discard_pending_control` 负责把 pending 收口为合并或丢弃。待实现：通用 operation ticket、
按节点日志对账（NODE-006/007）、跨设备撤销子任务展开。

<a id="groups-008"></a>

## GROUPS-008 · remove_account_devices

~~~text
remove_account_devices(group: GroupId, target: AccountId, expected: ControlHead) -> Result<OperationTicket>
~~~

**职责与输出**：一次授权控制操作移除目标账户的全部当前设备。

**输入与前置条件**：调用方有移除普通成员权限；target 不得是群主或同级管理员。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取完整设备集合。
2. 授权 RemoveMember。
3. 冻结预期集合版本。
4. 构建全部叶子移除。
5. 节点确认后正式应用。

**持久化、并发与副作用**：T2/T3；设备集合并发变化导致冲突后重新构造。

**失败处理**：PERMISSION_DENIED，STALE_CONTROL_HEAD，INVALID_CONTROL_RESULT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[GROUPS-003 propose_group_operation](groups.md#groups-003)、[DOMAIN-005 verify_control_result](domain.md#domain-005)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 目标两台设备均失去后续解密。
- [ ] 不能只踢当前在线设备。

**实现状态**：未开始（设备撤销语义）。**注意**：本轮实现的 `remove_member` 是“移除整个成员（账户）”，
对应 GroupOperation::RemoveMember 的策略操作与 MLS RemoveMembers 提交，见文末“移除成员与改名实现记录”；
按设备撤销（一台设备离开但账户仍在）仍属本函数，未实现。

<a id="groups-009"></a>

## GROUPS-009 · rename_group

~~~text
rename_group(group: GroupId, name: String, expected: ControlHead) -> Result<OperationTicket>
~~~

**职责与输出**：加密新群名，公开控制只暴露授权所需摘要。

**输入与前置条件**：仅群主/管理员；新名 UTF-8 字节/显示长度有界。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证名字。
2. 创建 Rename 单操作。
3. 密文保存新名。
4. 按正式日志排序。
5. 确认后刷新标题。

**持久化、并发与副作用**：T1/T2/T3；明文名不写节点公开元数据。

**失败处理**：INVALID_GROUP_NAME，PERMISSION_DENIED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[GROUPS-003 propose_group_operation](groups.md#groups-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 节点存储搜索不到测试群名。（未接节点：本轮不联网，群名只存在本地正式库的密封快照记录里，未写入任何节点公开元数据）
- [ ] 未确认 UI 保留旧正式名。（未实现：还没有“未确认”态；本轮改名一经 DOMAIN-004 生效即刷新本地快照）

**实现状态**：部分实现（本地策略层切片）。`groups::rename_group(store, group_id, actor_credential, new_name)`：
名称 1..=64 字节/无控制字符/首尾无空白；群主或管理员（DOMAIN-002）；**同名幂等无操作**；
未决控制提交期间显式拒绝；否则 DOMAIN-004 计算新 `policy_version`/`control_head` 并把新名写回密封快照。
证据：`cargo test --offline -p chat-core --test groups_membership`（rename_group_succeeds_and_same_name_is_idempotent /
invalid_names_and_non_governor_renames_are_rejected / membership_snapshot_survives_reopen）。
限制：不联网（不写节点公开元数据、没有 operation 票据与 expected head CAS）、新名不做端到端加密（本地库密封，文档记录）；
改名与成员治理共用“一次只有一个治理操作在飞”的约束。

<a id="groups-010"></a>

## GROUPS-010 · transfer_ownership

~~~text
transfer_ownership(group: GroupId, target: AccountId, expected: ControlHead) -> Result<OperationTicket>
~~~

**职责与输出**：生成明确唯一群主转让操作。

**输入与前置条件**：仅当前群主，target 为活跃成员。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证转让条件。
2. 构造单一 TransferOwnership。
3. 保留原群主为普通成员。
4. 提交确认。

**持久化、并发与副作用**：与一般控制同 T1/T2/T3；之后退出是独立操作。

**失败处理**：OWNER_TRANSFER_REQUIRED，PERMISSION_DENIED，INVALID_TARGET。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-007 validate_ownership_transfer](domain.md#domain-007)、[GROUPS-003 propose_group_operation](groups.md#groups-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 并发踢目标导致重校验。
- [ ] 正式快照始终一个 owner。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="groups-011"></a>

## GROUPS-011 · leave_group

~~~text
leave_group(group: GroupId, expected: ControlHead) -> Result<OperationTicket>
~~~

**职责与输出**：普通成员自退出并移除自己的全部设备。

**输入与前置条件**：群主必须先转让或显式解散。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查 actor。
2. 验证退出模式。
3. 构造 self-remove。
4. 保留本地旧历史受保护记录。
5. 确认后停止新接收授权。

**持久化、并发与副作用**：T2/T3；离线退出仍为 pending，不能假称所有人已撤销。

**失败处理**：OWNER_TRANSFER_REQUIRED，STALE_CONTROL_HEAD。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-006 validate_owner_exit](domain.md#domain-006)、[GROUPS-003 propose_group_operation](groups.md#groups-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 群主直接退出拒绝。
- [ ] 退出后不能获取新 epoch 内容。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="groups-012"></a>

## GROUPS-012 · dissolve_group

~~~text
dissolve_group(group: GroupId, expected: ControlHead, confirmation: UserConfirmation) -> Result<OperationTicket>
~~~

**职责与输出**：正式解散后拒绝新应用/控制事件，保留验证链和本地历史。

**输入与前置条件**：仅群主；显式确认绑定当前群。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验确认与权限。
2. 构造 Dissolve。
3. 正式日志确认。
4. 关闭投递能力与活跃群任务。
5. 保留必要控制/审计材料。

**持久化、并发与副作用**：T3 应用解散与本地任务取消一起提交。

**失败处理**：PERMISSION_DENIED，GROUP_DISSOLVED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[GROUPS-003 propose_group_operation](groups.md#groups-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 最后一条解散后消息被节点/客户端拒绝。
- [ ] 解散不声称销毁他人旧明文。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

## 建群与快照实现记录（GROUPS-001/002，2026-10-18，Windows x64）

代码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)（本切片只改这一个源码文件）；测试：
[crates/core/tests/groups_snapshot.rs](../../crates/core/tests/groups_snapshot.rs)（6 个用例，真实 OpenMLS + 临时正式加密库，不联网）。

### 公开面

~~~text
pub fn create_group(
    device: &mut MlsDevice,                 // 创建者（本机）真实 MLS 设备
    bootstrap_peer: &GroupBootstrapPeer,    // 首个成员：credential + 同进程设备句柄
    store: &mut Store,                      // 正式加密库（群状态与快照都落这里）
    request: &CreateGroupRequest,           // name / local_credential / policy_version / members
) -> GroupResult<GroupView>

pub fn group_snapshot(store: &Store, group_id: &str) -> GroupResult<GroupView>
~~~

- `CreateGroupRequest { name, local_credential, policy_version, members: Vec<GroupMemberKeyPackage> }`；
- `GroupMemberKeyPackage { credential, key_package }`（成员的真实 KeyPackage 字节）；
- `GroupView { group_id, name, epoch, policy_version, role, created_at_ms, mls_group_key, state_version, members, pending_members, pending }`；
  成员条目 `GroupMemberView { credential, role, is_local, pending }`；pending 条目
  `GroupPendingView { operation, base_epoch, target_epoch, added, removed, commit_digest }`；
- 常量：`MAX_GROUP_NAME_BYTES = 64`、`MAX_GROUP_MEMBERS = 32`（含创建者）、`GROUP_POLICY_VERSION = 1`。

### 关键语义

1. **写入前校验**（任何失败都不写 MLS/库）：名称 1..=64 字节、无控制字符、首尾无空白；凭据 1..=128 字节；
   成员总数 ≤ 32（否则 `GROUP_LIMIT_EXCEEDED`）；本地凭据不得出现在成员列表（“自己加自己” → `INVALID_INPUT`）；
   成员凭据与 KeyPackage 不得重复；KeyPackage 不得为空；策略版本必须是当前值（否则 `INVALID_POLICY_TRANSITION`）。
2. **真实 MLS 建群**：`create_and_invite` 建立含创建者与引导成员的真实群（本地合并引导提交 → epoch 1）；
   其余成员走 CRYPTO-005 `prepare_control(AddMembers)` 生成真实 **Commit + Welcome** 并写 pending
   （epoch 仍为 1，`target_epoch = 2`）。MLS 级再校验（自己加自己/已是成员/重复 KeyPackage）由 crypto 完成，错误码透传。
3. **快照持久化**：正式库目前没有群元数据表（只有 MLS 状态族），因此群快照以**独立 storage group_id 命名空间**
   `grpmeta/<sha256(协议群 ID)>` 写入 `mls_group_state` 的 `state` 槽位（storage 负责密封、版本与 epoch 单调校验）；
   MLS 自己的群状态仍按 MLS group id 存，两者互不覆盖。读取时 epoch 与 pending 以 **MLS 真实状态**为准。
4. **pending 不混入正式成员**：`members` 只含已确认成员（创建者 Owner + 引导成员 Member）；追加成员在
   `pending_members`（`pending = true`）与 `pending` 投影里。

### 错误码（全部既有冻结码，不新造协议码）

| 情形 | code |
|---|---|
| 非法名称 / 重复成员 / 自己加自己 | `INVALID_INPUT` |
| 非法空凭据 | `INVALID_CREDENTIAL` |
| 空 KeyPackage / 引导失败 | `INVALID_CANDIDATE` |
| 成员数超限 | `GROUP_LIMIT_EXCEEDED` |
| 非当前策略版本 | `INVALID_POLICY_TRANSITION` |
| 群 ID 不是 PROTOCOL-001 文本 | `INVALID_ID` |
| 未知群（无持久快照） | `INVALID_TARGET` |
| 快照/MLS 状态损坏或不一致 | `STATE_CORRUPTED` |
| crypto 控制提交拒绝 | crypto 冻结码（`INVALID_KEY_PACKAGE` / `INVALID_STATE_TRANSITION` / …） |
| storage 失败 | storage 冻结码（`STORAGE_*` / `STALE_EPOCH` / …） |

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test groups_snapshot   # 6 passed
cargo test --offline -p chat-core                          # 87 passed（既有 81 继续通过）
~~~

### 证据（测试名 → 覆盖点）

- `groups_snapshot::create_group_then_snapshot_matches`：建群 → epoch 1、role Owner、协议群 ID 与 MLS 群键派生一致、
  正式成员 2（alice owner / bob member）、pending_members 2（carol/dave）、pending 是真实 add_members 提交
  （base 1→target 2、added 2、commit 与 welcome 都存在）；`group_snapshot` 与建群结果逐字段一致。
- `groups_snapshot::snapshot_survives_reopen`：关闭库 → 重开同一库文件 → 名称/epoch/成员/pending/state_version/role 全一致。
- `groups_snapshot::invalid_requests_are_rejected_before_any_write`：5 种非法名称、重复凭据、自己加自己、空 KeyPackage、
  非当前策略版本、成员超限全部显式拒绝（逐条断言码），设备此后仍无群（`group_key()` 报错），随后一次合法建群成功。
- `groups_snapshot::mls_level_rejection_leaves_no_pending_and_no_snapshot`：本机 KeyPackage 混进追加成员 → crypto 拒绝
  （`INVALID_STATE_TRANSITION`）；引导已发生但没有 pending、没有快照记录（用派生群 ID 查快照 → `INVALID_TARGET`）。
- `groups_snapshot::unknown_and_malformed_group_ids_are_rejected`：未知群 `INVALID_TARGET`；4 种畸形群 ID `INVALID_ID`。
- `groups_snapshot::corrupted_mls_state_is_reported_as_corrupted`：把 MLS 群状态覆盖成非法字节后，快照读取 → `STATE_CORRUPTED`。

### 已知限制

1. **引导成员需要同进程设备句柄**：crypto 目前只有 `create_and_invite(key_package, peer_device)` 能建立群
   （CRYPTO-001 `create_crypto_group` 未实现）。要支持“任意 KeyPackage 成员”的建群，需要 crypto 提供自建空群 API。
2. **不联网**：本切片不创建节点群（NODE-005 register_group）、不投递 Commit/Welcome；创建回执幂等复用未实现。
3. **群元数据的临时载体**：快照存在 `mls_group_state` 的独立命名空间里；storage 提供正式 groups 表与“列出本机群”后应迁移。
4. **GroupView 不含 head/seq/coordinator/故障状态**：这些字段要等节点同步（NODE-006/007）接入后才有真实来源，本切片不臆造。
5. **治理范围**：角色变更（Owner/Admin/Member）与禁言已实现（见“成员治理实现记录”）；移除成员、改名、转让群主、退出、解散属 GROUPS-005~012（未开始）。
6. **成员身份是调用方声明的凭据文本**：MLS 侧真实凭据校验在 `prepare_control` 内部完成；本层不做跨账户目录核对。

## 成员治理实现记录（角色变更 set_role / 禁言 set_muted，2026-10-18，Windows x64）

代码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)（同一文件，无新依赖）；测试：
[crates/core/tests/groups_governance.rs](../../crates/core/tests/groups_governance.rs)（7 个用例，真实 OpenMLS + 临时正式加密库）。

> 编号说明：本轮任务的 GROUPS-003/004 指“角色变更 / 禁言”，对应本文档 GROUPS-003（治理意图）里的
> 角色/禁言子集；本文档 GROUPS-004 `accept_group_invite`（邀请加入）仍**未实现**。

### 公开面

~~~text
pub fn set_role(
    device: &mut MlsDevice,      // 本机真实 MLS 设备（生成控制提交）
    store: &mut Store,
    group_id: &str,              // PROTOCOL-001 grp:1:<hex>
    actor_credential: &str,      // 操作者（快照里的凭据身份）
    target_credential: &str,     // 目标成员（必须是已确认成员）
    role: MemberRole,            // Admin / Member（Owner 只能走转让）
) -> GroupResult<GroupView>

pub fn set_muted(
    store: &mut Store,
    group_id: &str,
    actor_credential: &str,
    target_credential: &str,
    muted: bool,
) -> GroupResult<GroupView>
~~~

`GroupView` 新增 `control_head`（小写 hex，下一次治理的 CAS 前置值）与 `policy_version: u64`；
`GroupMemberView` 新增 `muted**`——两者都来自**持久快照**，不是调用方自报。

### 关键语义

1. **顺序 = 先判权限、再算策略、最后才写**：读持久快照 → 构造 DOMAIN-001 正式快照（不变量校验）→
   DOMAIN-002 `authorize_group_operation` 判权限 → DOMAIN-004 `apply_policy_operation` 计算
   新的 `policy_version` 与 `control_head`（**不本地臆造**）→ （角色变更）CRYPTO-005
   `prepare_control(SelfUpdate)` 生成真实 MLS 控制提交并写 pending → 快照写回。任何一步失败都发生在
   写入之前（例外：`prepare_control` 已经成功后快照写库失败会留下未匹配的 pending，函数返回
   `STORAGE_*`，重试即可补齐——已文档化）。
2. **权限**（DOMAIN-002，不重复实现）：改角色**只有群主**；不能改群主角色（只能转让）；不能改自己；
   禁言需要群主或管理员、不能禁言自己；目标必须是已确认成员（pending 成员不可治理）。
3. **角色白名单**：`role` 只接受 `Admin`/`Member`；传 `Owner` → `PERMISSION_DENIED`。
4. **控制提交**：角色变更必须携带真实 MLS 提交（`SelfUpdate`，epoch 只在下一次接受合并时推进），
   因此快照里 `epoch` 不变、`pending` 非空；禁言是**纯策略层**操作（无 MLS 提交、无 pending），
   但有未决控制提交时**显式拒绝**（避免策略头与提交顺序不一致）。
5. **账户/设备标识**：正式快照需要的 `AccountId`/`DeviceId` 由成员凭据**确定性派生**
   （`sha256("chat/groups/account-id/v1" || credential)` 与 `.../device-id/v1`），同一凭据恒得同一标识；
   协调节点标识同样由 MLS 群键派生占位。**这不是真实账户 ID**，接入 IDENTITY 后应替换。

### 错误码（全部既有冻结码）

| 情形 | code |
|---|---|
| 非群主改角色 / 改群主 / 改自己 / 非管理员禁言 / 禁言自己 | `PERMISSION_DENIED` |
| 目标不是已确认成员 / 操作者不在群 / 群 ID 不属于本机设备 | `INVALID_TARGET` |
| 群 ID 不是 PROTOCOL-001 文本 | `INVALID_ID` |
| 未决控制提交期间的策略写入 / 第二个控制提交 | `INVALID_STATE_TRANSITION` |
| 本机 MLS epoch 与持久快照不一致 | `STALE_EPOCH` |
| 快照损坏/不变量被破坏 | `STATE_CORRUPTED` |
| 存储失败 | storage 冻结码 |

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test groups_governance   # 7 passed
cargo test --offline -p chat-core                            # 97 passed（既有 87 全部继续通过；lib 内另有 3 个并行任务新增用例）
~~~

### 证据（测试名 → 覆盖点）

- `groups_governance::owner_changes_a_role_and_policy_version_advances`：群主把 bob 改成 Admin →
  `policy_version 1→2`、`control_head` 由全零变成派生的 64 位 hex、`epoch` 仍为 1、
  `pending.operation == "self_update"`（base 1→target 2），设备侧 `pending_control()` 也是 self_update；
  快照与返回值一致。
- `groups_governance::non_owner_cannot_govern`：普通成员改角色/禁言 → `PERMISSION_DENIED`，且
  `policy_version`/`control_head`/pending 全部不变（拒绝不留写入）。
- `groups_governance::changing_the_owner_or_self_is_rejected`：群主改自己、把别人升为 Owner、禁言自己 →
  `PERMISSION_DENIED`；快照完全不变。
- `groups_governance::mute_roundtrip_updates_the_policy_snapshot`：禁言/解除禁言往返（`policy_version 2→3`，
  control_head 每次都变化，无 MLS pending），正式库快照与返回值一致。
- `groups_governance::unknown_members_and_illegal_transitions_are_rejected`：未知目标/未知操作者 `INVALID_TARGET`、
  畸形群 ID `INVALID_ID`，且不留写入。
- `groups_governance::pending_control_commit_blocks_further_governance`：角色变更后（pending self_update），
  禁言被拒（`INVALID_STATE_TRANSITION`）、第二个角色变更被 crypto 拒绝；第一次的结果保持一致。
- `groups_governance::governance_snapshot_survives_reopen`：禁言往返 + 改角色后**关闭库重新打开**，
  `policy_version/control_head/成员角色/禁言态/pending/epoch` 全部一致。

### 已知限制

1. **控制提交未接受**：`merge_accepted_pending` / 节点回执未接线，本轮把“策略快照已写入”视为本地生效，
   pending 如实报告；接节点后需要“接受→合并→再写快照”的顺序。
2. **没有 expected head 入参**：DOMAIN-004 会派生新头，但调用方还不能传“我看到的头”做 CAS；
   并发 UI/多设备场景需要补 `expected_control_head` 入参（STALE_CONTROL_HEAD 已经在域层就绪）。
3. **账户/设备标识是派生占位**（见上文第 5 条），接入真实账户目录后要替换；成员仍以凭据文本定位。
4. **有未决控制提交时策略层整体冻结**：一次只有一个治理操作在飞；批量治理需要先接接受/合并。
5. 待实现：转让群主、退出、解散、按设备撤销（GROUPS-008/010/011/012）；移除成员与改名已在下一节实现。

## 移除成员与改名实现记录（remove_member / rename_group，2026-10-18，Windows x64）

代码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)（同一文件，无新依赖）；测试：
[crates/core/tests/groups_membership.rs](../../crates/core/tests/groups_membership.rs)（7 个用例，真实 OpenMLS + 临时正式加密库）。

> 编号说明：本轮任务的 GROUPS-005/006 指“移除成员 / 改名”；本文档 GROUPS-005 是 decline_group_invite、
> GROUPS-008 是 remove_account_devices（按设备撤销）、GROUPS-009 是 rename_group。
> `remove_member` 对应 GroupOperation::RemoveMember（策略层移除**整个成员**），`rename_group` 对应本文档 GROUPS-009。

### 公开面

~~~text
pub fn remove_member(
    device: &mut MlsDevice,      // 本机真实 MLS 设备（生成移除提交）
    store: &mut Store,
    group_id: &str,              // PROTOCOL-001 grp:1:<hex>
    actor_credential: &str,      // 操作者（群主或管理员）
    target_credential: &str,     // 被移除的确认成员
) -> GroupResult<GroupView>

pub fn rename_group(
    store: &mut Store,
    group_id: &str,
    actor_credential: &str,      // 操作者（群主或管理员）
    new_name: &str,              // 1..=64 字节、无控制字符、首尾无空白
) -> GroupResult<GroupView>
~~~

### 关键语义

1. **顺序 = 先判权限、再算策略、最后才写**（与 set_role 一致）：读持久快照 → DOMAIN-001 快照 →
   本机 MLS 群键/epoch 一致（`INVALID_TARGET`/`STALE_EPOCH`）→ DOMAIN-002 授权 →
   DOMAIN-004 计算 `policy_version`/`control_head` → （移除）CRYPTO-005
   `prepare_control(RemoveMembers)` 生成真实 MLS 移除提交并写 pending → 快照写回。
2. **移除权限**（DOMAIN-002）：只有群主或管理员；不能移除自己（必须走 Leave）；不能移除群主（只能转让或解散）；
   管理员只能治理普通成员；目标必须是**确认成员**（pending 成员不参与策略层）。
3. **改名**：群主或管理员；名称校验在建群同一套规则上（1..=64、无控制字符、首尾无空白）；
   **同名幂等无操作**（不写库、不推进 `policy_version`、不改变记录版本）；
   改名是纯策略层操作（不产生 MLS 成员变化提交），但有未决控制提交时显式拒绝。
4. **移除是真实 MLS 提交**：`RemoveMembers` 按成员凭据身份定位（crypto 内部再校验唯一匹配、不能移除自己），
   epoch 只在下一次接受合并时推进，因此快照 `epoch` 不变、`pending.operation == "remove_members"`、
   `pending.removed == 1`。
5. **快照反映结果**：被移除者从 `members` 消失（候选策略里没有的确认成员即视为已移除），
   改名后 `name` 立即是新值；pending 成员与 pending 提交原样保留。

### 错误码（全部既有冻结码）

| 情形 | code |
|---|---|
| 非群主/管理员移除或改名 | `PERMISSION_DENIED` |
| 移除自己 / 移除群主 | `PERMISSION_DENIED` |
| 目标不是确认成员 / 未知群 / 本机不在该群 | `INVALID_TARGET` |
| 非法群名（空/超长/控制字符/首尾空白） | `INVALID_INPUT` |
| 群 ID 不是 PROTOCOL-001 文本 | `INVALID_ID` |
| 未决控制提交期间的策略写入 | `INVALID_STATE_TRANSITION` |
| 第二个控制提交（crypto PendingCommitExists） | `INVALID_STATE_TRANSITION` |
| 本机 MLS epoch 与快照不一致 | `STALE_EPOCH` |
| 快照/不变量损坏 | `STATE_CORRUPTED` |

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test groups_membership   # 7 passed
cargo test --offline -p chat-core                            # 109 passed（既有 97 全部继续通过；lib 内另有并行任务的附件用例，共 34 个 lib 用例）
~~~

### 证据（测试名 → 覆盖点）

- `groups_membership::remove_member_succeeds_and_policy_version_advances`：群主移除 bob →
  `policy_version 1→2`、`control_head` 变化、`members` 只剩 alice、`epoch` 仍 1、
  `pending.operation == "remove_members"` / `removed == 1` / base 1→target 2；设备侧 `pending_control()`
  同为 remove_members；正式库快照与返回值一致。
- `groups_membership::non_governor_cannot_remove_members`：普通成员移除别人 → `PERMISSION_DENIED`，
  且 `policy_version`/`control_head`/pending/成员全部不变。
- `groups_membership::cannot_remove_owner_self_or_unknown_member`：未知成员 `INVALID_TARGET`、移除自己
  `PERMISSION_DENIED`；把 bob 升为 Admin 后 bob 仍不能移除群主（`PERMISSION_DENIED`，**授权先于 MLS 提交**）。
- `groups_membership::rename_group_succeeds_and_same_name_is_idempotent`：改名成功（`policy_version 2`、
  control_head 变化、无 pending、epoch 不变）；同名再次调用幂等（`policy_version` 与 `state_version` 都不变）。
- `groups_membership::invalid_names_and_non_governor_renames_are_rejected`：5 种非法名 `INVALID_INPUT`、
  普通成员改名 `PERMISSION_DENIED`、未知群 `INVALID_TARGET`，且全部没有写入（名称/版本/头都不变）。
- `groups_membership::membership_snapshot_survives_reopen`：改名 + 移除后**关闭库重开**：
  名称/`policy_version 3`/control_head/成员只剩 alice/pending remove_members/epoch 全部一致。
- `groups_membership::pending_control_commit_blocks_further_governance`：移除后（pending remove_members）
  改名被拒 `INVALID_STATE_TRANSITION`、再次移除已被移除的成员 `INVALID_TARGET`；第一次结果保持不变。

### 已知限制

1. **控制提交的收口已接线（见下一节）**：`accept_pending_control` / `discard_pending_control` 现在能把 pending
   合并或丢弃；节点回执仍未接线（“本地接受证据 ≠ 节点回执”）。
2. **一次只有一个治理操作在飞**：有未决提交时策略层与第二个 MLS 提交都显式拒绝；接受/合并或丢弃之后才能继续。
3. **只能移除确认成员**：建群时追加的成员在被协调者接受前是 pending，不能被 `remove_member` 处理。
4. **账户/设备标识仍是派生占位**（见上一节第 5 条）；移除按 MLS 凭据文本定位，账户目录接入后要改用真实 ID。
5. **不联网**：不改节点群日志、不投递 Commit/Welcome；改名也不写节点公开元数据（名字只在本地密封快照里）。
6. 待实现：按设备撤销（GROUPS-008）、转让群主（010）、退出（011）、解散（012）。

## 控制提交接受与合并实现记录（accept_pending_control / discard_pending_control，2026-10-18，Windows x64）

代码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)（同一文件，无新依赖）；测试：
[crates/core/tests/groups_commit.rs](../../crates/core/tests/groups_commit.rs)（7 个用例，真实 OpenMLS + 临时正式加密库）。

### 公开面

~~~text
pub fn accept_pending_control(device: &mut MlsDevice, store: &mut Store, group_id: &str) -> GroupResult<GroupView>
pub fn discard_pending_control(store: &mut Store, group_id: &str, reason: &str) -> GroupResult<GroupView>
pub fn load_group_device(store: &Store, group_id: &str) -> GroupResult<MlsDevice>   // 丢弃后重新装载设备
~~~

`GroupView` 新增 `pending_removed`（本地已移除但尚未合并的成员）与 `last_control_cleared`
（最近一次被丢弃提交的审计投影：操作/base-target epoch/提交摘要/原因码/说明/时间）。

### 关键语义

1. **接受证据只来自正式库，不臆造**：`accept_pending_control` 先从库恢复一份设备、取出持久 pending 记录
   （`PendingControl`：group_id/base_epoch/target_epoch/commit_digest），并要求调用方设备持有**同一个**提交
   （不一致 → `STATE_CORRUPTED`；任一侧没有 pending → `PENDING_MISMATCH`）；之后才调用 CRYPTO-007
   `merge_accepted_pending(&acceptance, store)`。
2. **合并后的真实值**：epoch 前进到 `target_epoch`、pending 清空；`add_members` 的 pending 成员此刻
   **正式入群**，`remove_members` 的待移除成员被彻底删除；`policy_version` 不变（接受是对既有策略记录的确认）。
3. **丢弃 = 回滚到上一完整状态**：拒绝证据同样来自持久 pending 记录；epoch **不变**、pending 清空、
   `policy_version 不回退`（本地策略记录保留），并写入 `last_control_cleared`；`add_members` 的 pending
   成员直接丢弃（从未入群），`remove_members` 的待移除成员**恢复**为正式成员。
4. **失败不留半状态**：没有 pending / 设备不属于该群 / 证据不匹配都在写入前被拒；merge 失败保持 pending 与旧
   epoch（测试断言 `GroupView` 与失败前完全相等）。
5. **丢弃后需要重新装载设备**：`discard_pending_control` 按任务签名只拿 store（权威侧是库），调用方手里的
   内存设备仍持有旧 pending；继续治理前必须用 `load_group_device` 重新装载（否则 crypto 以
   `INVALID_STATE_TRANSITION` 明确拒绝，不会静默错误推进）。

### 语义边界（重要）

**本地接受证据 ≠ 节点回执**：本函数把“本地持久 pending 记录”当作接受证据，用于把本地状态收口
（epoch/成员/pending 一致）。它**不证明**节点已接受该 Commit，也没有按节点日志对账。
真实部署必须先拿到节点回执，再走 [group_node::accept_pending_control_with_receipt](../../crates/core/src/group_node.rs)
（GN 包，见下文“控制提交经节点回执再接受”）：那里会先校验回执绑定的 group/submission/kind/密文摘要/seq/log_hash/节点签名，
通过后才调用本函数；否则本地 epoch 可能与节点日志不一致。“节点离线永不 Accepted”的本轮做到的是“本地不伪造节点状态”，
GN 包进一步做到“没有节点回执就不推进 epoch”。

### 错误码（既有冻结码）

| 情形 | code |
|---|---|
| 没有 pending（接受/丢弃） | `PENDING_MISMATCH` |
| 调用方设备与库里的 pending 不一致 | `STATE_CORRUPTED` |
| 调用方设备不属于该群 / 未知群 | `INVALID_TARGET` |
| 群 ID 不是 PROTOCOL-001 文本 | `INVALID_ID` |
| 丢弃原因非法（空/超长/控制字符） | `INVALID_INPUT` |
| 合并/丢弃后 epoch 不符（crypto 检测） | `STALE_EPOCH` / `STATE_CORRUPTED` |
| 持久化失败 | storage 冻结码（`STORAGE_*`） |

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test groups_commit   # 7 passed
cargo test --offline -p chat-core                        # 128 passed（本记录写下时为 118；其后 lib 侧另有并行任务新增用例，GN 包新增 5 + 2）
~~~

### 证据（测试名 → 覆盖点）

- `groups_commit::accept_merges_the_commit_and_advances_the_epoch`：改角色（pending self_update，base 1→target 2）→
  接受后 **epoch 2**、pending 清空、`policy_version` 仍 2、角色保留、`state_version` 增长；设备侧 epoch 2 且无
  pending；快照与返回值一致；**第二次接受 → `PENDING_MISMATCH` 且状态不变**；之后可继续治理（新提交 base_epoch = 2）。
- `groups_commit::accept_without_pending_is_rejected_and_leaves_state`：没有 pending → `PENDING_MISMATCH`；
  外来设备 → `INVALID_TARGET`；两次失败后 `GroupView` 与失败前完全相等。
- `groups_commit::accept_promotes_pending_members_after_an_add_commit`：建群时追加 carol（pending add_members）→
  接受后 carol 进入正式成员、pending 列表清空、epoch 2。
- `groups_commit::discard_rolls_back_without_advancing_the_epoch`：移除 bob（pending remove_members）→ 丢弃后
  **epoch 仍 1**、pending 清空、`policy_version` 保持 2、`last_control_cleared` 记录
  remove_members/node_rejected/base1/target2/64 位摘要/说明，**bob 恢复为正式成员**；用 `load_group_device`
  重新装载后再移除并接受 → epoch 2、bob 彻底消失。
- `groups_commit::discard_of_an_add_commit_drops_the_pending_members`：丢弃 add_members 后 carol 从未入群、
  pending 列表清空、原因码 coordinator_timeout。
- `groups_commit::discard_without_pending_is_rejected_and_leaves_state`：没有 pending → `PENDING_MISMATCH`；
  空/空白/控制字符原因 → `INVALID_INPUT`；失败后仍能正常准备提交并接受。
- `groups_commit::accepted_state_survives_reopen`：接受后关闭库重开：epoch 2、无 pending、策略版本/控制头/成员一致。

### 已知限制

1. **本函数本身不做回执校验**（见上文语义边界）：需要节点回执的调用方必须走 GN 包的
   `accept_pending_control_with_receipt`（已验证回执绑定后才调用本函数）；直接调用本函数只代表“本地接受”。
2. **丢弃后必须重新装载设备**（`load_group_device`），因为丢弃只改库与库恢复出的设备。
3. **policy_version 不回退**：丢弃只回滚 MLS 提交与成员集合，策略版本按任务约定保留（审计需要）。
4. **一次只有一个治理操作在飞**：接受或丢弃之后才能准备下一个提交。
5. **不联网**：不接受对端 Commit（CRYPTO-006 `process_verified_entry` 未接线到 groups）、不投递 Welcome。

<a id="group-node"></a>

## 控制提交经节点回执再接受（GN 包，2026-10-19，Windows x64）

代码：[crates/core/src/group_node.rs](../../crates/core/src/group_node.rs)（新模块，`lib.rs` 加一行 `pub mod group_node;`，**不改** groups.rs/service.rs/attachments.rs/send_orchestrator.rs）；
测试：[crates/core/tests/group_node.rs](../../crates/core/tests/group_node.rs)（5 个用例，真 axum 节点 + 真 OpenMLS + 真设备签名）+ 模块内 2 个单元用例（提交标识确定性、错误码）。

### 公开面

```text
pub struct ControlReceipt { pub receipt: AppendResult, pub candidate: GroupCandidate, pub head: SignedLogHead }
pub struct ControlReceiptError { /* code: 'static str, detail: String */ }
pub type ControlReceiptResult<T> = Result<T, ControlReceiptError>;

pub fn submit_pending_control(device: &MlsDevice, store: &Store, group_id: &str, client: &NodeClient) -> ControlReceiptResult<ControlReceipt>
pub fn accept_pending_control_with_receipt(device: &mut MlsDevice, store: &mut Store, group_id: &str, receipt: &ControlReceipt) -> ControlReceiptResult<GroupView>
```

`ControlReceipt` 把“回执 + 被提交的候选原文 + 提交时的节点 head”**绑在一起**：只看回执字段无法排除
“拿 A 的回执去接受 B 的提交”，必须同时带上候选原文与 head 快照才能复核。

### 关键语义

1. **候选怎么构造**：正文是 pending 的序列化 MLS Commit（`PendingControl.commit`，小写 hex），`kind = control`；
   CAS 字段（`expected_control_head` / `policy_version` / `mls_epoch`）**只取自节点 head**（`get_head`），不做本地推断；
   作者是节点客户端绑定的设备（签名由 `NodeClient::append_entry` 用设备钥完成，本模块不碰私钥）。
2. **提交标识确定性**：`submission_id = SubmissionId(sha256(域 || group_id || base_epoch || target_epoch || commit_digest))`，
   同一个 pending 重试恒得同一个 SubmissionId —— 节点按 `(group, submission)` 去重，因此重试不会产生第二条日志。
3. **重试的对账路径**：控制候选的 CAS 字段依赖节点 head，而**我们自己的条目会让节点 control head 前进**，
   所以第二次提交的候选字节必然不同（节点回 `SUBMISSION_ID_CONFLICT`）。本模块此时**按 submission 翻节点日志对账**，
   取回原候选与原回执（并再用新 head 复核绑定）后返回，而不是伪造成“又提交成功了一次”。
4. **控制元数据（PROTOCOL-004 强要求）**：控制候选必须带 `control_metadata`（缺了协议层直接判非法候选）。
   `operation` 按本切片产生 pending 的治理操作映射：`add_members → invite`、`remove_members → remove`、
   `self_update → set_role`（本切片里 `SelfUpdate` 只由 GROUPS-003 改角色产生；`set_muted`/`rename_group` 是纯策略层，不产生 MLS 提交）；
   `target_digest` 用 pending 自带的提交摘要（已公开可算，不泄露目标身份）。
5. **接受前的绑定校验**（任一不满足 → 显式错误，**不推进 epoch、不清空 pending**）：
   ① 本地仍有 pending 且快照 epoch 停在 base_epoch、MLS 群键一致；② `receipt.group_id`/`candidate.group_id` 都等于本群；
   ③ `receipt.submission_id` 等于候选的；④ `candidate.kind == control` 且候选正文**逐字节等于**本 pending 的 Commit（密文摘要一致）；
   ⑤ `receipt.author_device_id` 等于候选作者；⑥ 回执自洽：节点身份与 head 一致、回执签名在 head 公钥下可验证、
   `candidate_hash` 由本地候选重算、`log_hash` 按 `log_chain_hash(group, seq, previous_hash, candidate_hash)` 重算 —— 都与回执一致。
   （节点侧 `AppendResult::verify` 本身也复核签名与日志链；本模块再按本地候选独立复算一遍，不依赖“客户端已经验过一次”。）
6. **通过之后才合并**：调用既有 `groups::accept_pending_control`（epoch 前进到 target_epoch、pending 清空、
   add 的 pending 成员入群 / remove 的待移除成员彻底删除、策略快照落库）；合并后复核 `epoch == target_epoch` 且 pending 已清空，
   否则按 `STATE_CORRUPTED` 失败（不回滚已提交的合并）。
7. **同步 API**：本 crate 的公开面是同步的，节点调用用 `std::thread::scope` + 独立 current-thread 运行时桥成同步
   （每次治理提交一个短命线程；即使调用方本身在 tokio 运行时里也不会嵌套 panic）。

### 语义边界（本轮仍然不等于“节点已验证 MLS 内容”）

- 回执证明的是：**节点接受了这条候选条目**（绑定 group/submission/kind/密文摘要/seq/log_hash 且节点签名有效）。
  它**不证明**节点或其它成员验证过 Commit 的 MLS 语义（MLS 内层验证仍需 CRYPTO-006 `process_verified_entry`，本轮未接线）。
- 本地策略控制头（DOMAIN-004 摘要）与节点可观察控制头（日志链摘要）**仍是两套值**：候选的 CAS 字段取节点 head，
  本地 `policy_version`/`control_head` 不参与节点校验。统一这两套头属于后续控制面切片。
- MLS 凭据（`alice`/`bob`）与节点设备标识（`dev:1:...`）在本切片是两套命名空间；映射属 IDENTITY/DOMAIN 后续切片。
- 只处理本机自己准备的 pending 提交；处理**对端** Commit（含 Welcome）仍未接线。

### 错误码（全部既有冻结码，不新造协议码）

| 情形 | code |
|---|---|
| 群 ID 不是 PROTOCOL-001 文本 | `INVALID_ID` |
| 没有 pending 提交 | `PENDING_MISMATCH` |
| 快照 epoch 与 pending 的 base_epoch 不一致 | `STALE_EPOCH` |
| pending 与该群的 MLS 群键不一致 / 合并后 epoch 不符 | `STATE_CORRUPTED` |
| 控制提交超过节点单条候选密文上限 | `CONTENT_TOO_LARGE` |
| 候选不是控制条目 / 正文与 pending Commit 不一致 / 候选非法 | `INVALID_CANDIDATE` |
| 回执绑定到别的群 / 别的 submission / 别的候选 / 别的节点 | `INVALID_RECEIPT_SCOPE` |
| 回执签名或日志链摘要不成立（含篡改 seq / log_hash） | `INVALID_SIGNATURE` |
| 回执是发给别的作者设备的 | `PERMISSION_DENIED` |
| 节点不可达 / 超时 | `NODE_UNAVAILABLE` |
| 其它节点拒绝（权限、配额、非法请求等） | 客户端已分类的稳定码（如 `INVALID_REQUEST`） |

### 复现命令（全程离线）

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test group_node   # 5 passed
cargo test --offline -p chat-core                     # 128 passed（0 警告）
```

### 证据（测试名 → 覆盖点）

- `group_node::control_commit_is_submitted_then_accepted_with_a_real_node_receipt`：建真群（真 OpenMLS，2 成员）→ 群主改
  bob 角色产生 pending（base 1 → target 2，epoch 不动）→ 提交到真节点拿到回执（seq 1、kind=control、正文与 pending Commit
  逐字节一致）→ **节点日志确实有这条控制条目**（submission/seq/log_hash 与回执一致，head 同步前进）→ 用回执接受后
  **epoch 2**、pending 清空、bob 变 Admin、持久快照同步前进、节点日志**仍只有 1 条**。
- `group_node::duplicate_submit_is_idempotent_and_appends_once`：连续两次提交拿到**同一个** SubmissionId / seq / log_hash /
  candidate_hash（第二次走“按 submission 对账”路径，因为节点 head 已被自己的条目前进），节点日志仍只有 1 条，任一份回执都能接受。
- `group_node::misbound_or_forged_receipts_are_rejected_without_advancing_epoch`：7 种伪造（改 seq、改 log_hash、换密文、
  换群、换作者设备、换节点公钥、把 kind 改成 application）逐个被拒（`INVALID_SIGNATURE` / `INVALID_RECEIPT_SCOPE` /
  `INVALID_CANDIDATE` / `PERMISSION_DENIED`），**每次拒绝后 epoch 仍为 1、pending 仍在**；最后真回执仍然能推进到 2。
- `group_node::unreachable_node_fails_explicitly_and_leaves_local_state_unchanged`：同设备身份指向一个必然连不上的端口 →
  `NODE_UNAVAILABLE`，本地 epoch/pending 不变、节点日志为空；随后换真节点提交 + 接受成功（失败不是破坏性的）。
- `group_node::submit_without_pending_control_fails_explicitly`：没有 pending → `PENDING_MISMATCH`；非规范群 ID →
  `INVALID_ID`（在触网之前拒绝）。
- 模块内单测 `group_node::tests::submission_ids_are_deterministic_and_bound_to_the_commit`（同 pending 恒同 id、
  换群/换 epoch/换摘要都换 id、结果是合法 PROTOCOL-001 submission 文本）与 `pending_errors_carry_frozen_codes`。

### 已知限制

1. **只证明节点接受了条目**：没有 MLS 内层验证（CRYPTO-006 `process_verified_entry`），也不校验其它成员是否看到该 Commit。
2. **两套控制头未统一**（本地 DOMAIN-004 摘要 vs 节点日志链摘要）；候选 CAS 只取节点 head。
3. **元数据映射是本切片特化**：`SelfUpdate → set_role` 只在“本切片里 SelfUpdate 只由改角色产生”时成立；新增会产生 SelfUpdate 的
   治理操作时必须同步扩展映射（否则公开元数据会说错操作种类）。`target_digest` 用提交摘要而非“操作目标的摘要”，
   因为当前 `GroupView` 不暴露被操作的目标身份。
4. **对账是 O(日志页)**：重试路径最多翻 20 页（每页 50 条）；超大群日志下应改为按 submission 的服务端查询端点。
5. **每次提交一个短命线程**：治理提交是低频操作，用同步桥避免把 async 传染到 FFI 侧；高频场景需要常驻桥（如 node_submitter 的做法）。
6. **不接受对端 Commit、不投递 Welcome**：本轮只做本机 pending 的提交与接受。

## 群治理接 FFI 与服务方法（GroupFfi，2026-10-18，Windows x64）

代码：[crates/ffi/src/groups_ffi.rs](../../crates/ffi/src/groups_ffi.rs)（新）+ [crates/core/src/service.rs](../../crates/core/src/service.rs)
的 `ConversationCore::group_*` 薄封装；测试：[crates/ffi/tests/groups_ffi.rs](../../crates/ffi/tests/groups_ffi.rs)
（6 个用例，经 FFI 走真实 OpenMLS + 临时持久库）。契约细节见 [bridge.md 的“群治理 FFI 契约”](bridge.md)。

### core 侧方法（都在 `ConversationCore` 上，只加方法，不改既有语义）

| 方法 | 说明 |
|---|---|
| `group_create(name, local_credential, bootstrap_credential, members)` | 建群；`local_credential` 为空时用**持久身份设备 ID**（未入库 → `INVALID_STATE_TRANSITION`）；引导成员设备在本进程临时创建 |
| `group_snapshot(group_id)` | 读正式快照（不联网） |
| `group_set_role(group_id, actor, target, role_text)` | 角色变更（`owner`/`admin`/`member`；未知文本 → `INVALID_INPUT`；`owner` 由 domain 拒绝） |
| `group_set_muted(group_id, actor, target, muted)` | 禁言（策略层） |
| `group_remove_member(group_id, actor, target)` | 移除成员（MLS 移除提交） |
| `group_rename(group_id, actor, new_name)` | 改名（同名幂等） |
| `group_accept_pending(group_id)` | 接受并合并控制提交（CRYPTO-007） |
| `group_discard_pending(group_id, reason)` | 丢弃回滚（CRYPTO-008） |

设备统一用 `MlsDevice::import_from_store` 从持久库恢复（建群除外：群还不存在，设备是新建的）；
禁用闭包 `with_group_device` 保证「先恢复、再执行、失败不留半状态」。
为了让 FFI 拿到冻结码，`service::error_code` 增加了 `GroupError` 的 downcast 分支（方法体 3 行改动，
不改任何既有语义）；角色 → 文本的映射以 `group_role_code` / `group_role_from_code` 暴露（FFI 不需要依赖 chat-domain）。

### GroupFfi（Kotlin 可见）

`open(database_path, master_key_hex, node)` + 上表 8 个方法，返回 `GroupViewFfi`（成员/角色/禁言/pending/
pending_removed/last_control_cleared 全是纯数据，不含密钥）。`node` 只保存不联网。契约细节见
[bridge.md 的“群治理 FFI 契约”](bridge.md)。

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi --test groups_ffi   # 6 passed
cargo test --offline -p chat-core -p chat-ffi        # 118 + 36 passed
~~~

### 证据

- `groups_ffi::create_snapshot_rename_mute_and_remove_through_ffi`：建群 → 快照一致 → 改名（pv2，同名幂等）→
  禁言（pv3）→ 移除（pv4，pending_removed=[bob]）→ 接受（epoch2、pending 清空、pv 不变）→ 快照一致。
- `groups_ffi::role_change_accept_and_discard_through_ffi`：改角色（pending self_update）→ 丢弃（epoch 不变、
  pv 不回退、审计 self_update/node_rejected）→ 改回并接受（epoch2）。
- `groups_ffi::error_codes_are_frozen_and_failures_leave_no_state`：`INVALID_TARGET`/`INVALID_ID`/
  `INVALID_INPUT`/`PERMISSION_DENIED`/`PENDING_MISMATCH` 逐条断言，失败后快照与建群结果完全一致。
- `groups_ffi::empty_local_credential_uses_the_persisted_identity_device`：持久身份建立后，空凭据建群的本地成员
  凭据 == 身份设备 ID。
- `groups_ffi::create_without_identity_and_empty_credential_fails`：`INVALID_STATE_TRANSITION` / `INVALID_CANDIDATE`。
- `groups_ffi::closed_object_reports_core_closed`：`shutdown` 后 `CORE_CLOSED`。

### 已知限制

1. **本地接受 ≠ 节点接受**（见上文节）：`group_accept_pending` 只收口本地状态。
2. **引导成员设备由本进程临时创建**：真实邀请加入（GROUPS-004）仍未实现。
3. **独占单写者**：`GroupFfi` 与 `LocalStoreFfi` / `AttachmentFfi` 互斥；需要身份数据时先关闭 `IdentityFfi`。
4. **同步阻塞**：FFI 的 `Mutex` 串行执行（含 MLS 提交生成）。
5. `node` 只保存；群治理当前不访问网络。

## 对端条目处理与 Welcome 加入实现记录（2026-10-18，Windows x64）

代码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)（同一文件，无新依赖）；测试：
[crates/core/tests/groups_peer.rs](../../crates/core/tests/groups_peer.rs)（7 个用例，**真 OpenMLS 双设备 + 两个独立临时库**）。

> 编号说明：本轮任务的“对端 Commit 处理”对应 CRYPTO-006 `process_verified_entry` 的群服务接线，
> 在本文档没有独立函数号（属 SYNC 日志应用路径）；“Welcome 加入”对应 GROUPS-004 的**加入侧**。

### 公开面

~~~text
pub fn group_process_peer_entry(device: &mut MlsDevice, store: &mut Store, input: &PeerEntryInput)
    -> GroupResult<PeerEntryOutcome>

pub struct PeerEntryInput {
    group_id, epoch, sender_identity, expectation: Option<CommitExpectation>,
    payload: Vec<u8>, added_members: Vec<String>, removed_members: Vec<String>,
}
pub enum PeerEntryOutcome {
    Commit { old_epoch, new_epoch, added, removed, updated, self_removed, view },
    Application { sender_identity, epoch, plaintext },
    Proposal { sender_identity, epoch, view },
    OwnPendingCommit { epoch },
}

pub fn group_join_from_welcome(
    device: &mut MlsDevice, store: &mut Store, name: &str, welcome: &[u8],
    acceptance: &WelcomeAcceptance, local_credential: &str, owner_credential: &str,
) -> GroupResult<GroupView>

pub fn create_group_with_welcome(...) -> GroupResult<(GroupView, Vec<u8>)>  // 额外返回引导成员的 Welcome 字节
~~~

### 关键语义

1. **绑定与会话状态先校验**：快照必须存在（未知群 → `INVALID_TARGET`）、本机设备必须是该群（否则
   `INVALID_TARGET`）、条目 epoch 必须等于快照 epoch（落后 → `STALE_EPOCH`，超前 → `MISSING_LOG_ENTRIES`）。
2. **成员身份与真实计数双重绑定**：crypto 用 MLS 认证复核群/epoch/作者与**真实 StagedCommit 计数**；
   本层在调用 crypto **之前**要求「声明的成员身份数量 == 预期计数」，因此合并成功后写回快照的成员增删
   既来自真实 Commit 计数、又能按身份落库（数量不符 → `INVALID_CONTROL_RESULT`，**在任何合并之前**拒绝）。
3. **epoch 恰 +1**：对端 Commit 合并后必须恰好前进一个 epoch（crypto 与本层各校验一次），随后
   `export_to_store` 把真实 MLS 状态写库，再更新快照的 epoch 与成员；应用消息/提案不改变快照。
4. **失败不留半状态**：所有拒绝都发生在合并之前；crypto 对致命错误会让该群分支 fail-closed，库内仍是上一次
   完整状态（测试逐一断言 `GroupView` 不变）。
5. **Welcome 加入**：crypto 复核外层绑定（群 ID/epoch/成员集合、本机身份必须在内）→ 真实成员与 epoch 写快照
   （名称由调用方给、群主来自已验证邀请元数据并要求确为成员）；同一群重复加入 → `INVALID_STATE_TRANSITION`；
   绑定不符 → `WELCOME_NOT_AUTHORIZED` 且**不写任何快照**。

### 错误码（既有冻结码）

| 情形 | code |
|---|---|
| 未知群 / 本机不在该群 | `INVALID_TARGET` |
| 条目 epoch 落后 / 超前（乱序） | `STALE_EPOCH` / `MISSING_LOG_ENTRIES` |
| 作者与 MLS 认证作者不符 / 认证解密失败 / epoch 不符 | `INVALID_CIPHERTEXT` |
| 声明成员身份与预期或真实计数不符 | `INVALID_CONTROL_RESULT` |
| 空 payload / 非法凭据 | `INVALID_CANDIDATE` / `INVALID_CREDENTIAL` |
| 本机有未决控制提交时收到竞争 Commit | `PENDING_MISMATCH` |
| Welcome 绑定不符 / 已被使用 | `WELCOME_NOT_AUTHORIZED` |
| 重复加入 / 群已初始化 | `INVALID_STATE_TRANSITION` |
| 合并后导出失败 | `STORAGE_FAILURE` |

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test groups_peer   # 7 passed
cargo test --offline -p chat-core                      # 140 passed（既有 133 继续通过）
~~~

### 证据（测试名 → 覆盖点）

- `groups_peer::welcome_join_then_peer_add_commit_advances_the_epoch`：A 建群（追加 carol → pending Add）→
  B 用**真实 Welcome** 加入（成员 2、epoch 1、本机 role=member、群主=alice、无 pending）→ A 的快照里 carol 仍是
  pending → B 处理 A 的真实 Add Commit（epoch 1、作者 alice、预期 added=1、声明成员 carol）→ outcome 断言
  old 1 / **new 2** / added 1 / removed 0 / updated 0 / self_removed=false → B 的快照 epoch 2、成员
  alice+bob+carol（carol 为 member）→ 快照一致且**关库重开**后仍一致。
- `groups_peer::duplicate_and_out_of_order_entries_are_rejected`：重复处理同一条目 → `STALE_EPOCH`；
  声称 epoch 3（乱序）→ `MISSING_LOG_ENTRIES`；epoch 声明与消息内 epoch 不符 → `INVALID_CIPHERTEXT`；
  三次拒绝后快照与成功合并后完全一致。
- `groups_peer::wrong_sender_and_declared_member_mismatch_are_rejected`：作者改成 mallory → `INVALID_CIPHERTEXT`；
  声明成员数 0 与预期 1 不符 → `INVALID_CONTROL_RESULT`（**合并前**拒绝）；两次失败后快照仍为 epoch 1、成员 2。
- `groups_peer::unknown_group_and_empty_payload_are_rejected`：未知群 `INVALID_TARGET`、空 payload `INVALID_CANDIDATE`。
- `groups_peer::welcome_join_rejects_wrong_binding`：epoch 绑定错 → `WELCOME_NOT_AUTHORIZED` 且**没有快照记录**
  （`INVALID_TARGET`）；被拒的 Welcome 已消耗 KeyPackage，换设备再用同一 Welcome 也被拒。
- `groups_peer::duplicate_join_is_rejected_and_snapshot_is_unchanged`：同一设备重复加入 → `INVALID_STATE_TRANSITION`，
  快照不变。
- `groups_peer::join_rejects_declared_owner_outside_the_group`：声明的群主不在成员集合 → `INVALID_CONTROL_RESULT`
  且不写快照（此时 MLS 状态已入群，因此第二次加入显式 `INVALID_STATE_TRANSITION`）。

### 已知限制

1. **成员身份来自外层已验证元数据**：MLS 只给出真实增删**计数**，身份集合必须由调用方（已验证控制日志）提供，
   本层用「身份数量 == 真实计数」把两者绑定；没有控制日志时无法单独从 MLS 推导“新增的是谁”。
2. **不接节点**：本条目不提交/不拉取；提交与回执属 group_node / NODE 侧，日志顺序与去重由 SYNC 负责。
3. **应用消息不落库**：`PeerEntryOutcome::Application` 只把认证后的明文交给可信核心路径，消息表写入属消息模块。
4. **自己的 pending 与对端 Commit 竞争**：crypto 要求先解决本机 pending（`PENDING_MISMATCH`），
   本层不自动丢弃（避免丢失可重新投递的候选）。
5. **Welcome 一次性**：被拒绝的 Welcome 已消耗 KeyPackage，必须重新获取邀请与 KeyPackage（GROUPS-004 票据未实现）。
6. **self_removed 只更新快照**：本机被移除后快照会去掉本机成员，但重新加入需要新的 Welcome（未实现）。

## 群治理经节点回执接进服务层与 FFI（2026-10-18，Windows x64）

本轮把 group_node 的真节点回执路径（NODE-006 `submit_pending_control` /
`accept_pending_control_with_receipt`）接到 `ConversationCore` 与 `GroupFfi`：界面可以"提交治理操作到节点
→ 拿到回执 → 用回执接受"。

代码：[crates/core/src/service.rs](../../crates/core/src/service.rs)（`group_submit_control` /
`group_accept_with_receipt` + 回执公开副本类型）、[crates/ffi/src/groups_ffi.rs](../../crates/ffi/src/groups_ffi.rs)
（`ControlReceiptFfi` 等 4 个记录 + 2 个方法）；测试：
[crates/ffi/tests/groups_ffi_receipt.rs](../../crates/ffi/tests/groups_ffi_receipt.rs)（3 个用例，**真 axum 节点 +
真 OpenMLS + 真临时库**，全程经 FFI）。FFI 契约细节见 [bridge.md 的“群治理经节点回执”](bridge.md)。

### 公开面（core）

~~~text
ConversationCore::group_submit_control(group_id, endpoint_url, node_id, node_public_key_hex: Option<&str>)
    -> ControlReceiptView
ConversationCore::group_accept_with_receipt(group_id, &ControlReceiptView) -> GroupView

ControlReceiptView { receipt: ControlAppendView, candidate: ControlCandidateView,
                     head: ControlHeadSnapshotView }   // 公开材料，无密钥
ControlReceiptView::submission_id() / seq() / log_hash()
~~~

### 关键语义

1. **设备身份来自持久身份**：提交时用 `load_identity_seeds` 派生设备签名钥（与 identity 同一个派生函数），
   节点看到的设备 ID 与库里一致；未入库身份 → `INVALID_STATE_TRANSITION`。
2. **先本地自证，再触网**：群快照/pending/群键/epoch 绑定全部由 group_node 前置校验；不满足就**不发请求**。
3. **回执 = 三要素**：回执 + 候选原文 + 提交时的 head 必须一起流转并一起交回；接受时重做三重校验
   （候选密文逐字节等于本机 pending Commit、节点验签、日志链摘要重算）。
4. **接受后真实推进**：epoch 前进到 target、pending 清空、策略快照写入；失败保持原状态（测试逐一断言
   `GroupView` 完全不变）。
5. **不联网的部分保持不联网**：`group_snapshot` / 策略层治理（改名、禁言）仍然不触网。

### 错误码（冻结）

`NODE_UNAVAILABLE`（不可达/超时）、`INVALID_RECEIPT_SCOPE`（错群/错提交/错节点/链摘要不符）、
`INVALID_SIGNATURE`（节点签名或候选摘要不符）、`INVALID_CANDIDATE`（候选结构/密文不符）、
`PENDING_MISMATCH` / `STALE_EPOCH`（本地 pending 状态不符）、`CORE_CLOSED` / `INVALID_TARGET`。

### 复现命令（全程离线；测试内起真实 loopback 节点）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-ffi --test groups_ffi_receipt   # 3 passed
cargo test --offline -p chat-core -p chat-ffi                # 155 + 39 passed（既有 151 + 36 继续通过）
~~~

### 证据

- `groups_ffi_receipt::submit_and_accept_with_a_real_node_receipt`：建群（本地凭据 = 持久身份设备 ID，
  该设备已在节点登记、群也已在节点建立）→ 改角色产生 pending self_update（epoch 1）→ 提交拿到真回执
  （`seq >= 1`、`sub:1:…`、候选 kind=control、operation=set_role、head 属于该节点/该群）→ 接受后
  **epoch 2**、pending 清空、bob 变 admin、快照一致；重复接受 → `PENDING_MISMATCH` 且快照不变。
- `groups_ffi_receipt::forged_receipts_are_rejected_without_advancing_the_epoch`：改链摘要 / 改节点签名 →
  `INVALID_SIGNATURE`；换密文 → `INVALID_CANDIDATE`；每次拒绝后快照仍是 epoch 1 + pending 仍在，
  最后真回执仍能推进到 epoch 2。
- `groups_ffi_receipt::unreachable_node_fails_explicitly_and_leaves_state`：指向 `127.0.0.1:1` →
  `NODE_UNAVAILABLE`、快照完全不变；换真节点后提交 + 接受成功。

### 已知限制

1. **pending 的来源**：本实现里 `rename` 是纯策略层（不产生 MLS 提交），能产生 pending 的是改角色 /
   移除成员 / 建群追加成员；端到端用例因此用改角色产生 pending。
2. **节点与 MLS 两套标识**：节点用"节点设备 ID"登记成员与群，MLS 用凭据文本；两侧都要各自登记
   （测试里显式做，生产由 NODE 侧流程负责）。
3. **同步阻塞**：提交走同步桥（独立线程 + 独立运行时），在 FFI 的 `Mutex` 内串行执行。
4. **只做提交与接受**：不拉取/不对账别人的条目（SYNC 侧）、不重试队列、不投递 Welcome。
5. ~~本地接受证据 ≠ 节点回执~~（本轮的**节点回执路径**已补上）；但 `group_accept_pending`（无回执接受）
   依然只是本地收口，两者不可混用。
6. `node_public_key_hex` 只影响提交时的端点固定；回执验签用回执自带的 head 公钥（NODE-006 既定语义）。



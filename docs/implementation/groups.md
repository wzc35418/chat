# 小群创建、邀请与治理用例

阶段：P3。状态：函数级设计；GROUPS-001 建群与 GROUPS-002 快照已实现（真实 OpenMLS + 正式加密库），成员治理的角色变更（`set_role`）与禁言（`set_muted`）也已实现（DOMAIN-002/004 策略层 + CRYPTO-005 控制提交）；移除成员（`remove_member`）与改名（`rename_group`）也已实现（同一条 DOMAIN-002/004 + CRYPTO-005 流程）；控制提交的**接受合并（`accept_pending_control`）与丢弃回滚（`discard_pending_control`）**也已接线（CRYPTO-007/008）；上述能力已通过 `GroupFfi` 暴露给 Kotlin（`group_create`/`snapshot`/`set_role`/`set_muted`/`remove_member`/`rename`/`accept_pending`/`discard_pending`）；对端条目处理（`group_process_peer_entry`，CRYPTO-006）与 Welcome 加入（`group_join_from_welcome`，CRYPTO-009）也已实现；群生命周期（转让群主 `transfer_ownership`、退出 `leave_group`、解散 `dissolve_group`）也已实现（GROUPS-010~012）。目标源码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)。

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
- [META 包：控制提交节点元数据映射扩展](groups.md#meta)

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

<a id="group-ffi"></a>

<a id="meta"></a>

## META 包：控制提交节点元数据改为显式意图（R-80）

- **ControlIntent + resolve_control_metadata**：`Invite`→`invite`、`RemoveMember{c}`→`remove`（目标 `H(域‖c)`）、`LeaveSelf`→`remove`（目标=本机设备摘要，要求 pending 为 RemoveMembers 且 removed==1）、`SetRole{c}`→`set_role`、`TransferOwnership{c}`→`transfer_ownership`（要求本地已被降级、目标=被转让凭据摘要）、`Dissolve`→`dissolve`。形态不符一律 `INVALID_CANDIDATE`。
- **旧入口不再猜**：只在能证明唯一语义时提交（本地仍是群主的 SelfUpdate → `set_role`）；已降级设备的 SelfUpdate 无法区分转让/管理员改角色 → 显式 `PROTOCOL_UNSUPPORTED` 且**不触网**。
- **证据**：`cargo test -p chat-core` 中 `group_node` 新增 4 单测 + 2 真节点集成——真节点上转让的日志元数据为 `transfer_ownership` 且目标摘要≠提交摘要，回执接受后 epoch 2、群主易主；同一 pending 用旧入口被拒且节点日志为空。
- **协议待补**：PROTOCOL-004 无“通用 self_update”值（建议 `PendingControl` 记领域操作或协议加字段）；`set_muted`/`rename_group` 无 MLS 提交不经过映射；`leave_group`/`dissolve_group` 本切片仍不产生 MLS 提交，故 `LeaveSelf`/`Dissolve` 映射已实现并单测但端到端不可达（文档不冒充已运行通路）。

## 接受入群邀请（GROUPS-017，2026-10-19，Windows x64）

入群以前只有底层 `group_join_from_welcome`（调用方要自己拼 `WelcomeAcceptance`）。本轮加一个**邀请语义**的
入口：接受邀请时把邀请元数据（邀请者/群名/成员集合/epoch）与 Welcome 的实际内容逐项核对。

代码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)（`GroupInviteMeta` + `accept_group_invite`）；
测试：[crates/core/tests/groups_invite.rs](../../crates/core/tests/groups_invite.rs)（4 个，真双设备两库）。

### 公开面（新增）

```text
pub struct GroupInviteMeta {
    group_id,          // 协议群文本 grp:1:<64 hex>
    mls_group_id,      // MLS 群 ID（小写 hex），进 WelcomeAcceptance
    name, inviter_credential, owner_credential, invitee_credential,
    members: Vec<String>, epoch: u64,
}
pub fn accept_group_invite(device, store, welcome: &[u8], invite: &GroupInviteMeta) -> GroupResult<GroupView>
```

### 关键语义

1. **元数据自校验（写入之前）**：群文本（`INVALID_ID`）、群名（`INVALID_INPUT`）、各凭据、成员集合非空、
   `epoch >= 1`；被邀请者必须是成员、不能是群主、邀请者必须是成员、邀请者不能邀请自己。
2. **群 ID 自洽**：`group_id` 必须等于由 `mls_group_id` 推导的协议群文本 → 否则 `STATE_CORRUPTED`
   （这一步本地可算，因此自相矛盾的邀请在碰 Welcome 之前就被拒）。
3. **重复接受**：本机已有该群快照 → `INVALID_STATE_TRANSITION`（不再进入 Welcome 路径，避免“已用过的
   KeyPackage”这类次要错误盖住真正原因）。
4. **Welcome 复核**：`mls_group_id` / `epoch` / `members` 直接作为 `WelcomeAcceptance` 交给 crypto，由它
   与 Welcome 里的真实群状态比对；不一致 → crypto 先删掉刚写入 provider 的群状态，**正式库零写入**，
   本层把 `WELCOME_NOT_AUTHORIZED` 归一为 `STATE_CORRUPTED`（“邀请元数据与 Welcome 不符”）。
5. **成功**：走既有 `group_join_from_welcome`（快照 + 凭据目录一起写，见 GROUPS-016/STORAGE-016），
   返回 `role = member` 的投影；若结果是群主（元数据说谎）则 `STATE_CORRUPTED`。

### 复现命令（全程离线）

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test groups_invite   # 4 passed
cargo test --offline -p chat-core                        # 全绿（既有 221 继续通过 + 本轮 4）
```

### 证据

- `groups_invite::accepting_an_invite_joins_as_a_member_and_writes_the_directory`：正常接受 → `role=member`、
  群名/epoch/成员集合与邀请一致、快照已落库（群主是 alice）、凭据目录写入 alice+bob 两条。
- `groups_invite::metadata_mismatches_are_rejected_without_writing_anything`：epoch +1 / 成员集合多一人 /
  群文本与 MLS 群 ID 不自洽 → `STATE_CORRUPTED`，且**没有群快照、目录为空、设备没有 MLS 群**。
- `groups_invite::accepting_the_same_invite_twice_is_an_invalid_transition`：第二次接受 →
  `INVALID_STATE_TRANSITION`，快照的 `state_version`/`epoch` 不变。
- `groups_invite::malformed_invite_metadata_is_rejected`：未知群格式 `INVALID_ID`；非法群名 / 邀请者非成员 /
  被邀请者不在成员集合 → `INVALID_INPUT`；全部失败都不留快照。

### 已知限制

1. **邀请元数据由调用方给**：可信度取决于外层已验证的邀请日志/协调者回执；本层只做与 Welcome 的一致性
   复核，不做独立事实发现。
2. **`mls_group_id` 必须由邀请携带**：协议层群文本无法反推 MLS 群 ID，因此邀请里同时带两者并做自洽校验。
2. **只支持“被邀请者即时加入”**：没有邀请过期/撤销/一次性令牌语义（重复接受靠快照存在性拒绝）。
3. **接受后不自动拉取历史**：入群只落快照与目录，消息/Commit 追赶走既有同步路径。

## 禁言 / 改名自动准备控制提交（GROUPS-016，2026-10-19，Windows x64）

### 服务层与 FFI 收口（task-84，2026-10-19）

GROUPS-016 只加了新入口；本轮把**调用方**切过去，界面上改名/禁言已经会准备提交：

- `crates/core/src/service.rs`：`group_set_muted` / `group_rename` 改走 `with_group_device` +
  `set_muted_with_commit(.., local_only = false)` / `rename_group_with_commit(.., false)`；
  返回的 `GroupView.pending` 就是准备好的 `SelfUpdate` 提交（epoch 在准备阶段不推进）。
- `crates/ffi/src/groups_ffi.rs`：签名不变，`GroupViewFfi.pending` 自动反映该提交（`group_view` 已映射
  `pending`），接受仍走 `group_accept_pending`。
- `crates/node-services/src/group.rs`：不调用旧入口（它只把 `"set_muted"` / `"rename"` 文本映射成
  控制操作码用于日志元数据），因此无需改动。
- 旧入口 `groups::set_muted` / `groups::rename_group` 保留为**纯本地别名**（等价于
  `*_with_commit(.., local_only = true)`，只是不需要设备），文档已标注；没有调用方再依赖它们的“无提交”
  语义来同步对端。

顺序语义提醒：改名/禁言准备好提交后，**再次**发起策略变更会得到 `INVALID_STATE_TRANSITION`，必须先用
`accept_pending_control`（FFI：`group_accept_pending`）接受或 `discard_pending_control` 丢弃。

验证证据（`cargo test --offline -p chat-core -p chat-ffi -p chat-node-services`）：**208 + 60 + 90 passed**，
其中 `chat-ffi::groups_ffi::create_snapshot_rename_mute_and_remove_through_ffi` 已按新契约更新：
改名 → pending base1→target2 → 接受（epoch 2）→ 同名幂等（pv/state_version 不动）→ 禁言 → pending →
接受（epoch 3、muted 保持）→ 移除 → pending base3→target4 → 接受（epoch 4、pv 4）。


GROUPS-015 已能让禁言与群名随控制提交传播，但发送侧仍要协调者手工准备一个 `SelfUpdate` 提交。
本轮收口：新增**设备感知**的变更入口，策略变更的同时准备真实 MLS 控制提交。

代码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)；
测试：[crates/core/tests/groups_mutations_commit.rs](../../crates/core/tests/groups_mutations_commit.rs)（4 个，真双设备两库）。

### 公开面（新增）

```text
pub fn set_muted_with_commit(device, store, group_id, actor, target, muted,
                             local_only: bool) -> GroupResult<GroupView>
pub fn rename_group_with_commit(device, store, group_id, actor, new_name,
                                local_only: bool) -> GroupResult<GroupView>
```

`local_only = false`（正常路径）：在策略落库之前 `prepare_control(SelfUpdate)` → pending 写入
（**epoch 不推进**）、`GroupView.pending` 如实报告；接受后 epoch 恰好 +1，禁言/群名保持。
`local_only = true`：退化为纯本地策略变更，不产生提交（等价于既有 `set_muted` / `rename_group`）。

为什么是**新增**而不是改签名：`set_muted` / `rename_group` 的现有签名叫法被
`crates/core/src/service.rs`、`crates/ffi/src/groups_ffi.rs`、`crates/node-services/src/group.rs`
以及既有用例调用，都不在本任务写作用域内；改成带 `device` 的新签名会让这些调用点编译失败。
因此保留旧函数（纯本地、契约不变），新增设备感知入口并给出显式 `local_only` 开关。

### 关键语义

1. **设备前置校验**：`group_key` 必须属于该群（否则 `INVALID_TARGET`）、设备 epoch 必须与持久化
   快照一致（否则 `STALE_EPOCH`）——**绝不静默降级成本地变更**。
2. **顺序**：读快照 → 校验 → DOMAIN-002 授权 + DOMAIN-004 派生 → 准备 `SelfUpdate` 提交 → 应用策略
   → 持久化 → 投影（带 pending）。授权/校验失败发生在准备提交之前，因此失败时设备与快照都不动。
3. **失败不留半状态**：策略持久化失败时会尝试丢弃刚准备的 pending（`discard_pending_control`），
   再返回原错误。
4. **同名幂等**：`rename_group_with_commit` 在名字未变时不写库、不推进策略版本、不准备提交。
5. **已有 pending** 时再次变更 → `INVALID_STATE_TRANSITION`（crypto `PendingCommitExists` 原样透出），
   且既有 pending 不被覆盖。

### 错误码（与 GROUPS-015 一致，未新增）

`PERMISSION_DENIED`（越权、自我禁言）、`INVALID_INPUT`（名字不合法）、`INVALID_TARGET`（目标不是
成员 / 设备不属于该群）、`STALE_EPOCH`（设备与快照 epoch 不一致）、`INVALID_STATE_TRANSITION`
（已有 pending）、`INVALID_STATE`（群已解散由 `reject_dissolved` 给出）。

### 复现命令（全程离线）

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test groups_mutations_commit   # 4 passed
cargo test --offline -p chat-core                                   # 204 passed（既有 200 继续通过）
```

### 证据

- `groups_mutations_commit::rename_prepares_a_pending_commit_and_accept_keeps_the_name`：默认路径
  `pending.is_some()`、epoch **不变**、名字与 `policy_version` 已更新；`accept_pending_control` 后
  epoch +1、名字保持、pending 清空。
- `groups_mutations_commit::mute_prepares_a_pending_commit_and_syncs_to_the_peer_after_acceptance`：
  禁言同样准备 pending（bob = muted、epoch 1）；接受后把该提交交给对端 + `policy_sync` 声明，
  **对端 bob 侧看到自己 muted**（端到端闭环，不再需要手工准备提交）。
- `groups_mutations_commit::local_only_does_not_prepare_a_control_commit`：`local_only = true` 时
  改名/禁言都只改策略，`GroupView.pending` 为空且 `device.pending_control()` 为空。
- `groups_mutations_commit::failures_leave_no_half_state`：普通成员越权改名（`PERMISSION_DENIED`）、
  群主禁言自己（`PERMISSION_DENIED`）、无群设备（`INVALID_TARGET`）、已有 pending 再变更
  （`INVALID_STATE_TRANSITION`）——四种失败都**没有产生 pending**，快照逐字段不变，第一次的提交完整保留。

### 已知限制

1. **旧入口仍是纯本地**：`set_muted` / `rename_group` 保持原语义（不产生提交），调用方需要同步到对端
   时必须改用 `*_with_commit`。等 `service.rs` / FFI / node-services 一并切到新入口（不在本任务写作用域）
   之后，可以再把旧入口标记为 local-only 的别名。
2. **提交的准备与接受是两步**：本函数只 prepare（epoch 不推进），接受走既有 `accept_pending_control`；
   与 `transfer_ownership` / `remove_member` 等既有提交型 API 的形态一致。
3. **一次提交一条策略**：禁言与改名不能共用同一条提交（与 GROUPS-015 的声明互斥规则一致）。

## 禁言与群名随控制提交同步（GROUPS-015，2026-10-19，Windows x64）

GROUPS-014 已能同步转让/改角色，但**禁言（muted）与群名**仍只在本机生效。本轮把这两类纯策略变更
也接到同一条“控制提交 + 策略声明”通道上。

代码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)（`PolicySyncIntent` + `PeerEntryInput.policy_sync`）；
测试：[crates/core/tests/groups_policy_sync.rs](../../crates/core/tests/groups_policy_sync.rs)（4 个，真双设备两库）。

### 公开面（扩展）

```text
pub enum PolicySyncIntent {
    SetMuted { credential: String, muted: bool },
    RenameGroup { name: String },
}

PeerEntryInput {
    ... 既有字段 ...
    policy_intent: Option<ControlIntent>,   // 角色/转让（GROUPS-014，语义不变）
    policy_role: Option<MemberRole>,
    policy_sync: Option<PolicySyncIntent>,  // 新增：禁言 / 改名
}
```

`policy_intent` 与 `policy_sync` **互斥**：同时出现 → `INVALID_INPUT`（不猜哪一个是权威）。

### 关键语义

1. **先算策略、再做 MLS 合并**：带 `policy_sync` 时，在调用 CRYPTO-006 之前完成 DOMAIN-002 授权 +
   DOMAIN-004 派生；授权/校验失败**连合并不发生**，快照逐字段不动。
2. **合并后应用**：核对真实成员变化必须为空（纯策略提交）→ `apply_domain_policy` 写回 `muted`
   （候选成员里带禁言位）→ 改名时再写回 `record.name`；随后 `record.validate()` 收口。
3. **校验**：目标必须是确认成员（`member_index` → `INVALID_TARGET`）；群主不能禁言自己 →
   `PERMISSION_DENIED`（自我禁言会让群失去唯一能解禁的人）；名字沿用既有 `validate_name`
   （1..=64 字节、无控制字符、无首尾空白 → `INVALID_INPUT`）。任务书写“≤80 字符”，实现保留这条
   **更严格**的既有规则（与 `rename_group`/建群一致，避免对端设出本机 API 拒绝的名字）。
4. **权限**：只有群主/管理员能禁言他人与改名（DOMAIN-002）；普通成员越权 → `PERMISSION_DENIED`。
5. **不声明 = 保持现状**（向后兼容）：只合并 MLS 状态，`muted`/群名/策略版本都不动。
6. **发送侧现实**：`set_muted` / `rename_group` 本身是**纯策略层**（不产生 MLS 提交，保持既有语义与
   既有用例），因此“随提交传播”由协调者另外准备一个 `SelfUpdate` 提交承载声明（测试即如此：
   先本地策略生效 → 准备并接受自更新提交 → 对端按 `policy_sync` 应用）。

### 错误码

`STATE_CORRUPTED`（声明与成员变化不一致）、`INVALID_INPUT`（名字不合法、两类声明同时出现）、
`INVALID_TARGET`（目标不是确认成员）、`PERMISSION_DENIED`（越权 / 群主禁言自己）。

### 复现命令（全程离线）

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test groups_policy_sync   # 4 passed
cargo test --offline -p chat-core                              # 200 passed（既有 196 继续通过）
```

### 证据

- `groups_policy_sync::mute_syncs_to_the_peer_through_the_control_commit`：A 禁言 bob → 真实
  `SelfUpdate` 提交 → B 处理带 `SetMuted{bob,true}` 声明的提交后 **bob = muted**、epoch 2；
  落库快照与返回值一致。
- `groups_policy_sync::rename_syncs_to_the_peer_through_the_control_commit`：A 改名 → B 处理后
  **群名更新**（返回值与 `group_snapshot` 都变）。
- `groups_policy_sync::member_cannot_rename_or_mute_others_and_the_snapshot_is_unchanged`：bob（普通成员）
  的提交声明改名 / 禁言 alice → 两次都 `PERMISSION_DENIED` 且快照**逐字段相等**；群主禁言自己 →
  `PERMISSION_DENIED`；含控制字符的名字与 65 字节名字 → `INVALID_INPUT` 且快照不变。
- `groups_policy_sync::without_a_policy_declaration_the_peer_behaviour_is_unchanged`：同一提交不带声明 →
  epoch 前进但群名 / 禁言**保持不变**（向后兼容）。

### 已知限制

1. **发送侧仍需协调者准备提交**：`set_muted`/`rename_group` 不产生 MLS 提交（保持既有契约），
   本轮没有把它们改成“自动准备提交”（那会改变它们“纯策略、无 pending”的语义与既有用例）。
2. **一次提交只同步一个非角色策略**：禁言与改名不能同一条提交，也不能与角色声明混用。
3. **群名不进节点公开元数据**：对端从受信任的控制声明里拿名字（DOMAIN-004 的候选只有摘要）。
4. **名字长度沿用既有 64 字节规则**（见上）；若要放宽到 80 字符，需同时改建群/改名 API 与本文档。
5. **声明可信度取决于外层日志**：本层只按本地快照做 DOMAIN-002/004 再校验，不做独立事实发现。

## 自移除提案编排（GROUPS-013，2026-10-18，Windows x64）

背景：MLS 1.0 禁止一条 Commit 移除提交者自己，唯一致命合法路径是"本机对自己发起 Remove 提案 → 群里
另一名成员把提案包进 Commit → 本机合并该 Commit（`self_removed`）"。crypto 侧三步已就绪
（`prepare_self_removal` / `process_proposal_entry` / `commit_pending_proposals`），本轮把编排接进
core/groups 与 FFI。

代码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)、[crates/ffi/src/groups_ffi.rs](../../crates/ffi/src/groups_ffi.rs)；
测试：[crates/core/tests/groups_proposal.rs](../../crates/core/tests/groups_proposal.rs)（4 个，真双设备两库）、
[crates/ffi/tests/groups_proposal_ffi.rs](../../crates/ffi/tests/groups_proposal_ffi.rs)（3 个，经 FFI）。

### 公开面

~~~text
pub fn group_prepare_leave(device, store, group_id) -> GroupResult<LeaveProposalView>
pub fn group_process_proposal(device, store, &ProposalEntryInput) -> GroupResult<ProposalOutcomeView>
pub fn group_commit_proposals(device, store, group_id) -> GroupResult<GroupView>

LeaveProposalView { group_id, base_epoch, epoch, removed, has_welcome, proposal: Vec<u8>,
                    proposal_digest, leaving_at_ms, state_version }
ProposalEntryInput { group_id, epoch, sender_identity, payload }
ProposalOutcomeView { kind, by, epoch, targets, proposals_pending, view }
~~~

`GroupView` 新增 `leaving_at_ms`；持久记录 v4 → **v5**（`leaving_at_ms` 为 `serde(default)` 字段）。

### 关键语义

1. **发起自移除（group_prepare_leave）**：群主必须先转让（`PERMISSION_DENIED`）；已解散/
   已发起过/有未决控制提交 → `INVALID_STATE_TRANSITION`。生成 MLS 提案（`device.leave_group`）
   **不推进 epoch、不产生 pending 控制提交**，本机仍在群里（能收发），快照记 `leaving_at_ms`。
2. **对端收提案（group_process_proposal）**：epoch/作者/群全部复核（`STALE_EPOCH` /
   `MISSING_LOG_ENTRIES` / `INVALID_CIPHERTEXT`）；提案进本地 proposal store 并导出到正式库；
   **epoch 不变**；返回 kind/by/epoch/targets/proposals_pending。
3. **对端打包（group_commit_proposals）**：没有待提交提案 → `NO_PENDING_PROPOSALS`；
   已有 pending → `PENDING_COMMIT_EXISTS`；生成 pending 控制提交但 **epoch 不推进**、成员不变，
   快照如实报告 `pending`（operation=remove_members、base→target）。
4. **本机合并**：复用 GROUPS-013 之前的 `group_process_peer_entry`（对端 Commit）→
   `self_removed = true`、epoch +1、本机从成员里消失；此后本机**无法加解密**（crypto fail-closed）。
5. **已解散的群**：三个新入口都显式 `INVALID_STATE_TRANSITION`。

### 错误码（既有冻结码）

`PERMISSION_DENIED`（群主发起）、`INVALID_STATE_TRANSITION`（已解散/重复发起/已有 pending）、
`STALE_EPOCH` / `MISSING_LOG_ENTRIES`（乱序）、`INVALID_CIPHERTEXT`（作者不符/认证失败）、
`DUPLICATE_PROPOSAL`、`NO_PENDING_PROPOSALS`、`PENDING_COMMIT_EXISTS`、`UNSUPPORTED_ENTRY`
（非 Remove 提案）、`INVALID_CANDIDATE`（空 payload）。

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test groups_proposal       # 4 passed
cargo test --offline -p chat-ffi --test groups_proposal_ffi    # 3 passed
cargo test --offline -p chat-core -p chat-ffi                  # 170 + 45 passed（既有 166 + 42 继续通过）
~~~

### 证据

- `groups_proposal::self_removal_proposal_end_to_end`：A 建群 + B 用 Welcome 加入 → A 转让群主给 B
  （A 合并后 epoch 2，B 用 `group_process_peer_entry` 合并同一条）→ A `group_prepare_leave`（base 2、
  removed 1、无 Welcome、摘要 64 hex、epoch/策略版本不变、无 pending、`leaving_at_ms` 有值、成员仍 2）→
  B `group_process_proposal`（kind=remove、by=alice、epoch 2、targets=[alice]、pending 1、B 的 epoch 不变）→
  B `group_commit_proposals`（epoch 仍 2、成员仍 2、pending=remove_members base2→target3 removed1）→
  A `group_process_peer_entry` 合并该提交（**self_removed=true**、epoch 3、成员只剩 bob）→
  **A 无法再加解密**（`encrypt` 显式失败）。
- `groups_proposal::owner_cannot_prepare_leave`：群主发起 → `PERMISSION_DENIED`，快照完全不变。
- `groups_proposal::duplicate_and_invalid_proposals_are_rejected`：重复发起 `INVALID_STATE_TRANSITION`；
  超前 epoch `MISSING_LOG_ENTRIES`、落后 `STALE_EPOCH`（都在解密之前拒绝，不消耗消息）；作者不符
  `INVALID_CIPHERTEXT`；无提案打包 `NO_PENDING_PROPOSALS`；未知群 `INVALID_TARGET`。
- `groups_proposal::dissolved_group_rejects_the_proposal_flow`：解散后三个入口全部 `INVALID_STATE_TRANSITION`。
- `groups_proposal_ffi::member_prepare_leave_process_and_commit_through_ffi`（经 FFI）：发起 → 重复发起被拒 →
  对端 process_proposal → 对端 commit_proposals（pending remove_members base2→target3）→ 再次打包显式失败。
- `groups_proposal_ffi::owner_prepare_leave_is_rejected_through_ffi` / `process_proposal_rejects_wrong_epoch_through_ffi`。

### 已知限制

1. **提案不能跨设备自动投递**：`proposal` 字节要由上层（消息/节点）转发；本轮只产出与校验，不投递。
2. **合并提交仍需本机主动处理**：`self_removed` 只有在本机处理那条 Commit 后才生效；在此之前本机仍能收发。
3. **对端角色不随 Commit 传播**：MLS Commit 只带成员增删，不含策略角色变化，因此"转让群主"这类纯策略
   变更需要另外的已验证控制日志同步（测试里两侧各自落地）。
4. **提交侧不校验权限**：任何确认成员都可以把已入队的提案打包成 Commit（MLS 允许）；策略层是否要收紧
   （例如只有群主/管理员能打包移除他人的提案）尚未决定。
5. **一次一条 pending**：提案打包后必须先接受/丢弃才能继续（既有语义）。
6. **节点公开元数据**：自移除提案目前不走节点日志；等接线时需要为提案/提交补 kind 映射。

## 提案投递与提交侧权限（GROUPS-013 续，2026-10-18，Windows x64）

上一轮提案字节靠测试直接传递、提交侧没有任何权限校验。本轮补上：提案经**真实节点**投递，
打包成提交前做策略层授权。

代码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)、[crates/core/src/group_node.rs](../../crates/core/src/group_node.rs)（定点新增）；
测试：[crates/core/tests/groups_proposal_submit.rs](../../crates/core/tests/groups_proposal_submit.rs)（3 个，真节点 + 真双设备两库）。

### 公开面

~~~text
pub fn group_submit_proposal(device, store, group_id, endpoint_url, node_id, node_public_key_hex)
    -> GroupResult<ProposalReceiptView>
ProposalReceiptView { group_id, submission_id, seq, log_hash, node_id, proposal_digest, state_version }

// group_node.rs
pub fn submit_proposal_entry(client, group_id, proposal: &[u8], base_epoch) -> ControlReceiptResult<ProposalSubmission>
pub fn build_node_client(store, endpoint_url, node_id, node_public_key_hex) -> ControlReceiptResult<NodeClient>
~~~

持久记录 v5 → **v6**：新增 `pending_proposal = {author, target, base_epoch, digest, proposal_hex, recorded_at_ms}`
（发起或收下提案时写入；打包成提交后清空）。

### 关键语义

1. **提交侧权限（group_commit_proposals）**：打包前依次校验——
   `pending_proposal` 存在（否则 `NO_PENDING_PROPOSALS`）；
   本机角色必须是**群主或管理员**（普通成员 → `PERMISSION_DENIED`）；
   **提案作者不能打包自己的提案**（`PERMISSION_DENIED`）；
   本机不能是提案目标（`PERMISSION_DENIED`）；
   提案目标必须仍是确认成员（否则 `INVALID_TARGET`）；
   提案 epoch 必须等于当前快照 epoch（`STALE_EPOCH`）。
   全部通过后才调 crypto；打包成功后清空 `pending_proposal`。
2. **提案投递（group_submit_proposal）**：把 `pending_proposal.proposal_hex` 作为**应用条目**写入节点日志
   （节点不解析提案内容；PROTOCOL-004 没有“独立提案”的控制操作值），回执来自 node-client 的真实签名响应，
   `submission_id` 由 (群, 提案摘要, base_epoch) 确定性派生（重复投递幂等），回执的群/提交/作者/节点身份与
   节点公钥签名全部复核；传输失败统一 `NODE_UNAVAILABLE`，**不改变本地快照**。
3. **收侧记录**：`group_process_proposal` 现在把提案的公开绑定（作者/目标/epoch/摘要/字节）写进快照，
   供提交侧授权与后续投递使用。

### 错误码

`NO_PENDING_PROPOSALS`（没有提案可提交/打包）、`PERMISSION_DENIED`（成员打包、作者打包自己的提案、
自己打包移除自己的提案）、`INVALID_TARGET`（目标不是确认成员）、`STALE_EPOCH`（提案 epoch 旧）、
`NODE_UNAVAILABLE`（节点不可达）、`INVALID_RECEIPT_SCOPE` / `INVALID_SIGNATURE`（回执绑定/验签失败）。

### 复现命令（全程离线；测试内起真实 loopback 节点）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test groups_proposal_submit   # 3 passed
cargo test --offline -p chat-core                                 # 173 passed（既有 170 继续通过）
~~~

### 证据

- `groups_proposal_submit::member_submits_proposal_and_owner_commits_it`：bob 建群（群主）+ alice 加入（成员）→
  alice `group_prepare_leave` → `group_submit_proposal` 拿到真回执（`seq >= 1`、`sub:1:…`、
  log_hash 64 hex、node_id 正确）且**节点日志恰好一条、内容就是提案字节** → bob 从日志取回提案
  `group_process_proposal`（kind=remove、targets=[alice]、pending 1）→ bob `group_commit_proposals`
  （pending=remove_members target 2）→ alice 合并（**self_removed**、epoch 2、成员只剩 bob、
  `encrypt` 显式失败）。
- `groups_proposal_submit::commit_side_permissions_are_enforced`：无提案打包 → `NO_PENDING_PROPOSALS`；
  成员（提案人）自己打包 → `PERMISSION_DENIED`；群主收下他人提案后打包成功；打包后再打包显式失败。
- `groups_proposal_submit::submitting_without_a_proposal_or_reachable_node_changes_nothing`：
  无提案投递 → `NO_PENDING_PROPOSALS`（不触网）；有提案但节点不可达 → `NODE_UNAVAILABLE` 且快照完全不变。
- 既有的 `groups_proposal::self_removal_proposal_end_to_end` 改为“bob 建群 + alice 加入”的拓扑
  （提交侧权限要求打包者是群主/管理员，而角色不会随 MLS Commit 传播——见限制）。

### 已知限制

1. **角色不随 MLS Commit 传播**：对端无法从 Commit 得知策略角色变化，因此“转让后再由新群主打包”在**对端
   记录**里仍是旧角色；本轮用例改用“群主从建群起就在本侧”的拓扑。管理员路径与群主共用同一校验分支，
   但需要角色同步才能端到端覆盖（未实现）。
2. **提案以应用条目投递**：节点不区分提案与普通消息；收侧仍会用 MLS 认证复核作者/epoch（安全边界不降级），
   但节点侧无法按提案类型审计（等 PROTOCOL-004 增加通用提案/自更新值时改造）。
3. **一次一条待提交提案**：打包后记录被清空，必须先接受/丢弃才能继续。
4. **投递不自动重试**：失败只返回稳定码，重试策略在上层（SYNC/发送队列）。
5. **提案字节仍在本地快照里**（hex），用于重试投递；对端不需要它也能处理（只靠节点日志）。

## 群列表查询（GROUPS-002 列表侧，2026-10-18，Windows x64）

代码：[crates/storage/src/formal/mls_state.rs](../../crates/storage/src/formal/mls_state.rs)（`list_mls_group_state_keys`）、
[crates/core/src/groups.rs](../../crates/core/src/groups.rs)（`group_list`）；测试：
[crates/core/tests/groups_list.rs](../../crates/core/tests/groups_list.rs)（3 个，真 OpenMLS + 真临时库）、
[crates/ffi/tests/groups_list_ffi.rs](../../crates/ffi/tests/groups_list_ffi.rs)（1 个，经 FFI）。

~~~text
// storage
pub fn list_mls_group_state_keys(prefix, limit) -> Result<Vec<MlsGroupStateKey>>
MlsGroupStateKey { group_id, state_version, updated_at_ms }
pub const MAX_GROUP_STATE_LIST: usize = 256;

// core
pub fn group_list(store) -> GroupResult<GroupListView>
GroupListView { groups: Vec<GroupSummaryView>, skipped: u32 }
GroupSummaryView { group_id, name, epoch, role, policy_version, dissolved, left, pending }
pub fn snapshot_key_for(group_id) -> String    // 诊断/测试用
~~~

### 关键语义

1. **只扫本模块命名空间**（`grpmeta/`），`limit` 必须在 `1..=256`（越界显式拒绝，不夹取），
   按 `updated_at` 倒序、同时间按 key 升序；列表**不解密**任何正文（只回键与时间）。
2. **逐条读快照做本地投影**：`group_list` 对每个键读记录（JSON 解析），不导入 MLS 设备，
   因此多群列表是纯本地轻量操作。
3. **损坏条目不静默丢弃**：解析失败/不是本模块记录/不变量不成立 → 计入 `skipped`，
   其余条目照常返回。
4. **`pending` 是"待收口线索"**：`leaving_at_ms > 0`（已发起自移除提案）或
   `pending_proposal.is_some()`（已收下待打包的提案）。**未决控制提交**的精确状态需要加载设备，
   列表视图不读 MLS——精确值请用 `group_snapshot`。
5. `epoch` 取快照记录里**写入时**的值（与 `group_snapshot` 的实时 epoch 可能相差一个未合并提交）。

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test groups_list     # 3 passed
cargo test --offline -p chat-ffi --test groups_list_ffi  # 1 passed
cargo test --offline -p chat-storage -p chat-core -p chat-ffi  # 94 + 176 + 46 passed（既有 94/173/45 继续通过）
~~~

### 证据

- `groups_list::list_reports_each_group_state_and_reflects_renames`：三个群（正常/已解散/已退出）→
  摘要 `dissolved/left/role` 全部正确；改名后列表名字立即更新，且 `policy_version/epoch` 与
  `group_snapshot` 一致。
- `groups_list::corrupted_entries_are_skipped_and_reported`：在同一命名空间塞一条非法正文记录 →
  列表只返回完好的那条、`skipped = 1`。
- `groups_list::storage_list_bounds_are_enforced`：`limit=0/257` 显式拒绝；`limit=2` 截断；
  非 `grpmeta/` 前缀为空；全量按 `updated_at` 倒序。
- `groups_list_ffi::group_list_reports_states_and_skips_corrupted_entries`（经 FFI）：损坏条目
  `skipped = 1`、两个群的名字/角色/改名正确；`shutdown` 后 `CORE_CLOSED`。

### 已知限制

1. **`pending` 不是"未决控制提交"的全集**（见语义第 4 条）；需要精确状态时按群调 `group_snapshot`。
2. **一次最多 256 个群**：超过时列表会截断（无分页游标）；真要更多群需要另外的分页键。
3. **列表不触发任何同步**：状态是最后一次本地写入的投影，未同步的对端变更不会出现。
4. **不返回成员数**：成员列表需要读整条快照（记录里有），列表页刻意只投影摘要字段。

## 策略角色随控制提交同步到对端（GROUPS-014，2026-10-18，Windows x64）

R-88 的已知限制是"角色不随 MLS Commit 传播"：转让后新群主在对端记录里仍是旧角色，因此无法打包他人的
提案。本轮把它打通：控制提交的处理入口可以带**策略语义声明**，对端据此推进自己的策略快照。

代码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)（`PeerEntryInput` 扩展 + 定点改动）；
测试：[crates/core/tests/groups_role_sync.rs](../../crates/core/tests/groups_role_sync.rs)（4 个，真双设备两库）。

### 公开面（扩展）

~~~text
PeerEntryInput {
    group_id, epoch, sender_identity, expectation, payload,
    added_members, removed_members,
    policy_intent: Option<chat_core::group_node::ControlIntent>,   // 新增
    policy_role: Option<MemberRole>,                               // 新增（SetRole 用）
}
~~~

`ControlIntent` 复用 group_node 的公开语义声明（`TransferOwnership { target_credential }` /
`SetRole { target_credential }` / 其它）。`ControlIntent::SetRole` 本身不带角色，因此额外用
`policy_role` 声明目标角色。

### 关键语义

1. **先算策略、再做 MLS 合并**：带 `policy_intent` 时，在调用 CRYPTO-006 **之前**完成
   DOMAIN-002 授权 + DOMAIN-004 派生（新 `policy_version` / `control_head`）。
   因此声明不成立（目标不是成员、缺角色、授权不足、声明的成员变化非空）时**连合并不发生**，
   快照完全不动。
2. **合并后应用**：MLS 提交合并成功后再核对真实成员变化必须为空（纯角色变更），然后把准备好的
   策略落到快照：`transfer_ownership` → 目标变 **owner**、原群主降为 **admin**；
   `set_role` → 目标变声明角色（`owner` 不允许，只能走转让）。
3. **不声明 = 保持现状**（向后兼容）：只合并 MLS 状态，角色/策略版本/控制头都不动——
   这也正是 R-88 记录里"未声明就没法跨设备治理"的原因。
4. **声明与真实提交形态不一致** → `STATE_CORRUPTED`（在合并后核对，作为第二道保险）。

### 错误码

`STATE_CORRUPTED`（声明与成员变化不一致）、`INVALID_INPUT`（SetRole 缺目标/缺角色、
声明了不支持的意图）、`INVALID_TARGET`（目标不是确认成员）、`PERMISSION_DENIED`
（声明 `owner` 角色）——以及原有 `STALE_EPOCH` / `INVALID_CIPHERTEXT` 等。

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-core --test groups_role_sync   # 4 passed
cargo test --offline -p chat-core                            # 180 passed（既有 176 继续通过）
~~~

### 证据

- `groups_role_sync::transfer_with_policy_intent_syncs_peer_roles_and_unlocks_committing`：
  A（alice 群主）转让给 B → B 处理带 `TransferOwnership` 声明的提交后 **bob = owner、
  alice = admin**、本机 role = Owner、`policy_version + 1`、`control_head` 前进、epoch 2 →
  **随后 B 成功打包 alice 的离开提案**（pending remove_members base 2 → target 3）→
  alice 合并后 `self_removed` 且不能加密。（这就是 R-88 限制的端到端解锁。）
- `groups_role_sync::without_a_declaration_peer_roles_stay_unchanged`：同一提交不带声明 →
  epoch 前进但角色/策略版本/控制头**全不变**，且 bob 打包他人提案仍 `PERMISSION_DENIED`
  （向后兼容 + 旧限制可复现）。
- `groups_role_sync::inconsistent_declarations_are_rejected_without_changing_the_snapshot`：
  声明转让但声明了成员变化 → 拒绝且快照完全不变；`SetRole` 目标不是成员 → `INVALID_TARGET`；
  `SetRole` 缺角色 → `INVALID_INPUT`；三次都**没有发生 MLS 合并**（快照逐字段相等）。
- `groups_role_sync::set_role_declaration_syncs_the_target_role`：群主把 bob 设为 admin →
  bob 侧合并带 `SetRole` 声明的提交后 bob = admin、alice 仍是 owner、`policy_version + 1`。

### 已知限制

1. **声明由调用方给，可信度取决于外层日志**：`policy_intent` 必须由已验证的控制元数据提供
   （节点日志/协调者回执）；本层只按本地快照做 DOMAIN-002/004 的**再校验**，不做独立的事实发现。
2. **只同步角色**：邀请/移除成员的成员集合同步走既有 `added_members/removed_members`；
   禁言（`muted`）与群名仍不在提交里传播。
3. **转让后原群主降级为 admin 是本切片的公开语义**（DOMAIN-004 的候选里是普通成员）；
   如果策略层要改成"降为 member"，需要同时改这里与节点元数据语义。
4. **声明必须在合并前成立**：目标在声明时还不是确认成员就整体失败（不会"合并后再补"）。
5. **一次提交只同步一个策略操作**（声明是单个 `ControlIntent`）。




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

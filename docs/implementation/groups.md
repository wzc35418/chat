# 小群创建、邀请与治理用例

阶段：P3。状态：函数级设计；GROUPS-001 建群与 GROUPS-002 快照已实现（真实 OpenMLS + 正式加密库），其余治理函数未开始。目标源码：[crates/core/src/groups.rs](../../crates/core/src/groups.rs)。

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

- [ ] 旧 UI 任免按最新角色重新校验。
- [ ] 同 ID 不同 op 拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 接受后断网恢复。
- [ ] 重复接受同 ticket。
- [ ] 无 Welcome 不假称可解密。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 旧 head 重建显示同业务意图新尝试。
- [ ] 节点离线永不 Accepted。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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

- [ ] 节点存储搜索不到测试群名。
- [ ] 未确认 UI 保留旧正式名。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

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
5. **角色只区分 Owner/Member**：Admin 与治理操作属 GROUPS-003~012（未开始）。
6. **成员身份是调用方声明的凭据文本**：MLS 侧真实凭据校验在 `prepare_control` 内部完成；本层不做跨账户目录核对。



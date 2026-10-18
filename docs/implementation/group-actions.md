# 逐项群操作的参数与授权分支

阶段：P3。状态：函数级设计，尚未实现。目标源码：[crates/core/src/group_actions.rs](../../crates/core/src/group_actions.rs)。

这些是 GroupService.propose 的内部动作处理器，不是另开一套绕过 generic propose 的入口。所有动作先 authorize，再生成单一操作批次。客户端和节点共享公开授权规则；客户端额外核验 MLS 实际成员变化。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [GROUP-ACTIONS-001 invite_group_member](group-actions.md#group-actions-001)
- [GROUP-ACTIONS-002 set_group_role](group-actions.md#group-actions-002)
- [GROUP-ACTIONS-003 set_group_muted](group-actions.md#group-actions-003)
- [GROUP-ACTIONS-004 sync_account_device_change](group-actions.md#group-actions-004)
- [GROUP-ACTIONS-005 rejoin_stale_device](group-actions.md#group-actions-005)

<a id="group-actions-001"></a>

## GROUP-ACTIONS-001 · invite_group_member

~~~text
invite_group_member(group: GroupId, target: AccountId, expected: ControlHead, operation: OperationId) -> Result<OperationTicket>
~~~

**职责与输出**：向普通新成员发受限邀请，接受后才加其全部获准设备。

**输入与前置条件**：群主/管理员；target 不在群；容量可用；对方是已接受联系人。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证角色与目标。
2. 建立带群/设备/期限的邀请。
3. 保存待对方接受票据。
4. 发授权邀请。
5. 等接受后走 prepare。

**持久化、并发与副作用**：T1 保存邀请和任务；不能先提交 Add Commit。

**失败处理**：PERMISSION_DENIED，ALREADY_MEMBER，GROUP_LIMIT_EXCEEDED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[GROUPS-003 propose_group_operation](groups.md#groups-003)。群邀请单独签发 group scope，联系人邀请能力不能直接复用。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [ ] 邀请中无法指定 ADMIN/OWNER。
- [ ] 对方拒绝不产生 MLS 成员。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="group-actions-002"></a>

## GROUP-ACTIONS-002 · set_group_role

~~~text
set_group_role(group: GroupId, target: AccountId, role: GroupRole, expected: ControlHead) -> Result<OperationTicket>
~~~

**职责与输出**：仅群主任免管理员，目标 owner 变更必须另走转让。

**输入与前置条件**：actor 是当前群主；role 只能 ADMIN/MEMBER；target 是普通或管理员成员。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证 owner。
2. 拒绝 target 为 owner/role OWNER。
3. 校验目标仍在群。
4. 构建 SetRole。
5. 确认后刷新正式角色。

**持久化、并发与副作用**：T1/T2/T3 控制路径；角色变化不允许附带 MLS 成员增删。

**失败处理**：PERMISSION_DENIED，INVALID_ROLE_CHANGE，STALE_CONTROL_HEAD。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[GROUPS-003 propose_group_operation](groups.md#groups-003)、[DOMAIN-005 verify_control_result](domain.md#domain-005)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 管理员不能给自己/别人升权。
- [ ] 群主不能用 SetRole 让群无主。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="group-actions-003"></a>

## GROUP-ACTIONS-003 · set_group_muted

~~~text
set_group_muted(group: GroupId, target: AccountId, muted: bool, expected: ControlHead) -> Result<OperationTicket>
~~~

**职责与输出**：群主/管理员禁言普通成员，生效点为正式 seq。

**输入与前置条件**：target 必须 MEMBER；原权限表不含对管理员禁言，MVP 统一拒绝。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验目标角色和 actor。
2. 与现值相同可幂等返回。
3. 构建 SetMuted。
4. 正式控制确认后新发送按新策略验证。

**持久化、并发与副作用**：T1/T2/T3；不能用作者时间决定禁言前后。

**失败处理**：PERMISSION_DENIED，INVALID_TARGET，STALE_CONTROL_HEAD。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[GROUPS-003 propose_group_operation](groups.md#groups-003)、[DOMAIN-001 validate_send_permission](domain.md#domain-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 普通成员不能禁言自己/别人。
- [ ] 旧密文预传不越过正式禁言序号。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="group-actions-004"></a>

## GROUP-ACTIONS-004 · sync_account_device_change

~~~text
sync_account_device_change(group: GroupId, proof: AccountDeviceChangeProof) -> Result<OperationTicket>
~~~

**职责与输出**：把主管签发的本账户设备增删映射为受约束 MLS 更新。

**输入与前置条件**：证明签名合法；账户仍是该群成员；提交设备仍有效；容量足够。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对账户主管签名和单调设备版本。
2. 核对仅改该账户设备。
3. 新增包验签。
4. 新增/撤销构建单操作。
5. 更新父任务进度。

**持久化、并发与副作用**：T1/T2/T3；账户已被移除不能借此重加；无有效旧设备需成员授权重新加入。

**失败处理**：INVALID_DEVICE_CHANGE_PROOF，NOT_MEMBER，DEVICE_LIMIT_EXCEEDED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)、[CRYPTO-003 validate_key_package](crypto.md#crypto-003)、[GROUPS-003 propose_group_operation](groups.md#groups-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 成员不能签别人的设备。
- [ ] 撤销通知不能携带新管理员角色。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="group-actions-005"></a>

## GROUP-ACTIONS-005 · rejoin_stale_device

~~~text
rejoin_stale_device(group: GroupId, device: DeviceId, consent: MemberAuthorization) -> Result<GroupJoinTicket>
~~~

**职责与输出**：日志不可补齐时由当前获准成员重新授权加入。

**输入与前置条件**：不能让节点直接提供明文群钥；设备凭据有效，权限策略允许当前成员邀请。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 证明缺口不可恢复。
2. 请求有权成员邀请。
3. 生成新一次性包。
4. 清理已失效待加入状态。
5. 等正式 Commit/Welcome。
6. 记录新加入起点。

**持久化、并发与副作用**：旧加密状态保留隔离直到新流程完成；不声称恢复旧历史。

**失败处理**：REJOIN_AUTHORIZATION_REQUIRED，INVALID_CREDENTIAL，GROUP_LIMIT_EXCEEDED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[GROUPS-004 accept_group_invite](groups.md#groups-004)、[CRYPTO-009 join_from_welcome](crypto.md#crypto-009)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 长期离线设备不能任意跳 epoch。
- [ ] 未获邀请拒绝重新加入。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。


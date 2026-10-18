# 设备直连、中继与节点客户端

阶段：P0 → P1。状态：函数级设计，尚未实现。目标源码：[crates/transport/src/network.rs](../../crates/transport/src/network.rs)。

网络层传送相同不可变协议帧，不负责判断已送达。节点公钥与 HTTPS 身份均验证；用户配置明确替换公共发现/中继。连接句柄不可给 UI 或插件。认证细节在 node-api 的函数计划中定义。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [TRANSPORT-001 start_endpoint](transport.md#transport-001)
- [TRANSPORT-002 verify_discovery_record](transport.md#transport-002)
- [TRANSPORT-003 connect_peer](transport.md#transport-003)
- [TRANSPORT-004 send_peer_frame](transport.md#transport-004)
- [TRANSPORT-005 read_peer_frame](transport.md#transport-005)
- [TRANSPORT-006 append_candidate](transport.md#transport-006)
- [TRANSPORT-007 fetch_group_entries](transport.md#transport-007)
- [TRANSPORT-008 put_mailbox](transport.md#transport-008)
- [TRANSPORT-009 pull_mailbox](transport.md#transport-009)
- [TRANSPORT-010 ack_mailbox](transport.md#transport-010)
- [TRANSPORT-011 subscribe_node_hints](transport.md#transport-011)
- [TRANSPORT-012 shutdown_transport](transport.md#transport-012)

<a id="transport-001"></a>

## TRANSPORT-001 · start_endpoint

~~~text
start_endpoint(config: TransportConfig, device_key: DeviceKeyRef) -> Result<EndpointHandle>
~~~

**职责与输出**：启动 iroh 端点并返回受控句柄。

**输入与前置条件**：P0 验证目标 ABI；密钥已保护；公共 fallback 默认显式策略。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证配置。
2. 装载设备端点密钥。
3. 建立运行时。
4. 注册有界入站队列。
5. 记录脱敏端点状态。

**持久化、并发与副作用**：启动可取消；失败关闭已创建任务；不写消息状态。

**失败处理**：INVALID_CONFIG，ENDPOINT_START_FAILED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 关闭公共服务后仍使用自托管。
- [ ] 端口冲突无遗留任务。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-002"></a>

## TRANSPORT-002 · verify_discovery_record

~~~text
verify_discovery_record(record: SignedDiscoveryRecord, credential: VerifiedCredential, cached_version: u64) -> Result<VerifiedPeerAddress>
~~~

**职责与输出**：验证设备地址版本、防旧记录回滚与期限。

**输入与前置条件**：记录携带设备签名，地址/URL受策略限制。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证签名。
2. 核对端点绑定。
3. 检查期限。
4. 比较单调版本。
5. 剔除未授权地址。
6. 输出已验证地址。

**持久化、并发与副作用**：只在验证后更新地址缓存；不能顺便更新信任根。

**失败处理**：INVALID_SIGNATURE，DISCOVERY_ROLLBACK，RECORD_EXPIRED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重放旧版本拒绝。
- [ ] 伪造局域网地址不能绑定他人设备。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-003"></a>

## TRANSPORT-003 · connect_peer

~~~text
connect_peer(peer: VerifiedPeerAddress, budget: ConnectBudget) -> Result<Connection>
~~~

**职责与输出**：按受支持路径尝试直连/穿透/中继。

**输入与前置条件**：peer 已验证；尝试共享总超时，禁止无限并发探测。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 尝试已知局域网/直接地址。
2. 进行库支持的穿透。
3. 失败按配置使用中继。
4. 验证对端 EndpointId。
5. 输出路径诊断。

**持久化、并发与副作用**：网络异步；取消关闭所有竞赛连接；不持 DB tx。

**失败处理**：NETWORK_UNAVAILABLE，PEER_UNREACHABLE，ENDPOINT_MISMATCH。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[TRANSPORT-002 verify_discovery_record](transport.md#transport-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 对称 NAT 回退。
- [ ] 中继不可用明确错误。
- [ ] 取消无后台重试泄漏。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-004"></a>

## TRANSPORT-004 · send_peer_frame

~~~text
send_peer_frame(connection: Connection, frame: ImmutableFrame) -> Result<TransportAck>
~~~

**职责与输出**：传输确认只表示字节接收。

**输入与前置条件**：帧已完成规范编码/签名，长度符合双方协商。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验长度。
2. 写入帧和边界。
3. 等待有限 ACK。
4. 验证连接关联。
5. 返回纯传输证据。

**持久化、并发与副作用**：不改变 ACCEPTED/DELIVERED；网络重试不能改密文。

**失败处理**：TRANSPORT_TIMEOUT，CONNECTION_CLOSED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-011 negotiate_capabilities](protocol.md#protocol-011)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] ACK 丢失后重发相同 bytes。
- [ ] P2P 待确认消息不触发正式插件行为。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-005"></a>

## TRANSPORT-005 · read_peer_frame

~~~text
read_peer_frame(connection: Connection, limits: FrameLimits) -> Result<UnverifiedFrame>
~~~

**职责与输出**：输出有界未认证帧交给核心验证。

**输入与前置条件**：连接身份只证明端点，不代表应用事件授权。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读长度头。
2. 预拒绝超限。
3. 按预算读取。
4. 有界解析。
5. 投递入站队列。
6. 队列满施加背压。

**持久化、并发与副作用**：不写正式 inbox 去重；断开时保留已耐久队列由上层处理。

**失败处理**：MALFORMED_FRAME，CONTENT_TOO_LARGE，BACKPRESSURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-003 decode_bounded](protocol.md#protocol-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 超大长度声明不分配巨内存。
- [ ] 伪造 EventId 不被提前去重。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-006"></a>

## TRANSPORT-006 · append_candidate

~~~text
append_candidate(node: VerifiedNode, candidate: ImmutableCandidate) -> Result<AppendOutcome>
~~~

**职责与输出**：返回正式签名回执、明确拒绝或结果未知。

**输入与前置条件**：固定 SubmissionId/候选字节；设备请求鉴权有效。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 规范请求并签名。
2. 带限时发送。
3. 有界读响应。
4. 校验回执绑定。
5. 超时返回 Unknown 而非 Rejected。

**持久化、并发与副作用**：无 DB tx；本地 worker 负责耐久记录结果。

**失败处理**：NODE_UNAVAILABLE，STALE_EPOCH，STALE_CONTROL_HEAD，ACCEPTANCE_UNKNOWN。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-007 verify_log_entry](protocol.md#protocol-007)。客户端用注入 RequestSigner 生成签名；authenticate_request 是节点端验签函数，transport 不调用服务器实现。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [ ] 响应丢失按同 ID 重新 append 得原结果。
- [ ] 篡改回执拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-007"></a>

## TRANSPORT-007 · fetch_group_entries

~~~text
fetch_group_entries(node: VerifiedNode, group: GroupId, after: u64, limit: u32) -> Result<UnverifiedLogPage>
~~~

**职责与输出**：有界拉取连续日志材料，正式验证归 sync。

**输入与前置条件**：节点授权设备属于允许可见范围。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 签名请求。
2. 限制条数/响应字节。
3. 读取页。
4. 核对群与起点。
5. 返回未验证条目。

**持久化、并发与副作用**：不提前推进本地 verified cursor。

**失败处理**：NODE_UNAVAILABLE，HISTORY_NOT_AUTHORIZED，MALFORMED_RESPONSE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-003 decode_bounded](protocol.md#protocol-003)。客户端用注入 RequestSigner 生成签名；authenticate_request 是节点端验签函数，transport 不调用服务器实现。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [ ] 节点跳号返回给 sync 检测。
- [ ] 巨响应截断为错误。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-008"></a>

## TRANSPORT-008 · put_mailbox

~~~text
put_mailbox(item: AuthorizedMailboxItem) -> Result<MailboxReceipt>
~~~

**职责与输出**：返回节点持久信箱 ACK，不能显示已送达。

**输入与前置条件**：目标设备/能力/TTL明确，payload 只含密文与必要路由。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 签名请求。
2. 写目标条目。
3. 验证 item 摘要回执。
4. 输出 MailboxStored 类型。

**持久化、并发与副作用**：网络重试同 item ID 与同摘要。

**失败处理**：INVALID_CAPABILITY，QUOTA_EXCEEDED，NODE_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或注入端口。客户端用注入 RequestSigner 生成签名；authenticate_request 是节点端验签函数，transport 不调用服务器实现。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [ ] 过期能力拒绝。
- [ ] ACK 后收件人离线仍不是 delivered。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-009"></a>

## TRANSPORT-009 · pull_mailbox

~~~text
pull_mailbox(cursor: Option<MailboxCursor>, limit: u32) -> Result<MailboxPage>
~~~

**职责与输出**：获取当前设备信箱条目，保持稳定翻页位置。

**输入与前置条件**：鉴权设备固定；cursor 不跨设备。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验游标。
2. 签名请求。
3. 有界读取。
4. 核对 target device。
5. 返回条目与游标候选。

**持久化、并发与副作用**：获取成功不删除条目、不推进正式群游标。

**失败处理**：INVALID_CURSOR，DEVICE_REVOKED，NODE_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-013 decode_page_cursor](protocol.md#protocol-013)。客户端用注入 RequestSigner 生成签名；authenticate_request 是节点端验签函数，transport 不调用服务器实现。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [ ] WS 丢失后仍完整拉取。
- [ ] 重复页上层幂等处理。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-010"></a>

## TRANSPORT-010 · ack_mailbox

~~~text
ack_mailbox(ids: Vec<MailboxItemId>, proof: LocalPersistProof) -> Result<()>
~~~

**职责与输出**：本地验证并耐久落盘后确认信箱消费。

**输入与前置条件**：proof 不由传输 ACK 生成；条目均归当前设备。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 限制批次。
2. 绑定 IDs 和设备签名。
3. 发送 ACK。
4. 成功记录可重试完成状态。

**持久化、并发与副作用**：与本地已提交消费记录配合；ACK 失败可重复发送。

**失败处理**：PERMISSION_DENIED，NODE_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或注入端口。客户端用注入 RequestSigner 生成签名；authenticate_request 是节点端验签函数，transport 不调用服务器实现。 依赖端口与代码边界见 [依赖说明](DEPENDENCIES.md)。

**完成验收**：

- [ ] 先 crash 再 ACK 仍能恢复。
- [ ] 他设备 item 不可确认。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-011"></a>

## TRANSPORT-011 · subscribe_node_hints

~~~text
subscribe_node_hints(token: OneTimeWsToken) -> Result<HintStream>
~~~

**职责与输出**：仅提供有更新提示，丢失提示由游标同步兜底。

**输入与前置条件**：短期一次性 WS token，不把长期秘密放 URL。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 升级连接。
2. 消费一次性 token。
3. 创建有界队列。
4. 合并重复提示。
5. 断线触发重新拉取。

**持久化、并发与副作用**：提示非权威存储；溢出触发 resync 标记。

**失败处理**：WS_AUTH_EXPIRED，NETWORK_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 提示重复/乱序不重复消息。
- [ ] token 重用拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="transport-012"></a>

## TRANSPORT-012 · shutdown_transport

~~~text
shutdown_transport(handle: EndpointHandle, deadline: Duration) -> Result<ShutdownReport>
~~~

**职责与输出**：关闭端点、WS 和网络任务并报告超时。

**输入与前置条件**：允许重复调用，不能再接收新任务。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 标记 draining。
2. 停止连接建立。
3. 取消流。
4. 等待有界完成。
5. 释放密钥引用和运行时。

**持久化、并发与副作用**：未完成可靠任务由 outbox 下次恢复。

**失败处理**：SHUTDOWN_TIMEOUT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重复 shutdown 不崩溃。
- [ ] 睡眠恢复用新连接。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。


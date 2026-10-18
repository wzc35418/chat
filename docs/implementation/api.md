# 节点 HTTP、鉴权、限流与运行入口

阶段：P1 → P5。状态：函数级设计。API-002 issue_auth_challenge 与 API-003 authenticate_request 已随 P1-03 在 [crates/node-api](../../crates/node-api) 与 [crates/node-services](../../crates/node-services) 落地（端点见“已实现端点（P1-03）”），其余函数仍未实现。目标源码：[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)。

所有 HTTP DTO 与错误结构单独版本化。公开 /capabilities 与 challenge 也限流。默认监听本地端口，通过明确的 TLS 部署入口提供 HTTPS/WSS；管理接口独立且默认关闭。每个 HTTP handler 只鉴权、解码、调用服务、映射结果，不重复实现领域规则。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 已实现端点（P1-03）

路由由 [crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs) 的 `build_router` 构造；DTO 与错误结构见 [dto.rs](../../crates/node-api/src/dto.rs)，状态映射见 [error.rs](../../crates/node-api/src/error.rs)。

|方法/路径|作用|鉴权|
|---|---|---|
|`POST /v1/auth/challenge`|签发一次性短期挑战（API-002）|匿名，受全局未消费挑战容量上限约束|
|`POST /v1/devices`|设备注册（NODE-001，需本文件的 API-003 鉴权）|设备签名 + 主管钥签发的凭据与注册能力|

请求/响应均为 JSON，带 `schema_version`（当前只接受 1，未知版本 400 `UNSUPPORTED_SCHEMA_VERSION`）。

- 挑战请求：`{"schema_version":1,"purpose":"device_registration"|"request_auth"}`；响应为 `{"schema_version":1,"challenge":AuthChallenge}`，挑战含 `challenge_id/nonce/node_id/purpose/issued_at/expires_at/node_public_key/signature`，节点签名覆盖 `node_id + node_public_key + challenge_id + nonce + 用途 + 签发时间 + 过期时间`。
- 注册请求：`{"schema_version":1,"account_id","credential","capability"}`（`credential` 为账户主管钥签发的设备凭据，`capability` 为只允许登记该设备的一次性注册能力）；响应为 `{"schema_version":1,"registration":DeviceRegistration}`，回执由节点签名。
- 鉴权头：`x-chat-device-id`、`x-chat-challenge-id`、`x-chat-challenge-nonce`、`x-chat-signature`。签名原文固定为 `域分隔 + node_id + device_id + 用途 + 大写 method + 规范 path + query + sha256(正文) + challenge nonce`，与 API-003 处理步骤 1 的规范化要求一致；path 拒绝百分号编码、`?`/`#`、反斜杠与 `.`/`..` 段，保证线上原文与签名原文只有唯一写法。
- 正文上限：64 KiB（`limits::MAX_HTTP_BODY_BYTES`，可用 `NodeAuthPolicy.max_request_body_bytes` 收紧）；超出返回 413 `PAYLOAD_TOO_LARGE`。
- 设备上限：每账户默认 2 台有效设备（`limits::DEFAULT_MAX_DEVICES_PER_ACCOUNT`，与产品口径一致；`NodeAuthPolicy.max_devices_per_account` 可配）；达到上限返回 409 `DEVICE_LIMIT_EXCEEDED`，不静默替换或截断旧设备。
- 错误体：`{"schema_version":1,"error":{"code","message"}}`；状态映射：400 INVALID_REQUEST/UNSUPPORTED_SCHEMA_VERSION，401 INVALID_CHALLENGE/NODE_ID_MISMATCH/AUTH_EXPIRED/AUTH_REPLAY/INVALID_SIGNATURE/INVALID_CREDENTIAL，403 DEVICE_REVOKED/INVALID_CAPABILITY/PERMISSION_DENIED，409 DEVICE_LIMIT_EXCEEDED，413 PAYLOAD_TOO_LARGE，429 RATE_LIMITED，503 CHALLENGE_CAPACITY_EXCEEDED/STORAGE_FAILURE，500 MIGRATION_FAILED/NODE_IDENTITY_MISMATCH/INVALID_CONFIG/ENCODING_FAILURE。

本阶段**没有**实现：API-005 逐 IP 限流（`RATE_LIMITED` 只预留错误码）、API-009 事件套接字、API-010 run_node（[apps/node](../../apps/node) 仍是明确 78 退出的骨架）、TLS 终止与管理接口。

## 函数导航

- [API-001 load_node_config](api.md#api-001)
- [API-002 issue_auth_challenge](api.md#api-002)
- [API-003 authenticate_request](api.md#api-003)
- [API-004 validate_capability](api.md#api-004)
- [API-005 enforce_rate_limit](api.md#api-005)
- [API-006 handle_append_http](api.md#api-006)
- [API-007 handle_mailbox_http](api.md#api-007)
- [API-008 handle_blob_http](api.md#api-008)
- [API-009 upgrade_event_socket](api.md#api-009)
- [API-010 run_node](api.md#api-010)

<a id="api-001"></a>

## API-001 · load_node_config

~~~text
load_node_config(path: ConfigPath, env: AllowedOverrides) -> Result<NodeConfig>
~~~

**职责与输出**：读取、验证限额与持久目录，拒绝不完整生产配置。

**输入与前置条件**：只允许明确 env 覆盖项；敏感配置不打印。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 有界解析 TOML。
2. 校验 bind/external URL。
3. 验证目录权限。
4. 检查限额乘法溢出。
5. 确认节点 key 来源与 TLS 模式。

**持久化、并发与副作用**：启动只读；错误不创建监听或空数据库冒充正常。

**失败处理**：INVALID_CONFIG，INSECURE_STORAGE_PATH。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 缺持久卷配置拒绝生产启动。
- [ ] 非法负限额拒绝。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="api-002"></a>

## API-002 · issue_auth_challenge

~~~text
issue_auth_challenge(client_hint: ClientHint, now: Time) -> Result<AuthChallenge>
~~~

**职责与输出**：生成节点签名的一次性短期挑战。

**输入与前置条件**：匿名 endpoint 有 IP/全局速率和库存上限。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 安全随机 nonce。
2. 绑定 node_id/用途/期限。
3. 存 challenge 状态。
4. 签名返回。
5. 限制同源未消费数量。

**持久化、并发与副作用**：短事务持久化或同样可靠状态机制；不能跨重启重放旧挑战。

**失败处理**：RATE_LIMITED，CHALLENGE_CAPACITY_EXCEEDED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 挑战过期拒绝。
- [x] 高并发不会无限分配内存。

**实现状态**：已实现（P1-03）。源码：[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs) 的 `NodeService::issue_auth_challenge`，类型与签名原文在 [auth.rs](../../crates/node-services/src/auth.rs)，HTTP 在 [crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs) 的 `issue_challenge`。

行为：32 字节 CSPRNG nonce + 随机 challenge_id；挑战绑定 `node_id + 用途 + issued_at + expires_at` 并用节点私钥签名；只持久化 nonce 的 SHA-256（复用 0001 的 `auth_challenges.nonce_hash`），短事务写入并限制同源未消费数量；每次签发按保留窗口有界清理过期挑战（单批最多 256 行，不整表删除）。

证据：`cargo test --offline -p chat-node-services -p chat-node-api` 全绿；过期拒绝由 `expired_challenge_is_rejected`（服务层）与 `expired_challenge_is_rejected`（HTTP 端到端）覆盖；有界性由 `malformed_and_oversized_inputs_are_rejected_with_bounded_errors`（容量上限 `CHALLENGE_CAPACITY_EXCEEDED`）与 `concurrent_challenge_issuance_stays_within_capacity`（4 线程并发签发，成功数恒等于容量上限）覆盖；挑战由节点签名的证据是 `challenge_endpoint_returns_signed_short_lived_challenge`（用固定节点公钥复核签名）。

已知限制：逐 IP 限流（API-005）未实现，当前只有全局容量上限；挑战只存 nonce 摘要，客户端必须回送 nonce。通过本函数用例不自动代表整个模块完成。

<a id="api-003"></a>

## API-003 · authenticate_request

~~~text
authenticate_request(meta: RequestMeta, body_hash: Digest, auth: SignedAuth, now: Time) -> Result<AuthenticatedDevice>
~~~

**职责与输出**：绑定方法/规范路径/查询/摘要/节点/挑战/期限并验设备签名。

**输入与前置条件**：请求正文先受限流式读取；设备凭据有效；不能仅传 accountId 登录。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 规范化 method/path/query。
2. 核对 node_id 和 challenge。
3. 验证设备签名/撤销状态。
4. 原子消费 challenge。
5. 返回 actor。

**持久化、并发与副作用**：消费挑战短事务；业务失败重试需新挑战但沿用业务 operation/submission ID。

**失败处理**：AUTH_REPLAY，AUTH_EXPIRED，INVALID_SIGNATURE，DEVICE_REVOKED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)、[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 换方法/查询/body/节点重放失败。
- [x] 同挑战并发只成功一次。

**实现状态**：已实现（P1-03）。源码：[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs) 的 `NodeService::authenticate_request`（已登记设备路径）与 `NodeService::verify_device_signature`（设备尚未入库的注册路径，返回只可由本模块构造的 `VerifiedRequestProof`），签名原文在 [auth.rs](../../crates/node-services/src/auth.rs)，HTTP 在 [crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs) 的 `register_device`。

行为：先规范化 method/path/query，再核对 `node_id`、用途与 challenge nonce 摘要，然后用设备公钥严格验签（`verify_strict`）并在同一 IMMEDIATE 事务内以 `UPDATE ... WHERE consumed_at IS NULL` 原子消费挑战；失败一律返回显式错误，错误签名不消费挑战（不影响合法客户端重试）。已登记设备额外校验 active 状态与凭据版本，返回 `AuthenticatedDevice`。

证据：`cargo test --offline -p chat-node-services -p chat-node-api` 全绿。换方法/路径/查询/正文摘要/节点：`signature_is_bound_to_method_path_query_body_and_node`（服务层，5 种绑定变体）与 `signature_is_bound_to_method_path_body_and_node`（HTTP 端到端）；重放：`second_use_of_the_same_challenge_is_rejected`、`replayed_challenge_is_rejected`；并发只成功一次：`concurrent_reuse_of_one_challenge_succeeds_at_most_once`（4 线程同一挑战，成功数恒为 1）；过期、错误密钥、未登记设备与畸形输入分别由 `expired_challenge_is_rejected`、`wrong_device_key_is_rejected_without_consuming_the_challenge`、`registered_device_request_auth_returns_actor_and_rejects_reuse`、`malformed_and_oversized_inputs_are_rejected_with_bounded_errors` 覆盖。

已知限制：签名原文覆盖 path 与 query，但不覆盖 HTTP 头；节点不解析代理头（`X-Forwarded-For` 不参与任何判断）。通过本函数用例不自动代表整个模块完成。

<a id="api-004"></a>

## API-004 · validate_capability

~~~text
validate_capability(token: Capability, actor: Actor, resource: Resource, action: Action, now: Time) -> Result<AuthorizedScope>
~~~

**职责与输出**：校验投递/注册/claim/blob 权限，不采用 token 自称权限。

**输入与前置条件**：发行者公钥可信，资源/动作/持有人/期限/版本明确。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 限长解析。
2. 验签。
3. 检查发行者。
4. 匹配 holder/resource/action。
5. 检查期限/撤销版本。
6. 返回最小范围。

**持久化、并发与副作用**：纯验证与只读撤销查询；单次能力消费在业务事务。

**失败处理**：INVALID_CAPABILITY，CAPABILITY_EXPIRED，CAPABILITY_REVOKED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-003 decode_bounded](protocol.md#protocol-003)、[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 联系人邀请 token 不能用于群管理。
- [ ] 一个 blob token 不能读另一 blob。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="api-005"></a>

## API-005 · enforce_rate_limit

~~~text
enforce_rate_limit(actor: Option<DeviceId>, route: RouteClass, cost: RequestCost, now: Time) -> Result<Permit>
~~~

**职责与输出**：按设备/IP/全局限制请求和字节成本。

**输入与前置条件**：匿名挑战与注册单独限额；可信代理头策略明确。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 取预算键。
2. 安全累计请求/字节。
3. 检查突发和持续配额。
4. 原子扣减。
5. 返回 retryAfter。

**持久化、并发与副作用**：进程内或持久计数按策略；重启不能成为无限配额绕过方式。

**失败处理**：RATE_LIMITED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 伪造 X-Forwarded-For 无效。
- [ ] 大上传按字节计而非仅请求数。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="api-006"></a>

## API-006 · handle_append_http

~~~text
handle_append_http(request: HttpRequest) -> HttpResponse
~~~

**职责与输出**：POST /v1/groups/{id}/entries 到排序服务的单一适配。

**输入与前置条件**：content-type/主版本/请求长度明确。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 先 route/字节限额。
2. 解析 bounded DTO。
3. 鉴权。
4. 路由 group 与 body 一致。
5. 调用 append。
6. 映射签名回执或稳定错误。

**持久化、并发与副作用**：不在 handler 另开与 service 冲突的独立事务。

**失败处理**：HTTP 401/403/409/413/429/503 映射稳定错误码，不泄漏正文。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-003 decode_bounded](protocol.md#protocol-003)、[API-003 authenticate_request](api.md#api-003)、[API-005 enforce_rate_limit](api.md#api-005)、[NODE-006 append_group_entry](node.md#node-006)、[PROTOCOL-014 map_error](protocol.md#protocol-014)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 409 stale head 与403权限可区分。
- [ ] 相同 submission 异字节409。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="api-007"></a>

## API-007 · handle_mailbox_http

~~~text
handle_mailbox_http(request: HttpRequest) -> HttpResponse
~~~

**职责与输出**：分派 POST item、GET page、POST ack 并保持 actor 绑定。

**输入与前置条件**：每路由 schema 独立，不允许客户端指定查询其他 device。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 限额。
2. 鉴权。
3. 校验参数。
4. 选择 store/read/ack 服务。
5. 输出分页和存储证据。

**持久化、并发与副作用**：事务全部由 node service 控制。

**失败处理**：INVALID_CURSOR，INVALID_CAPABILITY，QUOTA_EXCEEDED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[API-003 authenticate_request](api.md#api-003)、[NODE-009 store_mailbox_item](node.md#node-009)、[NODE-010 read_mailbox_page](node.md#node-010)、[NODE-011 ack_mailbox_items](node.md#node-011)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] GET 不删除。
- [ ] ACK 只能本人。
- [ ] 超响应预算分页而非截断密文。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="api-008"></a>

## API-008 · handle_blob_http

~~~text
handle_blob_http(request: HttpRequest) -> HttpResponse
~~~

**职责与输出**：分派上传创建/块/完成/下载，正文流式有界。

**输入与前置条件**：只允许密文上传；用户路径名不参与服务器文件定位。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验 route ID/index。
2. 验证设备和能力。
3. 限制字节流。
4. 调用分块服务。
5. 输出摘要回执。

**持久化、并发与副作用**：长上传不持全局 DB tx；取消触发临时文件恢复策略。

**失败处理**：CONTENT_TOO_LARGE，CHUNK_CONFLICT，INVALID_CAPABILITY。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[NODE-012 begin_blob_upload](node.md#node-012)、[NODE-013 put_blob_chunk](node.md#node-013)、[NODE-014 complete_blob_upload](node.md#node-014)、[NODE-015 read_blob_chunk](node.md#node-015)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 路径穿越输入拒绝。
- [ ] 网络取消无 published 半对象。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="api-009"></a>

## API-009 · upgrade_event_socket

~~~text
upgrade_event_socket(request: HttpRequest, token: OneTimeWsToken) -> Result<Socket>
~~~

**职责与输出**：建立只提示更新的 WS，不通过 URL 携带长期秘密。

**输入与前置条件**：一次性 token 由签名 HTTP 流程签发并绑定设备/期限。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对 origin/设备/token。
2. 原子消费。
3. 设置队列预算。
4. 推合并提示。
5. 慢消费者断开并说明需同步。

**持久化、并发与副作用**：短期 token 单次消费；WS 事件不替代持久日志。

**失败处理**：AUTH_REPLAY，WS_AUTH_EXPIRED，BACKPRESSURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 断线丢提示仍能补齐。
- [ ] 日志中 URL 无长期凭据。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="api-010"></a>

## API-010 · run_node

~~~text
run_node(config: NodeConfig, shutdown: ShutdownSignal) -> Result<NodeExit>
~~~

**职责与输出**：启动数据库、服务、HTTP、后台 GC 和优雅关闭。

**输入与前置条件**：配置已经验证；节点签名钥加载成功。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 打开持久库并恢复 staged 文件。
2. 创建服务依赖。
3. 绑定监听。
4. 启动限额内 worker。
5. 接关闭信号。
6. 停止接收并完成当前提交。
7. 释放资源。

**持久化、并发与副作用**：先持久层后监听；停止时回执已返回的提交必须耐久。

**失败处理**：NODE_START_FAILED，MIGRATION_FAILED，SHUTDOWN_TIMEOUT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[API-001 load_node_config](api.md#api-001)、[NODE-016 expire_node_data](node.md#node-016)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 节点在 append 返回后重启不丢日志。
- [ ] 无私钥/正文诊断输出。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。


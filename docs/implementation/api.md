# 节点 HTTP、鉴权、限流与运行入口

阶段：P1 → P5。状态：函数级设计。API-002 issue_auth_challenge 与 API-003 authenticate_request 已随 P1-03 在 [crates/node-api](../../crates/node-api) 与 [crates/node-services](../../crates/node-services) 落地（端点见“已实现端点（P1-03）”）。群日志与信箱的七个端点（NODE-005~011 的 HTTP 适配，对应 API-006 handle_append_http 与 API-007 handle_mailbox_http 的当前范围）已在同一轮落地；附件 blob 的四个端点（NODE-012~015 的 HTTP 适配，对应 API-008 handle_blob_http 的当前范围）在同表登记，见“已实现端点（群日志与信箱）”。客户端侧访问这些端点的实现见 [crates/node-client](../../crates/node-client)（见“客户端（crates/node-client）”）。其余函数仍未实现。目标源码：[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)。

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

## 已实现端点（群日志与信箱）

路由同样由 `build_router` 构造；全部端点都走 API-003 已登记设备鉴权（挑战 + 设备签名），没有任何绕过鉴权的入口，也没有仅凭账户/设备 ID 的查询路径。

|方法/路径|作用|附加签名/约束|
|---|---|---|
|`POST /v1/groups`|群登记（NODE-005）|正文内 `device_signature`：创建设备对创建原文的签名|
|`POST /v1/groups/{group_id}/entries`|日志追加（NODE-006）|正文内 `author_signature`：作者对 PROTOCOL-004 原文的签名；路径 group 必须等于候选 group|
|`GET /v1/groups/{group_id}/log?after=N&limit=M`|分页日志（NODE-007）|limit 上限取节点策略；after 大于 head 返回明确空页|
|`GET /v1/groups/{group_id}/head`|节点签名 head（NODE-008）|与条目回执使用不同签名域|
|`POST /v1/mailbox/items`|密文投递（NODE-009）|必须指向本人已接受的日志条目且目标设备在群内|
|`GET /v1/mailbox?cursor=...&limit=N`|本设备信箱分页（NODE-010）|游标由 PROTOCOL-012/013 认证并绑定账户/设备|
|`POST /v1/mailbox/ack`|幂等 ACK（NODE-011）|只作用于本设备条目；批量上限由策略决定|
|`POST /v1/blobs`|创建附件上传会话（NODE-012）|必须关联一条本人已接受的日志条目；同 operation + 同描述幂等|
|`PUT /v1/blobs/{upload_id}/chunks/{index}`|按序写入一块密文（NODE-013）|正文是**原始密文字节**（`application/octet-stream`）；顺序/边界/摘要严格校验|
|`POST /v1/blobs/{upload_id}/complete`|校验清单并发布（NODE-014）|清单块数/总长/整体摘要必须与已上传内容一致；重复完成返回同一回执|
|`GET /v1/blobs/{blob_id}/chunks/{index}`|取回一块密文（NODE-015）|成员窗口授权；未发布或越权一律拒绝；响应是原始密文字节|

附件 blob 端点的两点特殊约定：

- `PUT` 与 `GET` 共用同一个 URL 形状（`/v1/blobs/{id}/chunks/{index}`）：`PUT` 的 `{id}` 是**上传会话 id**，`GET` 的 `{id}` 是**已发布 blob id**。axum 不允许同名形状的两种参数命名，因此路由参数统一叫 `id`，由方法决定语义。
- 块正文不做 hex/JSON 二次编码：`PUT` 直接发送原始密文（content-type `application/octet-stream`），`GET` 直接返回原始密文；请求签名仍然覆盖 `sha256(正文)`。因此单块上限（`BlobPolicy.max_chunk_bytes`，默认 32 KiB）必须小于正文上限 64 KiB。

请求/响应同样带 `schema_version`（当前只接受 1）。校验顺序固定为：正文限长 → DTO 解码 → API-003 鉴权 → 领域调用 → 结果映射；handler 不另开事务、不重复领域规则。

- 查询串：只接受 `key=value`（不重复、无未知键），字符集限定为 `ASCII 字母数字 + _ - .`，不做百分号解码——线上原文就是签名原文，避免同一语义多种写法。为此查询串上限从 512 放宽到 2048（`limits::MAX_QUERY_BYTES`），以便放得下 PROTOCOL-013 的分页游标（token 上限 1024 字节）；整段查询串仍然参与签名。
- 正文上限：沿用 64 KiB（`limits::MAX_HTTP_BODY_BYTES`，可用 `NodeAuthPolicy.max_request_body_bytes` 收紧）；超出 413 `PAYLOAD_TOO_LARGE`。
- 单条候选密文上限：`GroupLogPolicy.max_candidate_ciphertext_bytes`（默认 16 KiB，密文以十六进制出现在 JSON 中，因此小于正文上限）；超出 413 `PAYLOAD_TOO_LARGE`。
- 稳定错误码（新增，全部登记在此处）：
  - 400 `INVALID_CURSOR`（游标非法/过期/不属于本设备）、`INVALID_ACK_BATCH`（空批量或超上限）。
  - 403 `HISTORY_NOT_AUTHORIZED`（设备不在该群允许的历史/成员窗口内）。
  - 404 `GROUP_NOT_FOUND`（群不存在或不由本节点协调）。
  - 409 `GROUP_ID_CONFLICT`、`GROUP_CAPACITY_EXCEEDED`、`SUBMISSION_ID_CONFLICT`、`STALE_CONTROL_HEAD`、`STALE_EPOCH`、`QUOTA_EXCEEDED`、`ITEM_ID_CONFLICT`。
  - 附件 blob（NODE-012~015）新增码：400 `CHUNK_OUT_OF_ORDER`（未按序）、404 `UPLOAD_NOT_FOUND`/`BLOB_NOT_FOUND`（会话或已发布对象不存在）、410 `UPLOAD_EXPIRED`（会话过期或已结束）、409 `BLOB_UPLOAD_CONFLICT`（同 operation 换描述或 blob_id 已占用）、`CHUNK_CONFLICT`（同序号相异摘要）、`MISSING_CHUNKS`（缺块，含缺末块）、`MANIFEST_MISMATCH`（块数/总长/整体摘要/单块边界不符）、413 `CONTENT_TOO_LARGE`（单块或声明总长超上限；与“正文超过 HTTP 上限”的 `PAYLOAD_TOO_LARGE` 区分）。
- 复用既有码：401 `INVALID_CHALLENGE/NODE_ID_MISMATCH/AUTH_EXPIRED/AUTH_REPLAY/INVALID_SIGNATURE/INVALID_CREDENTIAL`，403 `DEVICE_REVOKED/INVALID_CAPABILITY/PERMISSION_DENIED`，400 `INVALID_REQUEST/UNSUPPORTED_SCHEMA_VERSION`，503 `STORAGE_FAILURE`。
- 不泄漏语义：未知信箱条目、他人条目、越权目标都统一返回 403 `PERMISSION_DENIED`；日志读取的成员窗口失败返回 403 `HISTORY_NOT_AUTHORIZED`，不返回“该 seq 存在但你看不到”。

## 客户端（crates/node-client）

节点 HTTP 客户端在 [crates/node-client](../../crates/node-client)（本轮新增）。它只做四件事：取一次性挑战、按节点的 canonical 规则签名、发 HTTP、把稳定错误码映射成客户端错误；领域规则（谁能写、序号与哈希链、配额）全部由节点判定，客户端不重复实现，也不在失败后伪造成功。

|文件|内容|
|---|---|
|src/endpoint.rs|`NodeEndpoint`：URL + 固定 node_id（+ 可选固定节点公钥）；只允许 http/https、无凭据、无查询/片段、无路径前缀，请求 URL 由规范化 path/query 生成|
|src/client.rs|`NodeClient`：fetch_challenge、send_with_challenge（低层，单次发送）、register_device、create_group(+_signed)、append_entry(+_signed)、fetch_log、get_head、store_mailbox_item、read_mailbox_page、ack_mailbox_items；ClientLimits（响应/请求上限、超时、尝试预算）|
|src/error.rs|`NodeClientError`：PROTOCOL-014 码 + 分类（network/timeout/protocol/authentication/permission/conflict/not_found/invalid_request/limit/internal/config），只带固定静态说明|

端点与方法一一对应：`POST /v1/auth/challenge` → fetch_challenge；`POST /v1/devices` → register_device；`POST /v1/groups` → create_group；`POST /v1/groups/{id}/entries` → append_entry；`GET /v1/groups/{id}/log` → fetch_log；`GET /v1/groups/{id}/head` → get_head；`POST /v1/mailbox/items` → store_mailbox_item；`GET /v1/mailbox` → read_mailbox_page；`POST /v1/mailbox/ack` → ack_mailbox_items；`POST /v1/blobs` → begin_blob_upload（返回节点签名会话）；`PUT /v1/blobs/{id}/chunks/{index}` → put_blob_chunk（原始字节，回执绑定 `sha256(字节)`）；`POST /v1/blobs/{id}/complete` → complete_blob_upload（回执绑定清单三项）；`GET /v1/blobs/{id}/chunks/{index}` → read_blob_chunk（返回原始字节，摘要由调用方按清单校验）。

客户端新增码映射：`BLOB_UPLOAD_CONFLICT/CHUNK_CONFLICT → LOG_CONFLICT`，`UPLOAD_NOT_FOUND/BLOB_NOT_FOUND → INVALID_TARGET`（分类 not_found），`UPLOAD_EXPIRED → INVALID_STATE_TRANSITION`，`CHUNK_OUT_OF_ORDER → INVALID_SEQUENCE`，`MISSING_CHUNKS → MISSING_LOG_ENTRIES`，`MANIFEST_MISMATCH → INVALID_CIPHERTEXT`，`CONTENT_TOO_LARGE → CONTENT_TOO_LARGE`；这些错误都不会触发“重取挑战再试”，只会显式失败。

### 签名原文与挑战生命周期

- 签名原文直接复用节点侧实现（`chat_node_services::auth::request_signing_bytes` + `RequestMeta::canonical`），因此不存在“客户端一套 canonical、服务端另一套”的漂移：域分隔 + node_id + device_id + 用途 + 大写 method + 规范 path + query + sha256(正文) + nonce。
- 高层方法**每次调用都重新取一个一次性挑战**（不缓存、不复用），挑战只用一次；节点侧的重放/过期保护因此始终生效。
- 只在两类失败上重取一次后再返回稳定错误，总尝试次数硬上限为 2（`ClientLimits::challenge_attempts ∈ {1,2}`）：
  1. 挑战本身不可用：节点返回 `INVALID_CHALLENGE` / `AUTH_EXPIRED` / `AUTH_REPLAY`；
  2. 传输失败：连接失败或超时（所有业务操作都带幂等键 —— submission/item/建群摘要 —— 重发不会产生第二条日志）。
- 其他失败（签名不匹配、权限、冲突、越界、协议错误）立即返回，不做无意义重试；`INVALID_SIGNATURE` 不会因为换一个挑战而变好。

### 上限、超时与错误映射

- 响应体上限默认 1 MiB（`DEFAULT_MAX_RESPONSE_BYTES`）：先用 Content-Length 预检，再按块累计；超限立刻失败（`CONTENT_TOO_LARGE` / limit），不静默截断。请求正文上限默认 64 KiB，与节点正文上限一致。
- 单请求总超时默认 10s（`DEFAULT_TIMEOUT`），超时映射为 `NETWORK_UNAVAILABLE` + timeout 分类，且仍受“只重试一次”约束。
- 不跟随重定向（`redirect::Policy::none()`）：跳转后的路径与签名原文不一致，属于协议违规，按协议错误返回。TLS 完全走工作区的 rustls 默认校验，没有任何“接受非法证书”的降级开关。
- 固定节点公钥（`NodeEndpoint::with_node_public_key`）后：每个挑战都要用该公钥验签，且每个节点产物（注册回执、append 回执、head、日志条目回执、信箱回执）都要验签并绑定 node_id；未固定公钥时至少强制 `node_id` 一致（换 URL 不换节点）。

节点稳定码 → PROTOCOL-014 码（客户端唯一映射表，见 src/error.rs::code_for_node_code）：

|节点码|PROTOCOL-014|分类|
|---|---|---|
|INVALID_REQUEST / UNSUPPORTED_SCHEMA_VERSION / INVALID_ACK_BATCH|MALFORMED_FRAME|invalid_request|
|INVALID_CHALLENGE / AUTH_EXPIRED / AUTH_REPLAY / INVALID_CREDENTIAL|INVALID_CREDENTIAL|authentication|
|INVALID_SIGNATURE|INVALID_SIGNATURE|authentication|
|DEVICE_REVOKED|DEVICE_REVOKED|authentication|
|PERMISSION_DENIED|PERMISSION_DENIED|permission|
|HISTORY_NOT_AUTHORIZED|HISTORY_NOT_AUTHORIZED|permission|
|INVALID_CAPABILITY|REQUIRED_CAPABILITY_MISSING|permission|
|GROUP_NOT_FOUND / NODE_ID_MISMATCH|INVALID_TARGET|not_found / protocol|
|GROUP_ID_CONFLICT / SUBMISSION_ID_CONFLICT / ITEM_ID_CONFLICT|LOG_CONFLICT|conflict|
|STALE_CONTROL_HEAD / STALE_EPOCH|STALE_CONTROL_HEAD / STALE_EPOCH|conflict（可重试，提示重新对齐）|
|DEVICE_LIMIT_EXCEEDED / GROUP_CAPACITY_EXCEEDED / QUOTA_EXCEEDED / RATE_LIMITED / CHALLENGE_CAPACITY_EXCEEDED / PAYLOAD_TOO_LARGE|对应上限码|limit|
|INVALID_CURSOR|INVALID_CURSOR|invalid_request|
|STORAGE_FAILURE / MIGRATION_FAILED / NODE_IDENTITY_MISMATCH / INVALID_CONFIG / ENCODING_FAILURE|STORAGE_FAILURE / INTERNAL_FAILURE|internal|
|未知码|UNKNOWN_RESULT|protocol（不当作成功）|

没有可识别错误体时按 HTTP 状态归类（400/401/403/404/409/413/429/500/502-504），仍然是显式失败。

### 验证证据

命令与结果：`cargo test --offline -p chat-node-client` → 38 个测试全部通过（13 个 lib 单元测试 + 11 个 `tests/client_limits.rs` + 9 个 `tests/http_flow.rs` + 5 个 `tests/blob_flow.rs`），0 警告；三 crate 合计 117 个测试（node-services 63 + node-api 16 + node-client 38）全绿。

- 真 TCP 端到端（`tests/http_flow.rs`：在进程内用 `axum::serve` 起 [crates/node-api](../../crates/node-api) 的真实路由，服务端是真实 node-services）：`challenge_registration_and_signed_outputs_are_verified`（挑战由固定节点公钥验签、注册回执验签、重复注册幂等）、`group_log_head_and_idempotent_append_flow`（建群幂等、append 同 submission 同字节重试返回同 seq、fetch_log 分页、head 与页内 head 一致）、`mailbox_store_read_and_ack_flow`（投递幂等、读取分页与重复读、ACK 幂等与越权 403）、`wrong_device_key_is_rejected_without_retrying`、`replayed_and_expired_challenges_are_rejected`、`unknown_device_and_non_member_are_rejected`、`divergent_bytes_for_one_submission_are_a_conflict`、`wrong_node_identity_and_wrong_pinned_key_are_rejected`、`oversized_request_is_rejected_before_sending`。
- 边界语义（`tests/client_limits.rs`：脚本化最小 HTTP/1.1 服务）：`auth_failure_is_retried_once_and_then_succeeds`（旧挑战失败后重取一次成功，挑战请求恰好 2 次）、`retry_budget_is_exactly_one_retry`、`single_attempt_budget_sends_one_request`、`oversized_response_with_content_length_is_rejected`、`oversized_chunked_response_is_rejected_by_incremental_budget`、`signed_request_timeout_is_retried_once_then_reported`、`anonymous_challenge_timeout_is_reported_without_retry`、`network_failure_is_reported_and_bounded`、`redirects_are_not_followed`（重定向不产生第二次请求）、`wrong_node_id_in_challenge_is_rejected`、`unknown_error_code_is_not_silently_treated_as_success`。

- 附件 blob（`tests/blob_flow.rs` + node-api 的 `tests/group_mailbox_http.rs`）：`blob_upload_and_read_round_trip_over_http`（会话/分块/发布/读取往返；上传者与群内老成员都能读到逐字节一致的密文；群外设备 403 HISTORY_NOT_AUTHORIZED）、`duplicate_chunk_is_idempotent_and_size_violation_is_rejected`、`out_of_order_and_missing_chunks_are_rejected`、`digest_mismatch_is_rejected_and_unfinished_blob_is_not_readable`、`foreign_writer_and_non_canonical_ids_are_rejected`；HTTP 契约侧 `blob_http_error_statuses_are_stable`（409/410/413/400/404 逐项断言）与 `blob_endpoints_require_bound_authentication`（缺鉴权头 400、换方法/路径/正文 401、群外设备 403、方法/路由边界 405/404）。
### 已知限制

- 客户端不含 MLS/加密层的内容构造：候选密文、Welcome 等字节由上游加密层提供，本 crate 只负责传输、签名与错误映射（领域校验仍由节点与客户端加密层各自负责）。
- 未实现 API-005 的逐设备限流；`RATE_LIMITED` 只做码映射，客户端不内置退避循环（重试预算固定为一次，避免把限流变成忙等）。
- 未实现事件套接字（API-009）与密文文件端点（API-008）：本 crate 只覆盖本文件已落地的七个业务端点与挑战/注册。
- 固定节点公钥是可选项。未固定时只能保证挑战与产物里的 node_id 与端点声明一致，无法发现同一 node_id 的伪造节点；生产客户端应始终固定公钥（测试按固定公钥运行）。
- 客户端只把节点返回的 node_public_key 用于验签，不做跨重启的 key 变更检测（节点侧已通过 node_identity 表固定身份）。

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

- [x] 409 stale head 与403权限可区分。
- [x] 相同 submission 异字节409。

**实现状态**：当前范围已实现（P1，群日志与信箱轮次）。本轮交付的是本函数在现有阶段可落地的部分：群的 `POST /v1/groups` 与日志的 `POST /v1/groups/{group_id}/entries` 适配，源码 [crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)。

已实现行为：

1. 顺序固定：正文限长（超限 413，不进入领域逻辑）→ DTO/`schema_version` 解码 → API-003 鉴权（消费一次性挑战）→ 领域调用 → 映射签名回执或稳定错误。
2. 路由与正文一致：路径上的 group 与候选自带的 group 不一致即 403 `PERMISSION_DENIED`；作者/演员不一致同样 403。
3. 错误可区分：`STALE_CONTROL_HEAD`/`STALE_EPOCH`/`SUBMISSION_ID_CONFLICT` 都是 409，权限类是 403，鉴权类（换方法/路径/查询/正文/节点、重放挑战）是 401 —— 客户端可以按状态与错误码分流重试策略。
4. handler 不开独立事务：日志/控制头/head 的原子性完全由 [NODE-006](node.md#node-006) 的单个 IMMEDIATE 事务保证。

验证证据（`cargo test --offline -p chat-node-api` 全绿）：`group_registration_and_append_are_idempotent_over_http`（登记 + 追加幂等、409 SUBMISSION_ID_CONFLICT、409 STALE_CONTROL_HEAD 与 403 PERMISSION_DENIED 并列出现）、`append_requires_bound_authentication_and_matching_group`（缺鉴权头 400、换方法/路径/正文 401、路径与正文 group 不一致 403、挑战重放 401）、`non_member_is_forbidden_on_group_and_mailbox_endpoints`（未知群 404）。

已知限制：本函数规划中的“路由/字节限额”里的逐设备速率限额（API-005）仍未实现，当前只有正文与密文大小上限；日志端点没有 fanout/事件推送（API-009 未实现）。通过本函数用例不自动代表整个模块完成。

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

- [x] GET 不删除。
- [x] ACK 只能本人。
- [x] 超响应预算分页而非截断密文。

**实现状态**：当前范围已实现（P1，群日志与信箱轮次）。三个路由 `POST /v1/mailbox/items`、`GET /v1/mailbox`、`POST /v1/mailbox/ack` 已在 [crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs) 落地，分别调用 [NODE-009](node.md#node-009)/[NODE-010](node.md#node-010)/[NODE-011](node.md#node-011)。

已实现行为：

1. actor 绑定：三个路由都要求 API-003 鉴权，查询目标恒为鉴权设备本身，客户端无法指定 device id；`GET /v1/mailbox` 的未知查询键直接 400。
2. GET 不删除：读取只返回未过期未消费条目并给出下一游标与过期缺口计数，不改变任何状态。
3. ACK 只能本人：批量在写任何状态前先做归属校验，未知条目与他设备条目统一 403 `PERMISSION_DENIED`，整批原子拒绝。
4. 分页而非截断：单页受条数上限与字节预算双重约束，超出预算就返回更少条目并给出 `next_cursor`；单条密文自身必须能放进一页（策略启动时校验，否则拒绝启动）。
5. 游标：PROTOCOL-012/013 认证游标绑定账户/设备/资源/排序版本，跨设备、伪造、过期一律 400 `INVALID_CURSOR`。

验证证据（`cargo test --offline -p chat-node-api` 全绿）：`mailbox_store_read_and_ack_over_http`（投递 → 两页读取 → 跨设备/伪造游标 400 → ACK 幂等 → 他设备与未知条目 403）、`non_member_is_forbidden_on_group_and_mailbox_endpoints`。服务层证据见 NODE-009~011 的“验证证据”。

已知限制：Welcome/未入群设备的投递依赖 API-004 validate_capability（未实现），当前明确不支持；没有按设备的速率限额（API-005）与 TTL 回收任务（NODE-016）。通过本函数用例不自动代表整个模块完成。

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


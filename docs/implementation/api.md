# 节点 HTTP、鉴权、限流与运行入口

阶段：P1 → P5。状态：函数级设计。API-002 issue_auth_challenge 与 API-003 authenticate_request 已随 P1-03 在 [crates/node-api](../../crates/node-api) 与 [crates/node-services](../../crates/node-services) 落地（端点见“已实现端点（P1-03）”）。群日志与信箱的七个端点（NODE-005~011 的 HTTP 适配，对应 API-006 handle_append_http 与 API-007 handle_mailbox_http 的当前范围）已在同一轮落地；附件 blob 的四个端点（NODE-012~015 的 HTTP 适配，对应 API-008 handle_blob_http 的当前范围）在同表登记，见“已实现端点（群日志与信箱）”。客户端侧访问这些端点的实现见 [crates/node-client](../../crates/node-client)（见“客户端（crates/node-client）”）。**可部署节点入口**（API-010 run_node）与**基础限流**（API-005）也已落地：[apps/node](../../apps/node) 能真正监听并应答全部端点（含匿名的 `GET /v1/health`），超限请求返回 429 `RATE_LIMITED` 并带 `retry_after`；两者同表登记在“已实现端点（群日志与信箱）”。其余函数仍未实现。目标源码：[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)。

所有 HTTP DTO 与错误结构单独版本化。公开 /capabilities 与 challenge 也限流。默认监听本地端口，通过明确的 TLS 部署入口提供 HTTPS/WSS；管理接口独立且默认关闭。每个 HTTP handler 只鉴权、解码、调用服务、映射结果，不重复实现领域规则。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 撤销端点授权（CAP-AUTHZ，2026-10-19，Windows x64）

`POST /v1/capabilities/{token_id}/revoke` 以前只要求设备鉴权（任何设备都能撤销任何令牌）。现在：

- 请求仍需设备签名鉴权（同一套挑战流程），handler 把**调用方设备**传给服务层做授权判定：
  `service.revoke_capability(token_id, actor.device_id, reason, now_ms)`；
- 调用方设备 ≠ 签发设备且策略 `capability.revoke.self_only = true`（默认）→ **403** `PERMISSION_DENIED`；
- 未知 `token_id`（本节点从未签发）→ **404** `CAPABILITY_TOKEN_NOT_FOUND`；
- 成功 → 200 `{ token_id, revoked }`；重复撤销幂等（`revoked = false`）；
- 放宽策略（部署侧 `CHAT_NODE_CAPABILITY_REVOKE_ANY_DEVICE=1`）允许跨设备撤销，服务层记
  `authorization = policy_relaxed` 与调用方设备作为审计依据。

### 证据（`cargo test --offline -p chat-node-api`，30 passed）

- `group_mailbox_http::revocation_is_limited_to_the_issuing_device_over_http`：设备 22 撤销设备 21 签发的令牌
  → 403 `PERMISSION_DENIED` 且令牌仍然可用（撤销没有生效）；未知 token_id → 404 `CAPABILITY_TOKEN_NOT_FOUND`；
  签发设备自己撤销 → 200/`revoked=true`，重复 → `revoked=false`。
- 既有撤销 HTTP 用例（`capability_revocation_takes_effect_immediately_over_http`、
  `enforced_mode_requires_a_live_token_for_mailbox_reads`）保持通过：它们本来就是签发设备自己撤销。

### 已知限制

- 端点没有“运营者”主体：放宽策略下任何**已鉴权设备**都能撤销他人令牌（审计会记录是谁），
  真要做角色化撤销需要账户级角色模型（见 node.md 限制 2）。

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
- JSON DTO 正文上限：64 KiB（`limits::MAX_HTTP_BODY_BYTES`，可用 `NodeAuthPolicy.max_request_body_bytes` 收紧）；超出返回 413 `PAYLOAD_TOO_LARGE`。
- 原始密文块正文上限：**4 MiB**（`limits::MAX_BLOB_CHUNK_BODY_BYTES` = 分块硬上限），只作用于 `PUT /v1/blobs/{id}/chunks/{index}` 这一条路由（路由级 `DefaultBodyLimit` 覆盖全局 64 KiB）；其余路由仍是上面的 64 KiB。块正文的具体大小另由服务层按会话声明的 `chunk_bytes` 与 `BlobPolicy.max_chunk_bytes` 校验，超限仍是 `CONTENT_TOO_LARGE`（错误码不变）。
- 设备上限：每账户默认 2 台有效设备（`limits::DEFAULT_MAX_DEVICES_PER_ACCOUNT`，与产品口径一致；`NodeAuthPolicy.max_devices_per_account` 可配）；达到上限返回 409 `DEVICE_LIMIT_EXCEEDED`，不静默替换或截断旧设备。
- 错误体：`{"schema_version":1,"error":{"code","message"}}`；状态映射：400 INVALID_REQUEST/UNSUPPORTED_SCHEMA_VERSION，401 INVALID_CHALLENGE/NODE_ID_MISMATCH/AUTH_EXPIRED/AUTH_REPLAY/INVALID_SIGNATURE/INVALID_CREDENTIAL，403 DEVICE_REVOKED/INVALID_CAPABILITY/PERMISSION_DENIED，409 DEVICE_LIMIT_EXCEEDED，413 PAYLOAD_TOO_LARGE，429 RATE_LIMITED，503 CHALLENGE_CAPACITY_EXCEEDED/STORAGE_FAILURE，500 MIGRATION_FAILED/NODE_IDENTITY_MISMATCH/INVALID_CONFIG/ENCODING_FAILURE。

本阶段**没有**实现：API-005 逐 IP 限流（`RATE_LIMITED` 只预留错误码）、API-009 事件套接字、API-010 run_node（[apps/node](../../apps/node) 仍是明确 78 退出的骨架）、TLS 终止与管理接口。

## 已实现端点（群日志与信箱）

路由同样由 `build_router` 构造；除 `GET /v1/health` 外全部端点都走 API-003 已登记设备鉴权（挑战 + 设备签名），没有任何绕过鉴权的入口，也没有仅凭账户/设备 ID 的查询路径。

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
|GET /v1/health|健康检查（API-010）|**匿名、不鉴权、不限流**；只返回 `status/node_id/node_public_key/schema_version`，不含任何秘密与内部状态|
|POST /v1/capabilities|签发对象级能力令牌（API-004）|设备鉴权 + **对象级授权**（上传会话归属 / 附件可读性）后才签发；ttl 上限 1 小时（CAP 包可由策略下调）|
|（CAP 包）附件块端点|可选/强制携带 `x-chat-capability`|策略 `capability.required` 或 `CHAT_NODE_CAPABILITY_REQUIRED` 打开后必带；缺失 401 `AUTH_REQUIRED`|
|POST /v1/groups/{id}/entries（DM 包补充）|追加候选**可选带领域操作声明**（PROTOCOL-004 字段 55/56）|请求体多一个可选 `domainOperation: { operation, target_digest }`；声明参与作者签名原文，非法组合/未知枚举/零摘要 → 400 `INVALID_CANDIDATE`；不传 = 旧客户端行为|

健康检查的定位：运维/探活用，所以既不能要签名（探活方没有设备凭据），也不能被限流（否则过载时最先失去可观测性）。它不返回库路径、配置、队列长度等内部状态，避免成为信息泄露面。

能力令牌的传输约定：附件块端点（`PUT/GET /v1/blobs/…/chunks/{index}`）在设备鉴权之外**接受**可选的 `x-chat-capability` 请求头。带上时令牌必须与（对象 id、本设备、用途）绑定且未过期——否则 401/403；不带时仍按设备鉴权（会员窗口/上传者）判定。**强制模式尚未开启**（会破坏既有客户端，见 API-004 的限制）。

能力令牌强制模式（CAP 包）的传输约定：节点策略 `CapabilityPolicy.required`（默认 **false**）或进程环境变量
`CHAT_NODE_CAPABILITY_REQUIRED=1/true/yes/on` 打开后，附件块 `PUT/GET /v1/blobs/…/chunks/{index}` **必须**带
`x-chat-capability`（先 `POST /v1/capabilities` 取）。缺失 → **401 `AUTH_REQUIRED`**；令牌绑定不符 →
403 `INVALID_CAPABILITY`；令牌过期 → 401 `AUTH_EXPIRED`；令牌头存在但为空/超长 → 400 `INVALID_REQUEST`
（“空令牌”是畸形请求，不等于“没带令牌”，不会被当成 AUTH_REQUIRED）。关闭时行为完全不变（旧客户端零改动）。
`CHAT_NODE_CAPABILITY_MAX_TTL_SECONDS` 可调签发上限（默认 3600，硬上限仍是 1 小时）。

领域操作声明（DM 包）的传输约定：`POST /v1/groups/{id}/entries` 的请求体可选携带
`domainOperation: { "operation": "set_role" | "transfer_ownership" | "leave_self" | "dissolve", "target_digest": "<64 位小写 hex>" }`
（服务端同时接受 `domain_operation` 这一蛇形拼写）。**声明必须由客户端先折叠进签名原文再签名**：节点用“含声明”的形态
重建 PROTOCOL-004 签名原文，因此事后追加/修改声明一律 `INVALID_SIGNATURE`。响应与日志接口把节点侧投影暴露为
`entry.domain_operation` / `entry.domain_target_digest`（未声明为 `null`）。错误码：未知枚举、摘要非 32 字节或全零、
与公开 `operation` 组合非法、应用候选夹带声明 → 400 `INVALID_CANDIDATE`（不是 500，也不是静默忽略）。

附件 blob 端点的两点特殊约定：

- `PUT` 与 `GET` 共用同一个 URL 形状（`/v1/blobs/{id}/chunks/{index}`）：`PUT` 的 `{id}` 是**上传会话 id**，`GET` 的 `{id}` 是**已发布 blob id**。axum 不允许同名形状的两种参数命名，因此路由参数统一叫 `id`，由方法决定语义。
- 块正文不做 hex/JSON 二次编码：`PUT` 直接发送原始密文（content-type `application/octet-stream`），`GET` 直接返回原始密文；请求签名仍然覆盖 `sha256(正文)`。
- **分块上限与客户端冻结分块对齐（本轮修正）**：客户端附件按 **256 KiB 明文**分块（CRYPTO-011，已冻结），密文块 = 明文 + 29 字节封装（版本 1 + nonce 12 + tag 16）= **262_173 字节**。因此 `BlobPolicy.max_chunk_bytes` 默认 **1 MiB**、硬上限 **4 MiB**（旧值 32 KiB / 60 KiB 会让所有多块附件被 413 `CONTENT_TOO_LARGE` 拒绝）；块正文的传输上限相应为 4 MiB（见上一条），JSON DTO 路由仍是 64 KiB。总大小（`max_object_bytes` 8 MiB）、块数（`max_chunk_count` 512）、每设备配额（32 MiB）**均未改变**。

请求/响应同样带 `schema_version`（当前只接受 1）。校验顺序固定为：正文限长 → DTO 解码 → API-003 鉴权 → 领域调用 → 结果映射；handler 不另开事务、不重复领域规则。

- 查询串：只接受 `key=value`（不重复、无未知键），字符集限定为 `ASCII 字母数字 + _ - .`，不做百分号解码——线上原文就是签名原文，避免同一语义多种写法。为此查询串上限从 512 放宽到 2048（`limits::MAX_QUERY_BYTES`），以便放得下 PROTOCOL-013 的分页游标（token 上限 1024 字节）；整段查询串仍然参与签名。
- 正文上限：沿用 64 KiB（`limits::MAX_HTTP_BODY_BYTES`，可用 `NodeAuthPolicy.max_request_body_bytes` 收紧）；超出 413 `PAYLOAD_TOO_LARGE`。
- 单条候选密文上限：`GroupLogPolicy.max_candidate_ciphertext_bytes`（默认 16 KiB，密文以十六进制出现在 JSON 中，因此小于正文上限）；超出 413 `PAYLOAD_TOO_LARGE`。
- 稳定错误码（新增，全部登记在此处）：
  - 400 `INVALID_CURSOR`（游标非法/过期/不属于本设备）、`INVALID_ACK_BATCH`（空批量或超上限）。
  - 403 `HISTORY_NOT_AUTHORIZED`（设备不在该群允许的历史/成员窗口内）。
  - 404 `GROUP_NOT_FOUND`（群不存在或不由本节点协调）。
  - 409 `GROUP_ID_CONFLICT`、`GROUP_CAPACITY_EXCEEDED`、`SUBMISSION_ID_CONFLICT`、`STALE_CONTROL_HEAD`、`STALE_EPOCH`、`QUOTA_EXCEEDED`、`ITEM_ID_CONFLICT`。
  - 限流（API-005）：429 `RATE_LIMITED`，响应体带 `error.retry_after_ms`（毫秒，窗口剩余时间，至少 1），HTTP 头带 `retry-after`（秒，向上取整）。该码此前只是预留；现在真的会返回。错误体的 `retry_after_ms` 是**可选新增字段**（`skip_serializing_if`），其它错误不会出现，旧客户端可忽略。
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

命令与结果：`cargo test --offline -p chat-node-client` → 39 个测试全部通过（13 个 lib 单元测试 + 11 个 `tests/client_limits.rs` + 9 个 `tests/http_flow.rs` + 5 个 `tests/blob_flow.rs` + 1 个 `tests/cross_process.rs` 跨进程用例），0 警告；四 crate 合计 136 个测试（node-services 70 + node-api 19 + node-client 39 + chat-node 8）全绿。

- 真 TCP 端到端（`tests/http_flow.rs`：在进程内用 `axum::serve` 起 [crates/node-api](../../crates/node-api) 的真实路由，服务端是真实 node-services）：`challenge_registration_and_signed_outputs_are_verified`（挑战由固定节点公钥验签、注册回执验签、重复注册幂等）、`group_log_head_and_idempotent_append_flow`（建群幂等、append 同 submission 同字节重试返回同 seq、fetch_log 分页、head 与页内 head 一致）、`mailbox_store_read_and_ack_flow`（投递幂等、读取分页与重复读、ACK 幂等与越权 403）、`wrong_device_key_is_rejected_without_retrying`、`replayed_and_expired_challenges_are_rejected`、`unknown_device_and_non_member_are_rejected`、`divergent_bytes_for_one_submission_are_a_conflict`、`wrong_node_identity_and_wrong_pinned_key_are_rejected`、`oversized_request_is_rejected_before_sending`。
- 边界语义（`tests/client_limits.rs`：脚本化最小 HTTP/1.1 服务）：`auth_failure_is_retried_once_and_then_succeeds`（旧挑战失败后重取一次成功，挑战请求恰好 2 次）、`retry_budget_is_exactly_one_retry`、`single_attempt_budget_sends_one_request`、`oversized_response_with_content_length_is_rejected`、`oversized_chunked_response_is_rejected_by_incremental_budget`、`signed_request_timeout_is_retried_once_then_reported`、`anonymous_challenge_timeout_is_reported_without_retry`、`network_failure_is_reported_and_bounded`、`redirects_are_not_followed`（重定向不产生第二次请求）、`wrong_node_id_in_challenge_is_rejected`、`unknown_error_code_is_not_silently_treated_as_success`。

- 附件 blob（`tests/blob_flow.rs` + node-api 的 `tests/group_mailbox_http.rs`）：`blob_upload_and_read_round_trip_over_http`（会话/分块/发布/读取往返；上传者与群内老成员都能读到逐字节一致的密文；群外设备 403 HISTORY_NOT_AUTHORIZED）、`duplicate_chunk_is_idempotent_and_size_violation_is_rejected`、`out_of_order_and_missing_chunks_are_rejected`、`digest_mismatch_is_rejected_and_unfinished_blob_is_not_readable`、`foreign_writer_and_non_canonical_ids_are_rejected`；HTTP 契约侧 `blob_http_error_statuses_are_stable`（409/410/413/400/404 逐项断言）与 `blob_endpoints_require_bound_authentication`（缺鉴权头 400、换方法/路径/正文 401、群外设备 403、方法/路由边界 405/404）。
- 限流与运行入口（Z 包）：`cargo test --offline -p chat-node-services -p chat-node-api -p chat-node` 全绿 —— node-services 70（含 7 个 `rate_limit::tests` 单元用例）、node-api 19（含 `tests/rate_limit_http.rs` 的 3 个真 TCP 429 用例）、chat-node 8（6 个 lib 单测 + `tests/smoke.rs` 的进程内与真实二进制冒烟）。限流证据：`anonymous_endpoints_return_429_with_retry_after`、`device_endpoints_return_429_after_the_device_budget`、`health_is_never_rate_limited`；运行入口证据：`in_process_node_serves_health_and_shuts_down_cleanly`、`binary_starts_logs_only_public_identity_and_serves_health`（真实子进程 + 启动日志不含种子）。`cargo build --offline -p chat-node` 成功。

### 跨进程端到端（真 chat-node 进程 + NodeClient）

`crates/node-client/tests/cross_process.rs::node_process_and_client_survive_a_hard_restart` 是唯一一条**跨进程**证据：客户端与节点不在同一进程，节点是 [apps/node](../../apps/node) 的真实二进制。

运行方式（必须先构建节点二进制）：

`@powershell
. .\tools\activate.ps1
cargo build --offline -p chat-node
cargo test --offline -p chat-node-client --test cross_process
`@

二进制定位顺序：`CHAT_NODE_BIN` 环境变量（必须是文件）→ 测试可执行文件同级的 profile 目录（`target/debug/chat-node[.exe]`）→ workspace 的 `target/debug`（或 `CARGO_TARGET_DIR`）。**找不到时测试显式失败并打印已尝试路径与可操作命令**，不会静默跳过（避免把“什么都没测”当成通过）。

用例覆盖：

1. 用 `std::process::Command` 启动 node：`--bind 127.0.0.1:<随机空闲端口>`、临时 `--data-dir`、临时 `--seed-file`（64 位十六进制）、固定 `--node-id`；stdout/stderr 重定向到临时文件（避免管道写满阻塞子进程），轮询 `GET /v1/health` 直到 200（20 秒上限，超时/早退都打印 stderr 与 stdout 摘要）；端口冲突或启动失败自动换端口重试（最多 3 次，全部失败才报错并列出每次原因）。
2. 从 health 取 `node_id` 与 `node_public_key` 并固定到 `NodeEndpoint`，之后每个挑战/产物都按该公钥验签（换端口不换身份）。
3. 真链路：注册两台设备（真凭据 + 一次性注册能力，时间取系统时钟）→ 建两成员群 → append 一条密文并用同一 submission 重试证明幂等（seq/候选摘要/log_hash 相同）→ `fetch_log` + `get_head` → 信箱投递两条、读页、ACK 一条并确认只剩未 ACK 的那条 → 附件 blob `begin/put(含同块重传幂等)/complete/read` 往返（上传者与群内老成员都能读回逐字节一致的密文）。
4. 强杀：`child.kill()`（不走优雅关闭，模拟崩溃），确认进程退出且库文件已落盘。
5. 用**同一 data-dir 与同一 seed** 重启：节点身份（node_id + 公钥）必须完全一致；随后验证 `get_head` 的 seq 与 log_hash 不变、`fetch_log` 里的条目还在、未 ACK 的信箱条目还在、已发布 blob 的两块密文都能读回；最后再 append 一条新条目，断言 `seq = 旧 head + 1` 且 `previous_hash = 旧 log_hash`（重启后链继续，不是新链）。

这一步证明的是**持久性与跨进程协同**：节点数据在 `<data-dir>/node.db`（附件块也在库内），重启后由迁移与 SQLite 事务恢复，不依赖任何内存状态；客户端只通过 HTTP + 设备签名与节点交互，没有任何进程内直连。
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
- [x] 非法负限额拒绝。

**实现状态**：部分实现（Z 包，run_node 轮次）：命令行/环境变量版本已落地，TOML 配置文件尚未实现。源码：[apps/node/src/lib.rs](../../apps/node/src/lib.rs) 的 `NodeConfig::from_args_and_env`。

已实现：

1. 只接受显式开关（`--bind`/`--data-dir`/`--seed-file`/`--node-id`/`--expect-public-key`/`--allow-public-bind`/`--init-seed`），未知参数直接拒绝；同名环境变量可覆盖默认值，命令行优先。
2. `--bind` 必须能解析成 `ip:port`；非回环地址必须再给 `--allow-public-bind`（进程内没有 TLS 终止），否则拒绝启动。
3. `--seed-file` 必填：缺失即拒绝启动（`--init-seed` 才允许生成新种子）；`--expect-public-key` 必须是 64 位十六进制，且与种子派生的公钥不一致时**在打开数据库之前**拒绝，避免把错误身份写进库。
4. 限额全部是无符号类型且逐项范围校验（窗口 1..=3600 秒、请求数 1..=1_000_000、桶表 16..=65_536），负值在解析阶段就被拒绝（`INVALID_CONFIG`）；策略在 `NodeService::open` 时再次校验。

证据：`cargo test --offline -p chat-node` 的 `config_requires_a_seed_file_and_valid_bind`、`public_bind_requires_an_explicit_opt_in`、`environment_is_used_and_command_line_wins`、`expected_public_key_must_be_canonical_hex`、`node_id_is_derived_from_the_public_key_when_absent`（含固定公钥不一致时拒绝且不建库）。

已知限制：TOML 配置、目录权限检查（`INSECURE_STORAGE_PATH`）、外部 URL/TLS 模式声明与“限额乘法溢出”检查未实现，因此“缺持久卷配置拒绝生产启动”仍未勾选。通过本函数用例不自动代表整个模块完成。

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

- [x] 联系人邀请 token 不能用于群管理。
- [x] 一个 blob token 不能读另一 blob。

**实现状态**：已实现（NG 包）。源码：[crates/node-services/src/capability.rs](../../crates/node-services/src/capability.rs)（令牌格式/签发/校验），HTTP 在 [crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs) 的 `issue_capability` 与附件块处理。

令牌形状：`cap1.<hex(payload)>.<hex(signature)>`，payload 是域分隔 + 长度前缀的规范编码：
`domain || object_id || device_id || scope || issued_at(be64) || expires_at(be64) || node_id`。

已实现行为：

1. **签发** `NodeService::issue_capability(object_id, device_id, scope, ttl_seconds, now)`：校验对象标识形状（下载用途必须是 PROTOCOL-001 `obj:1:…` 文本；上传用途是节点生成的会话 id）、TTL `1..=3600` 秒、设备仍有效，然后用节点签名钥签 payload。
2. **校验** `NodeService::verify_capability(token, object_id, device_id, scope, now)`：限长解析 → payload **重编码并要求逐字节相同**（杜绝可塑编码）→ 验签（节点公钥）→ 绑定（对象/设备/用途）→ 节点身份（`node_id` 必须一致）→ 期限（`issued_at <= now < expires_at`，时钟早于签发也拒绝）。失败码：过期 `AUTH_EXPIRED@、绑定不符 `INVALID_CAPABILITY@、换节点 `NODE_ID_MISMATCH@、签名/编码 `INVALID_SIGNATURE@ / `INVALID_REQUEST@。
3. **无状态**：不落库、无需清理，过期即失效；撤销通过撤销设备（设备鉴权先失败）或换对象完成——因此没有 `capability_revocations` 依赖。
4. **对象级授权与令牌分离**：node-api 在签发前先做对象级授权（`authorize_blob_upload`：上传者本人 + 会话仍在 uploading 且未过期；`authorize_blob_read`：上传者本人或“附件存在时已在群内”的当前成员），再签发；令牌本身只证明“节点在 (对象, 设备, 用途, 期限) 上授权过”。
5. **HTTP 面**：新增 `POST /v1/capabilities`（设备鉴权 + 对象级授权 → 令牌）；附件块 `PUT`/`GET` 接受 **额外**请求头 `x-chat-capability`，带上时令牌必须与 (上传会话/对象, 本设备, blob:upload/blob:download) 绑定且未过期，与设备鉴权**同时满足**。

验证证据（`cargo test --offline -p chat-node-services -p chat-node-api` 全绿）：服务层 `capability::issued_tokens_verify_and_report_their_binding`、`expired_tokens_are_rejected`（边界前一秒通过、到点 `AUTH_EXPIRED@）、`wrong_object_device_or_scope_is_rejected`（跨对象重放 / 换设备 / 换用途都是 `INVALID_CAPABILITY@）、`tampered_or_foreign_tokens_are_rejected`（篡改与畸形令牌）、`tokens_from_another_node_are_rejected`、`ttl_and_object_shape_are_bounded`；单元 `capability::payload_round_trip_binding_and_tamper_are_enforced`；HTTP 端到端 `group_mailbox_http::capability_tokens_authorize_blob_chunks_over_http`（签发→带令牌上下传成功→用途不符 403）、`group_mailbox_http::capability_tokens_are_bound_to_one_object_and_one_device`（跨对象重放 403、篡改 401/403、他人会话拒绝签发 403 `PERMISSION_DENIED@、不存在对象 404、非法用途 400）。

已知限制：令牌**尚未强制**——附件块端点在没有 `x-chat-capability` 头时仍只靠设备鉴权（强制会破坏既有 node-client 调用，客户端改造不在本包作用域）；没有“能力撤销表 / 逐次消费”语义（纯无状态）；ttl 固定上限 1 小时且不可配置；`capability_revocations` 表未被本切片使用。通过本函数用例不自动代表整个模块完成。

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

- [x] 伪造 X-Forwarded-For 无效。
- [x] 大上传按字节计而非仅请求数。

**实现状态**：已实现（Z 包，run_node 轮次）。源码：[crates/node-services/src/rate_limit.rs](../../crates/node-services/src/rate_limit.rs)（策略与常量在 [limits.rs](../../crates/node-services/src/limits.rs)），HTTP 接线在 [crates/node-api/src/state.rs](../../crates/node-api/src/state.rs) 与 [handlers.rs](../../crates/node-api/src/handlers.rs)。

位置：**只在 HTTP 层生效**。匿名端点在 handler 首行调用 `limit_anonymous`；已登记设备端点在 `authenticate` 助手内（鉴权成功后）调用 `limit_device`，因此新增端点不可能“忘记限流”，而服务层单测与节点内部调用完全不经过限流器。

已实现行为：

1. 固定窗口（默认 60 秒，可配）进程内计数；桶表有界（默认 4096，上限 65536）：满表时先清理上一窗口的桶，仍满则显式拒绝新键（fail-closed，绝不无界增长）。
2. 三个维度：每设备请求数（默认 120/窗口）、每账户请求数（默认 240/窗口，同一账户多设备共享）、匿名全局请求数（默认 600/窗口）。设备桶额外计**请求正文字节**（默认 8 MiB/窗口），所以附件分块上传按字节计而不只是次数。
3. 超限返回 429 `RATE_LIMITED`：响应体带 `error.retry_after_ms`（毫秒，等于窗口剩余时间，至少 1），HTTP 头额外带 `retry-after`（秒，向上取整）。客户端据此退避，不需要猜。
4. **从不读取任何代理头**：`X-Forwarded-For`/`X-Real-IP` 完全不参与预算键，因此伪造它们不会得到额外配额；反过来也意味着没有逐 IP 维度（见限制）。
5. `/v1/health` 豁免限流（否则监控探活会被自己拒绝）。

验证证据（`cargo test --offline -p chat-node-services -p chat-node-api` 全绿）：

- 单元（`rate_limit::tests`）：`device_budget_is_enforced_and_resets_with_the_window`（设备预算、retry_after 范围、下一窗口恢复）、`account_budget_is_shared_across_devices`（账户预算跨设备共享）、`request_bytes_are_counted_per_device`（字节预算，含大请求）、`anonymous_requests_share_one_global_budget`、`disabled_policy_allows_everything`、`bucket_table_stays_bounded_and_fails_closed`、`invalid_policy_is_rejected`。
- 真 TCP（`crates/node-api/tests/rate_limit_http.rs`）：`anonymous_endpoints_return_429_with_retry_after`（挑战端点第 3 次 429，body 的 retry_after_ms 与 `retry-after` 头都体现）、`device_endpoints_return_429_after_the_device_budget`（设备预算 2，第 3 次鉴权请求 429，另一设备不受影响）、`health_is_never_rate_limited`。

已知限制：没有逐 IP/子网维度（不读代理头，同一 NAT 后共享匿名预算）；计数在进程内，多实例部署时每个实例各自计数（重启会清零，因此重启是绕过窗口预算的方式——生产部署需要集中式计数或持久计数）；没有“可信代理头策略”配置项，也没有按路由类别的差异化预算（RouteClass 粒度未落地）；429 不关闭连接、不惩罚性延长窗口。通过本函数用例不自动代表整个模块完成。

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

- [x] 节点在 append 返回后重启不丢日志。
- [x] 无私钥/正文诊断输出。

**实现状态**：已实现（Z 包，run_node 轮次）。源码：[apps/node/src/lib.rs](../../apps/node/src/lib.rs)（`spawn_node` / `RunningNode` / `run_from_env`，二进制约 [main.rs](../../apps/node/src/main.rs)），端点由 [crates/node-api](../../crates/node-api) 的 `build_router` 提供。

启动顺序（先持久层、后监听）：

1. 解析并校验配置（见 API-001 的当前范围）；
2. 读取种子、派生公钥（`--expect-public-key` 不一致则**在任何持久化之前**退出）；
3. 创建数据目录并 `NodeService::open`（执行 0001~0004 迁移并校验校验和、固定节点身份、校验策略）；
4. `NodeConfig.bind` 同步绑定端口（失败立即退出，不留“看似已启动”的进程）；
5. 在独立线程的 current-thread 运行时里 `axum::serve` 全部端点（挑战/注册/群日志/信箱/附件 blob/健康检查）；
6. 日志只输出一行 `chat-node started node_id=... address=... database=...`；
7. Ctrl+C → `with_graceful_shutdown` 停止接受新请求、等在途请求结束 → 服务线程退出、数据库连接随 `NodeService` 释放；最后打印 `chat-node stopped`。

`spawn_node` 返回的 `RunningNode` 携带真实绑定地址（`--bind 127.0.0.1:0` 时是系统分配的端口）、`node_id`、公钥与库路径，测试与运维都从返回值/日志拿到地址，不需要猜端口。

验证证据（`cargo test --offline -p chat-node` 全绿，8 个测试）：

- 进程内（`tests/smoke.rs::in_process_node_serves_health_and_shuts_down_cleanly`）：真 TCP `GET /v1/health` 返回 200 且带 `status/node_id/schema_version`，关闭后端口不可连，`node.db` 在监听之前已落盘；同时确认健康检查正文不含 `signing`/`secret`/`seed` 字样。
- 真实二进制（`tests/smoke.rs::binary_starts_logs_only_public_identity_and_serves_health`）：用 `CARGO_BIN_EXE_chat-node` 起子进程，解析启动日志里的 `address=`，做健康检查，并断言 stdout/stderr 都**不含种子的十六进制**。
- 库内单测 `node_id_is_derived_from_the_public_key_when_absent` 走真实 `spawn_node` + `shutdown`，并证明固定公钥不一致时拒绝启动且不建库。
- “append 返回后重启不丢日志”有**跨进程**证据：`crates/node-client/tests/cross_process.rs::node_process_and_client_survive_a_hard_restart` 用真实二进制跑完整链路后 `child.kill()`（强杀、不走优雅关闭），再用同一 data-dir + 同一 seed 重启，断言 head 的 seq/log_hash 不变、日志条目还在、未 ACK 的信箱条目还在、已发布 blob 仍可读，并验证重启后新 append 的 `seq = 旧 head + 1`、`previous_hash = 旧 log_hash`（详见“跨进程端到端（真 chat-node 进程 + NodeClient）”）。服务层另有 `crates/node-services` 的崩溃/重开用例（`tests/group_log.rs` 与 `p0_accept`）证明持久层语义。

已知限制：没有后台 GC worker（NODE-016）、没有 TLS 终止（默认只允许回环监听，公开部署需要前置 TLS 反向代理并显式 `--allow-public-bind`）、没有管理接口/指标、没有多实例协调与租约；`SHUTDOWN_TIMEOUT` 没有实现（优雅关闭没有超时上限，靠 axum 的在途请求自然结束）；进程崩溃后没有“staged 物恢复”步骤（附件块与日志都在 SQLite，单库单事务，因此不需要文件系统恢复）。通过本函数用例不自动代表整个模块完成。
## 能力令牌撤销接口与读端点强制（CAP-REV，2026-10-18，Windows x64）

~~~text
POST /v1/capabilities/{token_id}/revoke      （设备鉴权；body 可选 {"schema_version":1,"reason":"..."}）
  -> { schema_version, token_id, revoked: bool }     // 幂等：重复撤销 revoked=false
POST /v1/capabilities                          // 响应新增 capability.token_id（撤销用）
GET  /v1/mailbox、GET /v1/groups/{id}/log      // 强制模式下也要带未撤销的令牌（默认关闭）
~~~

- **读端点强制**（`AppState::capability_required()`，默认关闭、可由策略或环境变量打开）：
  请求必须带 `x-chat-capability`；令牌必须验签通过、**绑定到本设备**、未过期且**未被撤销**；
  缺失 → 401 `AUTH_REQUIRED`，被撤销 → 403 `CAPABILITY_REVOKED`，绑定不符 → 403 `INVALID_CAPABILITY`。
  关闭时完全放行（旧客户端行为不变）。
- 撤销立即生效：同一个令牌在撤销后的下一个请求就被拒（无需等过期）。
- 错误映射：`CAPABILITY_REVOKED` → 403（与其它凭据类拒绝一致）。

### 证据

- `group_mailbox_http::capability_revocation_takes_effect_immediately_over_http`：签发（响应带 token_id）
  → 带令牌写块 200 → 撤销（第一次 revoked=true，第二次 false）→ 同令牌写块 **403 CAPABILITY_REVOKED**。
- `group_mailbox_http::enforced_mode_requires_a_live_token_for_mailbox_reads`：默认关闭时信箱读取无令牌 200；
  打开强制后无令牌 401 `AUTH_REQUIRED`；带未撤销令牌 200；撤销后 403 `CAPABILITY_REVOKED`。
- 复现：`cargo test --offline -p chat-node-api` → 24 passed（既有 22 继续通过）。

## NODE-018 · 请求标识与请求级审计（2026-10-19，Windows x64）

每个 HTTP 请求都由中间件生成/透传一个请求标识，并在响应头与审计里贯通：

- 请求头 `x-chat-request-id`：调用方给的值若为 1..=64 字节可打印 ASCII 则原样复用，否则服务端派生 `req-<16 hex>`；
- 响应头 `x-chat-request-id`：**所有**响应都带（含错误响应）；
- 审计：每个请求写一条 `{kind: "http", outcome: ok|denied|failed, request_id, at_ms}`；
  撤销令牌额外写一条 `{kind: "capability_revoke", device_id, outcome, code?, request_id}`（成功/失败都记）。

代码：[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)（`request_audit_layer`）、
[crates/node-api/src/state.rs](../../crates/node-api/src/state.rs)（`RequestId` / `next_request_id` / `record_http_audit`）；
测试：[crates/node-api/tests/http_contract.rs](../../crates/node-api/tests/http_contract.rs)。

### 证据（`cargo test --offline -p chat-node-api`，31 passed）

- `http_contract::responses_carry_a_request_id_that_is_recorded_in_the_audit_tail`：无请求头时响应带
  `req-...` 且审计里能找到同一 id（outcome = ok）；调用方透传 `x-chat-request-id` 时响应原样回显、
  审计使用同一 id。

### 已知限制

1. 中间件只记录 HTTP 级结果；业务级细节（哪些对象、正文摘要）不记录（隐私约束）。
2. 请求标识由调用方可选提供，节点不做跨节点唯一性保证（只保证本节点内可检索）。

## NODE-018 · 审计只读端点 GET /v1/audit（2026-10-19，Windows x64）

审计日志有了，但只有进程内 API（`NodeService::audit_tail`）。本轮开放一个**只读 HTTP 端点**，
只暴露调用方自己的事件与节点级事件。

### 端点

    GET /v1/audit?limit=N&since_ms=T&kind=K
    响应 { events: [AuditEventDto], truncated: bool, filtered: u32, audit_enabled: bool }
    AuditEventDto { kind, at_ms, device_id (null = 节点级), group_id, outcome, code, request_id }

- **设备鉴权必需**（同一套签名挑战；查询串参与签名）；
- `limit`：默认 50、`1..=200`（越界 400，不静默夹取）；`truncated` 表示还有更早的事件被截掉；
- `since_ms`：只返回 `at_ms >= since_ms` 的事件；
- `kind`：白名单 `http / append / control / capability_issue / capability_revoke / auth / rate_limit`，
  未知种类 400；未知查询参数同样 400；
- **可见性**：只返回「本设备事件」与「节点级事件（`device_id = null`）」；其它设备的事件被过滤，
  并计入 `filtered`（不泄漏存在性之外的任何字段）；
- **审计关闭时**：`200 + events=[] + audit_enabled=false`（不报错、不伪造数据）。

### 证据（`cargo test --offline -p chat-node-api`，36 passed）

- `group_mailbox_http::audit_endpoint_returns_own_and_node_level_events_and_filters_others`：本设备事件与节点级事件可见，
  另一台设备的事件被过滤且 `filtered >= 1`；
- `audit_endpoint_validates_kind_and_limit`：已知 kind 200 / 未知 kind 400；`limit=200` 通过、`201`/`0` 400；未知查询参数 400；
- `audit_endpoint_truncates_and_respects_since_ms`：`limit=2` 时 `truncated=true` 且只回 2 条；`since_ms` 过滤生效；
- `audit_endpoint_requires_authentication`：**缺失**鉴权头 → 400 `INVALID_REQUEST`（与全仓既有约定一致）；
  带鉴权头但签名绑定到别的方法 → 401；
- `audit_endpoint_reports_a_disabled_audit_without_faking_events`：关闭审计后 `audit_enabled=false`、`events` 为空、`truncated=false`。

### 已知限制

1. 没有分页游标：只有 `limit` + `truncated`，要取更早的事件需要用 `since_ms` 反向逼近。
2. 过滤只按设备归属：群里其它成员的事件、以及「本设备不该看到的群事件」不做二次裁剪（本层不持有群成员关系）。
3. **缺失鉴权头返回 400 而不是 401**：这是全仓既有约定（`missing_auth_header` → `INVALID_REQUEST`），
   改成 401 会同时影响所有端点与既有用例，因此本端点沿用；401 保留给「有鉴权头但不合法/过期/签名不符」。


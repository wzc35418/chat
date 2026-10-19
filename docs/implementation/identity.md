# 账户、联系人、双设备与恢复

阶段：P1 → P2。状态：IDENTITY-001~012 已实现（001~007 纯密码学/状态层；008~012 设备凭据生命周期 + 正式加密存储持久化，验收命令与证据见下节）；IDENTITY-013/014（导出/导入恢复文件）未开始。目标源码：[crates/identity/src/service.rs](../../crates/identity/src/service.rs) 与 [crates/identity/src/registry.rs](../../crates/identity/src/registry.rs)。

AccountId 从规范长期公钥派生，设备密钥与传输端点分别绑定。初始设备保管主管身份密钥，第二设备不自动取得它。所有敏感读写经 storage 和系统密钥保护；签名与恢复 KDF 算法在 P0 锁定，不在此自造。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## 函数导航

- [IDENTITY-001 create_account](identity.md#identity-001)
- [IDENTITY-002 current_account](identity.md#identity-002)
- [IDENTITY-003 verify_credential](identity.md#identity-003)
- [IDENTITY-004 create_contact_invite](identity.md#identity-004)
- [IDENTITY-005 accept_contact_invite](identity.md#identity-005)
- [IDENTITY-006 confirm_contact](identity.md#identity-006)
- [IDENTITY-007 verification_fingerprint](identity.md#identity-007)
- [IDENTITY-008 begin_device_link](identity.md#identity-008)
- [IDENTITY-009 authorize_device](identity.md#identity-009)
- [IDENTITY-010 complete_device_link](identity.md#identity-010)
- [IDENTITY-011 list_devices](identity.md#identity-011)
- [IDENTITY-012 revoke_device](identity.md#identity-012)
- [IDENTITY-013 export_identity_recovery](identity.md#identity-013)
- [IDENTITY-014 import_identity_recovery](identity.md#identity-014)

## 实现状态与证据（P1-02 身份核心）

本轮交付 IDENTITY-001~007 的**纯密码学与状态层**：签名/验签、账户与设备 ID 派生、
人工核验指纹都是真实 Ed25519 / SHA-256；签名原文按 chat-protocol 的 canonical 编码
携带用途域。全部函数不访问网络、不写数据库、不模拟成功。

**验收命令与结果**（本轮实测）

~~~text
cargo test --offline -p chat-identity
  running 34 tests  ... test result: ok. 34 passed; 0 failed   (crates/identity/src/**)
  running 9 tests   ... test result: ok. 9 passed; 0 failed    (tests/identity_contract.rs)
cargo clippy --offline -p chat-identity --all-targets
  chat-identity 0 warnings
~~~

**源码位置**

| 文件 | 内容 |
|---|---|
| [crates/identity/src/keys.rs](../../crates/identity/src/keys.rs) | AccountKeys/DeviceKeys、AccountId/DeviceId 派生、Fingerprint、TrustAnchor |
| [crates/identity/src/device.rs](../../crates/identity/src/device.rs) | DeviceCapability/CapabilitySet、DeviceCredential 签发、verify_credential、RevocationSet |
| [crates/identity/src/invite.rs](../../crates/identity/src/invite.rs) | inv1 邀请口令 create_invite/verify_invite、InviteExpectations、VerifiedInvite |
| [crates/identity/src/contact.rs](../../crates/identity/src/contact.rs) | ContactState 状态机、Contact、record_identity/accept/confirm_identity_change |
| [crates/identity/src/service.rs](../../crates/identity/src/service.rs) | LocalAccount、AccountView、current_account、accept_contact_invite、confirm_contact、verification_fingerprint |
| [crates/identity/src/error.rs](../../crates/identity/src/error.rs) | IdentityError → chat-protocol ErrorCode 映射（不新增码） |
| [crates/identity/tests/identity_contract.rs](../../crates/identity/tests/identity_contract.rs) | 公开 API 端到端契约测试 |

**冻结的派生域与文本形式**

| 用途 | 规则 |
|---|---|
| AccountId | SHA-256(`chat/p1/account-id/v1` ‖ Ed25519 账户公钥) → `acct:1:<64 位小写 hex>` |
| DeviceId | SHA-256(`chat/p1/device-id/v1` ‖ AccountId 负载 ‖ 设备公钥) → `dev:1:<64 位小写 hex>` |
| 核验指纹 | SHA-256(`chat/p1/account-fingerprint/v1` ‖ 账户公钥)；展示为 16 组 4 位大写 hex、单空格分隔（79 字符） |
| 设备凭据签名 | `chat/p1/device-credential-signature/v1` ‖ canonical 正文 |
| 邀请口令签名 | `chat/p1/contact-invite-signature/v1` ‖ canonical 正文 |
| 邀请口令文本 | `inv1.<hex(固定 162 字节正文)>.<hex(64 字节签名)>` |

canonical 正文使用**位置固定**的 `WireValue::List`：协议层冻结的安全字段号表属于
protocol crate，identity 不能新增字段号，因此不借用 CURSOR_*/EXTENSION_* 字段号，
避免跨用途混淆。列表首元素是 schema 版本，随后是协议主/次版本，其余字段位置固定。
验签复用 protocol 的 `verify_ed25519`，签名复用 `sign_bytes`。

**独立测试向量**（用 OpenSSL/Node WebCrypto 独立算出，冻结派生与展示格式）

- 账户种子 `0x01 × 32` → 公钥 `8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c`
- AccountId `acct:1:804d17d85c433dc00878aceda0dff5d4d6d72dd6dbdabda188e4ae177304bffa`
- 指纹 `7EC8 C5E1 A37C C538 AFCB A0B0 BD28 C9E1 5A4B 1A3E 53DC 0F18 BE09 6DF9 4742 8EB4`
- 设备种子 `0x11 × 32` 在该账户下 DeviceId `dev:1:a4ec09edbb8ceb00f41b77fed48dbda0796e4456e2a99e813dfcad390bc9e1f5`

**上限（一律有界拒绝，不静默截断）**

| 项 | 上限 | 失败码 |
|---|---|---|
| 显示名 | 1..=64 字节、非空白、无控制字符 | INVALID_TARGET |
| 邀请有效期 | 60 s ..= 7 d（0 与溢出拒绝）；`DEFAULT_INVITE_TTL_MS` = 24 h 为产品默认 | INVALID_TARGET |
| 邀请口令文本 | 1024 字节（先限长再解析） | CONTENT_TOO_LARGE |
| 时钟偏差 | ±60 s | INVALID_CREDENTIAL |
| 设备凭据有效期 | 60 s ..= 400 d | INVALID_CREDENTIAL |
| 设备能力数量 | ≤ 8，未知能力位拒绝 | REQUIRED_CAPABILITY_MISSING / CONTENT_TOO_LARGE |
| 撤销集合 | ≤ 64 台（排序去重） | CONTENT_TOO_LARGE |
| 指纹文本 | ≤ 96 字节 | CONTENT_TOO_LARGE |

**错误码映射**（`IdentityError::code()`，全部取自 protocol 冻结码表）

| IdentityError | 冻结码 | 备注 |
|---|---|---|
| InvalidId(_) | INVALID_ID | 透传 PROTOCOL-001 原因 |
| InvalidDisplayName / InvalidInviteLifetime / WrongInviteNode / WrongInviteAccount / WrongInviteDevice | INVALID_TARGET | 参数/绑定目标不合法 |
| InvalidTimestamp / MalformedToken | MALFORMED_FRAME | 结构或时间戳畸形 |
| InvalidAccountPublicKey / AccountKeyMismatch / InvalidCredential / CredentialExpired / CredentialNotYetValid / CredentialStale / CrossAccountDevice / DeviceIdMismatch / IdentityChanged / InviteNotYetValid / MissingIdentity | INVALID_CREDENTIAL | 凭据/身份不可信（detail_key 区分细分原因） |
| InvalidSignature | INVALID_SIGNATURE | Ed25519 验签失败 |
| DeviceRevoked | DEVICE_REVOKED | 设备在撤销集合内 |
| EmptyCapabilities / UnknownCapability / WrongInviteScope | REQUIRED_CAPABILITY_MISSING | 作用域/能力不满足 |
| TooManyCapabilities / TooManyRevocations / TokenTooLong | CONTENT_TOO_LARGE | 超过上限 |
| UnsupportedTokenVersion | PROTOCOL_UNSUPPORTED | 口令/凭据版本不支持 |
| InviteExpired | CURSOR_EXPIRED | 码表无 INVITE_EXPIRED，复用「不透明令牌过期」码 |
| InvalidStateTransition | INVALID_STATE_TRANSITION | 联系人迁移非法 |
| ContactNotAccepted | CONTACT_NOT_ACCEPTED | 未确认联系人禁止发送 |

所有身份错误 `retryable() == false`（验证/状态类失败不得自动重试），有测试断言。

**已知限制**

- 不提供「先解析、后验签」的第二套入口：`classify_invite_text` 只校验口令信封
  （前缀/长度/hex 规范形式），账户/设备/节点等身份字段只有 `verify_invite` 之后才可得，
  避免 UI 展示未验证身份。
- 每账户 ≤2 有效设备的约束由 IDENTITY-009（authorize_device）在持久化注册表上执行（MAX_ACTIVE_DEVICES）；纯签发函数 create_device_credential 本身不计数。
- 持久化未接：IDENTITY-001 的 T1 事务与密钥包装、IDENTITY-002 的锁状态与数据库读取、
  邀请一次性消费、联系人/设备落盘都还没实现。crate 仍声明 `chat-storage` 依赖但当前
  不调用；这些由 storage/core 集成任务补齐，本层不假装已持久化。
- IDENTITY-013/014（导出/导入恢复文件）未开始；IDENTITY-008~012 已实现，证据见上节「设备凭据生命周期与撤销集合」。
- 设备凭据的端点绑定保存 32 字节端点标识，未绑定 iroh EndpointId 类型（不引入
  transport 依赖）；等 transport 集成时做显式映射。
- 邀请口令内只有 16 字节 nonce 供上层去重；重放窗口与一次性消费状态需要持久化。
- 时间由调用方注入 `now_ms`（生产用系统 UTC 时钟）；本层不做时钟漂移策略与回拨检测。

**Cargo.lock 变化**：`chat-identity` 的依赖列表新增 `ed25519-dalek 3.0.0`、`rand 0.10.3`、
`zeroize 1.9.1`（三者均已在 Cargo.lock 中，版本未变，无新增传递依赖）；
`chat-storage` 依赖保留。

## 设备凭据生命周期与撤销集合（IDENTITY-008~012）

本轮追加交付：设备配对 → 授权 → 完成 → 列表 → 撤销的持久化闭环。密码学仍为真实
Ed25519/SHA-256，持久化只经 chat-storage 的正式加密存储（迁移 0006），不访问网络。

**验收命令与结果**（本轮实测）

~~~text
cargo test --offline -p chat-storage
  11 + 6 + 2 + 1 + 7 + 8 + 6 + 6 + 8 + 7 + 17 项全绿（79 passed；含新增 formal_identity_devices 6 项）
cargo test --offline -p chat-identity
  34 lib 单元 + 9 device_registry + 9 identity_contract = 52 passed / 0 failed
cargo clippy --offline -p chat-storage -p chat-identity --all-targets
  新增/改动文件 0 警告（storage 另有 6 条既有警告，属旧代码）
~~~

**新增/改动的文件**

| 文件 | 内容 |
|---|---|
| [crates/storage/migrations/0006_device_credentials.sql](../../crates/storage/migrations/0006_device_credentials.sql) | 新迁移：`identity_device_credentials` / `identity_device_revocations` / `identity_device_revocation_epoch`（0001~0005 未改） |
| [crates/storage/src/formal/identity.rs](../../crates/storage/src/formal/identity.rs) | `save_device_credential` / `load_device_credential` / `list_device_credentials` / `revoke_device` / `revoked_devices` / `device_revocation_generation` |
| [crates/storage/src/formal/migrations.rs](../../crates/storage/src/formal/migrations.rs) | 登记 0006（**必要相邻改动**：迁移是显式登记表，不登记不会执行） |
| [crates/storage/src/formal/mod.rs](../../crates/storage/src/formal/mod.rs) | 导出新类型（**必要相邻改动**：`identity` 子模块私有） |
| [crates/storage/tests/formal_identity_devices.rs](../../crates/storage/tests/formal_identity_devices.rs) | 新增 6 项设备凭据/撤销存储测试 |
| [crates/storage/tests/formal_store.rs](../../crates/storage/tests/formal_store.rs)、[attachments_records.rs](../../crates/storage/tests/attachments_records.rs) | schema 版本断言 5 → 6（**必要相邻改动**：0006 把版本提升到 6） |
| [crates/identity/src/registry.rs](../../crates/identity/src/registry.rs) | IDENTITY-008~012 门面（配对/授权/完成/列表/撤销/发送前检查） |
| [crates/identity/src/error.rs](../../crates/identity/src/error.rs) | `StorageErrorKind` 与设备生命周期错误，仍映射冻结码表 |
| [crates/identity/tests/device_registry.rs](../../crates/identity/tests/device_registry.rs) | 新增 9 项端到端测试 |

**存储层语义（迁移 0006）**

- 表名统一加 `identity_` 前缀：0001 已有 P0 表 `device_credentials`，本迁移不复用、不改写它。
- 明文列只有不可逆索引与时间戳：`device_index` = SHA-256(`chat/storage/device-index/v1` ‖ account_id ‖ device_id)，
  `account_index` 同 0003；其余（设备公钥、端点、能力位、签名、撤销正文）都在密封记录里。
- 密封复用 identity-account 子密钥，但使用独立 AAD 标签 `device-credential-v1` / `device-revocation-v1`，
  且 AAD 绑定（设备索引 + 账户索引 + 标签）：挪行、换账户、换设备、换版本全部 STATE_CORRUPTED；
  明文时间列与密封记录不一致同样 STATE_CORRUPTED。
- 幂等：完全相同保存返回 `already_existed=true` 且不写行；同设备续期必须保持同一设备公钥、
  `issued_at` 不允许回退，否则 INVALID_STATE_TRANSITION；换设备公钥按 INVALID_INPUT 拒绝。
- 撤销终态：已撤销设备的任何再签发都 INVALID_STATE_TRANSITION（不可静默复活）；重复撤销幂等返回
  既有记录（版本与时间都不变）；账户撤销版本 `device_revocation_epoch` 单调 +1。
- 上限（有界拒绝）：凭据 ≤8/账户、撤销 ≤64/账户、能力位只允许冻结位、`issued_at > 0` 且 `expires_at > issued_at`。
- 跨账户隔离：索引由（账户, 设备）共同派生，用别的账户查询得到 `None`/空列表；行内账户索引不一致 → STATE_CORRUPTED。

**身份层语义（IDENTITY-008~012）**

- 配对：设备密钥安全随机 + 16 字节 nonce + 6 位核对短码；TTL 30 s..=10 min；过期后授权与完成都 LINK_EXPIRED。
- 授权：有效期 → 设备未撤销 → 有效设备数 < `MAX_ACTIVE_DEVICES`(2) → 用**当前账户撤销版本**签发 → 落库。
- 完成：先要求凭据绑定本机待配对公钥（LINK_MISMATCH），再用凭据内账户公钥建信任锚验签/期限/撤销。
- 列表：按稳定设备 ID 排序，状态 active·expired·revoked；跨账户查询为空。
- 撤销：最后一台有效设备拒绝（PERMISSION_DENIED，需恢复流程）；撤销推进账户撤销版本，
  其余设备必须重新签发否则按 INVALID_CREDENTIAL（detail `identity.credential_stale`）拒绝。
- 发送/解密前必须调用 `require_active_device`：过期 → INVALID_CREDENTIAL，撤销 → DEVICE_REVOKED，
  版本落后 → INVALID_CREDENTIAL，缺能力 → REQUIRED_CAPABILITY_MISSING；不存在 → INVALID_CREDENTIAL。

**错误码新增映射**（仍全部取自 protocol 冻结码表，无新增码）

| IdentityError | 冻结码 |
|---|---|
| Storage(Locked) | STORAGE_LOCKED（唯一可自动重试） |
| Storage(Corrupted) | STATE_CORRUPTED |
| Storage(Failure) | STORAGE_FAILURE |
| Storage(InvalidInput) | INVALID_TARGET |
| Storage(Conflict) | INVALID_CREDENTIAL |
| Storage(InvalidStateTransition) | INVALID_STATE_TRANSITION |
| DeviceLimitExceeded | DEVICE_LIMIT_EXCEEDED |
| LastDeviceRequiresRecovery | PERMISSION_DENIED |
| LinkExpired | CURSOR_EXPIRED（复用"不透明令牌过期"码） |
| InvalidLinkTtl | INVALID_TARGET |
| LinkMismatch / DeviceNotFound | INVALID_CREDENTIAL |
| MissingCapability | REQUIRED_CAPABILITY_MISSING |

**已知限制**

- 待配对秘密不持久化：重启后必须重新生成配对请求（PLATFORM-001 密钥保护未接入）；二维码编码未实现。
- 为全部会话/群排队密钥更新（SYNC-001、GROUPS-003、CRYPTO 组密钥轮换）与撤销广播未实现，本层不发网络请求。
- 跨进程并发授权没有额外串行化：当前依赖"单写者存储 + 单进程调用方"；"最后一台有效设备"检查是
  两次读取后的尽力而为判定（不在同一事务内），并发撤销需要 core 事务层串行化才能严格保证。
- 撤销一个设备会推进账户撤销版本，因此其余设备必须重新签发凭据；这是有意的严格策略
  （换取"撤销不可静默忽略"），已由 revocation_is_terminal_and_remaining_devices_must_be_reissued 固化。
- storage 层不做密码学校验（签名、ID 派生由 chat-identity 在写入前完成），只做结构/绑定自洽与密封。
- schema 版本因 0006 由 5 提升为 6：既有库在 `open_store` 时自动迁移并校验校验和；库内版本更高仍 SCHEMA_TOO_NEW。
- 本轮未改动任何依赖版本与 Cargo.lock（chat-storage 与 uuid dev-dep 已存在）。

<a id="identity-001"></a>

## IDENTITY-001 · create_account

~~~text
create_account(display_name: String, protector: KeyProtector) -> Result<LocalAccount>
~~~

**职责与输出**：原子建立账户、主管设备凭据和受保护密钥引用。

**输入与前置条件**：尚无本地账户；显示名有长度限制；安全随机与密钥保护可用。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验显示名。
2. 生成账户/设备密钥。
3. 派生 AccountId。
4. 签发初始设备凭据。
5. 包装存储主密钥。
6. 原子写入账户和凭据。

**持久化、并发与副作用**：T1：密钥引用/账户/初始凭据一事务；平台包装失败不写明文；孤立包装槽补偿删除。

**失败处理**：ACCOUNT_EXISTS，STORAGE_LOCKED，KEY_PROTECTION_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLATFORM-001 wrap_storage_key](platform.md#platform-001)、[IDENTITY-003 verify_credential](identity.md#identity-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 创建中崩溃只恢复完整账户。（需要 T1 存储事务，未实现）
- [x] 昵称改变不影响 ID。— tests/identity_contract.rs::account_identity_is_stable_across_restarts

**实现状态**：本地密码学部分已实现（[service.rs](../../crates/identity/src/service.rs) 的 `LocalAccount::create` / `LocalAccount::from_seed`，密钥与派生在 [keys.rs](../../crates/identity/src/keys.rs)）；显示名上限与非法字符由 `display_name_and_bounds_are_enforced` 覆盖。原子建号、密钥包装（PLATFORM-001）与初始凭据落盘（T1）未实现：本层不写库、不做密钥保护。

<a id="identity-002"></a>

## IDENTITY-002 · current_account

~~~text
current_account() -> Result<Option<LocalAccount>>
~~~

**职责与输出**：返回脱敏账户视图，无账户返回 None，锁定返回专门错误。

**输入与前置条件**：storage 已打开或明确处于锁定状态。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查锁状态。
2. 读取账户元数据。
3. 映射显示名/ID/设备摘要。
4. 过滤全部密钥字段。

**持久化、并发与副作用**：只读快照；不触发建号。

**失败处理**：STORAGE_LOCKED，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 首次启动 None。— `current_account(None, 0)` 返回 None（tests/identity_contract.rs::account_view_and_debug_do_not_leak_secrets）
- [ ] 数据库锁定不能伪装成无账户。（需要 storage 锁状态，未实现）

**实现状态**：脱敏投影已实现：`AccountView` 只含账户 ID、显示名、指纹与设备数，`current_account` 不读库、不做 I/O；Debug 脱敏有测试。锁状态与数据库读取由 storage 集成任务补齐，本层不把「无账户」与「锁定」混为一谈（调用方需自己区分）。

<a id="identity-003"></a>

## IDENTITY-003 · verify_credential

~~~text
verify_credential(credential: DeviceCredential, trust: TrustAnchor, now: Time) -> Result<VerifiedCredential>
~~~

**职责与输出**：校验设备公钥、端点、账户和有效期绑定。

**输入与前置条件**：主管账户公钥已由邀请/本地信任锚确定，时间偏差策略已配置。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 有界解码。
2. 重算账户 ID。
3. 验证账户签名。
4. 检查有效期/撤销版本。
5. 绑定设备签名公钥与 EndpointId。

**持久化、并发与副作用**：纯验证；服务端新公钥不能覆盖既有信任。

**失败处理**：INVALID_CREDENTIAL，DEVICE_REVOKED，IDENTITY_CHANGED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-001 parse_id](protocol.md#protocol-001)、[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 替换端点/设备公钥拒绝。— device::tests::tampered_signature_and_device_key_are_rejected（改端点、换设备公钥、改能力位：验签失败或绑定失败）
- [x] 时间偏差边界。— device::tests::expiry_and_clock_skew_are_enforced（±60 s；失效时刻即过期）
- [x] 信任根不同明确提示。— device::tests::cross_account_is_rejected → INVALID_CREDENTIAL

**实现状态**：已实现（[device.rs](../../crates/identity/src/device.rs) 的 `verify_credential`）。校验顺序固定：信任锚绑定 → 账户/设备 ID 自洽 → 撤销集合（设备撤销、版本落后）→ 账户签名（先验签后看时间）→ 期限与时钟偏差。`TrustAnchor` 拒绝不可解压与低阶（weak）公钥。

<a id="identity-004"></a>

## IDENTITY-004 · create_contact_invite

~~~text
create_contact_invite(expires_in: Duration) -> Result<ContactInvite>
~~~

**职责与输出**：生成带公钥指纹、节点提示和受限投递能力的一次性邀请。

**输入与前置条件**：已解锁账户；邀请有效期在本地/节点允许范围。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 分配邀请 ID/随机能力。
2. 限定 contact-request 作用域。
3. 签名正文。
4. 本地持久化期限和消费状态。
5. 编码二维码文本。

**持久化、并发与副作用**：T1：邀请记录先落盘后展示；失败不泄漏可用未记录能力。

**失败处理**：INVALID_EXPIRY，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 过期与重复消费拒绝。（过期已拒绝并有测试；一次性消费状态需要持久化，未实现）
- [x] 邀请不能直接授权所有群/旧历史。— 作用域冻结为 `InviteScope::ContactRequest`，正文里的其它作用域值解码即拒（invite::tests::tampered_body_is_rejected）

**实现状态**：已实现（[invite.rs](../../crates/identity/src/invite.rs) 的 `create_invite`）。token 为 `inv1.<hex(162 字节正文)>.<hex(64 字节签名)>`；有效期 1 min..=7 d；nonce 16 字节安全随机；正文含账户 ID/公钥、设备 ID、节点、作用域与时间。未落盘：一次性消费与「先落盘再展示」的 T1 事务由 storage 集成任务补齐。

<a id="identity-005"></a>

## IDENTITY-005 · accept_contact_invite

~~~text
accept_contact_invite(encoded: String) -> Result<ContactRequest>
~~~

**职责与输出**：建立待确认联系人请求，不自动信任身份变化。

**输入与前置条件**：来自二维码/粘贴的输入完全不可信。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 限长解码。
2. 验证邀请签名与期限。
3. 核对已有联系人指纹。
4. 保存待确认请求。
5. 提交受限请求消息。

**持久化、并发与副作用**：本地请求与 outbox 同事务；网络失败仍可恢复待处理状态。

**失败处理**：INVITE_EXPIRED，IDENTITY_CHANGED，INVALID_CAPABILITY。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)、[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重复扫描返回同请求。（需要持久化请求记录，未实现；口令内 nonce 供上层去重）
- [x] 换公钥进入警告。— 已固定信任锚与邀请公钥不一致 → INVALID_TARGET（invite_rejections_have_stable_codes）；已接受联系人观测到新指纹 → IdentityChanged 且 can_send=false（contact::tests::identity_change_blocks_sending_until_explicit_reconfirmation）
- [x] 未确认不能正式私聊。— `Contact::requested` 状态下 `can_send()==false`、`require_sendable()` → CONTACT_NOT_ACCEPTED（end_to_end_account_device_invite_and_contact）

**实现状态**：已实现（[invite.rs](../../crates/identity/src/invite.rs) 的 `classify_invite_text`、[service.rs](../../crates/identity/src/service.rs) 的 `accept_contact_invite`）。校验顺序：限长 → 前缀/结构 → hex 规范形式 → 正文长度/版本/作用域 → 时间窗口自洽 → 账户 ID 自洽 → 签名 → 信任锚 → 期望节点/账户/设备 → 本地过期。失败不会自动信任，联系人停在 Requested。outbox 请求消息与本地同事务未实现。

<a id="identity-006"></a>

## IDENTITY-006 · confirm_contact

~~~text
confirm_contact(request_id: RequestId) -> Result<Contact>
~~~

**职责与输出**：记录用户确认的信任锚和正常投递能力。

**输入与前置条件**：用户已核验 UI 展示的公钥指纹/请求 ID。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 锁请求。
2. 核对未过期未被拒绝。
3. 保存信任锚。
4. 完成双向能力握手。
5. 设置联系人 accepted。

**持久化、并发与副作用**：T1：联系人接受与待发送确认同事务；对端状态未完成时显示等待。

**失败处理**：CONTACT_REQUEST_NOT_FOUND，INVITE_EXPIRED，IDENTITY_CHANGED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 重复确认幂等。— contact::tests::requested_accept_pins_anchor_and_enables_send
- [x] 离线确认不假称对端已完成。— 只写本地 ContactState::Accepted 并固定信任锚，不产生任何「对端已完成」证据（本 crate 不发网络请求、不伪造回执）

**实现状态**：已实现（[contact.rs](../../crates/identity/src/contact.rs) 的 `Contact::accept`、[service.rs](../../crates/identity/src/service.rs) 的 `confirm_contact`）。接受需要已观测指纹；与已固定锚不一致时转 IdentityChanged 并失败；IdentityChanged 只能通过显式 `confirm_identity_change`（用户重新核对指纹）恢复。待发送确认事务与双向能力握手未实现。

<a id="identity-007"></a>

## IDENTITY-007 · verification_fingerprint

~~~text
verification_fingerprint(account_id: AccountId) -> Result<String>
~~~

**职责与输出**：返回稳定可人工核验指纹。

**输入与前置条件**：账户公钥来自已固定信任记录。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取公钥。
2. 加入 fingerprint 用途域。
3. 按锁定算法计算。
4. 分组格式化。
5. 返回可扫码版本。

**持久化、并发与副作用**：只读；显示名/节点地址不参与指纹。

**失败处理**：CONTACT_NOT_FOUND，STORAGE_LOCKED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 更换节点指纹不变。— 指纹只由账户公钥与用途域派生，节点/端点/显示名不参与（keys::tests::derivations_are_domain_separated_and_key_bound）
- [x] 公钥改变指纹改变。— keys::tests::account_id_and_fingerprint_match_independent_vectors（冻结向量）

**实现状态**：已实现（[keys.rs](../../crates/identity/src/keys.rs) 的 `Fingerprint`、[service.rs](../../crates/identity/src/service.rs) 的 `verification_fingerprint`）。展示为 16 组 4 位大写 hex（79 字符）；`Fingerprint::parse` 接受分组/紧凑形式并做限长与字符校验。二维码承载形式（可扫码版本）未实现，等 UI 集成。

<a id="identity-008"></a>

## IDENTITY-008 · begin_device_link

~~~text
begin_device_link() -> Result<DeviceLinkRequest>
~~~

**职责与输出**：第二设备生成本机密钥和一次性短期配对请求。

**输入与前置条件**：未关联账户；设备本地密钥保护可用。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 生成设备/端点密钥。
2. 创建 nonce 和短期请求。
3. 持久化本地待配对秘密。
4. 输出公钥/端点/验证码二维码。

**持久化、并发与副作用**：本机待配对记录原子写入；私钥永不进入二维码。

**失败处理**：LINK_ALREADY_ACTIVE，KEY_PROTECTION_UNAVAILABLE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLATFORM-001 wrap_storage_key](platform.md#platform-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 重启恢复配对。（待配对记录目前只在内存；持久化待配对秘密需要 platform 密钥保护，未实现）
- [x] 过期必须重新生成。— 过期后 authorize/complete 显式 LINK_EXPIRED（tests/device_registry.rs::link_mismatch_and_expired_links_are_rejected）
- [x] 二维码无私钥。— PendingDeviceLink 只暴露设备公钥与 6 位核对短码，Debug 脱敏（link_request_is_bounded_and_has_a_stable_verification_code）

**实现状态**：已实现纯本地部分（[registry.rs](../../crates/identity/src/registry.rs) 的 `begin_device_link` / `PendingDeviceLink`）：设备密钥安全随机、16 字节 nonce、TTL 30 s..=10 min、短码由公钥+nonce 域分隔摘要派生。待配对秘密的持久化（PLATFORM-001 密钥保护）与二维码编码未实现。

<a id="identity-009"></a>

## IDENTITY-009 · authorize_device

~~~text
authorize_device(request: DeviceLinkRequest, confirmation: UserConfirmation) -> Result<DeviceAuthorization>
~~~

**职责与输出**：初始主管设备签发第二设备授权及会话更新任务。

**输入与前置条件**：持有主管身份密钥；用户已核对短码；当前有效设备少于2。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验请求持有证明与期限。
2. 比较确认绑定摘要。
3. 签发设备凭据。
4. 登记设备。
5. 为全部会话生成加入任务。

**持久化、并发与副作用**：凭据/设备计数/任务一事务；网络副作用提交后进行。

**失败处理**：DEVICE_LIMIT_EXCEEDED，LINK_EXPIRED，INVALID_PROOF。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)、[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 两次并发授权仅允许一台新增设备。（当前是单写者存储；跨进程并发需要 core 事务层串行化，未实现）
- [x] 管理员身份不能替代账户主管授权。— 签发只接受 AccountKeys（账户根密钥），没有"管理员"这条路径（authorize_device 签名由账户公钥验签）

**实现状态**：已实现（[registry.rs](../../crates/identity/src/registry.rs) 的 `authorize_device`）：校验配对请求期限 → 设备未撤销 → 有效设备数 < 2 → 用当前账户撤销版本签发凭据 → 写入正式加密存储。测试：active_device_limit_is_two、revocation_is_terminal_and_remaining_devices_must_be_reissued。为全部会话生成加入任务（SYNC-001）未实现。

<a id="identity-010"></a>

## IDENTITY-010 · complete_device_link

~~~text
complete_device_link(result: DeviceAuthorization) -> Result<LinkStatus>
~~~

**职责与输出**：新设备保存自己的已授权凭据，逐会话等待 Welcome。

**输入与前置条件**：result 与本机待配对 nonce、公钥、端点完全匹配。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证主管签名。
2. 检查一次性关联。
3. 保存账户公钥和设备凭据。
4. 消费 nonce。
5. 建立 self-sync 加入任务。

**持久化、并发与副作用**：T1：凭据与 nonce 消费原子化；不会获得主管私钥/旧历史。

**失败处理**：INVALID_CREDENTIAL，LINK_MISMATCH，LINK_EXPIRED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[IDENTITY-003 verify_credential](identity.md#identity-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 授权给不同公钥拒绝。— LINK_MISMATCH（link_mismatch_and_expired_links_are_rejected）
- [x] 重放不创建重复设备。— 设备索引 = H(账户, 设备 ID) 主键 + 幂等保存（storage 测试 device_credential_roundtrip_survives_reopen 的幂等断言）
- [x] 加入前历史不自动出现。— 只保存设备凭据，不复制任何历史/密钥；存储里没有可复制的历史行

**实现状态**：已实现（[registry.rs](../../crates/identity/src/registry.rs) 的 `complete_device_link`）：先校验本机待配对公钥一致，再用凭据内账户公钥建信任锚、校验签名/期限/撤销，最后落库。self-sync 加入任务（SYNC-001）与 Welcome 未实现。

<a id="identity-011"></a>

## IDENTITY-011 · list_devices

~~~text
list_devices() -> Result<Vec<DeviceSummary>>
~~~

**职责与输出**：列出设备、撤销状态和各会话密钥更新进度。

**输入与前置条件**：账户已解锁；查询仅返回公有摘要。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读当前凭据。
2. 关联撤销任务。
3. 区分有效/已撤销/待更新。
4. 返回按稳定 ID 排序视图。

**持久化、并发与副作用**：只读快照；不以“最近在线”推断凭据有效。

**失败处理**：STORAGE_LOCKED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 离线设备仍显示。— 列表完全由持久化记录派生，不依赖在线状态（authorize_complete_and_list_survive_reopen 重开后仍列出）
- [ ] 部分群撤销未完成可追踪。（每群密钥更新任务属于 SYNC/GROUPS，未实现）

**实现状态**：已实现（[registry.rs](../../crates/identity/src/registry.rs) 的 `list_devices`）：返回设备 ID/公钥/能力/签发与失效时间/状态（active·expired·revoked），按稳定设备 ID 排序；跨账户查询天然为空。

<a id="identity-012"></a>

## IDENTITY-012 · revoke_device

~~~text
revoke_device(device: DeviceId) -> Result<RevocationJob>
~~~

**职责与输出**：持久标记撤销并建立每会话移除设备任务。

**输入与前置条件**：主管权限明确；默认防止误删唯一可管理设备。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对归属/授权。
2. 递增撤销版本。
3. 禁用本地新投递能力。
4. 枚举相关会话。
5. 为每群排队密钥更新并发布撤销记录。

**持久化、并发与副作用**：T1：撤销/子任务/outbox 原子化；跨群完成非原子，显示每群状态。

**失败处理**：PERMISSION_DENIED，DEVICE_NOT_FOUND，LAST_DEVICE_REQUIRES_RECOVERY。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[SYNC-001 enqueue_job](sync.md#sync-001)、[GROUPS-003 propose_group_operation](groups.md#groups-003)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 部分节点离线任务保持 pending。（outbox 任务与节点投递未接，本层不发网络请求）
- [ ] 完成的群后续消息撤销设备不可解密。（MLS 密钥更新由 CRYPTO/GROUPS 负责，未在此实现）

**实现状态**：已实现持久化部分（[registry.rs](../../crates/identity/src/registry.rs) 的 `revoke_device` + storage 的 `revoke_device`/`revoked_devices`）：最后一台有效设备拒绝撤销（PERMISSION_DENIED）、撤销幂等且版本单调 +1、撤销记录密封落盘、已撤销设备不能被再签发复活；发送/解密前由 `require_active_device` 显式拒绝。每群密钥更新任务与撤销广播未实现。

<a id="identity-013"></a>

## IDENTITY-013 · export_identity_recovery

~~~text
export_identity_recovery(destination: FileToken, secret: RecoverySecret) -> Result<ExportReceipt>
~~~

**职责与输出**：导出只含身份恢复材料的认证加密文件。

**输入与前置条件**：显式用户操作；恢复秘密经安全输入取得；token 仅授权目标文件。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取主管材料。
2. 选择版本化 KDF/AEAD。
3. 生成独立盐和参数。
4. 加密认证。
5. 写临时受控文件并同步后原子完成。
6. 清理缓冲区。

**持久化、并发与副作用**：外部文件不在 DB 事务；成功后记录无秘密的导出时间；失败清临时文件。

**失败处理**：PERMISSION_DENIED，WEAK_RECOVERY_SECRET，FILE_IO_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLATFORM-006 open_file_token](platform.md#platform-006)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 错误口令不泄漏明文。
- [ ] 断电不产出貌似有效半文件。
- [ ] 文件不包含聊天历史。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="identity-014"></a>

## IDENTITY-014 · import_identity_recovery

~~~text
import_identity_recovery(source: FileToken, secret: RecoverySecret) -> Result<RecoveryStatus>
~~~

**职责与输出**：恢复身份并生成新的本机设备，恢复后仍需处理旧设备和群加入。

**输入与前置条件**：无冲突本地身份；导入参数和 KDF 资源有上限。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 有界读取。
2. 验证版本/参数预算。
3. 解密并校验身份。
4. 生成新设备。
5. 包装存储密钥。
6. 原子保存。
7. 创建后续撤销/重新加入向导。

**持久化、并发与副作用**：账户/新设备一事务；失败不覆盖原账户；不能仅有身份钥就假称已有各群 MLS 状态。

**失败处理**：RECOVERY_AUTH_FAILED，RECOVERY_LIMIT_EXCEEDED，IDENTITY_CONFLICT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PLATFORM-006 open_file_token](platform.md#platform-006)、[IDENTITY-001 create_account](identity.md#identity-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 伪造 KDF 极大内存拒绝。（`ALLOWED_ARGON2_PARAMS` 白名单：m_cost 只接受 19456/47104/65536 KiB，
  其它值导出与导入都显式 `RECOVERY_UNSUPPORTED`；见 recovery 用例的“头部参数改成白名单外的值”断言）
- [x] 错误口令无副作用。（KCV 不匹配 → `RECOVERY_BAD_PASSPHRASE`，目标库读写前后都是空）
- [x] 旧历史仍不可恢复。（恢复文件**不含存储主密钥**，因此旧库的消息/附件无法解开——见下节限制）

**实现状态**：已实现（IDENTITY-013/014）。见下文“身份恢复文件实现记录”节的公开面、语义与证据。
恢复文件里**没有**存储主密钥，所以"换机器拿回身份"≠"拿回旧库历史"。

## 身份恢复文件实现记录（IDENTITY-013/014，2026-10-18，Windows x64）

代码：[crates/identity/src/recovery.rs](../../crates/identity/src/recovery.rs)（新）；测试：
[crates/identity/tests/recovery.rs](../../crates/identity/tests/recovery.rs)（4 个，真临时加密库）。

### 公开面

~~~text
pub fn export_recovery_bundle(store: &Store, passphrase: &str) -> RecoveryResult<Vec<u8>>
pub fn export_recovery_bundle_with_params(store, passphrase, Argon2Params) -> RecoveryResult<Vec<u8>>
pub fn import_recovery_bundle(store: &mut Store, bundle: &[u8], passphrase: &str)
    -> RecoveryResult<RecoveryIdentityView>

RecoveryIdentityView { account_id, device_id, account_public_key, device_public_key,
                       display_name, created_at_ms, devices_digest }
Argon2Params { m_cost_kib, t_cost, p_cost }        ALLOWED_ARGON2_PARAMS（3 组白名单）
RecoveryError::code() -> RECOVERY_BAD_PASSPHRASE / RECOVERY_CORRUPTED / RECOVERY_UNSUPPORTED /
                         RECOVERY_TARGET_NOT_EMPTY / INVALID_INPUT / storage 冻结码
pub const MAX_RECOVERY_BUNDLE_BYTES = 1 MiB
~~~

### 文件格式（全大端）

~~~text
magic(8)="chatreco" | version(1)=1 | m_cost(4) | t_cost(4) | p_cost(4) | salt(16) | kcv(16)
| nonce(24) | XChaCha20-Poly1305(明文 JSON ‖ tag)
明文 JSON：format_version, account_id, device_id, display_name, created_at_ms,
           account_public_key, device_public_key, devices_digest, account_seed, device_seed
AAD = magic ‖ version ‖ 参数 ‖ salt（头被改就解不开）
~~~

### 关键语义

1. **KDF**：argon2id（V0x13）派生 32 字节口令密钥；盐每次导出随机 16 字节；
   参数写成文件头并可版本化；**参数必须在白名单里**（导出与导入都校验）。
2. **口令错误 vs 文件被篡改**：文件头带 **KCV**（`H("recovery-kcv/v1"‖密钥)` 前 16 字节）。
   导入先比 KCV：不匹配 → `RECOVERY_BAD_PASSPHRASE`；KCV 匹配但 AEAD 认证失败 →
   `RECOVERY_CORRUPTED`（正文被改）。这是本轮为“两种错误要给出不同稳定码”而加的唯一设计取舍。
3. **绝不覆盖**：导入前要求目标库既没有账户投影也没有种子记录，否则 `RECOVERY_TARGET_NOT_EMPTY`。
4. **自洽校验**：种子必须能派生出载荷里声明的账户/设备 ID 与公钥，设备列表摘要也必须重算一致，
   否则 `RECOVERY_CORRUPTED`（防止"改 ID 指向别人的账户"）。
5. **不含主密钥**：恢复文件只有身份材料；旧库的消息/附件/联系人**无法解密**（主密钥不参与导出）。

### 错误码（本层自有码表，不改 PROTOCOL-014）

| 情形 | code |
|---|---|
| 口令错（KCV 不匹配） | `RECOVERY_BAD_PASSPHRASE` |
| 结构损坏/正文被篡改/自洽校验失败 | `RECOVERY_CORRUPTED` |
| 版本或 argon2 参数不受支持 | `RECOVERY_UNSUPPORTED` |
| 目标库已有身份 | `RECOVERY_TARGET_NOT_EMPTY` |
| 口令/文件越界、库里没有身份 | `INVALID_INPUT` |
| 存储失败 | storage 冻结码（`STORAGE_*`） |

### 复现命令（全程离线）

~~~powershell
. .\tools\activate.ps1
cargo test --offline -p chat-identity --test recovery   # 4 passed
cargo test --offline -p chat-identity -p chat-storage    # 56 + 94 passed（既有 52/94 继续通过）
~~~

### 证据

- `recovery::export_then_import_restores_the_same_identity`：固定种子建身份 → 导出（文件里**找不到**
  明文账户种子与主密钥字节）→ 空库导入 → 账户/设备 ID、设备公钥、显示名、创建时间全一致 →
  库里既有查询（`load_identity_account` / `load_identity_seeds`）能读到，且种子重建出的设备公钥相同。
- `recovery::four_rejections_leave_the_target_untouched`：错误口令 → `RECOVERY_BAD_PASSPHRASE`；
  翻转正文一位 → `RECOVERY_CORRUPTED`；版本改 9 → `RECOVERY_UNSUPPORTED`；
  目标库已有身份 → `RECOVERY_TARGET_NOT_EMPTY`（原身份不变）。前两种都断言目标库仍是空库（零写入）。
- `recovery::oversized_inputs_and_unknown_kdf_parameters_are_rejected`：文件 1 MiB+1 →
  `INVALID_INPUT`（解密前拒绝）；口令 5/257 字节 → `INVALID_INPUT`；白名单外参数导出 →
  `RECOVERY_UNSUPPORTED`；三组白名单参数都能导出并成功导入；把头部参数改成白名单外 → 导入拒绝。
- `recovery::export_without_an_identity_is_rejected`：库里没有身份 → `INVALID_INPUT`（不产出文件）。

### 已知限制

1. **不含主密钥**：恢复的是身份（账户/设备种子），旧库的消息/附件/联系人仍不可读——这是设计选择，
   "换机器还能拿回身份"不等于"还能读旧历史"。
2. **只有本机一台设备**：`devices_digest` 是"账户 + 设备"的摘要；多设备列表与撤销集合的恢复待
   IDENTITY-011/012 的跨设备同步接上后再扩展。
3. **KCV 由定长摘要组成**：它让离线口令验证不经过 AEAD，但成本仍由 argon2id 决定（与猜 AEAD 标签等价）；
   不提供口令强度策略，只做长度/控制字符校验。
4. **导入是"新建身份"**：不迁移旧数据、不合并、不删除目标库里其它数据（目标库必须没有身份）。
5. **参数白名单是固定的三组**：升级 argon2 参数需要发版（文件头带版本，旧文件仍可导入）。
### FFI 接线（IdentityFfi，2026-10-18，Windows x64）

~~~text
IdentityFfi::open_recovery_target(database_path, master_key_hex, node) -> Arc<IdentityFfi>
IdentityFfi.export_recovery_bundle(passphrase) -> Vec<u8>
IdentityFfi.import_recovery_bundle(passphrase, bytes) -> RecoveryImportViewFfi
RecoveryImportViewFfi { account_id, device_id, account_public_key, device_public_key,
                        display_name, created_at_ms, devices_digest }
~~~

- **导出**：需要 `open_persisted` 身份且库里已有身份，否则 `INVALID_INPUT`（非持久化对象）；
  导出期间短暂释放正式库的独占写锁，导出后自动重开并恢复身份句柄（`account()` 仍可用）；
  关闭后 → `CORE_CLOSED`。
- **导入**：只允许在**空库**上执行，且对象必须已 `shutdown()`（先释放写锁），否则
  `INVALID_STATE_TRANSITION`；目标库已有身份 → `RECOVERY_TARGET_NOT_EMPTY`（绝不覆盖）。
  导入成功后对象仍是关闭态，调用方用 `open_persisted` 打开同一库即拿到恢复的身份。
- **空库从哪来**：`open_recovery_target` 打开（必要时创建）库、确认没有身份、随即关闭并保持关闭态；
  目标库已有身份时它直接返回 `RECOVERY_TARGET_NOT_EMPTY`。
- **错误码原样透出**：`RECOVERY_BAD_PASSPHRASE` / `RECOVERY_CORRUPTED` / `RECOVERY_UNSUPPORTED` /
  `RECOVERY_TARGET_NOT_EMPTY` / `INVALID_INPUT` / `CORE_CLOSED` / `INVALID_STATE_TRANSITION` /
  storage 冻结码。
- 测试：[crates/ffi/tests/identity_recovery_ffi.rs](../../crates/ffi/tests/identity_recovery_ffi.rs)（3 个，真临时库）。
  复现：`cargo test --offline -p chat-ffi` → 60 passed（既有 57 继续通过）。

FFI 侧已知限制：① `open_recovery_target` 的 `account()` 在导入前只是占位值（不可信，已注释）；
② 导入后必须重开对象才能用身份（不会自动重建句柄）；③ 导出要求先持久化打开——纯 `create()` 内存身份
无法导出（它没有库，也就没有可恢复的落盘身份）。
## 设备登记与撤销集合（IDENTITY-011/012 落库，2026-10-18）

~~~text
chat_identity::devices::register_device(store, device_id, account_id, public_key, label) -> DeviceEntry
chat_identity::devices::registered_devices(store, account_id, include_revoked) -> Vec<DeviceEntry>
chat_identity::devices::revoke_registered_device(store, account_id, device_id, local_device_id, reason, now_ms) -> bool
chat_identity::devices::device_is_revoked(store, device_id) -> bool
chat_identity::devices::load_device(store, device_id) -> Option<DeviceEntry>
DeviceEntry { device_id, account_id, device_public_key(hex), label, added_at_ms,
              revoked_at_ms, revoked_reason }        // is_revoked() 便捷方法
DeviceError::code() -> DEVICE_SELF_REVOKE / INVALID_INPUT / 身份层冻结码 / storage 冻结码
~~~

- **幂等登记**：同设备重复登记只刷新公钥/label 与更新时间，**首次登记时间保留**；
- **撤销单向**：已撤销设备**不会**因重新登记而复活（upsert 不碰 revoked 字段）；重复撤销返回 `false`
  且保留首次撤销时间与原因；
- **不能撤销本机** → `DEVICE_SELF_REVOKE`（换机只能走恢复流程）；跨账户撤销 → `INVALID_INPUT`；
- **默认列表隐藏已撤销**，`include_revoked = true` 时连历史行一起返回（撤销不删行）。

存储层见 [storage.md 的“设备登记与撤销集合”](storage.md)（迁移 0013 `identity_devices`）。

恢复文件导入（IDENTITY-014）现在会在 `import_recovery_bundle` 成功后**登记本机设备**
（label = 恢复的显示名），历史设备行保留。

### 证据

[crates/identity/tests/devices.rs](../../crates/identity/tests/devices.rs)（5 个，真临时加密库）：
登记/列出/幂等（重复登记不新增行、公钥被刷新、首次时间保留）；撤销后 `is_revoked=true`、
默认列表只剩未撤销的那台、`include_revoked` 能看到撤销时间与原因、重复撤销返回 false 且时间不变、
重新登记不复活；撤销本机 `DEVICE_SELF_REVOKE`；重启后撤销状态与 label/原因都在；
多账户隔离且跨账户撤销被拒（对方设备不受影响）。

复现：`cargo test --offline -p chat-identity -p chat-storage` → **157 passed**
（既有 56 / 95 继续通过）。

### 已知限制

1. **撤销只影响本地登记集合**：不会自动广播到其它设备/节点（跨设备撤销同步未做）；
2. **设备公钥不校验与 device_id 的自洽**：由上层（凭据/凭据目录）保证；
3. **每账户上限 512 台**（`MAX_DEVICE_REGISTRY_ROWS`），越界显式 `INVALID_INPUT`。
### 设备集合接 FFI（IdentityFfi，2026-10-18）

~~~text
IdentityFfi.list_devices(include_revoked: bool) -> Vec<DeviceViewFfi>
IdentityFfi.revoke_device(device_id: String, reason: String) -> bool
IdentityFfi.device_is_revoked(device_id: String) -> bool

DeviceViewFfi { device_id, account_id, label, device_public_key_hex, added_at_ms,
                revoked_at_ms, revoked_reason, is_local }
~~~

- **需要持久化身份**（`open_persisted`）：纯内存对象 → `INVALID_STATE_TRANSITION`；关闭后 → `CORE_CLOSED`；
- `list_devices` 首次调用会**懒登记本机设备**（幂等），所以新建身份后列表一定有本机一台且
  `is_local = true`（`is_local` 依据当前设备 ID 判定）；
- `revoke_device` 撤销本机 → `DEVICE_SELF_REVOKE`；未登记设备 → `INVALID_INPUT`；跨账户 → `INVALID_INPUT`；
  撤销后默认列表不含该设备，`include_revoked = true` 时可见撤销时间与原因；
- 实现细节：设备集合在 storage 层（`identity_devices`），而身份对象以独占写者持有同一个库，
  因此这三个方法采用"**短暂释放写锁 → 操作 → 无条件恢复身份句柄**"的时序（与恢复文件导出同一套）。

测试：[crates/ffi/tests/identity_devices_ffi.rs](../../crates/ffi/tests/identity_devices_ffi.rs)（5 个，真库）：
建身份 → list 含本机且 `is_local=true`、公钥 64 hex；撤销本机 `DEVICE_SELF_REVOKE` 且状态不变；
登记第二台 → 两条 → 撤销 → 默认一条、`include_revoked` 可见原因、重复撤销返回 false；
撤销未登记 → `INVALID_INPUT`；关闭后三个方法都 `CORE_CLOSED`；纯内存对象 → `INVALID_STATE_TRANSITION`。

复现：`cargo test --offline -p chat-ffi --test identity_devices_ffi` → 5 passed；
`cargo test --offline -p chat-ffi` → **76 passed**（既有 71 继续通过）。
### 设备登记入口（IdentityFfi.register_device，2026-10-18）

~~~text
IdentityFfi.register_device(device_id, device_public_key_hex, label) -> DeviceViewFfi
~~~

- 需要持久化身份（纯内存 → `INVALID_STATE_TRANSITION`；关闭后 → `CORE_CLOSED`）；
- `device_id` 必须是 **PROTOCOL-001 设备文本**（`dev:1:<64 hex>`），否则 `INVALID_INPUT`；
- `device_public_key_hex` 必须**正好 64 个 hex 字符**（32 字节），否则 `INVALID_INPUT`；
- **幂等**：重复登记只刷新公钥/label，`added_at_ms` 保留首次值，不新增行；
- **归属保护**：同一 `device_id` 已属于其它账户 → `INVALID_INPUT`（不偷偷改归属）；
- **撤销不可逆**：已撤销设备重新登记**不复活**，返回视图仍带 `revoked_at_ms/revoked_reason`；
- 返回 `is_local` 依据当前设备 ID（登记本机即 true）。

测试：[crates/ffi/tests/identity_device_register.rs](../../crates/ffi/tests/identity_device_register.rs)（6 个，真库）：
登记第二台 → 两条 → 撤销 → 默认一条 → 再登记仍撤销（原因保留、label 刷新）；重复登记 `added_at_ms` 不变且不新增行；
显式登记本机 → `is_local = true`；公钥 63/65 hex 与非 hex、`device_id` 为空/非设备文本/账户文本 → `INVALID_INPUT`
且集合无半写入；关闭后 `CORE_CLOSED`；纯内存对象 `INVALID_STATE_TRANSITION`。

复现：`cargo test --offline -p chat-ffi --test identity_device_register` → 6 passed；
`cargo test --offline -p chat-ffi` → **82 passed**（既有 76 继续通过）。
### 恢复导入后的群播种（IdentityFfi，2026-10-18）

~~~text
IdentityFfi.seed_revoked_devices_for_all_groups() -> u32   // 返回新增条数合计
~~~

- 对库里**每个群**调用一次 `seed_revoked_devices_from_registry`（账户视角撤销集合 → 群快照并集）；
- 幂等：并集没有新增时返回 0，且不写盘；
- 单个群失败（快照损坏等）**不阻断**其它群，只是不计入合计；
- 身份恢复导入（`import_recovery_bundle`）成功后**自动**对库里已有的群各播种一次（失败只记录、不阻断导入）；
- 需持久化身份：纯内存 → `INVALID_STATE_TRANSITION`；关闭 → `CORE_CLOSED`。

证据：[crates/ffi/tests/identity_seed_groups.rs](../../crates/ffi/tests/identity_seed_groups.rs)（3 个，真库 + 真 OpenMLS）：
建群时账户已有撤销设备 → 视图 `seeded_revoked_devices=1` 且列表计数 1；建群后写入撤销 →
`seed_revoked_devices_for_all_groups` 两个群各 +1（合计 2）、重复调用 0、落盘计数都为 1；
恢复导入到"已有群 + 已有撤销"的库 → 导入后列表计数立刻为 1，再用恢复身份播种返回 0。








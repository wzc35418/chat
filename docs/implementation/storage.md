# SQLite、统一事务、数据表与崩溃恢复

阶段：P0 → P2。状态：函数级设计；P0 正式存储路径的 STORAGE-001/003/006/007/009/010/011（含 002 的迁移器）与 STORAGE-012/013/014/015（分页查询与 outbox 租约）已在 Windows x64 实测通过，STORAGE-004/005/008 未开始。正式源码：[crates/storage/src/formal/mod.rs](../../crates/storage/src/formal/mod.rs)；隔离实验：[crates/storage/src/store.rs](../../crates/storage/src/store.rs)。

状态补充（2026-10-18）：`store.rs` 已有独立的未加密 P0 事务试验表，覆盖 T1/C01 意图与准备任务和 T2 MLS/attempt/outbox 崩溃断点，局部证据见 [P0-03 报告](../P0_PERSISTENCE.md)。`crates/storage/src/formal/` 新增正式加密存储路径，实现 STORAGE-001/003/006/007/009/010/011 与 002 的迁移器，证据见下文“P0-04 正式存储路径实现记录”；其余 STORAGE-004/005/008 仍未实现，MLS 加密语义（CRYPTO-004 等）也未接入。

状态补充（2026-10-18）：STORAGE-012/013/014/015 已实现，新增 `crates/storage/src/formal/cursor.rs`（内部认证分页游标）、`query.rs`（消息页与会话页）、`outbox_lease.rs`（租约领取与三态结果），证据见下文“分页查询与 outbox 租约实现记录”。本轮未新增迁移、未改动 0001/0002 既有内容，也未改动 `store.rs`/`persistence.rs`/`formal_crash_worker.rs`。

Rust storage 唯一拥有客户端核心数据库；节点库单独实现于 node-services，不能复用客户端敏感数据写入口。默认先验证全库加密或等价方案，因为会话 ID、索引、时间戳同样敏感。事务句柄不跨 FFI、不跨网络 await；读分页使用稳定快照。

返回类型与字段约束见 [类型字典](TYPES.md)，事务编号见 [事务和并发约定](TRANSACTIONS.md)，纵向流程见 [关键调用链](FLOWS.md)。所有 Result<T> 表示显式可恢复错误，不允许安全验证失败后继续推进。

## P0-04 正式存储路径实现记录（2026-10-18，Windows x64）

正式路径位于 [crates/storage/src/formal/](../../crates/storage/src/formal/)（mod.rs 公开 API、seal.rs 主密钥与 AEAD、migrations.rs 版本化迁移、error.rs 稳定错误码），附加 schema 见 [migrations/0002_formal_store.sql](../../crates/storage/migrations/0002_formal_store.sql)。与 `store.rs` 的 P0-03 未加密实验完全分开：不共享连接、表或错误类型；既有实验代码与测试未改动，仍全部通过。

- 打开与锁定：`open_store` 以 `PRAGMA locking_mode=EXCLUSIVE` + `BEGIN IMMEDIATE` 探测取得单写者独占锁（探测等锁 0 超时，第二个打开返回 `STORAGE_LOCKED`），随后设置 `journal_mode=WAL`、`synchronous=FULL`、`foreign_keys=ON`；`busy_timeout` 只影响取得独占锁之后的语句。
- 密钥与列加密：主密钥固定 32 字节（`ProtectedStorageKey`，Drop 时 zeroize、`Debug` 不输出字节、不实现 Clone），草稿正文、MLS 记录值、密钥校验记录、隔离诊断四个用途各自派生独立子密钥；ChaCha20-Poly1305 封装格式为 `version||nonce(12)||ciphertext||tag(16)`，AAD 由用途与行身份字段的长度前缀编码构成，搬移密文或改记录版本都会认证失败；错误密钥与篡改统一返回 `STATE_CORRUPTED`，不区分原因。
- 迁移：脚本用 `include_str!` 编入二进制，逐版本单事务执行，记录规范化 SQL 的 SHA-256；校验和不符返回 `MIGRATION_FAILED`，库内版本更高返回 `SCHEMA_TOO_NEW`，重复启动不重复执行。
- 事务：`begin_transaction` 只发 `BEGIN IMMEDIATE`，句柄绑定作用域与库版本且只能消费一次；写入失败把句柄标记 poisoned，`commit_transaction` 拒绝提交并回滚；Drop 未提交等价 rollback；提交前做版本比较与 `PRAGMA foreign_key_check`，`COMMIT` 成功才返回 `CommitToken`。
- 意图与尝试：T1 在单事务写草稿（正文 AEAD 加密）、准备任务与恢复标记；T2 在单事务写不可变 attempt、加密后的 MLS 记录、发送任务并完成准备任务。同一 operation ID 只产生一条草稿（确定性 draft id + 唯一索引 + 请求摘要），同一 SubmissionId 只接受同一密文摘要。
- 恢复：恢复标记与写事务一起提交；`recover_incomplete_writes` 只核对既有字节——清除已核对的标记、重置过期 outbox 租约、从已提交 attempt 重建缺失的发送任务、把摘要不符的记录写入 `store_quarantine` 并把 attempt 置为 `unknown`；恢复路径不调用任何加密或 MLS 代码，因此不会重新加密同一尝试。

复现命令（全程离线）：

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-storage   # 3 lib + 2 formal_crash + 17 formal_store = 22 passed
cargo test --offline -p chat-crypto    # 3 engine + 5 formal_master_key + 7 p0_persistence = 15 passed
```

已知限制：只在 Windows x64 实测；当前是应用层列级加密，SQLite 页头、索引键、时间戳等元数据仍是明文，未证明“全库加密或等价方案”；未模拟断电/电源缓存丢失，不能从 `synchronous=FULL` 推出断电耐久保证；MLS 加密本身（CRYPTO-004/006/007/008）未接入，T2 只持久化调用方提供的 PreparedWrite；SQLite 回滚失败路径无法注入，fail-closed 闸门经 `mark_faulted` 公开入口验证。

## 分页查询与 outbox 租约实现记录（2026-10-18，Windows x64）

实现位置（全部在正式路径内，未新增迁移、未改动既有实验文件）：

- [crates/storage/src/formal/cursor.rs](../../crates/storage/src/formal/cursor.rs)：内部认证分页游标（编码/解码/过期/跨范围校验）。
- [crates/storage/src/formal/query.rs](../../crates/storage/src/formal/query.rs)：STORAGE-012 `query_message_page` 与 STORAGE-013 `query_conversation_page`，以及 `messages.protected_content` 的封装契约 `Store::seal_message_content`。
- [crates/storage/src/formal/outbox_lease.rs](../../crates/storage/src/formal/outbox_lease.rs)：STORAGE-014 `claim_outbox_batch` 与 STORAGE-015 `mark_outbox_result`。
- 稳定错误码新增 `INVALID_CURSOR`、`PERMISSION_DENIED`、`STORAGE_BUSY`、`JOB_LEASE_CONFLICT`、`INVALID_STATE_TRANSITION`（[error.rs](../../crates/storage/src/formal/error.rs)）；封装用途新增 `message-content` 与 `page-cursor`（[seal.rs](../../crates/storage/src/formal/seal.rs)）；`WriteScope` 新增 `outbox()` 作用域（[mod.rs](../../crates/storage/src/formal/mod.rs)）。

分页语义（STORAGE-012/013）：

- 排序固定：正式消息 `(group_seq DESC, message_id DESC)`；本机待发意图 `(created_at DESC, draft_id DESC)`；会话列表 `(last_activity_at DESC, conversation_id DESC)`。只用键集（keyset），不用 OFFSET；查询固定取 `limit+1` 判断是否还有下一页，`next_cursor = None` 表示没有更多。
- 方向：`before/cursor = None` = 第一页（最新）；`Some` = “严格更旧的一页”。没有“更新方向”的游标，客户端必须从最新页开始。
- 合并顺序：本机待发意图在前（本机最新动作），随后是正式消息。待发段与正式段没有可比较的排序键，因此待发游标额外携带“正式序号水位”：第一页取当前 `MAX(group_seq)`，已处于待发段的游标只沿用（取小）原水位，翻页期间新到的正式消息不会插进更旧的一页，也不会让后续页漏项。
- 去重：待发投影只取 `drafts.state = 'queued'` 且 `messages` 里没有相同 `draft_id` 的草稿，待发投影 ID 用 `draft_id`（本地稳定 ID），`submission_id` 取该草稿最近的 attempt；正式消息 ID 用 `messages.message_id`。单页内不会出现重复 ID。
- 只读快照：两个查询都在一个 `unchecked_transaction`（BEGIN DEFERRED）里完成，两条 SQL（待发段与正式段）看到同一快照。
- 上限与拒绝：`limit` 契约规定 1..100，取 `MAX_MESSAGE_PAGE_LIMIT = 100` / `MAX_CONVERSATION_PAGE_LIMIT = 100`（任务板的 200 建议被契约覆盖）；越界、时间戳为负、ID 畸形返回 `INVALID_INPUT`；会话不存在或已 `left`/`dissolved` 返回 `PERMISSION_DENIED`；游标超长/格式/认证/过期/跨会话/跨资源返回 `INVALID_CURSOR`；密文被篡改返回 `STATE_CORRUPTED`；fail-closed 后返回 `STORAGE_FAILURE`。查询失败绝不退化为“跳到最新页”。
- 会话摘要：`last_activity_at` = 该会话 `messages.display_created_at` 与 `drafts.created_at` 的较大值（0 表示无活动）；`pending_intents`、`failed_outbox_jobs`、`unknown_attempts` 为真实计数；`unread_count` 恒为 `None`（正式未读需要 read-cursor 存储，尚未实现，不伪造 0）。

outbox 语义（STORAGE-014/015）：

- 领取：单笔 `BEGIN IMMEDIATE` 短事务；只领取 `state='pending' AND retry_at <= now` 或 `state='leased' AND lease_until <= now`，跳过 `cancel_requested = 1` 与依赖未 `complete` 的任务；每条任务按 `lease_version` CAS（`WHERE lease_version = 观测值`），受影响行数不为 1 即 `JOB_LEASE_CONFLICT`，领取成功 `lease_version + 1`、`lease_until = now + 30s`。条数上限 `MAX_CLAIM_BATCH = 32`，载荷字节上限 `MAX_CLAIM_BATCH_BYTES = 4 MiB`（按 attempt 密文/草稿密文的实际长度累计）。写锁被短暂占用时返回 `STORAGE_BUSY`。
- 结果三态：success（`TransportAccepted`/`MailboxAck`/`FormallyAccepted`/`Delivered`）、failed（`retryable` 决定重排或终态）、unknown。success 把任务置 `complete` 并清租约；可重试失败置 `pending`、`retry_count + 1`、`retry_at = now + 5s × 2^(n-1)`（上限 5 分钟）；终态失败置 `failed` 并把 attempt 置 `rejected`。
- `unknown` 不推进为完成：任务保持 `leased`、保留 `lease_owner` 与 `lease_version`，`lease_until` 延长到 6 小时对账窗口，attempt 置 `unknown`；窗口内任何 worker 都不能重领，任务行与 payload 引用都保留。窗口结束后仍可被重领（版本递增），对账得到正式送达后再置 `complete`。
- 幂等与冲突：同一租约（版本 + 持有者一致）的重复结果按“结果类别”幂等返回——不改行、不提交事务、库版本不前进；类别冲突返回 `INVALID_STATE_TRANSITION`；旧版本/别的持有者返回 `JOB_LEASE_CONFLICT`；已 `cancelled` 的任务不能记录结果。attempt 状态只允许前进（`accepted`/`rejected` 不允许回退），因此 `MailboxAck` 只把 attempt 置 `submitted`，不会被升级为接受或送达。

复现命令（全程离线，本轮实测）：

```powershell
. .\tools\activate.ps1
cargo test --offline -p chat-storage   # 5 lib + 2 formal_crash + 8 formal_outbox + 7 formal_query + 17 formal_store = 39 passed
```

证据（测试名 → 覆盖的契约点）：

- `formal_query::message_page_merges_pending_and_accepted_without_duplicates_or_skips`：翻页期间插入新消息与新待发意图，旧游标不重复、不跳页，7 条历史各出现一次，顺序为“待发新 → 待发旧 → 正式 seq 降序”。
- `formal_query::message_page_enforces_limits_and_rejects_bad_cursors`：limit 0/101/负时间戳 `INVALID_INPUT`；不存在/已离开会话 `PERMISSION_DENIED`；篡改/跨会话/过期/跨资源游标 `INVALID_CURSOR`。
- `formal_query::message_page_reports_end_of_stream_and_bounds_the_last_page`：空会话无游标；恰好 limit 条时 `next_cursor = None`；100 条满页。
- `formal_query::message_page_fails_closed_on_tampered_content`：消息与草稿密文被篡改都返回 `STATE_CORRUPTED`。
- `formal_query::conversation_page_orders_by_activity_with_stable_tiebreak`：活动时间降序 + 同时间用 `conversation_id` 稳定次键；摘要计数与 `unread_count = None`；分页键集不重复；limit 与游标跨资源拒绝。
- `formal_query::queries_fail_closed_after_the_store_is_marked_faulted`：fail-closed 后两个查询都 `STORAGE_FAILURE`，不返回旧摘要。
- `formal_query::prepared_attempts_are_exposed_as_pending_projection_with_submission_id`：待发投影带 `draft_id` 与 attempt 的 `submission_id`。
- `formal_outbox::claim_leases_due_jobs_once_and_reclaims_after_expiry`：到期才领取、租约内第二个 worker 拿不到、到期重领且版本递增、迟到旧 worker `JOB_LEASE_CONFLICT`。
- `formal_outbox::claim_skips_dependencies_and_cancelled_jobs`：依赖未完成与 `cancel_requested` 的任务不领取，依赖完成后可领取。
- `formal_outbox::claim_bounds_the_batch_by_payload_bytes`：3 MiB + 3 MiB 载荷超过 4 MiB 预算，一批只领取一条。
- `formal_outbox::unknown_result_stays_reconcilable_and_never_completes`：`unknown` 后任务仍 `leased`、无 `complete` 投递任务、窗口内不可重领、窗口后重领 → 正式送达后 `complete` 且 attempt `accepted`。
- `formal_outbox::mailbox_ack_does_not_upgrade_delivery`：`MailboxAck` 只把 attempt 置 `submitted`，不写消息行。
- `formal_outbox::mark_is_idempotent_and_rejects_conflicting_or_stale_results`：重复结果幂等且库版本不前进；类别冲突 `INVALID_STATE_TRANSITION`；旧版本/伪造 worker `JOB_LEASE_CONFLICT`；已取消任务显式拒绝。
- `formal_outbox::retryable_failure_reschedules_with_backoff_and_terminal_failure_stops`：退避期不重领、到期重领版本递增、终态失败 attempt `rejected` 且不再领取。
- `formal_outbox::claim_and_mark_reject_malformed_input`：limit/worker/时间戳/ID 畸形 `INVALID_INPUT`，不存在的任务 `JOB_LEASE_CONFLICT`。

已知限制（本记录只对已完成部分负责）：

1. **游标是内部临时实现**：线协议 PROTOCOL-012/013 的 `cur1` 令牌要求账户/设备/会话的 32 字节二进制身份，而正式存储当前只有 TEXT 会话标识、也不保存本机账户/设备身份。本模块用 `spg1` 前缀的内部 AEAD 令牌（主密钥派生用途 `page-cursor`，AAD 绑定会话 + 资源 + 排序版本，TTL 10 分钟、允许 60 秒时钟超前）替代；令牌不能跨到桥接层或网络，待身份映射就绪后应替换为线协议令牌。
2. **待发投影 ID**：待发意图暂时以 `draft_id` 作为稳定本地 ID；正式 MessageId 的关联由尚未实现的 STORAGE-008（`apply_verified_entry`）写 `messages.draft_id`/`messages.submission_id` 完成。
3. **未读计数**：返回 `None`。正式未读需要 read-cursor 存储（schema 里没有读位置表），本轮不新增迁移。
4. **消息正文封装契约**：`messages.protected_content` 用用途 `message-content`、AAD `(conversation_id, message_id)` 封装，唯一入口是 `Store::seal_message_content`；STORAGE-008 写入时必须使用同一契约，否则查询会显式 `STATE_CORRUPTED`。
5. **诊断不落盘**：P0 schema 没有 outbox 诊断列，且本轮不新增迁移，结果诊断只截断（≤200 字符、去控制字符）后随 `JobState` 返回给上层日志。
6. **unknown 的对账**：本函数只保留可对账状态（6 小时窗口 + attempt `unknown`）。真正的对账（按 SubmissionId 查询节点接受状态）属于同步/节点模块，未在本函数实现；窗口结束后若无人对账，任务仍可被重领，存在重复提交风险，需要在同步模块接入前保持 worker 侧幂等。
7. **并发唯一性**：`open_store` 是单写者独占打开，进程内并发由 CAS 版本保证；跨进程/双 worker 的真实竞争以两把独立连接的 SQLite 锁与 CAS 语义表达，本轮用“顺序领取 + 版本 CAS”验证（无法在同一进程内同时打开第二个 Store）。
8. **会话列表解密**：`conversations.protected_metadata`（标题等）没有在本函数解密——它属于尚未定义的会话元数据封装契约，本轮不猜测用途，只返回 `kind`/`state` 与计数摘要。

## 函数导航

- [STORAGE-001 open_store](storage.md#storage-001)
- [STORAGE-002 migrate_schema](storage.md#storage-002)
- [STORAGE-003 begin_transaction](storage.md#storage-003)
- [STORAGE-004 crypto_provider_read](storage.md#storage-004)
- [STORAGE-005 crypto_provider_write](storage.md#storage-005)
- [STORAGE-006 persist_send_intent](storage.md#storage-006)
- [STORAGE-007 persist_prepared_attempt](storage.md#storage-007)
- [STORAGE-008 apply_verified_entry](storage.md#storage-008)
- [STORAGE-009 commit_transaction](storage.md#storage-009)
- [STORAGE-010 rollback_transaction](storage.md#storage-010)
- [STORAGE-011 recover_incomplete_writes](storage.md#storage-011)
- [STORAGE-012 query_message_page](storage.md#storage-012)
- [STORAGE-013 query_conversation_page](storage.md#storage-013)
- [STORAGE-014 claim_outbox_batch](storage.md#storage-014)
- [STORAGE-015 mark_outbox_result](storage.md#storage-015)

<a id="storage-001"></a>

## STORAGE-001 · open_store

~~~text
open_store(options: StoreOptions, key: ProtectedStorageKey) -> Result<Store>
~~~

**职责与输出**：打开加密客户端库并验证版本/完整性。

**输入与前置条件**：数据目录权限符合平台惯例，系统密钥已解包，存储加密方案已决策。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 独占迁移锁。
2. 配置加密和必要 PRAGMA。
3. 验证密钥与 schema。
4. 执行迁移。
5. 检查未完成事务恢复标记。
6. 返回连接池。

**持久化、并发与副作用**：启动阶段串行；不能错误密钥时创建同名新空库。

**失败处理**：STORAGE_LOCKED，STATE_CORRUPTED，MIGRATION_FAILED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[STORAGE-002 migrate_schema](storage.md#storage-002)、[STORAGE-011 recover_incomplete_writes](storage.md#storage-011)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 错误密钥不覆盖数据：`formal_store::wrong_key_or_wrong_provenance_is_rejected_without_overwriting_data`（错误密钥与不一致的 KDF 来源都返回 STATE_CORRUPTED，数据仍能用原密钥完整读出）。
- [x] 重复启动单写者：`formal_store::second_open_is_rejected_while_the_first_holds_the_writer_lock`（第二个打开返回 STORAGE_LOCKED）、`formal_store::open_store_configures_durable_single_writer_store`（重复启动幂等，迁移与密钥记录不重复写入）。
- [x] 数据库/WAL/临时文件敏感数据抽查：`formal_store::plaintext_body_and_master_key_never_appear_in_store_files`（提交后直接扫描 .db/-wal/-shm 原始字节，明文正文、MLS 秘密与 32 字节主密钥都不出现，重开仍能解密）。

**实现状态**：已实现（Windows x64 实测）。`open_store` = 独占锁探测 + PRAGMA(WAL/FULL/外键) + 密钥校验记录 + 迁移 + 恢复标记检查，见 [crates/storage/src/formal/mod.rs](../../crates/storage/src/formal/mod.rs) 与 [seal.rs](../../crates/storage/src/formal/seal.rs)。证据：`cargo test --offline -p chat-storage`（3 lib + 17 formal_store + 2 formal_crash 全绿）。限制：未模拟断电；SQLite 临时文件未单独构造扫描；全库加密方案仍待决策（当前是应用层列级加密）。

<a id="storage-002"></a>

## STORAGE-002 · migrate_schema

~~~text
migrate_schema(store: Store, target: SchemaVersion) -> Result<MigrationReport>
~~~

**职责与输出**：按顺序完整迁移且记录校验和。

**输入与前置条件**：版本递增，不支持未知降级；迁移前备份策略明确。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读取 schema_version。
2. 验证迁移链和 checksum。
3. 创建可恢复备份。
4. 每次完整迁移。
5. 更新版本。
6. 重新校验关键约束。

**持久化、并发与副作用**：每步事务；对无法事务化动作先记录恢复日志；失败保留旧可恢复库。

**失败处理**：SCHEMA_TOO_NEW，MIGRATION_FAILED，DISK_FULL。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 中途断电恢复。（未验证：无断电/掉盘模拟）
- [x] 重复启动不重复执行：`formal_store::open_store_configures_durable_single_writer_store`（schema_migrations 与 store_key_check 行数不随重开增长）。
- [ ] 回滚失败禁止继续写。（未验证：迁移回滚失败无法稳定注入；失败时库停留在上一完整版本由逐版本单事务与 MIGRATION_FAILED 保证，但未专项测试）

**实现状态**：部分实现。`migrations::migrate` 逐版本单事务执行、记录规范化 SQL 的 SHA-256，并由 `open_store` 调用；校验和不符与更高 schema 版本显式失败（`formal_store::migration_checksum_mismatch_fails_closed`、`formal_store::too_new_schema_version_is_rejected`）。文档契约里独立的 `migrate_schema(store, target)` 入口、迁移前备份与恢复日志尚未实现，因此不标整体完成。

<a id="storage-003"></a>

## STORAGE-003 · begin_transaction

~~~text
begin_transaction(scope: WriteScope) -> Result<CoreTransaction>
~~~

**职责与输出**：取得受限写事务并绑定群/版本上下文。

**输入与前置条件**：单群执行器已持锁，避免其他写者绕过。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查运行状态。
2. 取得写连接。
3. begin。
4. 读取当前版本。
5. 创建只能一次消费的事务句柄。

**持久化、并发与副作用**：T1/T2/T3/T4 统一起点；禁止持有网络调用。

**失败处理**：STORAGE_BUSY，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 超时返回可解释错误。（跨进程等锁在 open_store 阶段以 STORAGE_LOCKED 表达并有测试；独占锁下同一连接不会出现等锁，`begin_transaction` 的 busy 超时路径未单独验证）
- [x] 事务句柄 Drop 必须 rollback：`formal_store::transaction_drop_rolls_back_and_version_is_checked`（未提交即 Drop 后 drafts/outbox/恢复标记均为 0、库版本不变，随后可正常开新事务）。

**实现状态**：已实现（限本模块事务语义）。`begin_transaction` 只发 BEGIN IMMEDIATE，绑定作用域与期望库版本，句柄只能消费一次；提交/回滚见 STORAGE-009/010；写入失败会把句柄标记 poisoned 以防止半提交。

<a id="storage-004"></a>

## STORAGE-004 · crypto_provider_read

~~~text
crypto_provider_read(tx: CoreTransaction, key: CryptoRecordKey) -> Result<Option<SecretBytes>>
~~~

**职责与输出**：OpenMLS 从当前事务读取密钥与状态。

**输入与前置条件**：provider 不能暗中开启独立连接或事务。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 校验群作用域。
2. 查询版本化键。
3. 解密记录。
4. 返回敏感缓冲。
5. 标记读取版本。

**持久化、并发与副作用**：使用调用方同一事务；不缓存未提交跨群状态。

**失败处理**：STATE_CORRUPTED，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 同事务读到刚写内容。
- [ ] 回滚后 provider 缓存不可泄漏新状态。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="storage-005"></a>

## STORAGE-005 · crypto_provider_write

~~~text
crypto_provider_write(tx: CoreTransaction, changes: CryptoStorageChanges) -> Result<()>
~~~

**职责与输出**：在同一事务写入 OpenMLS 记录变化。

**输入与前置条件**：changes 归属群与事务相同；删除与新增均完整收集。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证记录类型/版本。
2. 加密敏感值。
3. 批量 upsert/delete。
4. 记录受影响版本。
5. 不发布内存状态。

**持久化、并发与副作用**：T2/T3 的组成步骤，不能单独 commit。

**失败处理**：CRYPTO_SCOPE_MISMATCH，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[STORAGE-004 crypto_provider_read](storage.md#storage-004)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 故意插入错误群记录整体拒绝。
- [ ] 部分写入失败全部回滚。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="storage-006"></a>

## STORAGE-006 · persist_send_intent

~~~text
persist_send_intent(tx: CoreTransaction, request: ValidatedSendRequest) -> Result<DraftRecord>
~~~

**职责与输出**：以用户操作 ID 建立唯一草稿和初始排队状态。

**输入与前置条件**：clientOperationId 在本账户作用域唯一；内容敏感。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 查 operationId 唯一索引。
2. 同请求返回旧草稿。
3. 不同摘要同 ID 拒绝。
4. 保存加密内容/回复/来源。
5. 插入待准备任务。

**持久化、并发与副作用**：T1：草稿与准备任务一事务；UI 得到成功前必须 commit。

**失败处理**：OPERATION_ID_CONFLICT，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 连点发送只一条草稿：`formal_store::send_intent_is_idempotent_and_conflicting_requests_fail`（同请求重放返回同一 draft_id，drafts/outbox_jobs/恢复标记各 1 行）。
- [x] 同 ID 换正文不能静默复用：同一用例（OPERATION_ID_CONFLICT，且事务被标记 poisoned、commit 被拒绝且无新行）。

**实现状态**：已实现。T1 单事务写 drafts（正文 AEAD 加密）+ outbox prepare 任务 + draft_prepared 恢复标记；draft id 由会话与 operation ID 确定性派生，请求摘要不包含 created_at（重试时钟可变）。限制：正文只实现 Text（附件/扩展未实现）；operation ID 在单账户库内全局唯一（沿用 schema 草稿的唯一索引）。

<a id="storage-007"></a>

## STORAGE-007 · persist_prepared_attempt

~~~text
persist_prepared_attempt(tx: CoreTransaction, prepared: PreparedWrite) -> Result<AttemptRecord>
~~~

**职责与输出**：保存不可变密文、摘要、SubmissionId 与草稿关联。

**输入与前置条件**：crypto 状态变化来自同 tx；attempt 不可原地换密文。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对 draft 和 epoch。
2. 写 message_attempts。
3. 验证 submission 唯一性。
4. 写 crypto 变化。
5. 排入发送 outbox。

**持久化、并发与副作用**：T2：密文/MLS/attempt/outbox 原子提交；失败不能单独发布任何一项。

**失败处理**：SUBMISSION_ID_CONFLICT，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[STORAGE-005 crypto_provider_write](storage.md#storage-005)、[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 提交前后 crash 恢复有完整可重发尝试：`formal_crash::attempt_abort_before_commit_leaves_no_partial_message`（真实子进程 abort 于 AfterAttempt/AfterMlsRecords/AfterOutbox，重开后 attempt 与发送任务均为 0、草稿与准备任务完整；恢复后可用同一草稿重新准备并提交）。
- [x] 相同 ID 不同摘要拒绝：`formal_store::prepared_attempt_is_atomic_idempotent_and_version_checked`（SUBMISSION_ID_CONFLICT；同草稿第二个活跃尝试、MLS 记录版本倒退、草稿摘要不符也整体回滚）。

**实现状态**：已实现。单事务写 message_attempts（不可变密文 + 候选摘要）、密封后的 mls_state_records、send outbox 任务，并把 prepare 任务置 complete；同事务可读到未提交密文。限制：MLS 加密与 provider 状态推进（CRYPTO-004/005）未接入，本函数不能自行产生密文；pending Commit 分支未实现。

<a id="storage-008"></a>

## STORAGE-008 · apply_verified_entry

~~~text
apply_verified_entry(tx: CoreTransaction, entry: FullyVerifiedEntry, changes: PreparedWrite) -> Result<AppliedEntry>
~~~

**职责与输出**：一次写入正式事件、消息视图、MLS、去重、游标和必要回执。

**输入与前置条件**：日志连续、领域与加密校验全部通过；EventId 已认证。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 检查 seq/hash 和 EventId 唯一约束。
2. 写日志。
3. 更新 crypto/策略。
4. 物化消息。
5. 记录去重。
6. 推进游标。
7. 入队端到端回执。

**持久化、并发与副作用**：T3：所有写入同事务；不跨群共享 mutable provider。

**失败处理**：LOG_CONFLICT，STATE_CORRUPTED，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[STORAGE-005 crypto_provider_write](storage.md#storage-005)、[SYNC-001 enqueue_job](sync.md#sync-001)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] 任一步失败都不推进游标。
- [ ] 重复事件不重复可见、不重复回执。

**实现状态**：未开始。实现时补提交引用、自动化/真机用例位置与实际结果；通过本函数用例不自动代表整个模块完成。

<a id="storage-009"></a>

## STORAGE-009 · commit_transaction

~~~text
commit_transaction(tx: CoreTransaction) -> Result<CommitToken>
~~~

**职责与输出**：耐久提交并返回唯一发布凭证。

**输入与前置条件**：所有不变量/外键/期望版本已检查，T2/3 中无网络 I/O。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 最终版本比较。
2. 提交数据库。
3. 确认 durable 策略。
4. 消耗 tx。
5. 返回已提交版本和变化摘要。

**持久化、并发与副作用**：失败 rollback；返回成功才可更新内存或发布 UI 事件。

**失败处理**：DISK_FULL，STORAGE_FAILURE，VERSION_CONFLICT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [ ] fsync 边界 crash。（未验证：无断电/掉盘模拟）
- [ ] 提交成功回调前 crash 可重查。（未验证：没有在 COMMIT 之后、返回之前注入终止点）
- [x] 失败无半消息：`formal_store::send_intent_is_idempotent_and_conflicting_requests_fail`（poisoned 事务 commit 返回 STORAGE_FAILURE 且无新行）与 `formal_crash` 两个用例（事务内 abort 后全部回滚）。

**实现状态**：已实现。`commit_transaction` 先比较库版本、执行 PRAGMA foreign_key_check，再 COMMIT，成功后才返回 CommitToken（含提交版本与计数写入行数）；提交失败把连接标记 fail-closed。

<a id="storage-010"></a>

## STORAGE-010 · rollback_transaction

~~~text
rollback_transaction(tx: CoreTransaction) -> Result<()>
~~~

**职责与输出**：丢弃未提交变更并使 provider 临时状态失效。

**输入与前置条件**：未提交 tx 句柄，Drop 亦调用同清理路径。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 执行 rollback。
2. 作废 PreparedWrite。
3. 清理敏感缓冲与缓存。
4. 释放锁。
5. 如无法确认则关闭连接并标故障。

**持久化、并发与副作用**：不恢复已提交或已消费的历史密钥快照。

**失败处理**：ROLLBACK_FAILED 进入 fail-closed。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 事务 drop/取消相同语义：`formal_store::transaction_drop_rolls_back_and_version_is_checked`。
- [x] 失败连接不能回池继续使用：`formal_store::fail_closed_store_rejects_every_operation`（mark_faulted 后 begin/read/store_version/outbox 全部返回 STORAGE_FAILURE）。限制：SQLite 回滚失败本身无法稳定注入，验证走的是同一条 fail-closed 闸门。

**实现状态**：已实现（含上述限制）。`rollback_transaction` 消费句柄并回滚，回滚失败置 fail-closed 并返回 ROLLBACK_FAILED；未显式结束时由 rusqlite 的 Drop 回滚。PreparedWrite 的秘密值用 Zeroizing 持有，封装后立即释放。

<a id="storage-011"></a>

## STORAGE-011 · recover_incomplete_writes

~~~text
recover_incomplete_writes(store: Store) -> Result<RecoveryReport>
~~~

**职责与输出**：启动时恢复耐久 outbox 与未发布状态，不重造已存在密文。

**输入与前置条件**：只能使用 P0 通过验证的事务/provider 策略。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 读恢复记录。
2. 检查提交版本。
3. 加载完整已提交群。
4. 重置过期任务租约。
5. 原字节恢复尝试。
6. 将无法证明一致项隔离。

**持久化、并发与副作用**：统一事务方案依 DB WAL；备用 journal 方案需另有 ADR 与阶段机，不混用。

**失败处理**：STATE_CORRUPTED，RECOVERY_REQUIRED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[STORAGE-010 rollback_transaction](storage.md#storage-010)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 每个注入崩溃点得到旧完整或新完整状态：`formal_crash` 五个断点（T1 AfterDraft/AfterPrepareJob，T2 AfterAttempt/AfterMlsRecords/AfterOutbox）+ 回滚注入（poisoned commit），另有 `formal_store::recover_incomplete_writes_clears_markers_and_resets_leases`、`recovery_repairs_a_missing_prepare_job_without_creating_messages`、`recovery_quarantines_a_tampered_committed_attempt`。
- [x] 恢复不重新 encrypt 同尝试：同一组用例（恢复只重算摘要核对既有密文、从已提交 attempt 重建 outbox 任务，恢复路径不调用 crypto/MLS 代码）。

**实现状态**：已实现（限本模块已提交状态）。恢复标记随写事务提交；恢复核对草稿请求摘要与候选摘要、清除已核对标记、重置过期租约、重建可推导的 outbox 任务、把无法证明一致的记录隔离并把 attempt 置 unknown；标记超过 4096 条时有界拒绝为 RECOVERY_REQUIRED。限制：不处理 pending Commit/跨群撤销等 T3 类恢复（依赖 CRYPTO-007/008），也不处理文件系统与密钥槽副作用。

<a id="storage-012"></a>

## STORAGE-012 · query_message_page

~~~text
query_message_page(conversation: ConversationId, before: Option<Cursor>, limit: u32) -> Result<Page<MessageView>>
~~~

**职责与输出**：返回稳定分页含本机待发意图与已接受消息的展示投影。

**输入与前置条件**：limit 1..100；会话可见；不导出密钥或 pending Commit。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证游标范围。
2. 固定查询排序。
3. 取 limit+1。
4. 映射只读 DTO。
5. 生成下页游标。
6. 不读全历史。

**持久化、并发与副作用**：只读快照；正式按 seq，待发用独立本地排序键。

**失败处理**：INVALID_CURSOR，PERMISSION_DENIED，STORAGE_LOCKED。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-013 decode_page_cursor](protocol.md#protocol-013)、[PROTOCOL-012 encode_page_cursor](protocol.md#protocol-012)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 并发到达消息翻页不重复/跳页：`formal_query::message_page_merges_pending_and_accepted_without_duplicates_or_skips`（翻页中途插入新消息与新待发意图，旧游标既不重复也不跳页，7 条历史各返回一次，含待发/正式合并顺序）、`formal_query::message_page_reports_end_of_stream_and_bounds_the_last_page`（边界与满页）。
- [ ] 待发转正式 ID 关联稳定：部分验证——待发投影以 `draft_id` 为稳定本地 ID、并带已有 attempt 的 `submission_id`（`formal_query::prepared_attempts_are_exposed_as_pending_projection_with_submission_id`），且已进入 `messages` 的草稿不再作为待发返回；正式 MessageId 关联依赖尚未实现的 STORAGE-008 写入 `messages.draft_id`/`messages.submission_id`，因此本项不勾选。

**实现状态**：已实现（Windows x64 实测，仅读取已提交状态）。入口 `query_message_page(conversation_id, before, limit)` 与注入时钟的 `query_message_page_at(..., now_unix_ms)`，见 [query.rs](../../crates/storage/src/formal/query.rs)；游标见 [cursor.rs](../../crates/storage/src/formal/cursor.rs)。证据：`cargo test --offline -p chat-storage`（`formal_query` 7 个用例中本函数 5 个：合并排序/边界/游标/篡改/待发投影）。限制：游标是内部 `spg1` 临时令牌（非 PROTOCOL-012 `cur1`）；待发投影 ID 暂用 `draft_id`；limit 上限取契约的 100（未采用任务板的 200 建议）；`conversations.protected_metadata` 不在本函数解密。

<a id="storage-013"></a>

## STORAGE-013 · query_conversation_page

~~~text
query_conversation_page(cursor: Option<Cursor>, limit: u32) -> Result<Page<ConversationView>>
~~~

**职责与输出**：返回列表、未读/待发/故障摘要。

**输入与前置条件**：会话索引受存储加密保护，不建立 Kotlin 第二数据库。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 验证分页参数。
2. 固定 last_activity/local_id 排序。
3. 读取摘要与故障状态。
4. 输出游标。

**持久化、并发与副作用**：只读快照；活动时间不参与协议授权。

**失败处理**：INVALID_CURSOR，STORAGE_FAILURE。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[PROTOCOL-013 decode_page_cursor](protocol.md#protocol-013)、[PROTOCOL-012 encode_page_cursor](protocol.md#protocol-012)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 同时间会话用稳定次键：`formal_query::conversation_page_orders_by_activity_with_stable_tiebreak`（`conv-2`/`conv-3` 最近活动时间相同，按 `conversation_id DESC` 稳定排序；`last_activity_at` 取消息展示时间与本地意图创建时间的较大值）。
- [x] 锁库时不泄漏旧缓存摘要：本函数不维护任何缓存，且 fail-closed 闸门直接拒绝——`formal_query::queries_fail_closed_after_the_store_is_marked_faulted`（`STORAGE_FAILURE`）。限制：进程内无法制造“另一个进程持有锁”的状态（`open_store` 已独占打开），等价验证走 fail-closed 闸门。

**实现状态**：已实现（Windows x64 实测）。入口 `query_conversation_page(cursor, limit)` 与 `query_conversation_page_at(..., now_unix_ms)`，见 [query.rs](../../crates/storage/src/formal/query.rs)；摘要包含待发意图数、`failed` outbox 任务数、`unknown` 尝试数。证据：`cargo test --offline -p chat-storage`（`formal_query::conversation_page_orders_by_activity_with_stable_tiebreak`）。限制：`unread_count` 恒为 `None`（无 read-cursor 存储，不伪造 0）；会话标题等 `protected_metadata` 未解密（封装契约未定义）；limit 上限 100。

<a id="storage-014"></a>

## STORAGE-014 · claim_outbox_batch

~~~text
claim_outbox_batch(now: Time, limit: u32, worker: WorkerId) -> Result<Vec<LeasedJob>>
~~~

**职责与输出**：原子租用到期任务，防双 worker 重发状态竞争。

**输入与前置条件**：任务有 retry_at/lease_until/cancel_state；群内任务有顺序约束。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 筛选到期任务。
2. 排除未满足依赖。
3. 原子 CAS 租约。
4. 返回不可变 payload 引用。
5. 限制批量字节。

**持久化、并发与副作用**：短写事务；发送在事务外；成功 ACK 再短事务完成任务。

**失败处理**：STORAGE_BUSY，JOB_LEASE_CONFLICT。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：底层规则或平台原语；不依赖其他业务函数。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] worker 崩溃租约到期恢复：`formal_outbox::claim_leases_due_jobs_once_and_reclaims_after_expiry`（租约 30 秒内第二个 worker 领不到，到期后被重新领取且 `lease_version` 递增）。
- [x] 两 worker 不同时拥有同 lease：同一用例（租约有效窗口内 `worker-b` 返回空）+ CAS 版本语义（`claim_skips_dependencies_and_cancelled_jobs` 验证依赖与取消跳过；`mark_is_idempotent_and_rejects_conflicting_or_stale_results` 验证伪造/旧版本 `JOB_LEASE_CONFLICT`）。限制：`open_store` 单写者独占，无法在同一进程内同时打开两个 Store 制造真并发，用“顺序领取 + `lease_version` CAS”表达同一不变式。

**实现状态**：已实现（Windows x64 实测）。`claim_outbox_batch(now, limit, worker)` 在单笔短事务内按 `lease_version` CAS 领取，见 [outbox_lease.rs](../../crates/storage/src/formal/outbox_lease.rs)。上限：条数 `MAX_CLAIM_BATCH = 32`、载荷字节 `MAX_CLAIM_BATCH_BYTES = 4 MiB`（`formal_outbox::claim_bounds_the_batch_by_payload_bytes`）；跳过 `cancel_requested` 与依赖未完成；写锁短暂占用映射为 `STORAGE_BUSY`；畸形输入（limit/worker/时间戳）返回 `INVALID_INPUT`（`formal_outbox::claim_and_mark_reject_malformed_input`）。限制：真实跨进程竞争未在同一进程内复现；群内任务顺序约束只实现 `depends_on` 这一层。

<a id="storage-015"></a>

## STORAGE-015 · mark_outbox_result

~~~text
mark_outbox_result(lease: LeasedJob, result: JobResult) -> Result<JobState>
~~~

**职责与输出**：只允许当前 lease owner 更新结果。

**输入与前置条件**：result 区分 transport/mailbox/正式接受/送达。参数类型含义见类型字典；调用方只传必要范围，秘密不得进入日志或普通 DTO。

**处理步骤**：

1. 核对 lease 版本。
2. 保存明确结果。
3. 按 retry 决策更新 next_attempt。
4. 清除租约。
5. 记录不含正文的诊断。

**持久化、并发与副作用**：短事务；取消与已接受竞争遵从正式回执证据。

**失败处理**：JOB_LEASE_CONFLICT，INVALID_STATE_TRANSITION。网络类错误按持久退避处理；验证/损坏类错误不自动放宽安全条件。

**调用关系**：[DOMAIN-010 classify_retry](domain.md#domain-010)、[DOMAIN-008 transition_send_state](domain.md#domain-008)。此处列主要逻辑协作关系，纯函数之间的调用不代表共享事务，也不代表代码已经存在。

**完成验收**：

- [x] 迟到旧 worker 不覆盖新结果：`formal_outbox::claim_leases_due_jobs_once_and_reclaims_after_expiry`（旧 worker 用版本 1 更新版本 2 的租约 → `JOB_LEASE_CONFLICT`，行状态未被改写）、`formal_outbox::mark_is_idempotent_and_rejects_conflicting_or_stale_results`（旧版本/伪造持有者拒绝；重复相同结果幂等且不改行、不推进库版本）。
- [x] mailbox ACK 不升级送达：`formal_outbox::mailbox_ack_does_not_upgrade_delivery`（`MailboxAck` 只把 attempt 置 `submitted`，不写 `messages` 行、不产生送达投影）；`formal_outbox::unknown_result_stays_reconcilable_and_never_completes` 证明 `unknown` 不落成 `complete`。

**实现状态**：已实现（Windows x64 实测）。`mark_outbox_result(lease, result)` 与注入时钟的 `mark_outbox_result_at(..., now_unix_ms)`，见 [outbox_lease.rs](../../crates/storage/src/formal/outbox_lease.rs)。三态语义：success → 任务 `complete`；可重试失败 → `pending` + 指数退避（5 秒起、上限 5 分钟）；终态失败 → `failed` + attempt `rejected`；`unknown` → 保持 `leased`、延长 6 小时对账窗口、attempt `unknown`，绝不当作成功删除投递任务。attempt 只允许前进（`accepted`/`rejected` 不回退）。限制：诊断不落盘（P0 schema 无诊断列且本轮不新增迁移，只随 `JobState` 返回）；`unknown` 的真实对账（按 SubmissionId 查询节点状态）属于同步/节点模块，未实现；重试退避策略是本地取值，正式策略待 DOMAIN-010 `classify_retry` 接入后对齐。


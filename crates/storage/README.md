# crates/storage

Rust storage 模块。

- `formal/`：正式加密客户端存储路径。`open_store` 独占打开 + WAL/FULL/外键 + 密钥校验 + 版本化迁移（当前 schema v7）；`CoreTransaction` 只发 `BEGIN IMMEDIATE` 且只能消费一次；`persist_send_intent`/`persist_prepared_attempt` 原子写 T1/T2；`recover_incomplete_writes` 只核对既有字节。消息正文、MLS 记录值、附件记录元数据、附件 FileKey、隔离诊断用主密钥派生的 ChaCha20-Poly1305 子密钥加密，错误密钥与篡改返回 `STATE_CORRUPTED`。
- `formal/attachments.rs`：附件记录持久化（迁移 0005，复用 0001 的 `attachments` 主表与密封列）：`save_attachment/load_attachment/list_attachments/mark_attachment_state/delete_attachment`，AAD 绑定 attachment_id，state 白名单 + 迁移表，输入有界。
- `formal/attachments.rs`（续）：附件 FileKey 持久化（迁移 0007）：`attachment_secrets` 表（外键 `ON DELETE CASCADE`）保存 file_id + FileKey 的密封记录（用途 `attachment-secret`，AAD 绑 attachment_id），`save_attachment_secret/load_attachment_secret/delete_attachment_secret`；换钥显式 `OPERATION_ID_CONFLICT`，篡改 `STATE_CORRUPTED`。
- `store.rs`：脱离正式路径的 [P0-03 SQLite/OpenMLS 事务实验](../../docs/P0_PERSISTENCE.md)，未加密，只用于一次性测试库；与 `formal/` 不共享连接、表或类型。
- `migrations/`：`0001_initial.sql`（schema 草稿，正式路径按版本执行）到 `0007_attachment_secrets.sql`。

验证：`. .\tools\activate.ps1; cargo test --offline -p chat-storage`（84 passed）。
设计契约见 [SQLite、统一事务、数据表与崩溃恢复](../../docs/implementation/storage.md)。

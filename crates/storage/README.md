# crates/storage

Rust storage 模块。

- `formal/`：正式加密客户端存储路径（P0-04 基础）。`open_store` 独占打开 + WAL/FULL/外键 + 密钥校验 + 版本化迁移；`CoreTransaction` 只发 `BEGIN IMMEDIATE` 且只能消费一次；`persist_send_intent`/`persist_prepared_attempt` 原子写 T1/T2；`recover_incomplete_writes` 只核对既有字节。消息正文、MLS 记录值、隔离诊断用主密钥派生的 ChaCha20-Poly1305 子密钥加密，错误密钥与篡改返回 `STATE_CORRUPTED`。
- `store.rs`：隔离的 [P0-03 SQLite/OpenMLS 事务实验](../../docs/P0_PERSISTENCE.md)，未加密，只用于一次性测试库；与 `formal/` 不共享连接、表或类型。
- `migrations/`：`0001_initial.sql`（schema 草稿，P0-04 起被正式路径按版本执行）、`0002_formal_store.sql`（密钥校验记录、库版本、恢复标记、隔离表）。

验证：`. .\tools\activate.ps1; cargo test --offline -p chat-storage`（3 lib + 17 formal_store + 2 formal_crash = 22 passed）。
设计契约见 [SQLite、统一事务、数据表与崩溃恢复](../../docs/implementation/storage.md)。

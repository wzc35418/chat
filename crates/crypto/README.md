# crates/crypto

Rust crypto 模块。

- `key_store.rs`：P0-04 存储主密钥。Argon2id（v0x13）由口令派生 32 字节主密钥并返回可写库的 `KeyProvenance`；`load_master_key_file` 只接受恰好 32 字节的普通文件。参数与口令长度全部有界拒绝。
- `attachment.rs`：CRYPTO-011/012 附件分块 AEAD。每个附件独立随机 FileKey + FileId，固定 256 KiB 分块，密文格式 `version||nonce||ciphertext||tag`，AAD 绑定 file_id/index/total_chunks/plain_length/is_final/format_version；篡改、截断、乱序、重复块、错误密钥与伪造最后一块全部显式拒绝。`FileKey::copy_bytes` 只给“写入受保护存储/封装描述符”的边界用（Zeroizing 缓冲）。
- `secret_descriptor.rs`：附件 FileKey 的 E2EE SecretDescriptor（固定 93 字节：版本 + nonce + 密文 + 摘要），用调用方给的包装密钥做 ChaCha20-Poly1305，AAD 绑 attachment_id；摘要先校验、失败统一 `ATTACHMENT_CORRUPTED`，解出的 FileKey 用 Zeroizing 缓冲。
- `control.rs`：CRYPTO-005~009 群成员控制。`prepare_control` 只 stage pending（本地 epoch 不变）、`merge_accepted_pending` 需接受证据绑定 group/epoch/提交摘要、`discard_rejected_pending` 需拒绝证据并回滚已提交快照、`process_verified_entry` 先校验群/epoch/作者再解密或 merge、`join_from_welcome` 核对群绑定后持久化。pending 元数据随群状态快照写入正式加密库（群级头版本 2，兼容读取版本 1）。
- `engine.rs`：真实 OpenMLS 设备（建群/加入/加解密）+ CRYPTO-004/010 的已提交状态持久化切片。
- `persistence.rs` + `crash_worker.rs`：隔离的 P0-03 未加密崩溃实验，与正式加密存储路径无关，保持原有测试通过。

验证：`. .\tools\activate.ps1; cargo test --offline -p chat-crypto`（44 passed：3 engine + 9 attachment_chunks + 5 formal_master_key + 4 formal_mls_state + 12 mls_control + 7 p0_persistence + 4 secret_descriptor）。
设计契约见 [MLS、单群串行状态与附件密码学](../../docs/implementation/crypto.md)。

仍未完成：同事务 provider（TxnCryptoProvider）、PreparedWrite 与外层候选签名、T3 日志/游标事务、DOMAIN-004/IDENTITY 的授权与回执验签、提案式控制流、SecretDescriptor 的包装密钥派生（MLS application secret）与平台密钥槽。P0 局部验证不表示该模块全部功能已完成。

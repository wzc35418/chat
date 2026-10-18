# crates/crypto

Rust crypto 模块。

- `key_store.rs`：P0-04 存储主密钥。Argon2id（v0x13）由口令派生 32 字节主密钥并返回可写库的 `KeyProvenance`；`load_master_key_file` 只接受恰好 32 字节的普通文件。参数与口令长度全部有界拒绝。
- `attachment.rs`：CRYPTO-011/012 附件分块 AEAD。每个附件独立随机 FileKey + FileId，固定 256 KiB 分块，密文格式 `version||nonce||ciphertext||tag`，AAD 绑定 file_id/index/total_chunks/plain_length/is_final/format_version；篡改、截断、乱序、重复块、错误密钥与伪造最后一块全部显式拒绝。
- `engine.rs`：临时 P0 MLS 设备（仅内存）。
- `persistence.rs` + `crash_worker.rs`：隔离的 P0-03 未加密崩溃实验，与正式加密存储路径无关，保持原有测试通过。

验证：`. .\tools\activate.ps1; cargo test --offline -p chat-crypto`（3 engine + 9 attachment_chunks + 5 formal_master_key + 7 p0_persistence = 24 passed）。
设计契约见 [MLS、单群串行状态与附件密码学](../../docs/implementation/crypto.md)。

生产统一事务、MLS provider 同事务接入、上传/下载链路与全库加密方案仍未完成；P0 局部验证不表示该模块全部功能已完成。

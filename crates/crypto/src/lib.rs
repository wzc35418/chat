//! crypto 模块：接口与实现顺序见 docs/implementation。
//! engine implements ephemeral P0 MLS; durable crypto storage remains a P0 blocker.
//! key_store 实现 P0-04 存储主密钥的 Argon2id 派生与 32 字节密钥文件加载。
//! attachment 实现 CRYPTO-011/012 的附件分块 AEAD（ChaCha20-Poly1305）。
//! control 实现 CRYPTO-005~009 的群成员控制提交、对端条目处理、pending 合并/丢弃与 Welcome 加入。
#![forbid(unsafe_code)]
pub mod attachment;
pub mod control;
pub mod engine;
pub mod key_store;
pub mod persistence;

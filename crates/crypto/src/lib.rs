//! crypto 模块：接口与实现顺序见 docs/implementation。
//! engine implements ephemeral P0 MLS; durable crypto storage remains a P0 blocker.
//! key_store 实现 P0-04 存储主密钥的 Argon2id 派生与 32 字节密钥文件加载。
#![forbid(unsafe_code)]
pub mod engine;
pub mod key_store;
pub mod persistence;

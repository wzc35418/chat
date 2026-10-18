//! storage 模块：接口与实现顺序见 docs/implementation。
//! formal 是正式加密存储路径（STORAGE-001/003/006/007/009/010/011 + P0-04 基础）；
//! store 保留隔离的 P0-03 SQLite/OpenMLS 未加密事务实验，两者互不复用连接或表。
#![forbid(unsafe_code)]
pub mod formal;
pub mod store;

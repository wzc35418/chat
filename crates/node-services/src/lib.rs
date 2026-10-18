//! node-services 模块：接口与实现顺序见 docs/implementation。
//! 正式节点服务的挑战/鉴权/设备注册在 auth、registration、services；存储上限在 limits。
//! 群日志（NODE-005~008）在 group，密文信箱（NODE-009~011）在 mailbox。
//! P0 isolated SQLite acceptance experiment is in p0_accept; product node remains incomplete.
#![forbid(unsafe_code)]
pub mod auth;
pub mod blob;
pub mod canonical;
pub mod error;
pub mod group;
pub mod limits;
pub mod mailbox;
pub mod p0_accept;
pub mod registration;
pub mod services;

//! node-services 模块：接口与实现顺序见 docs/implementation。
//! 正式节点服务的挑战/鉴权/设备注册在 auth、registration、services；存储上限在 limits。
//! P0 isolated SQLite acceptance experiment is in p0_accept; product node remains incomplete.
#![forbid(unsafe_code)]
pub mod auth;
pub mod canonical;
pub mod error;
pub mod limits;
pub mod p0_accept;
pub mod registration;
pub mod services;

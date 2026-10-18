//! core 模块：接口与实现顺序见 docs/implementation。
//! p0 contains real ephemeral MLS/iroh verification; product services remain planned.
#![forbid(unsafe_code)]
pub mod attachments;
pub mod group_actions;
pub mod groups;
pub mod p0;
pub mod service;

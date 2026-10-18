//! 节点 HTTP 客户端（crates/node-client）。
//!
//! 只做四件事：签发/消费一次性挑战、按节点的 canonical 规则签名、发 HTTP、把稳定错误码
//! 映射成 [`NodeClientError`]。领域规则（谁能写、序号与哈希链、配额）全在节点侧，
//! 客户端不重复判断，也不在失败后伪造成功结果。
#![forbid(unsafe_code)]

pub mod client;
pub mod endpoint;
pub mod error;

pub use client::{ClientLimits, NodeClient};
pub use endpoint::NodeEndpoint;
pub use error::{NodeClientError, NodeClientErrorKind};

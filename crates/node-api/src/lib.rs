//! node-api 模块：节点 HTTP、鉴权与运行入口。详细函数契约见 docs/implementation/api.md。
//!
//! 本 crate 只做“鉴权、解码、调用服务、映射结果”四件事：
//! - dto：版本化 HTTP DTO 与稳定错误结构；
//! - error：NodeError 到 HTTP 状态与错误码的唯一映射；
//! - state：服务与时钟注入（生产用系统时钟，测试用固定时钟推进过期）；
//! - handlers：axum 路由 /v1/auth/challenge 与 /v1/devices。
#![forbid(unsafe_code)]
pub mod dto;
pub mod error;
pub mod handlers;
pub mod state;

pub use handlers::build_router;

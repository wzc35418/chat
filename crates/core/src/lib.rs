//! core 模块：接口与实现顺序见 docs/implementation。
//! p0 contains real ephemeral MLS/iroh verification; product services remain planned.
#![forbid(unsafe_code)]
pub mod attachment_message;
pub mod attachment_node;
pub mod attachment_transport;
pub mod attachments;
pub mod core_events;
pub mod group_actions;
pub mod group_node;
pub mod groups;
pub mod node_submitter;
pub mod p0;
pub mod send_orchestrator;
pub mod send_runtime;
pub mod sync_runtime;
pub mod sync_worker;
pub mod service;

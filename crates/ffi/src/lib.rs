//! ffi 模块：接口与实现顺序见 docs/implementation。
//! bridge exposes the ephemeral P0 session through generated UniFFI bindings.
#![forbid(unsafe_code)]
pub mod attachments_ffi;
pub mod bridge;
pub mod identity_ffi;
pub mod local_store;
uniffi::setup_scaffolding!();

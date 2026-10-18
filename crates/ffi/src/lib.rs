//! ffi 模块：接口与实现顺序见 docs/implementation。
//! bridge exposes the ephemeral P0 session through generated UniFFI bindings.
#![forbid(unsafe_code)]
pub mod bridge;
uniffi::setup_scaffolding!();

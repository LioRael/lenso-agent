//! Durable, owner-scoped task lifecycle state for Agent Plugins.

#[allow(dead_code)]
mod contract;
mod runtime;

pub use runtime::*;

include!("generated.rs");

//! Ephemeral provider authority captured before an Agent Turn starts.

#[allow(dead_code)]
mod contract;

include!("generated.rs");

/// Host-issued opaque turn binding, never persisted in Session history.
pub const SCOPE_EXTENSION: &str = "lenso.agent.turn-binding.scope";

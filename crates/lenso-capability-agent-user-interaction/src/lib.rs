//! Portable user interaction Capability.

#[allow(dead_code)]
mod contract;

include!("generated.rs");

/// Host-issued Invocation Context marker allowing a Turn to wait for a user.
#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
pub struct InteractiveSurface;

impl lenso::TypedExtension for InteractiveSurface {
    const KEY: &'static str = "lenso.agent.interactive-surface@1";
}

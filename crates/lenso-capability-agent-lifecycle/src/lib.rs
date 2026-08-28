//! Portable typed Agent lifecycle observation Capability.

#[allow(dead_code)]
mod contract;

include!("generated.rs");

mod runtime;

pub use runtime::observe_all;

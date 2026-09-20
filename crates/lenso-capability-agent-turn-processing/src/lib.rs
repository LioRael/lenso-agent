//! Portable, ordered processing stages for Agent turns.

#[allow(dead_code)]
mod contract;
mod runtime;

pub use runtime::*;

include!("generated.rs");

//! Plan-bound dynamic selection and execution revalidation for Agent Tools.

#[allow(dead_code)]
mod contract;
mod runtime;

pub use runtime::*;

include!("generated.rs");

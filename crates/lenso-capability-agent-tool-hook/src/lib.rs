//! Portable Agent Tool Hook Capability.

#[allow(dead_code)]
mod contract;

include!("generated.rs");

mod runtime;

pub use runtime::{
    HookBlock, HookExecution, HookTerminal, NormalizeArgumentsError, finish_hooks,
    normalize_arguments, start_hooks,
};

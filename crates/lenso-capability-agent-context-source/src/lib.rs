//! Portable catalogs for user-selected prompts and application-selected resources.

#[allow(dead_code)]
mod contract;

include!("generated.rs");

pub const MAX_PROMPTS: usize = 256;
pub const MAX_RESOURCES: usize = 1_024;
pub const MAX_TEXT_BYTES: usize = 1_048_576;

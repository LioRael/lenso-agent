//! Typed authoring support for Agent Tool Provider Plugins.
//!
//! `#[tool_provider]` is the single Tool authoring interface. With the native
//! `lenso` facade it generates linked Provider endpoints; with the portable
//! `lenso-plugin-sdk` facade it generates the same Tool catalog and dispatcher
//! before Runtime lowering to Wasm or Process.

pub use lenso_agent_tool_sdk_macros::tool_provider;

/// Imports the Tool Provider contract alias required by generated Plugin glue.
pub mod prelude {
    pub use crate::tool_provider;
    pub use lenso_capability_agent_tool_provider as tool_provider_contract;
    pub use lenso_capability_agent_tool_provider::{ContentType, ExecuteError, ExecuteResponse};
}

#[doc(hidden)]
pub mod __private {
    pub use lenso_capability_agent_tool_provider as contract;
    pub use schemars;
    pub use serde_json;
}

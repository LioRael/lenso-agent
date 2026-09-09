//! Typed authoring support for Agent Tool Provider Plugins.
//!
//! `#[tool_provider]` is the single Tool authoring interface. With the native
//! `lenso` facade it generates linked Provider endpoints; with the portable
//! `lenso-plugin-sdk` facade it generates the same Tool catalog and dispatcher
//! before Runtime lowering to Wasm or Process.
//!
//! Native methods calling other Capabilities can return
//! `lenso::PluginResult<ExecuteResponse, ExecuteError>` to preserve both domain
//! rejections and runtime failures. Ordinary `Result<ExecuteResponse,
//! ExecuteError>` methods remain supported. Spell the native result as
//! `PluginResult` (qualified or imported); custom result aliases are not detected.
//! Forward the supplied `lenso::Ctx` through the required client's
//! `*_with_context` operation rather than constructing a new invocation context.
//! This preserves existing context; it does not authenticate a business actor
//! or grant permission to call the downstream provider.

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

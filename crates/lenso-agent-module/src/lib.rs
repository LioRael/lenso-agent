//! Source-first authoring Interface for Agent Modules.

pub use lenso_agent_module_macros::tool;
pub use lenso_capability_agent_tool_provider::{
    ExecuteError as ToolError, ExecuteResponse as ToolOutput,
    ExecuteResponseContentType as ToolOutputType,
};
pub use schemars::JsonSchema;

/// Implementation details referenced by generated Agent Module glue.
#[doc(hidden)]
pub mod __private {
    pub use futures::future::{LocalBoxFuture, ready};
    pub use lenso_capability_agent_tool_provider::{
        CatalogError, CatalogRequest, CatalogResponse, CatalogResponseToolsItem, ExecuteError,
        ExecuteRequest, ExecuteResponse, ToolProviderEndpoint, ToolProviderProvider,
    };
    pub use lenso_kernel::{CancellationToken, InvocationContext, NativeRequestEndpoint};
    pub use lenso_native_adapter::RuntimeFailure;
    pub use schemars::schema_for;
    pub use serde_json;
}

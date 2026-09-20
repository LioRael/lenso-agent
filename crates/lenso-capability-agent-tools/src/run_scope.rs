//! Turn-local Tool authority selected by a Host before invocation.

use std::collections::BTreeSet;

use lenso::{CtxExt, TypedExtension};
use lenso_kernel::InvocationContext;

/// Host-issued Invocation Context key for one Turn's narrowed Tool authority.
pub const RUN_SCOPE_EXTENSION: &str = "lenso.agent.run-scope@1";

/// One immutable Turn-local authority scope. Names must come from the Plan-bound Tool catalog.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunScope {
    /// Exact Tool names admitted for this Turn. An empty set disables Tools.
    pub allowed_tools: BTreeSet<String>,
}

impl RunScope {
    /// Creates a deterministic scope from requested Tool names.
    pub fn new(tools: impl IntoIterator<Item = impl Into<String>>) -> Result<Self, String> {
        let mut allowed_tools = BTreeSet::new();
        for tool in tools {
            let tool = tool.into();
            if tool.is_empty() || tool.len() > 128 {
                return Err("Run Scope contains an invalid Tool name".to_owned());
            }
            allowed_tools.insert(tool);
        }
        Ok(Self { allowed_tools })
    }

    /// Attaches this scope to one root Invocation Context.
    pub fn attach(self, context: InvocationContext) -> Result<InvocationContext, String> {
        context
            .with_typed_extension(&self)
            .map_err(|error| format!("failed to attach Run Scope: {error}"))
    }
}

impl TypedExtension for RunScope {
    const KEY: &'static str = RUN_SCOPE_EXTENSION;
}

#[cfg(test)]
mod tests {
    use super::RunScope;

    #[test]
    fn deduplicates_and_rejects_invalid_tool_names() {
        let scope = RunScope::new(["workspace.read", "text.echo", "workspace.read"]).unwrap();
        assert_eq!(scope.allowed_tools.len(), 2);
        assert!(RunScope::new([""]).is_err());
    }
}

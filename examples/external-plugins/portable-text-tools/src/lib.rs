use lenso_plugin_sdk::AgentTool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Arguments {
    #[schemars(length(max = 4096))]
    text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum ToolError {
    InvalidArguments,
}

#[derive(Default)]
struct Plugin;

impl AgentTool for Plugin {
    type Arguments = Arguments;
    type Error = ToolError;

    const NAME: &'static str = "uppercase";
    const DESCRIPTION: &'static str = "Convert one UTF-8 string to uppercase.";

    fn execute(&self, arguments: Arguments) -> Result<String, ToolError> {
        if arguments.text.is_empty() {
            Err(ToolError::InvalidArguments)
        } else {
            Ok(arguments.text.to_uppercase())
        }
    }
}

lenso_plugin_sdk::export_agent_tool!(Plugin);

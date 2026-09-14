use lenso_agent_tool_sdk::prelude::*;
use schemars::JsonSchema;

#[derive(Debug, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Arguments {
    #[schemars(length(max = 4096))]
    text: String,
}

#[lenso::plugin]
#[derive(Clone, Copy, Debug, Default)]
struct Plugin {}

#[lenso_agent_tool_sdk::tool_provider]
impl Plugin {
    #[tool(
        name = "marketplace_proof",
        description = "Process one UTF-8 string.",
        execution = "parallel_safe"
    )]
    fn execute(arguments: Arguments) -> Result<ExecuteResponse, ExecuteError> {
        if arguments.text.is_empty() {
            return Err(ExecuteError::InvalidArguments);
        }
        Ok(ExecuteResponse {
            content: arguments.text,
            content_blocks: None,
            content_type: ContentType::Text,
            metadata_json: "{}"
                .try_into()
                .expect("static Tool metadata must be valid JSON"),
        })
    }
}

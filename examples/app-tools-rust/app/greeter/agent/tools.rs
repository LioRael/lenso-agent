use lenso_agent_tool_sdk::prelude::*;
use schemars::JsonSchema;

#[derive(Debug, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Arguments {
    name: String,
}

#[lenso::plugin]
#[derive(Clone, Copy, Debug, Default)]
struct Plugin {}

#[lenso_agent_tool_sdk::tool_provider]
impl Plugin {
    #[tool(name = "greet", description = "Greet a person.", execution = "parallel_safe")]
    fn greet(arguments: Arguments) -> Result<ExecuteResponse, ExecuteError> {
        Ok(ExecuteResponse {
            content: format!("Hello, {}!", arguments.name),
            content_blocks: None,
            content_type: ContentType::Text,
            metadata_json: "{}".try_into().expect("static JSON"),
        })
    }
}

use lenso_agent_tool_sdk::prelude::*;
use schemars::JsonSchema;

#[derive(Debug, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Arguments {
    id: String,
}

#[lenso::plugin]
#[derive(Clone, Copy, Debug, Default)]
struct Plugin {}

#[lenso_agent_tool_sdk::tool_provider]
impl Plugin {
    #[tool(
        name = "lookup_billing",
        description = "Look up a billing record.",
        execution = "parallel_safe"
    )]
    fn lookup_billing(arguments: Arguments) -> Result<ExecuteResponse, ExecuteError> {
        Ok(ExecuteResponse {
            content: format!("Billing {}", arguments.id),
            content_blocks: None,
            content_type: ContentType::Text,
            metadata_json: "{}".try_into().expect("static JSON"),
        })
    }
}

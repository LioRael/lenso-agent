use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Arguments {
    #[schemars(length(max = 4096))]
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolError {
    InvalidArguments,
}

pub fn execute(arguments: Arguments) -> Result<String, ToolError> {
    if arguments.text.is_empty() {
        Err(ToolError::InvalidArguments)
    } else {
        Ok(arguments.text.to_uppercase())
    }
}

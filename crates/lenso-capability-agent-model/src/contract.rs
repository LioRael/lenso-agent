//! Authoritative source for the Agent Model Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CompleteOpen {
    #[schemars(length(min = 1, max = 256))]
    pub model: String,
    #[schemars(length(max = 256))]
    pub messages: Vec<CompleteMessageInput>,
    #[schemars(length(max = 256))]
    pub tools: Vec<CompleteTool>,
    #[schemars(range(min = 0, max = 2))]
    pub temperature: f64,
    #[schemars(range(min = 1, max = 1_000_000))]
    pub max_output_tokens: i64,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CompleteMessageInput {
    pub role: CompleteMessageRole,
    #[schemars(length(max = 1_048_576))]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", length(max = 128))]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", length(max = 128))]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", length(max = 262_144))]
    pub arguments_json: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompleteMessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CompleteTool {
    #[schemars(length(min = 1, max = 128))]
    pub name: String,
    #[schemars(length(max = 4_096))]
    pub description: String,
    #[schemars(length(min = 2, max = 65_536))]
    pub input_schema_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CompleteMessage {
    #[schemars(extend("format" = "uint64"))]
    pub sequence: String,
    pub kind: CompleteMessageKind,
    #[schemars(length(max = 65_536))]
    pub text: String,
    #[schemars(length(max = 128))]
    pub tool_call_id: String,
    #[schemars(length(max = 128))]
    pub tool_name: String,
    #[schemars(length(max = 262_144))]
    pub arguments_json: String,
    #[schemars(extend("format" = "uint64"))]
    pub input_tokens: String,
    #[schemars(extend("format" = "uint64"))]
    pub output_tokens: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompleteMessageKind {
    TextDelta,
    ToolCall,
    Usage,
}

#[derive(lenso::DomainError)]
pub enum CompleteError {
    InvalidRequest,
    UnsupportedModel,
    ContentRejected,
}

#[lenso::capability(
    id = "lenso.agent.model",
    major = 1,
    version = "1.1.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait Model {
    async fn complete(
        &self,
        context: lenso::Ctx<'_>,
        request: CompleteOpen,
    ) -> lenso::Stream<CompleteMessage, CompleteError>;
}

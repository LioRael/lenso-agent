//! Authoritative source for the Agent Turn Processing Capability contract.

use lenso_contract_authoring as lenso;

/// The model-visible message form intentionally excludes execution authority.
#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ModelMessage {
    pub role: ModelMessageRole,
    #[schemars(length(max = 1_048_576))]
    pub content: String,
    #[schemars(length(max = 16))]
    pub images: Option<Vec<ModelImage>>,
    #[schemars(length(max = 128))]
    pub tool_call_id: Option<String>,
    #[schemars(length(max = 128))]
    pub tool_name: Option<String>,
    #[schemars(length(max = 262_144))]
    pub arguments_json: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelMessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ModelImage {
    #[schemars(length(min = 1, max = 128))]
    pub media_type: String,
    #[schemars(length(min = 1, max = 2_796_204))]
    pub data_base64: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ModelTool {
    #[schemars(length(min = 1, max = 128))]
    pub name: String,
    #[schemars(length(max = 4_096))]
    pub description: String,
    #[schemars(length(min = 2, max = 65_536))]
    pub input_schema_json: String,
}

/// An optional context projection immediately before one model request.
/// Provider/model selection, temperature, token budgets, and executable Tool
/// authority are not mutable through this stage.
#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ProjectModelRequest {
    #[schemars(length(min = 1, max = 128))]
    pub turn_id: String,
    #[schemars(length(max = 256))]
    pub messages: Vec<ModelMessage>,
    #[schemars(length(max = 256))]
    pub tools: Vec<ModelTool>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ProjectModelResponse {
    #[schemars(length(max = 256))]
    pub messages: Vec<ModelMessage>,
    #[schemars(length(max = 256))]
    pub tools: Vec<ModelTool>,
    #[schemars(length(min = 1, max = 128))]
    pub transformation_id: String,
    #[schemars(length(min = 1, max = 64))]
    pub transformation_version: String,
}

/// A narrow, pre-authorization Tool argument transformation. It cannot change
/// the selected Tool name. Every emitted argument value is normalized and
/// schema-validated by the Tool aggregate before final authorization runs.
#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct TransformToolCallRequest {
    #[schemars(length(min = 1, max = 128))]
    pub turn_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub tool_call_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub tool_name: String,
    #[schemars(length(min = 2, max = 262_144))]
    pub arguments_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct TransformToolCallResponse {
    #[schemars(length(min = 2, max = 262_144))]
    pub arguments_json: String,
    #[schemars(length(min = 1, max = 128))]
    pub transformation_id: String,
    #[schemars(length(min = 1, max = 64))]
    pub transformation_version: String,
}

/// Immutable factual Tool execution data. Result projectors receive it for
/// rendering only and cannot replace any stored outcome or metadata.
#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ToolResultFact {
    #[schemars(length(min = 1, max = 128))]
    pub tool_call_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub tool_name: String,
    #[schemars(length(min = 2, max = 262_144))]
    pub arguments_json: String,
    pub outcome: ToolResultOutcome,
    #[schemars(length(max = 1_048_576))]
    pub content: String,
    #[schemars(length(min = 2, max = 65_536))]
    pub metadata_json: String,
    #[schemars(length(max = 128))]
    pub provider_code: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultOutcome {
    Success,
    DomainError,
    RuntimeFailure,
}

/// A model-visible projection of one immutable Tool result.
#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ProjectToolResultRequest {
    #[schemars(length(min = 1, max = 128))]
    pub turn_id: String,
    pub fact: ToolResultFact,
    #[schemars(length(max = 1_048_576))]
    pub presentation: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ProjectToolResultResponse {
    #[schemars(length(max = 1_048_576))]
    pub presentation: String,
    #[schemars(length(min = 1, max = 128))]
    pub transformation_id: String,
    #[schemars(length(min = 1, max = 64))]
    pub transformation_version: String,
}

#[derive(lenso::DomainError)]
pub enum ProcessingError {
    Rejected,
}

#[lenso::capability(
    id = "lenso.agent.turn-processing",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait TurnProcessing {
    async fn project_model_request(
        &self,
        context: lenso::Ctx<'_>,
        request: ProjectModelRequest,
    ) -> Result<ProjectModelResponse, ProcessingError>;

    async fn transform_tool_call(
        &self,
        context: lenso::Ctx<'_>,
        request: TransformToolCallRequest,
    ) -> Result<TransformToolCallResponse, ProcessingError>;

    async fn project_tool_result(
        &self,
        context: lenso::Ctx<'_>,
        request: ProjectToolResultRequest,
    ) -> Result<ProjectToolResultResponse, ProcessingError>;
}

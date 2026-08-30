//! Authoritative source for the Agent Tool Provider Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CatalogRequest {}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CatalogResponse {
    #[schemars(length(max = 256))]
    pub tools: Vec<ToolDefinition>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ToolDefinition {
    #[schemars(length(min = 1, max = 128))]
    pub name: String,
    #[schemars(length(max = 4_096))]
    pub description: String,
    #[schemars(length(min = 2, max = 65_536))]
    pub input_schema_json: String,
    /// Static safety classification enforced by the Agent Loop scheduler.
    pub execution: ToolExecutionClass,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionClass {
    ParallelSafe,
    Exclusive,
}

#[derive(lenso::DomainError)]
pub enum CatalogError {
    CatalogInvalid,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ExecuteRequest {
    #[schemars(length(min = 1, max = 128))]
    pub name: String,
    #[schemars(length(min = 2, max = 262_144))]
    pub arguments_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ExecuteResponse {
    pub content_type: ContentType,
    #[schemars(length(max = 1_048_576))]
    pub content: String,
    #[schemars(length(max = 64))]
    pub content_blocks: Option<Vec<ContentBlock>>,
    #[schemars(length(min = 2, max = 65_536))]
    pub metadata_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ContentBlock {
    pub kind: ContentBlockKind,
    #[schemars(length(max = 1_048_576))]
    pub text: Option<String>,
    #[schemars(length(min = 2, max = 1_048_576))]
    pub value_json: Option<String>,
    #[schemars(length(max = 128))]
    pub mime_type: Option<String>,
    #[schemars(length(max = 5_592_408))]
    pub data_base64: Option<String>,
    #[schemars(length(max = 4_096))]
    pub uri: Option<String>,
    #[schemars(length(max = 256))]
    pub name: Option<String>,
    #[schemars(length(max = 4_096))]
    pub handle: Option<String>,
    #[schemars(length(max = 4_096))]
    pub description: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentBlockKind {
    Text,
    Json,
    Image,
    Audio,
    ResourceLink,
    Artifact,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    Text,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ExecutionFailedPayload {
    #[schemars(length(min = 1, max = 128))]
    pub reason_code: String,
    #[schemars(length(max = 4_096))]
    pub message: String,
    #[schemars(length(min = 2, max = 65_536))]
    pub details_json: String,
}

#[derive(lenso::DomainError)]
pub enum ExecuteError {
    InvalidArguments,
    PermissionDenied,
    NotFound,
    OutputLimitExceeded,
    ExecutionFailed { payload: ExecutionFailedPayload },
}

#[lenso::capability(
    id = "lenso.agent.tool-provider",
    major = 2,
    version = "2.1.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait ToolProvider {
    async fn catalog(
        &self,
        context: lenso::Ctx<'_>,
        request: CatalogRequest,
    ) -> Result<CatalogResponse, CatalogError>;

    async fn execute(
        &self,
        context: lenso::Ctx<'_>,
        request: ExecuteRequest,
    ) -> Result<ExecuteResponse, ExecuteError>;
}

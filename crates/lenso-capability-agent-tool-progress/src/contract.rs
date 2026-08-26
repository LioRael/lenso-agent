//! Authoritative source for the Agent Tool Progress Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CatalogRequest {}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CatalogResponse {
    #[schemars(length(max = 256))]
    pub tools: Vec<ToolProgressDefinition>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ToolProgressDefinition {
    #[schemars(length(min = 1, max = 128))]
    pub name: String,
}

#[derive(lenso::DomainError)]
pub enum CatalogError {
    CatalogInvalid,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ExecuteOpen {
    #[schemars(length(min = 1, max = 128))]
    pub name: String,
    #[schemars(length(min = 2, max = 262_144))]
    pub arguments_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ExecuteProgress {
    pub kind: ExecuteProgressKind,
    pub content_type: ContentType,
    #[schemars(length(max = 1_048_576))]
    pub content: String,
    #[schemars(length(min = 2, max = 65_536))]
    pub metadata_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteProgressKind {
    Stdout,
    Stderr,
    Completed,
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
    id = "lenso.agent.tool-progress",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait ToolProgress {
    async fn progress_catalog(
        &self,
        context: lenso::Ctx<'_>,
        request: CatalogRequest,
    ) -> Result<CatalogResponse, CatalogError>;

    async fn execute_progress(
        &self,
        context: lenso::Ctx<'_>,
        request: ExecuteOpen,
    ) -> lenso::Stream<ExecuteProgress, ExecuteError>;
}

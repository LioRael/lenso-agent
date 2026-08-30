//! Authoritative source for the validated terminal command runtime role.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CatalogRequest {}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CatalogResponse {
    #[schemars(length(max = 256))]
    pub commands: Vec<CommandDefinition>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CommandDefinition {
    #[schemars(length(min = 1, max = 128))]
    pub id: String,
    #[schemars(length(min = 1, max = 8))]
    pub path: Vec<String>,
    #[schemars(length(min = 1, max = 256))]
    pub summary: String,
    #[schemars(length(max = 4_096))]
    pub description: String,
    #[schemars(length(max = 64))]
    pub parameters: Vec<CommandParameter>,
    #[schemars(length(min = 1, max = 2))]
    pub output_formats: Vec<OutputFormat>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CommandParameter {
    #[schemars(length(min = 1, max = 64))]
    pub id: String,
    pub kind: ParameterKind,
    #[schemars(length(min = 1, max = 64))]
    pub long: Option<String>,
    #[schemars(length(min = 1, max = 1))]
    pub short: Option<String>,
    #[schemars(length(min = 1, max = 64))]
    pub value_name: Option<String>,
    #[schemars(length(max = 1_024))]
    pub description: String,
    pub required: bool,
    pub multiple: bool,
    #[schemars(length(max = 64))]
    pub choices: Vec<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterKind {
    Positional,
    Option,
    Flag,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(lenso::DomainError)]
pub enum CatalogError {
    CatalogInvalid,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ExecuteOpen {
    #[schemars(length(min = 1, max = 128))]
    pub id: String,
    #[schemars(length(min = 2, max = 262_144))]
    pub arguments_json: String,
    pub output_format: OutputFormat,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ExecuteMessage {
    pub kind: OutputKind,
    pub content_type: ContentType,
    #[schemars(length(max = 1_048_576))]
    pub content: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    Stdout,
    Stderr,
    Progress,
    Result,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    Text,
    Json,
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
    id = "lenso.terminal.command",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait TerminalCommand {
    async fn catalog(
        &self,
        context: lenso::Ctx<'_>,
        request: CatalogRequest,
    ) -> Result<CatalogResponse, CatalogError>;

    async fn execute(
        &self,
        context: lenso::Ctx<'_>,
        request: ExecuteOpen,
    ) -> lenso::Stream<ExecuteMessage, ExecuteError>;
}

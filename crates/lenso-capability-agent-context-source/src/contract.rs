//! Authoritative source for the Agent Context Source Capability.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SnapshotRequest {}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SnapshotResponse {
    #[schemars(length(max = 256))]
    pub prompts: Vec<PromptDefinition>,
    #[schemars(length(max = 1_024))]
    pub resources: Vec<ResourceDefinition>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PromptDefinition {
    #[schemars(length(min = 1, max = 128))]
    pub source: String,
    #[schemars(length(min = 1, max = 256))]
    pub name: String,
    #[schemars(length(max = 1_024))]
    pub description: String,
    #[schemars(length(max = 65_536))]
    pub arguments_schema_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ResourceDefinition {
    #[schemars(length(min = 1, max = 128))]
    pub source: String,
    #[schemars(length(min = 1, max = 4_096))]
    pub uri: String,
    #[schemars(length(min = 1, max = 256))]
    pub name: String,
    #[schemars(length(max = 1_024))]
    pub description: String,
    #[schemars(length(max = 256))]
    pub mime_type: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct RenderPromptRequest {
    #[schemars(length(min = 1, max = 128))]
    pub source: String,
    #[schemars(length(min = 1, max = 256))]
    pub name: String,
    #[schemars(length(max = 65_536))]
    pub arguments_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct RenderPromptResponse {
    #[schemars(length(max = 1_024))]
    pub description: String,
    #[schemars(length(min = 1, max = 64))]
    pub messages: Vec<ContextMessage>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ContextMessage {
    pub role: ContextRole,
    #[schemars(length(min = 1, max = 1_048_576))]
    pub text: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRole {
    User,
    Assistant,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ReadResourceRequest {
    #[schemars(length(min = 1, max = 128))]
    pub source: String,
    #[schemars(length(min = 1, max = 4_096))]
    pub uri: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ReadResourceResponse {
    #[schemars(length(min = 1, max = 64))]
    pub contents: Vec<ResourceContent>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ResourceContent {
    #[schemars(length(min = 1, max = 4_096))]
    pub uri: String,
    #[schemars(length(max = 256))]
    pub mime_type: String,
    #[schemars(length(max = 1_048_576))]
    pub text: String,
}

#[derive(lenso::DomainError)]
pub enum ContextSourceError {
    InvalidRequest,
    NotFound,
    UnsupportedContent,
    UpstreamFailed,
}

#[lenso::capability(
    id = "lenso.agent.context-source",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait ContextSource {
    async fn snapshot(
        &self,
        context: lenso::Ctx<'_>,
        request: SnapshotRequest,
    ) -> Result<SnapshotResponse, ContextSourceError>;

    async fn render_prompt(
        &self,
        context: lenso::Ctx<'_>,
        request: RenderPromptRequest,
    ) -> Result<RenderPromptResponse, ContextSourceError>;

    async fn read_resource(
        &self,
        context: lenso::Ctx<'_>,
        request: ReadResourceRequest,
    ) -> Result<ReadResourceResponse, ContextSourceError>;
}

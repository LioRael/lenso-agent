//! Authoritative source for the Agent Session Presentation Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ProjectRequest {
    #[schemars(length(min = 1, max = 128))]
    pub session_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub turn_id: String,
    #[schemars(length(min = 1, max = 262_144))]
    pub user_input: String,
    #[schemars(length(min = 1, max = 262_144))]
    pub assistant_output: String,
    #[schemars(length(min = 1, max = 256))]
    pub current_title: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ProjectResponse {
    #[schemars(length(min = 1, max = 256))]
    pub title: String,
    #[schemars(length(min = 1, max = 1_024))]
    pub latest_preview: String,
}

#[derive(lenso::DomainError)]
pub enum ProjectError {
    InvalidTurn,
    ContentTooLarge,
    ProjectionFailed,
}

#[lenso::capability(
    id = "lenso.agent.session-presentation",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait SessionPresentation {
    async fn project(
        &self,
        context: lenso::Ctx<'_>,
        request: ProjectRequest,
    ) -> Result<ProjectResponse, ProjectError>;
}

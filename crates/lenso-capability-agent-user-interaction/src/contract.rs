//! Authoritative source for the Agent User Interaction Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AskRequest {
    #[schemars(length(min = 1, max = 128))]
    pub interaction_id: String,
    #[schemars(length(min = 1, max = 4_096))]
    pub prompt: String,
    #[schemars(length(max = 16))]
    pub options: Vec<String>,
    pub allow_freeform: bool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AskResponse {
    #[schemars(length(min = 1, max = 4_096))]
    pub answer: String,
}

#[derive(lenso::DomainError)]
pub enum AskError {
    Unavailable,
    InvalidRequest,
    TooManyPending,
    Timeout,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PendingRequest {}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PendingResponse {
    #[schemars(length(max = 16))]
    pub interactions: Vec<PendingInteraction>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PendingInteraction {
    #[schemars(length(min = 1, max = 128))]
    pub interaction_id: String,
    #[schemars(length(min = 1, max = 4_096))]
    pub prompt: String,
    #[schemars(length(max = 16))]
    pub options: Vec<String>,
    pub allow_freeform: bool,
}

#[derive(lenso::DomainError)]
pub enum PendingError {
    Unavailable,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AnswerRequest {
    #[schemars(length(min = 1, max = 128))]
    pub interaction_id: String,
    #[schemars(length(min = 1, max = 4_096))]
    pub answer: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AnswerResponse {}

#[derive(lenso::DomainError)]
pub enum AnswerError {
    NotFound,
    InvalidAnswer,
}

#[lenso::capability(
    id = "lenso.agent.user-interaction",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait UserInteraction {
    async fn ask(
        &self,
        context: lenso::Ctx<'_>,
        request: AskRequest,
    ) -> Result<AskResponse, AskError>;

    async fn pending(
        &self,
        context: lenso::Ctx<'_>,
        request: PendingRequest,
    ) -> Result<PendingResponse, PendingError>;

    async fn answer(
        &self,
        context: lenso::Ctx<'_>,
        request: AnswerRequest,
    ) -> Result<AnswerResponse, AnswerError>;
}

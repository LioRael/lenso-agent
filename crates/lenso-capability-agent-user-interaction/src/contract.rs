//! Authoritative source for the Agent User Interaction Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct InteractionOption {
    #[schemars(length(min = 1, max = 128))]
    pub option_id: String,
    #[schemars(length(min = 1, max = 256))]
    pub label: String,
    #[schemars(length(max = 1_024))]
    pub description: String,
    pub preview: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct InteractionQuestion {
    #[schemars(length(min = 1, max = 128))]
    pub question_id: String,
    #[schemars(length(min = 1, max = 64))]
    pub header: String,
    #[schemars(length(min = 1, max = 4_096))]
    pub prompt: String,
    #[schemars(length(max = 16))]
    pub options: Vec<InteractionOption>,
    pub multi_select: bool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct InteractionAnswer {
    #[schemars(length(min = 1, max = 128))]
    pub question_id: String,
    #[schemars(length(max = 16))]
    pub selected_option_ids: Vec<String>,
    pub other: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AskRequest {
    #[schemars(length(min = 1, max = 128))]
    pub interaction_id: String,
    #[schemars(length(min = 1, max = 8))]
    pub questions: Vec<InteractionQuestion>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AskResponse {
    #[schemars(length(min = 1, max = 8))]
    pub answers: Vec<InteractionAnswer>,
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
    #[schemars(length(min = 1, max = 8))]
    pub questions: Vec<InteractionQuestion>,
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
    #[schemars(length(min = 1, max = 8))]
    pub answers: Vec<InteractionAnswer>,
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
    major = 2,
    version = "2.0.0",
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

//! Authoritative source for the turn-scoped Agent Model Selection Capability.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SelectRequest {
    #[schemars(length(min = 1, max = 128))]
    pub policy: String,
    #[schemars(length(min = 1, max = 128))]
    pub selection_id: String,
    #[schemars(length(min = 1, max = 262_144))]
    pub input: String,
    #[schemars(length(min = 1, max = 16))]
    pub candidates: Vec<SelectCandidate>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SelectCandidate {
    #[schemars(length(min = 1, max = 256))]
    pub model: String,
    pub selected_by_default: bool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SelectResponse {
    #[schemars(length(min = 1, max = 256))]
    pub model: String,
    #[schemars(length(min = 1, max = 64))]
    pub strategy: String,
    #[schemars(length(min = 1, max = 128))]
    pub reason_code: String,
}

#[derive(lenso::DomainError)]
pub enum SelectError {
    UnknownPolicy,
    NoCandidate,
    SelectionFailed,
}

#[lenso::capability(
    id = "lenso.agent.model-selection",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait ModelSelection {
    async fn select(
        &self,
        context: lenso::Ctx<'_>,
        request: SelectRequest,
    ) -> Result<SelectResponse, SelectError>;
}

//! Authoritative source for the active Agent Turn input Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SubmitRequest {
    #[schemars(length(min = 1, max = 128))]
    pub session_id: String,
    #[schemars(length(min = 1, max = 262_144))]
    pub input: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SubmitResponse {
    #[schemars(length(min = 1, max = 128))]
    pub session_id: String,
    #[schemars(extend("format" = "uint64"))]
    pub accepted_revision: String,
}

#[derive(lenso::DomainError)]
pub enum SubmitError {
    InvalidInput,
    TurnNotActive,
    InputClosed,
}

#[lenso::capability(
    id = "lenso.agent.turn-input",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait TurnInput {
    async fn submit(
        &self,
        context: lenso::Ctx<'_>,
        request: SubmitRequest,
    ) -> Result<SubmitResponse, SubmitError>;
}

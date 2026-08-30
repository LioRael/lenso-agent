//! Authoritative source for the Agent Session Control Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CompactSessionRequest {
    #[schemars(length(min = 1, max = 128))]
    pub session_id: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CompactSessionResponse {
    #[schemars(length(min = 1, max = 20), regex(pattern = r"^(0|[1-9][0-9]*)$"))]
    pub revision: String,
    #[schemars(length(min = 1, max = 20), regex(pattern = r"^(0|[1-9][0-9]*)$"))]
    pub compacted_through_revision: String,
    pub source_message_count: u32,
}

#[derive(lenso::DomainError)]
pub enum CompactSessionError {
    InvalidSession,
    EmptyHistory,
    ActiveTurn,
    ConcurrentSession,
}

#[lenso::capability(
    id = "lenso.agent.session-control",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait SessionControl {
    async fn compact_session(
        &self,
        context: lenso::Ctx<'_>,
        request: CompactSessionRequest,
    ) -> Result<CompactSessionResponse, CompactSessionError>;
}

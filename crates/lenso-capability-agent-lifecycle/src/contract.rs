//! Authoritative source for the Agent Lifecycle Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ObserveRequest {
    #[schemars(length(min = 1, max = 256))]
    pub event_id: String,
    pub kind: LifecycleEventKind,
    #[schemars(length(min = 1, max = 128))]
    pub session_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub turn_id: Option<String>,
    #[schemars(length(min = 1, max = 64))]
    pub occurred_at: String,
    #[schemars(length(min = 71, max = 71))]
    pub generation_spec_digest: String,
    #[schemars(length(min = 2, max = 262_144))]
    pub payload_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleEventKind {
    SessionStarted,
    SessionResumed,
    TurnStarted,
    TurnCompleted,
    TurnFailed,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ObserveResponse {}

#[derive(lenso::DomainError)]
pub enum ObserveError {
    ObserverRejected,
}

#[lenso::capability(
    id = "lenso.agent.lifecycle",
    major = 1,
    version = "1.1.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait Lifecycle {
    async fn observe(
        &self,
        context: lenso::Ctx<'_>,
        request: ObserveRequest,
    ) -> Result<ObserveResponse, ObserveError>;
}

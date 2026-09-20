//! Host-issued model selection data shared by a selection Policy and an Agent Loop.

use lenso::TypedExtension;
use lenso_capability_agent_model::ResolvedTurnProfile;

/// Host-issued selection intent narrowed to model profiles admitted by one Generation.
pub const TURN_MODEL_SELECTION_EXTENSION: &str = "lenso.agent.turn-model-selection@1";

/// One dynamic policy request plus the exact model profiles it may select.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct TurnModelSelection {
    pub policy: String,
    pub candidates: Vec<ResolvedTurnProfile>,
}

impl TypedExtension for TurnModelSelection {
    const KEY: &'static str = TURN_MODEL_SELECTION_EXTENSION;
}

/// Durable evidence explaining one dynamic model decision.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelectionEvidence {
    pub policy: String,
    pub strategy: String,
    pub reason_code: String,
}

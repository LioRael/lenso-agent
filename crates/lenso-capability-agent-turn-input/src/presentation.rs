//! Surface presentation input that does not alter the durable Turn request.

use lenso::TypedExtension;

/// Original surface text when a Host has added explicit context to model input.
///
/// This affects history presentation only; replay continues to use the full input.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct TurnInputPresentation {
    pub input: String,
}

impl TypedExtension for TurnInputPresentation {
    const KEY: &'static str = "lenso.agent.turn-input-presentation.v1";
}

//! Host-issued Agent context and durable Turn provenance shared by Agent implementations.

use lenso::TypedExtension;
use lenso_capability_agent_model::ResolvedTurnProfile;
use lenso_capability_agent_model_selection::ModelSelectionEvidence;
use lenso_capability_agent_tools::RunScope;

/// Host-issued Invocation Context key for the surface-neutral Agent dependency closure.
pub const AGENT_BEHAVIOR_PROVENANCE_EXTENSION: &str = "lenso.agent.behavior-provenance@1";

/// Authoring Profile selected by the Host for one immutable Turn lease.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionProfile {
    pub name: Option<String>,
}

impl TypedExtension for SessionProfile {
    const KEY: &'static str = "lenso.agent.session-profile@1";
}

/// Surface-neutral identity of the immutable Agent behavior selected for one Turn.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBehaviorProvenance {
    pub digest: String,
}

impl AgentBehaviorProvenance {
    /// Validates one content-addressed Agent behavior digest.
    pub fn new(digest: String) -> Result<Self, String> {
        if !canonical_sha256_digest(&digest) {
            return Err("Agent behavior digest is not canonical SHA-256".to_owned());
        }
        Ok(Self { digest })
    }
}

impl TypedExtension for AgentBehaviorProvenance {
    const KEY: &'static str = AGENT_BEHAVIOR_PROVENANCE_EXTENSION;
}

/// One validated Turn-to-Generation provenance reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnGenerationProvenance {
    /// Durable Session revision of the `turn_started` event.
    pub revision: u64,
    /// Stable Turn identity.
    pub turn_id: String,
    /// Exact content-addressed App Generation Spec digest.
    pub generation_spec_digest: String,
    /// Surface-neutral digest of the selected Agent dependency closure, when recorded.
    pub agent_behavior_digest: Option<String>,
    /// Exact Provider/model profile resolved for the Turn, when recorded.
    pub resolved_turn_profile: Option<ResolvedTurnProfile>,
    /// Dynamic selection decision that produced the resolved profile, when used.
    pub model_selection: Option<ModelSelectionEvidence>,
}

/// Interprets the stable portion of a durable `turn_started` payload.
///
/// The Loop remains responsible for producing its behavior. This parser belongs
/// to the public Agent contract owner so diagnostics do not need to depend on a
/// particular Loop implementation.
pub fn inspect_turn_generation_provenance(
    revision: u64,
    turn_id: Option<&str>,
    payload_json: &str,
) -> Result<TurnGenerationProvenance, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TurnStartedPayload {
        #[serde(default, rename = "attachments")]
        _attachments: Option<serde_json::Value>,
        #[serde(default, rename = "display_input")]
        _display_input: Option<String>,
        generation_spec_digest: String,
        #[serde(default)]
        agent_behavior_digest: Option<String>,
        input: String,
        #[serde(default)]
        run_scope: Option<RunScope>,
        #[serde(default)]
        resolved_turn_profile: Option<ResolvedTurnProfile>,
        #[serde(default)]
        model_selection: Option<ModelSelectionEvidence>,
    }

    let payload = serde_json::from_str::<TurnStartedPayload>(payload_json)
        .map_err(|error| format!("Turn provenance payload is invalid: {error}"))?;
    let _ = payload.input;
    let _ = payload.run_scope;
    if !canonical_sha256_digest(&payload.generation_spec_digest) {
        return Err("Turn Generation Spec digest is invalid".to_owned());
    }
    if payload
        .agent_behavior_digest
        .as_deref()
        .is_some_and(|digest| !canonical_sha256_digest(digest))
    {
        return Err("Turn Agent behavior digest is invalid".to_owned());
    }
    Ok(TurnGenerationProvenance {
        revision,
        turn_id: turn_id
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "Turn provenance has no Turn ID".to_owned())?
            .to_owned(),
        generation_spec_digest: payload.generation_spec_digest,
        agent_behavior_digest: payload.agent_behavior_digest,
        resolved_turn_profile: payload.resolved_turn_profile,
        model_selection: payload.model_selection,
    })
}

fn canonical_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[cfg(test)]
mod tests {
    use super::{AgentBehaviorProvenance, inspect_turn_generation_provenance};

    #[test]
    fn rejects_noncanonical_behavior_digest() {
        assert!(AgentBehaviorProvenance::new("invalid".to_owned()).is_err());
    }

    #[test]
    fn inspects_a_minimal_stable_turn_record() {
        let payload = r#"{
            "generation_spec_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "input":"hello"
        }"#;
        let provenance = inspect_turn_generation_provenance(4, Some("turn-1"), payload).unwrap();
        assert_eq!(provenance.revision, 4);
        assert_eq!(provenance.turn_id, "turn-1");
    }
}

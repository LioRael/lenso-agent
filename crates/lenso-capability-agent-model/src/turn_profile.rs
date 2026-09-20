//! Host-issued model facts shared by Agent surfaces and Loop implementations.

use lenso::TypedExtension;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Host-issued Invocation Context key for the exact Provider/model profile of one Turn.
pub const RESOLVED_TURN_PROFILE_EXTENSION: &str = "lenso.agent.resolved-turn-profile@1";

/// Model limits known to the active Host. None is unknown, not unlimited.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLimits {
    pub context_window_tokens: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

/// Input forms accepted by one Provider/model path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelInputModality {
    Text,
    Image,
    Audio,
}

/// One Provider-authored option for a portable Model control.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelControlOption {
    pub id: String,
    pub name: String,
    pub description: String,
}

/// Whether and how one model accepts a reasoning selection.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "kind")]
pub enum ModelReasoningControl {
    Unknown,
    Unsupported,
    Selectable {
        efforts: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<ModelControlOption>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
    },
    Toggle {
        default_enabled: bool,
        options: Vec<ModelControlOption>,
    },
    BudgetTokens {
        minimum: u64,
        maximum: u64,
        default: u64,
    },
}

/// Whether and how one model accepts a provider service/speed tier.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "kind")]
pub enum ModelServiceTierControl {
    Unknown,
    Unsupported,
    Selectable { tiers: Vec<String> },
}

/// Model features implemented by the exact Provider/model path.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCapabilities {
    pub input_modalities: Vec<ModelInputModality>,
    pub text_output: bool,
    pub tool_calls: bool,
    pub parallel_tool_calls: bool,
    pub reasoning: ModelReasoningControl,
    pub service_tiers: ModelServiceTierControl,
}

/// Versioned Provider wire identity used for one model path.
///
/// The established values retain their wire representation. Other values are
/// accepted as a bounded opaque identity so a Provider does not need to
/// masquerade as an official protocol family.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelWireProtocol {
    Fixture,
    OpenaiResponses,
    OpenaiChatCompletions,
    Other(String),
}

impl ModelWireProtocol {
    /// Parses either a built-in wire identity or a bounded Provider-owned
    /// identity.
    pub fn from_id(id: impl Into<String>) -> Result<Self, String> {
        let id = id.into();
        match id.as_str() {
            "fixture" => Ok(Self::Fixture),
            "openai_responses" => Ok(Self::OpenaiResponses),
            "openai_chat_completions" => Ok(Self::OpenaiChatCompletions),
            _ => Self::other(id),
        }
    }

    /// Creates a non-built-in protocol identity.
    pub fn other(id: impl Into<String>) -> Result<Self, String> {
        let id = id.into();
        if id.is_empty()
            || id.len() > 128
            || !id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'@' | b'/')
            })
        {
            return Err("Model wire protocol identity is invalid".to_owned());
        }
        if matches!(
            id.as_str(),
            "fixture" | "openai_responses" | "openai_chat_completions"
        ) {
            return Err("built-in Model wire protocol must use its named variant".to_owned());
        }
        Ok(Self::Other(id))
    }

    /// Returns the stable protocol identity stored in a resolved Turn profile.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Fixture => "fixture",
            Self::OpenaiResponses => "openai_responses",
            Self::OpenaiChatCompletions => "openai_chat_completions",
            Self::Other(id) => id,
        }
    }
}

impl Serialize for ModelWireProtocol {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ModelWireProtocol {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let id = String::deserialize(deserializer)?;
        Self::from_id(id).map_err(serde::de::Error::custom)
    }
}

/// How the selected Provider acquired and validated one frozen model catalog.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCatalogProvenance {
    pub source: ModelCatalogSource,
    pub freshness: ModelCatalogFreshness,
    pub fetched_at_unix_seconds: Option<u64>,
    pub validated_at_unix_seconds: Option<u64>,
    pub revision: Option<String>,
    pub max_stale_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCatalogSource {
    Live,
    Cache,
    Configured,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCatalogFreshness {
    Fresh,
    Revalidated,
    Stale,
}

/// Exact inference profile resolved from one active Generation.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedTurnProfile {
    pub catalog_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_provenance: Option<ModelCatalogProvenance>,
    pub provider_id: String,
    pub provider_instance: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_budget_tokens: Option<u64>,
    pub service_tier: Option<String>,
    pub limits: ModelLimits,
    pub capabilities: ModelCapabilities,
    pub wire_protocol: ModelWireProtocol,
    pub compaction_compatibility: String,
}

impl TypedExtension for ResolvedTurnProfile {
    const KEY: &'static str = RESOLVED_TURN_PROFILE_EXTENSION;
}

#[cfg(test)]
mod tests {
    use super::ModelWireProtocol;

    #[test]
    fn retains_built_in_wire_values_and_accepts_versioned_external_identity() {
        let fixture = serde_json::to_string(&ModelWireProtocol::Fixture).unwrap();
        assert_eq!(fixture, "\"fixture\"");
        let custom: ModelWireProtocol =
            serde_json::from_str("\"example.vendor.model-wire@1\"").unwrap();
        assert_eq!(custom.as_str(), "example.vendor.model-wire@1");
        assert_eq!(
            serde_json::to_string(&custom).unwrap(),
            "\"example.vendor.model-wire@1\""
        );
    }
}

//! Authoritative source for the Agent Plugin Selection Authority Capability contract.

use lenso_contract_authoring as lenso;

#[derive(Clone, lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AuthoritySource {
    #[schemars(length(min = 1, max = 64))]
    pub kind: String,
    #[schemars(length(min = 1, max = 256))]
    pub reference: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SetEnabledRequest {
    pub enabled: bool,
    #[schemars(length(min = 71, max = 71))]
    pub expected_revision: String,
    #[schemars(length(min = 1, max = 128))]
    pub instance: String,
    #[schemars(length(min = 1, max = 128))]
    pub plugin_id: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SetEnabledResponse {
    pub authority: AuthoritySource,
    #[schemars(length(min = 71, max = 71))]
    pub base_revision: String,
    pub enabled: bool,
    #[schemars(length(min = 1, max = 128))]
    pub instance: String,
    #[schemars(length(min = 1, max = 128))]
    pub plugin_id: String,
    #[schemars(length(min = 71, max = 71))]
    pub revision: String,
    #[schemars(length(min = 1, max = 128))]
    pub schema: String,
}

#[derive(lenso::DomainError)]
pub enum PluginSelectionError {
    InvalidRequest,
    NotFound,
    Conflict,
    NotDisableable,
    AlreadySelected,
    Unsupported,
}

#[lenso::capability(
    id = "lenso.agent.plugin-selection-authority",
    major = 1,
    version = "1.0.0",
    portable = false,
    cross_lane_transfer = false
)]
pub trait PluginSelectionAuthority {
    async fn set_enabled(
        &self,
        context: lenso::Ctx<'_>,
        request: SetEnabledRequest,
    ) -> Result<SetEnabledResponse, PluginSelectionError>;
}

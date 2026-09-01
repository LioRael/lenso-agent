//! Authoritative source for the Agent Plugin Configuration Authority Capability contract.

use lenso_contract_authoring as lenso;

#[derive(Clone, lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AuthoritySource {
    #[schemars(length(min = 1, max = 64))]
    pub kind: String,
    #[schemars(length(min = 1, max = 256))]
    pub reference: String,
}

#[derive(Clone, lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PluginInstanceInspection {
    pub disableable: bool,
    pub has_root_difference: bool,
    #[schemars(length(min = 1, max = 128))]
    pub instance_key: String,
    #[schemars(length(min = 1, max = 32))]
    pub origin: String,
    #[schemars(range(min = 0, max = 262_144))]
    pub root_configuration_bytes: u32,
    pub root_configuration_present: bool,
    #[schemars(length(min = 1, max = 32))]
    pub selection: String,
    #[schemars(length(min = 71, max = 71))]
    pub source_digest: String,
}

#[derive(Clone, lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PluginInspection {
    #[schemars(length(max = 4_096))]
    pub instances: Vec<PluginInstanceInspection>,
    #[schemars(length(min = 1, max = 128))]
    pub package_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub package_revision: String,
    #[schemars(length(min = 1, max = 32))]
    pub source: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct InspectRequest {}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct InspectResponse {
    pub authority: AuthoritySource,
    #[schemars(range(min = 0, max = 65_536))]
    pub binding_count: u32,
    #[schemars(range(min = 0, max = 65_536))]
    pub enabled_instance_count: u32,
    #[schemars(length(max = 1_024))]
    pub plugins: Vec<PluginInspection>,
    #[schemars(length(min = 71, max = 71))]
    pub revision: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ProposeRequest {
    #[schemars(length(min = 1, max = 7_168))]
    pub configuration_toml: String,
    #[schemars(length(min = 71, max = 71))]
    pub expected_revision: String,
    #[schemars(length(min = 1, max = 128))]
    pub instance: String,
    #[schemars(length(min = 1, max = 128))]
    pub plugin_id: String,
}

#[derive(Clone, lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ProposalDiagnostic {
    #[schemars(length(min = 1, max = 128))]
    pub code: String,
    #[schemars(length(max = 4_096))]
    pub detail: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ProposeResponse {
    pub authority: AuthoritySource,
    #[schemars(length(min = 1, max = 32))]
    pub application: String,
    #[schemars(length(min = 71, max = 71))]
    pub base_revision: String,
    #[schemars(length(min = 71, max = 71))]
    pub base_source_digest: String,
    #[schemars(length(min = 71, max = 71))]
    pub candidate_revision: String,
    #[schemars(length(max = 128))]
    pub diagnostics: Vec<ProposalDiagnostic>,
    #[schemars(length(min = 1, max = 128))]
    pub instance: String,
    #[schemars(length(min = 1, max = 128))]
    pub plugin_id: String,
    #[schemars(length(min = 71, max = 71))]
    pub proposal_digest: String,
    #[schemars(length(min = 1, max = 128))]
    pub schema: String,
    #[schemars(length(min = 1, max = 32))]
    pub status: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PublishRequest {
    #[schemars(length(min = 1, max = 7_168))]
    pub configuration_toml: String,
    #[schemars(length(min = 71, max = 71))]
    pub expected_revision: String,
    #[schemars(length(min = 1, max = 128))]
    pub instance: String,
    #[schemars(length(min = 1, max = 128))]
    pub plugin_id: String,
    #[schemars(length(min = 71, max = 71))]
    pub proposal_digest: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PublishResponse {
    pub authority: AuthoritySource,
    #[schemars(length(min = 71, max = 71))]
    pub base_revision: String,
    #[schemars(length(min = 71, max = 71))]
    pub base_source_digest: String,
    #[schemars(length(min = 71, max = 71))]
    pub proposal_digest: String,
    #[schemars(length(min = 71, max = 71))]
    pub revision: String,
    #[schemars(length(min = 1, max = 128))]
    pub schema: String,
}

#[derive(lenso::DomainError)]
pub enum PluginConfigurationError {
    InvalidRequest,
    NotFound,
    Conflict,
    ProposalMismatch,
    ProposalNotReady,
}

#[lenso::capability(
    id = "lenso.agent.plugin-configuration-authority",
    major = 1,
    version = "1.0.0",
    portable = false,
    cross_lane_transfer = false
)]
pub trait PluginConfigurationAuthority {
    async fn inspect(
        &self,
        context: lenso::Ctx<'_>,
        request: InspectRequest,
    ) -> Result<InspectResponse, PluginConfigurationError>;

    async fn propose(
        &self,
        context: lenso::Ctx<'_>,
        request: ProposeRequest,
    ) -> Result<ProposeResponse, PluginConfigurationError>;

    async fn publish(
        &self,
        context: lenso::Ctx<'_>,
        request: PublishRequest,
    ) -> Result<PublishResponse, PluginConfigurationError>;
}

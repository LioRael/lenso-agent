//! Authoritative source for the Console Agent Plugin Management Target Capability.

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
pub struct InspectRequest {
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct InspectResponse {
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
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
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
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
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
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
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
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
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
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

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct HistoryRequest {
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub instance: String,
    #[schemars(range(min = 1, max = 50))]
    pub limit: u32,
    #[schemars(length(min = 1, max = 128))]
    pub plugin_id: String,
}

#[derive(Clone, lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PublicationRecord {
    #[schemars(length(min = 71, max = 71))]
    pub base_revision: String,
    #[schemars(length(min = 71, max = 71))]
    pub base_source_digest: Option<String>,
    #[schemars(length(min = 71, max = 71))]
    pub proposal_digest: String,
    #[schemars(range(min = 0))]
    pub published_at_unix_ms: i64,
    #[schemars(length(min = 71, max = 71))]
    pub revision: String,
    #[schemars(length(min = 71, max = 71))]
    pub rollback_of_proposal_digest: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct HistoryResponse {
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
    pub authority: AuthoritySource,
    #[schemars(length(min = 1, max = 128))]
    pub instance: String,
    #[schemars(length(min = 1, max = 128))]
    pub plugin_id: String,
    #[schemars(length(max = 50))]
    pub publications: Vec<PublicationRecord>,
    #[schemars(length(min = 71, max = 71))]
    pub revision: String,
    #[schemars(length(min = 1, max = 128))]
    pub schema: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ProposeRollbackRequest {
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
    #[schemars(length(min = 71, max = 71))]
    pub expected_revision: String,
    #[schemars(length(min = 1, max = 128))]
    pub instance: String,
    #[schemars(length(min = 1, max = 128))]
    pub plugin_id: String,
    #[schemars(length(min = 71, max = 71))]
    pub publication_proposal_digest: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ProposeRollbackResponse {
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
    #[schemars(length(min = 1, max = 32))]
    pub application: String,
    pub authority: AuthoritySource,
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
    #[schemars(length(min = 71, max = 71))]
    pub rollback_of_proposal_digest: String,
    #[schemars(length(min = 1, max = 128))]
    pub schema: String,
    #[schemars(length(min = 1, max = 32))]
    pub status: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PublishRollbackRequest {
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
    #[schemars(length(min = 71, max = 71))]
    pub expected_revision: String,
    #[schemars(length(min = 1, max = 128))]
    pub instance: String,
    #[schemars(length(min = 1, max = 128))]
    pub plugin_id: String,
    #[schemars(length(min = 71, max = 71))]
    pub proposal_digest: String,
    #[schemars(length(min = 71, max = 71))]
    pub publication_proposal_digest: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PublishRollbackResponse {
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
    pub authority: AuthoritySource,
    #[schemars(length(min = 71, max = 71))]
    pub base_revision: String,
    #[schemars(length(min = 71, max = 71))]
    pub base_source_digest: String,
    #[schemars(length(min = 71, max = 71))]
    pub proposal_digest: String,
    #[schemars(length(min = 71, max = 71))]
    pub revision: String,
    #[schemars(length(min = 71, max = 71))]
    pub rollback_of_proposal_digest: String,
    #[schemars(length(min = 1, max = 128))]
    pub schema: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SetEnabledRequest {
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
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
    #[schemars(length(min = 1, max = 64))]
    pub agent_id: String,
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
pub enum PluginManagementTargetError {
    InvalidRequest,
    TargetNotFound,
    Unsupported,
    PluginNotFound,
    Conflict,
    ProposalMismatch,
    ProposalNotReady,
    NotDisableable,
    AlreadySelected,
    PublicationNotFound,
}

#[lenso::capability(
    id = "lenso.agent.plugin-management-target",
    major = 1,
    version = "1.1.0",
    portable = false,
    cross_lane_transfer = false
)]
pub trait PluginManagementTarget {
    async fn inspect(
        &self,
        context: lenso::Ctx<'_>,
        request: InspectRequest,
    ) -> Result<InspectResponse, PluginManagementTargetError>;

    async fn propose(
        &self,
        context: lenso::Ctx<'_>,
        request: ProposeRequest,
    ) -> Result<ProposeResponse, PluginManagementTargetError>;

    async fn publish(
        &self,
        context: lenso::Ctx<'_>,
        request: PublishRequest,
    ) -> Result<PublishResponse, PluginManagementTargetError>;

    async fn history(
        &self,
        context: lenso::Ctx<'_>,
        request: HistoryRequest,
    ) -> Result<HistoryResponse, PluginManagementTargetError>;

    async fn propose_rollback(
        &self,
        context: lenso::Ctx<'_>,
        request: ProposeRollbackRequest,
    ) -> Result<ProposeRollbackResponse, PluginManagementTargetError>;

    async fn publish_rollback(
        &self,
        context: lenso::Ctx<'_>,
        request: PublishRollbackRequest,
    ) -> Result<PublishRollbackResponse, PluginManagementTargetError>;

    async fn set_enabled(
        &self,
        context: lenso::Ctx<'_>,
        request: SetEnabledRequest,
    ) -> Result<SetEnabledResponse, PluginManagementTargetError>;
}

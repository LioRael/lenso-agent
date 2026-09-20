//! Authoritative source for Plan-bound dynamic resource authority.
//!
//! A Provider authenticates its subject from a Host-issued context assertion;
//! this wire contract deliberately does not trust a caller-supplied subject
//! string. The selected Provider returns a snapshot and revalidates it at the
//! moment a resource is actually used.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ResourceIdentity {
    #[schemars(length(min = 1, max = 64))]
    pub kind: String,
    #[schemars(length(min = 1, max = 128))]
    pub name: String,
    #[schemars(length(min = 1, max = 256))]
    pub provider_instance: String,
    /// Exact version or schema digest selected by the already immutable Plan.
    #[schemars(length(min = 1, max = 192))]
    pub revision: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SelectRequest {
    #[schemars(length(min = 1, max = 128))]
    pub selection_id: String,
    #[schemars(length(max = 256))]
    pub requested_resources: Vec<ResourceIdentity>,
    /// Bounded non-authoritative request metadata. The Provider uses a trusted
    /// Host identity assertion, not this JSON, to choose a tenant or subject.
    #[schemars(length(min = 2, max = 16_384))]
    pub context_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SelectResponse {
    #[schemars(length(min = 1, max = 192))]
    pub snapshot_id: String,
    #[schemars(length(min = 1, max = 192))]
    pub policy_revision: String,
    #[schemars(length(min = 1, max = 256))]
    pub authority_instance: String,
    #[schemars(length(max = 256))]
    pub granted_resources: Vec<ResourceIdentity>,
    /// Unix milliseconds as a decimal u64. A Provider may choose a short
    /// lifetime; it must still check revocation during `revalidate`.
    #[schemars(extend("format" = "uint64"))]
    pub expires_at_unix_ms: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct RevalidateRequest {
    #[schemars(length(min = 1, max = 192))]
    pub snapshot_id: String,
    #[schemars(length(min = 1, max = 192))]
    pub policy_revision: String,
    pub resource: ResourceIdentity,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct RevalidateResponse {
    pub decision: AuthorityDecision,
    #[schemars(length(max = 128))]
    pub reason_code: String,
    #[schemars(length(max = 4_096))]
    pub message: String,
    #[schemars(length(min = 1, max = 192))]
    pub observed_policy_revision: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityDecision {
    Allow,
    Deny,
}

#[derive(lenso::DomainError)]
pub enum DynamicAuthorityError {
    InvalidSelection,
    Unauthorized,
    SnapshotExpired,
    SnapshotUnknown,
    Revoked,
}

#[lenso::capability(
    id = "lenso.agent.dynamic-authority",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait DynamicAuthority {
    async fn select(
        &self,
        context: lenso::Ctx<'_>,
        request: SelectRequest,
    ) -> Result<SelectResponse, DynamicAuthorityError>;

    async fn revalidate(
        &self,
        context: lenso::Ctx<'_>,
        request: RevalidateRequest,
    ) -> Result<RevalidateResponse, DynamicAuthorityError>;
}

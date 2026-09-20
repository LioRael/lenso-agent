//! Authoritative source for owner-scoped Agent extension state.
//!
//! This is associated Plugin state, not a second Agent Session authority. A
//! selected state Provider derives the producing Plugin instance from the
//! invocation context; callers cannot nominate another owner in the request.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "wire fields explicitly distinguish associated identities from embedded state"
)]
pub struct ExtensionAssociation {
    #[schemars(length(min = 1, max = 128))]
    pub session_id: Option<String>,
    #[schemars(length(min = 1, max = 128))]
    pub run_id: Option<String>,
    #[schemars(length(min = 1, max = 128))]
    pub task_id: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SafePresentation {
    /// A trusted presentation type selected by an already installed package.
    /// It is descriptive data only; it is never a code-loading instruction.
    #[schemars(length(min = 1, max = 192))]
    pub kind: String,
    #[schemars(length(max = 512))]
    pub title: String,
    #[schemars(length(max = 4_096))]
    pub summary: String,
    /// A bounded JSON object for safe, text-only inspection metadata.
    #[schemars(length(min = 2, max = 16_384))]
    pub attributes_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AppendRequest {
    #[schemars(length(min = 1, max = 192))]
    pub namespace: String,
    #[schemars(length(min = 1, max = 64))]
    pub schema_version: String,
    #[schemars(length(min = 1, max = 192))]
    pub event_id: String,
    /// A decimal monotonic sequence chosen by the owning Plugin for this
    /// association. The Provider checks collisions and ordering.
    #[schemars(length(min = 1, max = 20))]
    pub sequence: String,
    #[schemars(length(min = 1, max = 192))]
    pub idempotency_key: String,
    pub association: ExtensionAssociation,
    /// Bounded JSON facts only. Credentials, private reasoning, and rendered
    /// HTML/JavaScript are outside this contract.
    #[schemars(length(min = 2, max = 65_536))]
    pub payload_json: String,
    pub presentation: SafePresentation,
    /// Unknown records with this flag block a recovery attempt rather than
    /// being silently skipped after a Plugin upgrade or removal.
    pub recovery_required: bool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ExtensionRecord {
    #[schemars(length(min = 1, max = 192))]
    pub namespace: String,
    #[schemars(length(min = 1, max = 64))]
    pub schema_version: String,
    #[schemars(length(min = 1, max = 192))]
    pub event_id: String,
    #[schemars(length(min = 1, max = 20))]
    pub sequence: String,
    #[schemars(length(min = 1, max = 192))]
    pub idempotency_key: String,
    /// Trusted producer identity derived by the selected Provider from the
    /// resolved caller, rather than copied from `AppendRequest`.
    #[schemars(length(min = 1, max = 256))]
    pub owner_instance: String,
    pub association: ExtensionAssociation,
    #[schemars(length(min = 2, max = 65_536))]
    pub payload_json: String,
    pub presentation: SafePresentation,
    pub recovery_required: bool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AppendResponse {
    pub record: ExtensionRecord,
    /// A matching owner/namespace/idempotency tuple returns the original
    /// record without creating another durable fact.
    pub duplicate: bool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ReadRequest {
    #[schemars(length(min = 1, max = 192))]
    pub namespace: String,
    pub association: ExtensionAssociation,
    #[schemars(length(min = 1, max = 20))]
    pub after_sequence: Option<String>,
    #[schemars(range(min = 1, max = 128))]
    pub limit: u32,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ReadResponse {
    #[schemars(length(max = 128))]
    pub records: Vec<ExtensionRecord>,
}

#[derive(lenso::DomainError)]
pub enum ExtensionStateError {
    InvalidRecord,
    OwnerUnavailable,
    SequenceConflict,
    IdempotencyConflict,
    Unavailable,
}

#[lenso::capability(
    id = "lenso.agent.extension-state",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait ExtensionState {
    async fn append(
        &self,
        context: lenso::Ctx<'_>,
        request: AppendRequest,
    ) -> Result<AppendResponse, ExtensionStateError>;

    async fn read(
        &self,
        context: lenso::Ctx<'_>,
        request: ReadRequest,
    ) -> Result<ReadResponse, ExtensionStateError>;
}

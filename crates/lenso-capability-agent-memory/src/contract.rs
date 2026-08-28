//! Authoritative source for the Agent Memory Capability contract.

use lenso_contract_authoring as lenso;

#[derive(Clone, lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct MemorySource {
    #[schemars(length(min = 1, max = 128))]
    pub session_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub turn_id: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ObserveRequest {
    pub source: MemorySource,
    #[schemars(length(min = 1, max = 262_144))]
    pub user_input: String,
    #[schemars(length(min = 1, max = 262_144))]
    pub assistant_output: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ObserveResponse {
    #[schemars(length(max = 64))]
    pub memory_ids: Vec<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct RecallRequest {
    #[schemars(length(min = 1, max = 128))]
    pub session_id: String,
    #[schemars(length(min = 1, max = 32_768))]
    pub query: String,
    #[schemars(range(min = 1, max = 64))]
    pub max_items: u32,
    #[schemars(range(min = 256, max = 262_144))]
    pub max_characters: u32,
}

#[derive(Clone, lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct MemoryItem {
    #[schemars(length(min = 1, max = 128))]
    pub memory_id: String,
    #[schemars(length(min = 1, max = 262_144))]
    pub content: String,
    pub source: MemorySource,
    #[schemars(range(min = 0, max = 1000))]
    pub confidence_milli: u32,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct RecallResponse {
    #[schemars(length(max = 64))]
    pub items: Vec<MemoryItem>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct RememberRequest {
    #[schemars(length(min = 1, max = 128))]
    pub session_id: String,
    #[schemars(length(min = 1, max = 262_144))]
    pub content: String,
    #[schemars(range(min = 0, max = 1000))]
    pub confidence_milli: u32,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct RememberResponse {
    #[schemars(length(min = 1, max = 128))]
    pub memory_id: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ForgetRequest {
    #[schemars(length(min = 1, max = 64))]
    pub memory_ids: Vec<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ForgetResponse {
    #[schemars(range(min = 0, max = 64))]
    pub forgotten: u32,
}

#[derive(lenso::DomainError)]
pub enum MemoryError {
    InvalidRequest,
    ContentTooLarge,
}

#[lenso::capability(
    id = "lenso.agent.memory",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait Memory {
    async fn observe(
        &self,
        context: lenso::Ctx<'_>,
        request: ObserveRequest,
    ) -> Result<ObserveResponse, MemoryError>;

    async fn recall(
        &self,
        context: lenso::Ctx<'_>,
        request: RecallRequest,
    ) -> Result<RecallResponse, MemoryError>;

    async fn remember(
        &self,
        context: lenso::Ctx<'_>,
        request: RememberRequest,
    ) -> Result<RememberResponse, MemoryError>;

    async fn forget(
        &self,
        context: lenso::Ctx<'_>,
        request: ForgetRequest,
    ) -> Result<ForgetResponse, MemoryError>;
}

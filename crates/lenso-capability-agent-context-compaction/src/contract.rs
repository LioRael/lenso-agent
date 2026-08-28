//! Authoritative source for the Agent Context Compaction Capability contract.

use lenso_contract_authoring as lenso;

#[derive(Clone, lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ContextMessage {
    pub role: ContextMessageRole,
    #[schemars(length(min = 1, max = 262_144))]
    pub content: String,
}

#[derive(Clone, lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMessageRole {
    User,
    Assistant,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CompactRequest {
    #[schemars(length(min = 1, max = 128))]
    pub session_id: String,
    #[schemars(length(min = 1, max = 262_144))]
    pub previous_summary: Option<String>,
    #[schemars(length(min = 1, max = 256))]
    pub messages: Vec<ContextMessage>,
    #[schemars(range(min = 256, max = 262_144))]
    pub target_summary_characters: u32,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CompactResponse {
    #[schemars(length(min = 1, max = 262_144))]
    pub summary: String,
    #[schemars(length(max = 128))]
    pub retained_messages: Vec<ContextMessage>,
}

#[derive(lenso::DomainError)]
pub enum CompactError {
    InvalidContext,
    ContextTooLarge,
    CompactionFailed,
}

#[lenso::capability(
    id = "lenso.agent.context-compaction",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait ContextCompaction {
    async fn compact(
        &self,
        context: lenso::Ctx<'_>,
        request: CompactRequest,
    ) -> Result<CompactResponse, CompactError>;
}

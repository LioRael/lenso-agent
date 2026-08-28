//! Authoritative source for the Agent TUI Suggestion Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SnapshotRequest {}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SnapshotResponse {
    #[schemars(length(max = 2_048))]
    pub suggestions: Vec<Suggestion>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct Suggestion {
    #[schemars(length(min = 1, max = 128))]
    pub id: String,
    pub kind: SuggestionKind,
    #[schemars(length(min = 1, max = 256))]
    pub label: String,
    #[schemars(length(min = 1, max = 1_024))]
    pub insert_text: String,
    #[schemars(length(max = 512))]
    pub description: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionKind {
    Command,
    File,
    Prompt,
    Resource,
    Skill,
}

#[derive(lenso::DomainError)]
pub enum SnapshotError {
    SnapshotInvalid,
}

#[lenso::capability(
    id = "lenso.agent.tui-suggestion",
    major = 1,
    version = "1.2.0",
    portable = false,
    cross_lane_transfer = false
)]
pub trait TuiSuggestion {
    async fn snapshot(
        &self,
        context: lenso::Ctx<'_>,
        request: SnapshotRequest,
    ) -> Result<SnapshotResponse, SnapshotError>;
}

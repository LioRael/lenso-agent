//! Authoritative source for the semantic read-only TUI Panel Capability.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SnapshotRequest {}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SnapshotResponse {
    #[schemars(length(max = 16))]
    pub panels: Vec<PanelItem>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PanelItem {
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[a-z0-9][a-z0-9._/-]*$")
    )]
    pub id: String,
    #[schemars(length(min = 1, max = 80))]
    pub title: String,
    #[schemars(length(max = 65_536))]
    pub body: String,
}

#[derive(lenso::DomainError)]
pub enum SnapshotError {
    SnapshotInvalid,
}

#[lenso::capability(
    id = "lenso.tui.panel",
    major = 1,
    version = "1.0.0",
    portable = false,
    cross_lane_transfer = false
)]
pub trait TuiPanel {
    async fn snapshot(
        &self,
        context: lenso::Ctx<'_>,
        request: SnapshotRequest,
    ) -> Result<SnapshotResponse, SnapshotError>;
}

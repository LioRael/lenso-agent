//! Authoritative source for the Agent Worktree Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AllocateRequest {
    #[schemars(length(min = 1, max = 64))]
    pub task_id: String,
    #[schemars(length(min = 1, max = 256))]
    pub agent: String,
    #[schemars(length(min = 1, max = 4_096))]
    pub source_workspace: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AllocateResponse {
    pub kind: WorkspaceAllocationKind,
    #[schemars(length(min = 1, max = 4_096))]
    pub workspace: String,
    #[schemars(length(min = 1, max = 255))]
    pub branch: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAllocationKind {
    Current,
    IsolatedWorktree,
}

#[derive(lenso::DomainError)]
pub enum AllocateError {
    InvalidRequest,
    SourceWorkspaceMismatch,
    CapacityExceeded,
    TaskAlreadyAllocated,
    GitOperationFailed,
}

#[lenso::capability(
    id = "lenso.agent.worktree",
    major = 1,
    version = "1.0.0",
    portable = false,
    cross_lane_transfer = false
)]
pub trait Worktree {
    async fn allocate(
        &self,
        context: lenso::Ctx<'_>,
        request: AllocateRequest,
    ) -> Result<AllocateResponse, AllocateError>;
}

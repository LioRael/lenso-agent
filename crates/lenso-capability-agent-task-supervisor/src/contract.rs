//! Authoritative source for the Agent Task Supervisor Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SnapshotRequest {}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SnapshotResponse {
    #[schemars(length(max = 64))]
    pub tasks: Vec<TaskSnapshot>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct TaskSnapshot {
    #[schemars(length(min = 1, max = 64))]
    pub task_id: String,
    pub owner: TaskOwner,
    #[schemars(length(min = 1, max = 256))]
    pub agent: String,
    pub status: TaskStatus,
    #[schemars(length(min = 1, max = 128))]
    pub child_session_id: Option<String>,
    #[schemars(length(min = 71, max = 71))]
    pub generation_spec_digest: String,
    #[schemars(length(min = 1, max = 4_096))]
    pub workspace: String,
    pub terminal_result: Option<TerminalResult>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
#[allow(clippy::struct_field_names)]
pub struct TaskOwner {
    #[schemars(length(min = 1, max = 128))]
    pub session_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub turn_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub tool_call_id: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    CancellationRequested,
    Completed,
    Failed,
    Cancelled,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct TerminalResult {
    #[schemars(length(max = 16_384))]
    pub content: String,
    pub content_truncated: bool,
    #[schemars(length(min = 1, max = 128))]
    pub reason_code: Option<String>,
}

#[derive(lenso::DomainError)]
pub enum SnapshotError {
    SnapshotInvalid,
}

#[lenso::capability(
    id = "lenso.agent.task-supervisor",
    major = 1,
    version = "1.0.0",
    portable = false,
    cross_lane_transfer = false
)]
pub trait TaskSupervisor {
    async fn snapshot(
        &self,
        context: lenso::Ctx<'_>,
        request: SnapshotRequest,
    ) -> Result<SnapshotResponse, SnapshotError>;
}

//! Authoritative source for durable Agent task lifecycle state.
//!
//! Providers persist serializable state machines and explicit steps. They never
//! persist a Rust Future, closure, UI connection, or an unobserved external
//! effect retry instruction.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct StartRequest {
    #[schemars(length(min = 1, max = 128))]
    pub task_id: String,
    #[schemars(length(min = 1, max = 192))]
    pub idempotency_key: String,
    #[schemars(length(min = 1, max = 192))]
    pub workflow_kind: String,
    #[schemars(length(min = 1, max = 64))]
    pub state_version: String,
    #[schemars(length(min = 2, max = 65_536))]
    pub state_json: String,
    #[schemars(length(min = 1, max = 128))]
    pub parent_task_id: Option<String>,
    #[schemars(extend("format" = "uint64"))]
    pub deadline_unix_ms: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct TaskSnapshot {
    #[schemars(length(min = 1, max = 128))]
    pub task_id: String,
    /// Derived by the selected Provider from the resolved caller identity.
    #[schemars(length(min = 1, max = 256))]
    pub owner_instance: String,
    #[schemars(length(min = 1, max = 192))]
    pub workflow_kind: String,
    #[schemars(length(min = 1, max = 64))]
    pub state_version: String,
    #[schemars(extend("format" = "uint64"))]
    pub revision: String,
    pub status: TaskStatus,
    #[schemars(length(min = 2, max = 65_536))]
    pub state_json: String,
    #[schemars(length(max = 128))]
    pub parent_task_id: Option<String>,
    #[schemars(extend("format" = "uint64"))]
    pub deadline_unix_ms: Option<String>,
    #[schemars(length(max = 128))]
    pub terminal_reason_code: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Ready,
    WaitingForSignal,
    Running,
    Completed,
    Cancelled,
    TimedOut,
    UncertainExternalEffect,
    UpgradeRequired,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct StartResponse {
    pub task: TaskSnapshot,
    pub duplicate: bool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ObserveRequest {
    #[schemars(length(min = 1, max = 128))]
    pub task_id: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ObserveResponse {
    pub task: TaskSnapshot,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SignalRequest {
    #[schemars(length(min = 1, max = 128))]
    pub task_id: String,
    #[schemars(length(min = 1, max = 192))]
    pub signal_id: String,
    /// The state revision the signal was approved for. A stale or mismatched
    /// signal must not resume a later state transition.
    #[schemars(extend("format" = "uint64"))]
    pub expected_revision: String,
    #[schemars(length(min = 1, max = 128))]
    pub kind: String,
    #[schemars(length(min = 2, max = 65_536))]
    pub payload_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct SignalResponse {
    pub task: TaskSnapshot,
    pub duplicate: bool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CancelRequest {
    #[schemars(length(min = 1, max = 128))]
    pub task_id: String,
    #[schemars(length(max = 128))]
    pub reason_code: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CancelResponse {
    pub task: TaskSnapshot,
    pub already_terminal: bool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct RecoverRequest {
    #[schemars(length(min = 1, max = 128))]
    pub task_id: String,
    /// Explicit serializable state versions understood by the new Plugin
    /// artifact. Unsupported durable state must be surfaced, not skipped.
    #[schemars(length(min = 1, max = 32))]
    pub supported_state_versions: Vec<String>,
    #[schemars(length(min = 1, max = 192))]
    pub recoverer_artifact: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct RecoverResponse {
    pub task: TaskSnapshot,
    pub action: RecoveryAction,
    #[schemars(length(max = 4_096))]
    pub message: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    Resume,
    Wait,
    ObserveOnly,
    BlockedUpgrade,
    BlockedUnknownRequiredState,
    BlockedUncertainExternalEffect,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct WaitOpen {
    #[schemars(length(min = 1, max = 128))]
    pub task_id: String,
    #[schemars(extend("format" = "uint64"))]
    pub after_revision: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct TaskUpdate {
    pub task: TaskSnapshot,
}

#[derive(lenso::DomainError)]
pub enum DurableTaskError {
    InvalidTask,
    NotFound,
    OwnerMismatch,
    IdempotencyConflict,
    StaleSignal,
    TaskCancelled,
    TaskTerminal,
    UpgradeRequired,
    UncertainExternalEffect,
    OutputLimitExceeded,
}

#[lenso::capability(
    id = "lenso.agent.durable-task",
    major = 1,
    version = "1.0.0",
    portable = false,
    cross_lane_transfer = false
)]
pub trait DurableTask {
    async fn start(
        &self,
        context: lenso::Ctx<'_>,
        request: StartRequest,
    ) -> Result<StartResponse, DurableTaskError>;

    async fn observe(
        &self,
        context: lenso::Ctx<'_>,
        request: ObserveRequest,
    ) -> Result<ObserveResponse, DurableTaskError>;

    async fn signal(
        &self,
        context: lenso::Ctx<'_>,
        request: SignalRequest,
    ) -> Result<SignalResponse, DurableTaskError>;

    async fn cancel(
        &self,
        context: lenso::Ctx<'_>,
        request: CancelRequest,
    ) -> Result<CancelResponse, DurableTaskError>;

    async fn recover(
        &self,
        context: lenso::Ctx<'_>,
        request: RecoverRequest,
    ) -> Result<RecoverResponse, DurableTaskError>;

    async fn wait(
        &self,
        context: lenso::Ctx<'_>,
        request: WaitOpen,
    ) -> lenso::Stream<TaskUpdate, DurableTaskError>;
}

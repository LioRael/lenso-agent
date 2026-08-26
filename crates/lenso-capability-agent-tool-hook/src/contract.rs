//! Authoritative source for the Agent Tool Hook Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct BeforeExecuteRequest {
    #[schemars(length(min = 1, max = 128))]
    pub execution_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub tool_name: String,
    #[schemars(length(min = 2, max = 262_144))]
    pub arguments_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct BeforeExecuteResponse {
    pub decision: HookDecision,
    #[schemars(length(max = 128))]
    pub reason_code: String,
    #[schemars(length(max = 4_096))]
    pub message: String,
    #[schemars(length(min = 2, max = 65_536))]
    pub context_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookDecision {
    Allow,
    Ask,
    Deny,
}

#[derive(lenso::DomainError)]
pub enum BeforeExecuteError {
    HookFailed,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AfterExecuteRequest {
    #[schemars(length(min = 1, max = 128))]
    pub execution_id: String,
    #[schemars(length(min = 1, max = 128))]
    pub tool_name: String,
    #[schemars(length(min = 2, max = 262_144))]
    pub arguments_json: String,
    #[schemars(length(min = 2, max = 65_536))]
    pub context_json: String,
    pub outcome: HookOutcome,
    #[schemars(length(max = 1_048_576))]
    pub content: String,
    #[schemars(length(min = 2, max = 65_536))]
    pub metadata_json: String,
    #[schemars(length(max = 128))]
    pub provider_code: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookOutcome {
    Success,
    DomainError,
    RuntimeFailure,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AfterExecuteResponse {}

#[derive(lenso::DomainError)]
pub enum AfterExecuteError {
    HookFailed,
}

#[lenso::capability(
    id = "lenso.agent.tool-hook",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait ToolHook {
    async fn before_execute(
        &self,
        context: lenso::Ctx<'_>,
        request: BeforeExecuteRequest,
    ) -> Result<BeforeExecuteResponse, BeforeExecuteError>;

    async fn after_execute(
        &self,
        context: lenso::Ctx<'_>,
        request: AfterExecuteRequest,
    ) -> Result<AfterExecuteResponse, AfterExecuteError>;
}

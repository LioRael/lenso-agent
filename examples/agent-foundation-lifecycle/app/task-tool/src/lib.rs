//! A single-writer fixture task, composed through the public Tool Provider API.
//!
//! The two "effects" are local durable receipts in the same atomic JSON commit.
//! There are no external effects and no claim of a general exactly-once protocol.
//! `read` is a synthetic README workflow, not workspace filesystem authority.

use std::{fs, io::Write, path::Path};

use lenso::prelude::*;
use lenso_capability_agent_tool_provider::{
    self as tools, CatalogError, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
};

#[derive(Clone, Debug, serde::Deserialize, lenso::PluginConfig)]
#[serde(deny_unknown_fields)]
struct TaskConfig {
    storage_path: String,
    pause_before_second_step: bool,
}

#[lenso::plugin]
#[derive(Clone, Debug)]
struct TaskTool {
    #[config]
    config: TaskConfig,
}

pub fn link() {
    link_plugin();
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskState {
    version: u32,
    task_id: String,
    status: String,
    attempts: u32,
    retryable_failures: u32,
    effects: Vec<String>,
}

impl Default for TaskState {
    fn default() -> Self {
        Self {
            version: 1,
            task_id: "fixture-readme-review".into(),
            status: "ready".into(),
            attempts: 0,
            retryable_failures: 0,
            effects: Vec::new(),
        }
    }
}

fn failure(reason: &str) -> PluginError<ExecuteError> {
    PluginError::domain(ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: reason.into(),
            message: reason.into(),
            details_json: "{}".try_into().unwrap(),
        },
    })
}

impl TaskTool {
    fn load(&self) -> Result<TaskState, PluginError<ExecuteError>> {
        let path = Path::new(&self.config.storage_path);
        if !path.is_absolute() {
            return Err(failure("fixture_store_requires_absolute_path"));
        }
        let state: TaskState = match fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| failure("fixture_store_invalid"))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => TaskState::default(),
            Err(_) => return Err(failure("fixture_store_unavailable")),
        };
        if state.version != 1
            || state.task_id != "fixture-readme-review"
            || state.effects.len() > 2
            || !state
                .effects
                .iter()
                .enumerate()
                .all(|(index, effect)| effect == &format!("readme-step-{}", index + 1))
        {
            return Err(failure("fixture_state_requires_explicit_recovery"));
        }
        Ok(state)
    }

    fn persist(&self, state: &TaskState) -> Result<(), PluginError<ExecuteError>> {
        let path = Path::new(&self.config.storage_path);
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(state).map_err(|_| failure("fixture_encode"))?;
        let mut file = fs::File::create(&temporary).map_err(|_| failure("fixture_store_write"))?;
        file.write_all(&bytes)
            .map_err(|_| failure("fixture_store_write"))?;
        file.sync_all().map_err(|_| failure("fixture_store_sync"))?;
        fs::rename(&temporary, path).map_err(|_| failure("fixture_store_commit"))?;
        fs::File::open(
            path.parent()
                .ok_or_else(|| failure("fixture_store_parent"))?,
        )
        .and_then(|directory| directory.sync_all())
        .map_err(|_| failure("fixture_store_sync"))?;
        Ok(())
    }
}

#[lenso::provides(tools::ToolProvider)]
impl TaskTool {
    async fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> PluginResult<CatalogResponse, CatalogError> {
        Ok(CatalogResponse {
            tools: vec![
                ToolDefinition {
                    name: "read".into(),
                    description: "Advance the synthetic README review task by one durable step."
                        .into(),
                    input_schema_json: serde_json::json!({
                        "type": "object", "additionalProperties": false,
                        "required": ["path"],
                        "properties": {"path": {"const": "README.md"}}
                    })
                    .to_string()
                    .try_into()
                    .unwrap(),
                    execution: ToolExecutionClass::Exclusive,
                },
                ToolDefinition {
                    name: "fixture_task_status".into(),
                    description: "Inspect the fixture task without advancing or retrying it."
                        .into(),
                    input_schema_json:
                        r#"{"type":"object","additionalProperties":false}"#.try_into().unwrap(),
                    execution: ToolExecutionClass::Exclusive,
                },
            ],
        })
    }

    async fn execute(
        &self,
        context: Ctx,
        request: ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        let arguments: serde_json::Value = serde_json::from_str(request.arguments_json.as_str())
            .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))?;
        let mut state = self.load()?;
        match request.name.as_str() {
            "fixture_task_status" if arguments == serde_json::json!({}) => {}
            "read" if arguments == serde_json::json!({"path": "README.md"}) => {
                state.attempts += 1;
                if state.retryable_failures == 0 {
                    // The failure is durably known to precede the effect. The
                    // scripted Model's next call is an explicit safe retry.
                    state.retryable_failures = 1;
                    state.status = "retryable_before_effect".into();
                    self.persist(&state)?;
                    return Err(failure("fixture_transient_before_effect"));
                }
                if self.config.pause_before_second_step && state.effects.len() == 1 {
                    state.status = "waiting_before_step_2".into();
                    self.persist(&state)?;
                    context.cancellation().cancelled().await;
                    state.status = "cancelled_before_step_2".into();
                    self.persist(&state)?;
                    return Err(PluginError::runtime(RuntimeFailure::Cancelled {
                        request_id: context.request_id(),
                    }));
                }
                if state.effects.len() < 2 {
                    state
                        .effects
                        .push(format!("readme-step-{}", state.effects.len() + 1));
                }
                state.status = if state.effects.len() == 2 {
                    "completed"
                } else {
                    "step_1_committed"
                }
                .into();
                self.persist(&state)?;
            }
            "read" | "fixture_task_status" => {
                return Err(PluginError::domain(ExecuteError::InvalidArguments));
            }
            _ => return Err(PluginError::domain(ExecuteError::NotFound)),
        }
        let content = serde_json::to_string(&state).map_err(|_| failure("fixture_encode"))?;
        Ok(ExecuteResponse {
            content_type: ContentType::Text,
            content,
            content_blocks: None,
            metadata_json: r#"{"task":"fixture-readme-review","effects":"local-receipts-only"}"#
                .try_into()
                .unwrap(),
        })
    }
}

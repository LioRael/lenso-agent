//! Bounded Tool projection over one explicitly composed child Agent.

use futures::future::ready;
use lenso::prelude::*;
use lenso_capability_agent::{
    self as agent_contract, AgentInvocationError, RunTurnError, RunTurnRequest, RunTurnResponseKind,
};
use lenso_capability_agent_tool_provider::{
    self as tool_provider_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
    ToolProviderProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure, StreamEvent};

/// Stable model-visible Tool name.
pub const DELEGATE_TOOL: &str = "delegate";
const RESULT_METADATA_SCHEMA: &str = "lenso.agent.subagent-result@1";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SubagentToolsConfig {
    max_output_bytes: usize,
    max_task_bytes: usize,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegateArguments {
    task: String,
}

fn validate_config(config: &SubagentToolsConfig) -> Result<(), RuntimeFailure> {
    if config.max_output_bytes == 0
        || config.max_output_bytes > 1_048_576
        || config.max_task_bytes == 0
        || config.max_task_bytes > 262_144
    {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "subagent Tool limits are invalid".to_owned(),
        });
    }
    Ok(())
}

#[lenso::plugin(
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct SubagentToolsPlugin {
    #[config]
    config: SubagentToolsConfig,
    agent: Port<agent_contract::AgentClient>,
}

#[lenso::provides(tool_provider_contract::ToolProvider)]
impl ToolProviderProvider for SubagentToolsPlugin {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_provider_contract::ToolProviderCatalog> {
        Box::pin(ready(Ok(Ok(CatalogResponse {
            tools: vec![ToolDefinition {
                name: DELEGATE_TOOL.to_owned(),
                description: "Delegate one bounded task to an independently composed child Agent. The child has its own durable Session and only the Capabilities selected for it by App Composition.".to_owned(),
                input_schema_json: serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "task": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": self.config.max_task_bytes
                        }
                    },
                    "required": ["task"]
                })
                .to_string()
                .try_into()
                .expect("subagent Tool schema must be valid JSON"),
                execution: ToolExecutionClass::Exclusive,
            }],
        }))))
    }

    fn execute(
        &self,
        context: InvocationContext,
        request: ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_provider_contract::ToolProviderExecute> {
        if request.name != DELEGATE_TOOL {
            return Box::pin(ready(Ok(Err(ExecuteError::NotFound))));
        }
        let Ok(arguments) =
            serde_json::from_str::<DelegateArguments>(request.arguments_json.as_str())
        else {
            return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
        };
        if arguments.task.trim().is_empty() || arguments.task.len() > self.config.max_task_bytes {
            return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
        }
        let agent = self.agent.clone();
        let max_output_bytes = self.config.max_output_bytes;
        let task_bytes = arguments.task.len();
        Box::pin(execute_delegation(
            agent,
            context,
            arguments.task,
            task_bytes,
            max_output_bytes,
        ))
    }
}

async fn execute_delegation(
    agent: Port<agent_contract::AgentClient>,
    context: InvocationContext,
    task: String,
    task_bytes: usize,
    max_output_bytes: usize,
) -> Result<Result<ExecuteResponse, ExecuteError>, RuntimeFailure> {
    let mut progress = ChildRunProgress::default();
    let stream = match agent
        .run_turn_with_context(
            context,
            RunTurnRequest {
                input: task,
                session_id: None,
            },
        )
        .await
    {
        Ok(stream) => stream,
        Err(AgentInvocationError::Domain(error)) => {
            return Ok(Err(map_agent_error(
                error,
                &progress,
                task_bytes,
                max_output_bytes,
            )));
        }
        Err(AgentInvocationError::Runtime(error)) => return Err(error),
    };
    stream.close_send().await?;
    loop {
        match stream.receive().await? {
            StreamEvent::Message(message) => {
                if let Err(error) = progress.observe_message(&message, task_bytes, max_output_bytes)
                {
                    return Ok(Err(error));
                }
            }
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => break,
            StreamEvent::Terminal(Err(error)) => {
                return Ok(Err(map_agent_error(
                    error,
                    &progress,
                    task_bytes,
                    max_output_bytes,
                )));
            }
        }
    }
    if progress.child_session_id.is_none() {
        return Ok(Err(execution_failed(
            "missing_child_session",
            "Child Agent completed without a durable Session identity",
            &progress.metadata("failed", task_bytes, max_output_bytes),
        )));
    }
    if progress.output_limit_exceeded {
        return Ok(Err(execution_failed(
            "child_output_limit_exceeded",
            "Child Agent output exceeded the delegated result limit",
            &progress.metadata("failed", task_bytes, max_output_bytes),
        )));
    }
    let metadata_json = progress
        .metadata("completed", task_bytes, max_output_bytes)
        .to_string()
        .try_into()
        .expect("subagent Tool metadata must be valid JSON");
    Ok(Ok(ExecuteResponse {
        content: progress.output,
        content_type: ContentType::Text,
        metadata_json,
    }))
}

#[derive(Default)]
struct ChildRunProgress {
    child_session_id: Option<String>,
    message_count: u64,
    observed_output_bytes: usize,
    output: String,
    output_limit_exceeded: bool,
    text_delta_count: u64,
    tool_call_count: u64,
}

impl ChildRunProgress {
    fn observe_message(
        &mut self,
        message: &agent_contract::RunTurnResponse,
        task_bytes: usize,
        output_limit_bytes: usize,
    ) -> Result<(), ExecuteError> {
        self.observe_session(
            message.session_id.as_deref(),
            task_bytes,
            output_limit_bytes,
        )?;
        self.message_count = self.message_count.saturating_add(1);
        if matches!(message.kind, Some(RunTurnResponseKind::ToolStarted)) {
            self.tool_call_count = self.tool_call_count.saturating_add(1);
        }
        if message.is_text_delta() {
            self.text_delta_count = self.text_delta_count.saturating_add(1);
            self.observed_output_bytes = self
                .observed_output_bytes
                .saturating_add(message.text.len());
            if self.observed_output_bytes > output_limit_bytes {
                self.output_limit_exceeded = true;
            } else if !self.output_limit_exceeded {
                self.output.push_str(&message.text);
            }
        }
        Ok(())
    }

    fn observe_session(
        &mut self,
        observed: Option<&str>,
        task_bytes: usize,
        output_limit_bytes: usize,
    ) -> Result<(), ExecuteError> {
        let Some(observed) = observed else {
            return Ok(());
        };
        match self.child_session_id.as_deref() {
            None => {
                self.child_session_id = Some(observed.to_owned());
                Ok(())
            }
            Some(expected) if expected == observed => Ok(()),
            Some(_) => Err(execution_failed(
                "inconsistent_child_session",
                "Child Agent emitted more than one Session identity",
                &self.metadata("failed", task_bytes, output_limit_bytes),
            )),
        }
    }

    fn metadata(
        &self,
        status: &str,
        task_bytes: usize,
        output_limit_bytes: usize,
    ) -> serde_json::Value {
        serde_json::json!({
            "schema": RESULT_METADATA_SCHEMA,
            "status": status,
            "context_mode": "fresh",
            "child_session_id": self.child_session_id,
            "task_bytes": task_bytes,
            "output_bytes": self.observed_output_bytes,
            "output_limit_bytes": output_limit_bytes,
            "message_count": self.message_count,
            "text_delta_count": self.text_delta_count,
            "tool_call_count": self.tool_call_count,
        })
    }
}

fn map_agent_error(
    error: RunTurnError,
    progress: &ChildRunProgress,
    task_bytes: usize,
    output_limit_bytes: usize,
) -> ExecuteError {
    let reason = match error {
        RunTurnError::ConcurrentTurn => "child_busy",
        RunTurnError::ContextLimitExceeded => "context_limit_exceeded",
        RunTurnError::InvalidSession => "invalid_child_session",
        RunTurnError::StepLimitExceeded => "step_limit_exceeded",
        RunTurnError::ToolCallLimitExceeded => "tool_call_limit_exceeded",
        RunTurnError::Unknown(unknown) => {
            return execution_failed(
                &unknown.code,
                "Child Agent returned an unknown error",
                &progress.metadata("failed", task_bytes, output_limit_bytes),
            );
        }
    };
    execution_failed(
        reason,
        "Child Agent rejected the delegated task",
        &progress.metadata("failed", task_bytes, output_limit_bytes),
    )
}

fn execution_failed(reason_code: &str, message: &str, details: &serde_json::Value) -> ExecuteError {
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: message.to_owned(),
            details_json: details
                .to_string()
                .try_into()
                .expect("subagent error details must be valid JSON"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_session_identity_is_stable() {
        let mut progress = ChildRunProgress::default();
        progress.observe_session(Some("child-1"), 12, 1024).unwrap();
        progress.observe_session(Some("child-1"), 12, 1024).unwrap();

        let error = progress
            .observe_session(Some("child-2"), 12, 1024)
            .unwrap_err();
        let ExecuteError::ExecutionFailed { payload } = error else {
            panic!("expected execution failure");
        };
        assert_eq!(payload.reason_code, "inconsistent_child_session");
        let details: serde_json::Value =
            serde_json::from_str(payload.details_json.as_str()).unwrap();
        assert_eq!(details["child_session_id"], "child-1");
        assert_eq!(details["status"], "failed");
    }

    #[test]
    fn result_metadata_is_versioned_and_bounded() {
        let progress = ChildRunProgress {
            child_session_id: Some("child-1".to_owned()),
            message_count: 5,
            observed_output_bytes: 4,
            output: "done".to_owned(),
            output_limit_exceeded: false,
            text_delta_count: 2,
            tool_call_count: 1,
        };

        let metadata = progress.metadata("completed", 12, 1024);
        assert_eq!(metadata["schema"], RESULT_METADATA_SCHEMA);
        assert_eq!(metadata["context_mode"], "fresh");
        assert_eq!(metadata["child_session_id"], "child-1");
        assert_eq!(metadata["output_bytes"], 4);
        assert_eq!(metadata["output_limit_bytes"], 1024);
        assert_eq!(metadata["tool_call_count"], 1);
    }

    #[test]
    fn configuration_limits_fail_closed() {
        assert!(
            validate_config(&SubagentToolsConfig {
                max_output_bytes: 1_048_576,
                max_task_bytes: 262_144,
            })
            .is_ok()
        );
        assert!(
            validate_config(&SubagentToolsConfig {
                max_output_bytes: 0,
                max_task_bytes: 262_144,
            })
            .is_err()
        );
        assert!(
            validate_config(&SubagentToolsConfig {
                max_output_bytes: 1_048_577,
                max_task_bytes: 262_145,
            })
            .is_err()
        );
    }
}

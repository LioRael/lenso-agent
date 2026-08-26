//! Bounded Tool projection over one explicitly composed child Agent.

use futures::future::ready;
use lenso::prelude::*;
use lenso_capability_agent::{
    self as agent_contract, AgentInvocationError, RunTurnError, RunTurnRequest,
};
use lenso_capability_agent_tool_provider::{
    self as tool_provider_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
    ToolProviderProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure, StreamEvent};

/// Stable model-visible Tool name.
pub const DELEGATE_TOOL: &str = "delegate";

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

#[lenso::module(
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct SubagentToolsModule {
    #[config]
    config: SubagentToolsConfig,
    agent: Port<agent_contract::AgentClient>,
}

#[lenso::provides(tool_provider_contract::ToolProvider)]
impl ToolProviderProvider for SubagentToolsModule {
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
        Box::pin(async move {
            let stream = match agent
                .run_turn_with_context(
                    context,
                    RunTurnRequest {
                        input: arguments.task,
                        session_id: None,
                    },
                )
                .await
            {
                Ok(stream) => stream,
                Err(AgentInvocationError::Domain(error)) => {
                    return Ok(Err(map_agent_error(error)));
                }
                Err(AgentInvocationError::Runtime(error)) => return Err(error),
            };
            stream.close_send().await?;
            let mut output = String::new();
            let mut child_session_id = None;
            loop {
                match stream.receive().await? {
                    StreamEvent::Message(message) => {
                        child_session_id = message.session_id.clone().or(child_session_id);
                        if message.is_text_delta() {
                            if output.len().saturating_add(message.text.len()) > max_output_bytes {
                                return Ok(Err(ExecuteError::OutputLimitExceeded));
                            }
                            output.push_str(&message.text);
                        }
                    }
                    StreamEvent::PeerHalfClosed => {}
                    StreamEvent::Terminal(Ok(())) => break,
                    StreamEvent::Terminal(Err(error)) => {
                        return Ok(Err(map_agent_error(error)));
                    }
                }
            }
            let Some(child_session_id) = child_session_id else {
                return Ok(Err(execution_failed(
                    "missing_child_session",
                    "Child Agent completed without a durable Session identity",
                )));
            };
            Ok(Ok(ExecuteResponse {
                content: output,
                content_type: ContentType::Text,
                metadata_json: serde_json::json!({
                    "child_session_id": child_session_id,
                })
                .to_string()
                .try_into()
                .expect("subagent Tool metadata must be valid JSON"),
            }))
        })
    }
}

fn map_agent_error(error: RunTurnError) -> ExecuteError {
    let reason = match error {
        RunTurnError::ConcurrentTurn => "child_busy",
        RunTurnError::ContextLimitExceeded => "context_limit_exceeded",
        RunTurnError::InvalidSession => "invalid_child_session",
        RunTurnError::StepLimitExceeded => "step_limit_exceeded",
        RunTurnError::ToolCallLimitExceeded => "tool_call_limit_exceeded",
        RunTurnError::Unknown(unknown) => {
            return execution_failed(&unknown.code, "Child Agent returned an unknown error");
        }
    };
    execution_failed(reason, "Child Agent rejected the delegated task")
}

fn execution_failed(reason_code: &str, message: &str) -> ExecuteError {
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: message.to_owned(),
            details_json: "{}"
                .to_owned()
                .try_into()
                .expect("static subagent error details must be valid JSON"),
        },
    }
}

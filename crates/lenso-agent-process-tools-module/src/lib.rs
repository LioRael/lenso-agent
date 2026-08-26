//! Tool projection for one explicitly bound process provider.

use std::{cell::RefCell, rc::Rc};

use lenso::prelude::*;
use lenso_capability_agent_process::{
    self as process_contract, CatalogRequest as ProcessCatalogRequest, ProcessRunInvocationError,
    RunError, RunRequest,
};
use lenso_capability_agent_tool_provider::{
    self as tool_provider_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
    ToolProviderProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};

/// Stable structured process Tool name.
pub const EXEC_TOOL: &str = "run_process";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessToolsConfig {
    default_timeout_ms: u64,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecArguments {
    program: String,
    #[serde(rename = "arguments")]
    args: Vec<String>,
    #[serde(default = "default_cwd")]
    cwd: String,
    timeout_ms: Option<u64>,
}

#[derive(Debug)]
struct ProcessToolsState {
    catalog: CatalogResponse,
}

fn validate_config(config: &ProcessToolsConfig) -> Result<(), RuntimeFailure> {
    if config.default_timeout_ms == 0 || config.default_timeout_ms > 3_600_000 {
        return Err(invalid_plan(
            "default_timeout_ms must be between 1 and 3600000",
        ));
    }
    Ok(())
}

#[lenso::module(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct ProcessToolsModule {
    #[config]
    config: ProcessToolsConfig,
    process: Port<process_contract::ProcessClient>,
    state: Rc<RefCell<Option<ProcessToolsState>>>,
}

#[lenso::provides(tool_provider_contract::ToolProvider)]
impl ToolProviderProvider for ProcessToolsModule {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<lenso_capability_agent_tool_provider::ToolProviderCatalog>
    {
        let result = self
            .state
            .borrow()
            .as_ref()
            .map(|state| state.catalog.clone())
            .ok_or(RuntimeFailure::Unavailable {
                capability: lenso_capability_agent_tool_provider::CAPABILITY_ID,
            });
        Box::pin(futures::future::ready(result.map(Ok)))
    }

    fn execute(
        &self,
        context: InvocationContext,
        request: ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<lenso_capability_agent_tool_provider::ToolProviderExecute>
    {
        if request.name != EXEC_TOOL {
            return Box::pin(futures::future::ready(Ok(Err(ExecuteError::NotFound))));
        }
        let Ok(arguments) = serde_json::from_str::<ExecArguments>(request.arguments_json.as_str())
        else {
            return Box::pin(futures::future::ready(Ok(Err(
                ExecuteError::InvalidArguments,
            ))));
        };
        let process = self.process.clone();
        let timeout_ms = arguments
            .timeout_ms
            .unwrap_or(self.config.default_timeout_ms);
        Box::pin(async move {
            match process
                .run_with_context(
                    context,
                    RunRequest {
                        program: arguments.program.clone(),
                        arguments: arguments.args,
                        cwd: arguments.cwd,
                        timeout_ms: timeout_ms.to_string(),
                    },
                )
                .await
            {
                Ok(response) => {
                    let output = format_process_output(
                        &response.exit_code,
                        &response.stdout,
                        &response.stderr,
                    );
                    if output.len() > 1_048_576 {
                        return Ok(Err(ExecuteError::OutputLimitExceeded));
                    }
                    Ok(Ok(ExecuteResponse {
                        content: output,
                        content_type: ContentType::Text,
                        metadata_json: serde_json::json!({
                            "program": arguments.program,
                            "exit_code": response.exit_code,
                            "duration_ms": response.duration_ms,
                        })
                        .to_string()
                        .try_into()
                        .expect("serde_json values must produce valid JSON"),
                    }))
                }
                Err(ProcessRunInvocationError::Domain(error)) => Ok(Err(map_process_error(error))),
                Err(ProcessRunInvocationError::Runtime(error)) => Err(error),
            }
        })
    }
}

impl Lifecycle for ProcessToolsModule {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        let process_catalog = self
            .process
            .catalog(ProcessCatalogRequest {})
            .await
            .map_err(|error| RuntimeFailure::ModuleFailure {
                detail: format!("process catalog is unavailable: {error:?}"),
            })?;
        let program_names = process_catalog
            .programs
            .into_iter()
            .map(|program| program.name)
            .collect::<Vec<_>>();
        if program_names.is_empty() {
            return Err(invalid_plan("process catalog cannot be empty"));
        }
        let input_schema_json = serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "program": { "type": "string", "enum": program_names },
                    "arguments": {
                        "type": "array",
                        "maxItems": 128,
                        "items": { "type": "string" }
                    },
                    "cwd": { "type": "string", "description": "Workspace-relative working directory; defaults to the workspace root." },
                    "timeout_ms": { "type": "integer", "minimum": 1 }
                },
                "required": ["program", "arguments"]
            })
            .to_string()
            .try_into()
            .expect("Tool input schema must be valid JSON");
        self.state.replace(Some(ProcessToolsState {
                catalog: CatalogResponse {
                    tools: vec![ToolDefinition {
                        name: EXEC_TOOL.to_owned(),
                        description: "Run one explicitly allowed executable without shell parsing. The command can still execute trusted project code and is not a sandbox.".to_owned(),
                        input_schema_json,
                        execution: ToolExecutionClass::Exclusive,
                    }],
                },
            }));
        Ok(())
    }

    #[allow(clippy::unused_async_trait_impl)]
    async fn deactivate(&self, _context: DeactivateContext) -> Result<(), RuntimeFailure> {
        self.state.replace(None);
        Ok(())
    }
}

fn default_cwd() -> String {
    ".".to_owned()
}

fn format_process_output(exit_code: &str, stdout: &str, stderr: &str) -> String {
    format!("exit_code: {exit_code}\nstdout:\n{stdout}\nstderr:\n{stderr}")
}

fn map_process_error(error: RunError) -> ExecuteError {
    let (reason_code, message) = match error {
        RunError::InvalidRequest => ("invalid_request", "Process request exceeded policy limits"),
        RunError::ProgramNotAllowed => ("program_not_allowed", "Program is not allowed"),
        RunError::InvalidWorkingDirectory => (
            "invalid_working_directory",
            "Working directory is outside the workspace or unavailable",
        ),
        RunError::Timeout => ("timeout", "Process exceeded its configured timeout"),
        RunError::OutputLimitExceeded => (
            "output_limit_exceeded",
            "Process exceeded its combined output limit",
        ),
        RunError::Terminated => ("terminated", "Process terminated without an exit code"),
        RunError::Unknown(unknown) => {
            return execution_failed(&unknown.code, "Process provider returned an unknown error");
        }
    };
    execution_failed(reason_code, message)
}

fn execution_failed(reason_code: &str, message: &str) -> ExecuteError {
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: message.to_owned(),
            details_json: "{}"
                .to_owned()
                .try_into()
                .expect("static Tool error details must be valid JSON"),
        },
    }
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

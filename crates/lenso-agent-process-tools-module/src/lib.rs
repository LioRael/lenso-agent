//! Tool projection for one explicitly bound process provider.

use std::{cell::RefCell, rc::Rc};

use lenso::prelude::*;
use lenso_capability_agent_process::{
    self as process_contract, CatalogRequest as ProcessCatalogRequest, ProcessRunInvocationError,
    ProcessRunStreamInvocationError, RunError, RunRequest, RunStreamError, RunStreamRequest,
    RunStreamResponseKind,
};
use lenso_capability_agent_tool_progress::{
    self as progress_contract, CatalogRequest as ProgressCatalogRequest,
    CatalogResponse as ProgressCatalogResponse, ContentType as ProgressContentType, ExecuteOpen,
    ExecuteProgress, ExecuteProgressError as ProgressExecuteError, ExecuteProgressKind,
    ExecutionFailedPayload as ProgressExecutionFailedPayload, ToolProgressDefinition,
};
use lenso_capability_agent_tool_provider::{
    self as tool_provider_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
};
use lenso_kernel::{InvocationContext, RuntimeFailure, StreamEvent};

/// Stable structured process Tool name.
pub const EXEC_TOOL: &str = "run_process";
const MAX_TOOL_OUTPUT_BYTES: usize = 1_048_576;

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
    #[tasks]
    tasks: ManagedTasks,
}

#[lenso::provides(tool_provider_contract::ToolProvider, progress_contract::ToolProgress)]
impl ProcessToolsModule {
    fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> impl std::future::Future<
        Output = ModuleResult<CatalogResponse, tool_provider_contract::CatalogError>,
    > {
        let result = self
            .state
            .borrow()
            .as_ref()
            .map(|state| state.catalog.clone())
            .ok_or(RuntimeFailure::Unavailable {
                capability: lenso_capability_agent_tool_provider::CAPABILITY_ID,
            });
        futures::future::ready(result.map_err(ModuleError::runtime))
    }

    async fn execute(
        &self,
        context: Ctx,
        request: ExecuteRequest,
    ) -> ModuleResult<ExecuteResponse, ExecuteError> {
        if request.name != EXEC_TOOL {
            return Err(ModuleError::domain(ExecuteError::NotFound));
        }
        let Ok(arguments) = serde_json::from_str::<ExecArguments>(request.arguments_json.as_str())
        else {
            return Err(ModuleError::domain(ExecuteError::InvalidArguments));
        };
        let process = self.process.clone();
        let timeout_ms = arguments
            .timeout_ms
            .unwrap_or(self.config.default_timeout_ms);
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
                let output =
                    format_process_output(&response.exit_code, &response.stdout, &response.stderr);
                if output.len() > MAX_TOOL_OUTPUT_BYTES {
                    return Err(ModuleError::domain(ExecuteError::OutputLimitExceeded));
                }
                Ok(ExecuteResponse {
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
                })
            }
            Err(ProcessRunInvocationError::Domain(error)) => {
                Err(ModuleError::domain(map_process_error(error)))
            }
            Err(ProcessRunInvocationError::Runtime(error)) => Err(ModuleError::runtime(error)),
        }
    }

    fn progress_catalog(
        &self,
        _context: Ctx,
        _request: ProgressCatalogRequest,
    ) -> impl std::future::Future<
        Output = ModuleResult<ProgressCatalogResponse, progress_contract::ProgressCatalogError>,
    > {
        let available = self.state.borrow().is_some();
        futures::future::ready(if available {
            Ok(ProgressCatalogResponse {
                tools: vec![ToolProgressDefinition {
                    name: EXEC_TOOL.to_owned(),
                }],
            })
        } else {
            Err(ModuleError::runtime(RuntimeFailure::Unavailable {
                capability: progress_contract::CAPABILITY_ID,
            }))
        })
    }

    async fn execute_progress(
        &self,
        context: Ctx,
        request: ExecuteOpen,
    ) -> ModuleResult<
        ProviderStream<progress_contract::ToolProgressExecuteProgress>,
        ProgressExecuteError,
    > {
        if request.name != EXEC_TOOL {
            return Err(ModuleError::domain(ProgressExecuteError::NotFound));
        }
        let Ok(arguments) = serde_json::from_str::<ExecArguments>(request.arguments_json.as_str())
        else {
            return Err(ModuleError::domain(ProgressExecuteError::InvalidArguments));
        };
        let process = self.process.clone();
        let timeout_ms = arguments
            .timeout_ms
            .unwrap_or(self.config.default_timeout_ms);
        let tasks = self.tasks.clone();
        let (stream, channel) =
            ProviderStream::<progress_contract::ToolProgressExecuteProgress>::channel(&context, 8);
        tasks
            .spawn_local(async move {
                produce_tool_progress(process, context, arguments, timeout_ms, channel).await;
            })
            .map_err(|error| {
                ModuleError::runtime(RuntimeFailure::ModuleFailure {
                    detail: format!("Tool progress task failed to start: {error:?}"),
                })
            })?;
        Ok(stream)
    }
}

async fn produce_tool_progress(
    process: Port<process_contract::ProcessClient>,
    context: InvocationContext,
    arguments: ExecArguments,
    timeout_ms: u64,
    mut channel: ProviderStreamChannel<progress_contract::ToolProgressExecuteProgress>,
) {
    let result = stream_process_tool(&process, context, &arguments, timeout_ms, &mut channel).await;
    let _ = channel.complete(result).await;
}

async fn stream_process_tool(
    process: &Port<process_contract::ProcessClient>,
    context: InvocationContext,
    arguments: &ExecArguments,
    timeout_ms: u64,
    channel: &mut ProviderStreamChannel<progress_contract::ToolProgressExecuteProgress>,
) -> ModuleResult<(), ProgressExecuteError> {
    let stream = process
        .run_stream_with_context(
            context,
            RunStreamRequest {
                program: arguments.program.clone(),
                arguments: arguments.args.clone(),
                cwd: arguments.cwd.clone(),
                timeout_ms: timeout_ms.to_string(),
            },
        )
        .await
        .map_err(map_process_stream_open_error)?;
    stream.close_send().await.map_err(ModuleError::runtime)?;
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut completed = false;
    loop {
        match stream.receive().await.map_err(ModuleError::runtime)? {
            StreamEvent::Message(_) if completed => {
                return Err(ModuleError::runtime(RuntimeFailure::ProtocolViolation {
                    capability: process_contract::CAPABILITY_ID,
                }));
            }
            StreamEvent::Message(message) => match message.kind {
                RunStreamResponseKind::Stdout => {
                    append_bounded_output(&mut stdout, &message.content, stderr.len())?;
                    channel
                        .send(ExecuteProgress {
                            kind: ExecuteProgressKind::Stdout,
                            content_type: ProgressContentType::Text,
                            content: message.content,
                            metadata_json: "{}".to_owned().try_into().expect("empty JSON is valid"),
                        })
                        .await
                        .map_err(ModuleError::runtime)?;
                }
                RunStreamResponseKind::Stderr => {
                    append_bounded_output(&mut stderr, &message.content, stdout.len())?;
                    channel
                        .send(ExecuteProgress {
                            kind: ExecuteProgressKind::Stderr,
                            content_type: ProgressContentType::Text,
                            content: message.content,
                            metadata_json: "{}".to_owned().try_into().expect("empty JSON is valid"),
                        })
                        .await
                        .map_err(ModuleError::runtime)?;
                }
                RunStreamResponseKind::Completed => {
                    completed = true;
                    let exit_code = message.exit_code.ok_or_else(|| {
                        ModuleError::runtime(RuntimeFailure::ProtocolViolation {
                            capability: process_contract::CAPABILITY_ID,
                        })
                    })?;
                    let duration_ms = message.duration_ms.ok_or_else(|| {
                        ModuleError::runtime(RuntimeFailure::ProtocolViolation {
                            capability: process_contract::CAPABILITY_ID,
                        })
                    })?;
                    let output = format_process_output(&exit_code, &stdout, &stderr);
                    if output.len() > MAX_TOOL_OUTPUT_BYTES {
                        return Err(ModuleError::domain(
                            ProgressExecuteError::OutputLimitExceeded,
                        ));
                    }
                    channel
                        .send(ExecuteProgress {
                            kind: ExecuteProgressKind::Completed,
                            content_type: ProgressContentType::Text,
                            content: output,
                            metadata_json: serde_json::json!({
                                "program": arguments.program,
                                "exit_code": exit_code,
                                "duration_ms": duration_ms,
                            })
                            .to_string()
                            .try_into()
                            .expect("serde_json values must produce valid JSON"),
                        })
                        .await
                        .map_err(ModuleError::runtime)?;
                }
            },
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) if completed => return Ok(()),
            StreamEvent::Terminal(Ok(())) => {
                return Err(ModuleError::runtime(RuntimeFailure::ProtocolViolation {
                    capability: process_contract::CAPABILITY_ID,
                }));
            }
            StreamEvent::Terminal(Err(error)) => {
                return Err(ModuleError::domain(map_process_stream_error(error)));
            }
        }
    }
}

fn append_bounded_output(
    target: &mut String,
    content: &str,
    other_length: usize,
) -> ModuleResult<(), ProgressExecuteError> {
    let within_limit = target
        .len()
        .checked_add(other_length)
        .and_then(|length| length.checked_add(content.len()))
        .is_some_and(|length| length <= MAX_TOOL_OUTPUT_BYTES);
    if !within_limit {
        return Err(ModuleError::domain(
            ProgressExecuteError::OutputLimitExceeded,
        ));
    }
    target.push_str(content);
    Ok(())
}

fn map_process_stream_open_error(
    error: ProcessRunStreamInvocationError,
) -> ModuleError<ProgressExecuteError, RuntimeFailure> {
    match error {
        ProcessRunStreamInvocationError::Domain(error) => {
            ModuleError::domain(map_process_stream_error(error))
        }
        ProcessRunStreamInvocationError::Runtime(error) => ModuleError::runtime(error),
    }
}

fn map_process_stream_error(error: RunStreamError) -> ProgressExecuteError {
    match error {
        RunStreamError::InvalidRequest => ProgressExecuteError::InvalidArguments,
        RunStreamError::ProgramNotAllowed | RunStreamError::InvalidWorkingDirectory => {
            ProgressExecuteError::PermissionDenied
        }
        RunStreamError::OutputLimitExceeded => ProgressExecuteError::OutputLimitExceeded,
        RunStreamError::Timeout => progress_execution_failed("timeout", "Process timed out"),
        RunStreamError::Terminated => {
            progress_execution_failed("terminated", "Process was terminated")
        }
        RunStreamError::Unknown(error) => progress_execution_failed(
            &error.code,
            "Process provider returned an unknown Domain Error",
        ),
    }
}

fn progress_execution_failed(code: &str, message: &str) -> ProgressExecuteError {
    ProgressExecuteError::ExecutionFailed {
        payload: ProgressExecutionFailedPayload {
            reason_code: code.to_owned(),
            message: message.to_owned(),
            details_json: "{}".to_owned().try_into().expect("empty JSON is valid"),
        },
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

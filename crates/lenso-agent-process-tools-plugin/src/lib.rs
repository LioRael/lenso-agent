//! Tool projection for one explicitly bound process provider.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use lenso::prelude::*;
use lenso_agent_native_support::{TOOL_TASK_OWNER_EXTENSION, ToolTaskOwner};
use lenso_capability_agent_process::{
    self as process_contract, CatalogRequest as ProcessCatalogRequest, ProcessRunInvocationError,
    ProcessRunStreamInvocationError, RunError, RunRequest, RunStreamError, RunStreamRequest,
    RunStreamResponseKind,
};
use lenso_capability_agent_session::{
    self as session_contract, AppendError as SessionAppendError, AppendSessionRequest,
    AppendSessionRequestEventsItem, AppendSessionRequestEventsItemKind,
    SessionAppendInvocationError,
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
use lenso_kernel::{CancellationToken, InvocationContext, RuntimeFailure, StreamEvent};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Stable structured process Tool name.
pub const EXEC_TOOL: &str = "run_process";
/// Stable Tool name for starting one generation-owned background process.
pub const START_TOOL: &str = "start_process";
/// Stable Tool name for reading one background process snapshot.
pub const READ_TOOL: &str = "read_process";
/// Stable Tool name for cancelling one background process.
pub const CANCEL_TOOL: &str = "cancel_process";
/// Stable Tool name for discovering retained background process handles.
pub const LIST_TOOL: &str = "list_processes";
const MAX_TOOL_OUTPUT_BYTES: usize = 1_048_576;
const TERMINAL_APPEND_ATTEMPTS: usize = 32;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessToolsConfig {
    default_timeout_ms: u64,
    #[serde(default = "default_max_background_processes")]
    max_background_processes: usize,
    #[serde(default = "default_max_background_log_bytes")]
    max_background_log_bytes: usize,
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessIdArguments {
    process_id: String,
    #[serde(default)]
    release: bool,
}

#[derive(Default, Debug)]
struct BackgroundProcessRegistry {
    processes: BTreeMap<String, Rc<RefCell<BackgroundProcess>>>,
}

#[derive(Debug)]
struct BackgroundProcess {
    process_id: String,
    owner: ToolTaskOwner,
    program: String,
    stdout: String,
    stderr: String,
    logs_truncated: bool,
    cancel_requested: bool,
    cancellation: CancellationToken,
    terminal: Option<BackgroundTerminal>,
    durable_terminal: bool,
    persistence_error: Option<String>,
}

#[derive(Clone, Debug)]
enum BackgroundTerminal {
    Completed {
        exit_code: String,
        duration_ms: String,
    },
    Domain {
        reason_code: String,
    },
    Runtime {
        reason_code: String,
    },
}

#[derive(Debug)]
struct ProcessToolsState {
    catalog: CatalogResponse,
}

fn validate_config(config: &ProcessToolsConfig) -> Result<(), RuntimeFailure> {
    if config.default_timeout_ms == 0
        || config.default_timeout_ms > 3_600_000
        || !(1..=64).contains(&config.max_background_processes)
        || !(1_024..=1_048_576).contains(&config.max_background_log_bytes)
    {
        return Err(invalid_plan(
            "process Tool timeout, background capacity, or background log limit is invalid",
        ));
    }
    Ok(())
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct ProcessToolsPlugin {
    #[config]
    config: ProcessToolsConfig,
    process: Port<process_contract::ProcessClient>,
    session: Port<session_contract::SessionClient>,
    state: Rc<RefCell<Option<ProcessToolsState>>>,
    registry: Rc<RefCell<BackgroundProcessRegistry>>,
    #[tasks]
    tasks: ManagedTasks,
}

#[lenso::provides(tool_provider_contract::ToolProvider, progress_contract::ToolProgress)]
impl ProcessToolsPlugin {
    fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> impl std::future::Future<
        Output = PluginResult<CatalogResponse, tool_provider_contract::CatalogError>,
    > {
        let result = self
            .state
            .borrow()
            .as_ref()
            .map(|state| state.catalog.clone())
            .ok_or(RuntimeFailure::Unavailable {
                capability: lenso_capability_agent_tool_provider::CAPABILITY_ID,
            });
        futures::future::ready(result.map_err(PluginError::runtime))
    }

    async fn execute(
        &self,
        context: Ctx,
        request: ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        match request.name.as_str() {
            START_TOOL => return self.execute_start(context, &request).await,
            READ_TOOL => return self.execute_read(&request),
            CANCEL_TOOL => return self.execute_cancel(&request),
            LIST_TOOL => return self.execute_list(&request),
            EXEC_TOOL => {}
            _ => return Err(PluginError::domain(ExecuteError::NotFound)),
        }
        let Ok(arguments) = serde_json::from_str::<ExecArguments>(request.arguments_json.as_str())
        else {
            return Err(PluginError::domain(ExecuteError::InvalidArguments));
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
                    return Err(PluginError::domain(ExecuteError::OutputLimitExceeded));
                }
                Ok(ExecuteResponse {
                    content_blocks: None,
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
                Err(PluginError::domain(map_process_error(error)))
            }
            Err(ProcessRunInvocationError::Runtime(error)) => Err(PluginError::runtime(error)),
        }
    }

    async fn execute_start(
        &self,
        context: InvocationContext,
        request: &ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        let arguments = serde_json::from_str::<ExecArguments>(request.arguments_json.as_str())
            .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))?;
        let owner = context
            .typed_extension::<ToolTaskOwner>()
            .map_err(|error| {
                PluginError::runtime(RuntimeFailure::PluginFailure {
                    detail: format!("background process owner is invalid: {error}"),
                })
            })?
            .ok_or_else(|| {
                PluginError::runtime(RuntimeFailure::PluginFailure {
                    detail: "background process is missing its parent Tool owner".to_owned(),
                })
            })?;
        if owner.session_id.is_empty()
            || owner.turn_id.is_empty()
            || owner.tool_call_id.is_empty()
            || owner.session_id.len() > 128
            || owner.turn_id.len() > 128
            || owner.tool_call_id.len() > 128
        {
            return Err(PluginError::runtime(RuntimeFailure::PluginFailure {
                detail: "background process owner is invalid".to_owned(),
            }));
        }
        if self.registry.borrow().processes.len() >= self.config.max_background_processes {
            return Err(PluginError::domain(execution_failed(
                "background_capacity_exceeded",
                "Release a terminal process handle before starting another background process",
            )));
        }
        let process_id = uuid::Uuid::new_v4().to_string();
        let cancellation = CancellationToken::new();
        let process_context =
            detached_context(&context, cancellation.clone()).map_err(PluginError::runtime)?;
        let persistence_context =
            detached_context(&context, CancellationToken::new()).map_err(PluginError::runtime)?;
        let task = Rc::new(RefCell::new(BackgroundProcess {
            process_id: process_id.clone(),
            owner,
            program: arguments.program.clone(),
            stdout: String::new(),
            stderr: String::new(),
            logs_truncated: false,
            cancel_requested: false,
            cancellation,
            terminal: None,
            durable_terminal: false,
            persistence_error: None,
        }));
        self.registry
            .borrow_mut()
            .processes
            .insert(process_id.clone(), Rc::clone(&task));
        let process = self.process.clone();
        let session = self.session.clone();
        let max_log_bytes = self.config.max_background_log_bytes;
        let timeout_ms = arguments
            .timeout_ms
            .unwrap_or(self.config.default_timeout_ms);
        if let Err(error) = self.tasks.spawn_local(async move {
            run_background_process(
                process,
                session,
                process_context,
                persistence_context,
                arguments,
                timeout_ms,
                max_log_bytes,
                task,
            )
            .await;
        }) {
            self.registry.borrow_mut().processes.remove(&process_id);
            return Err(PluginError::runtime(RuntimeFailure::PluginFailure {
                detail: format!("background process task failed to start: {error:?}"),
            }));
        }
        let task = self
            .registry
            .borrow()
            .processes
            .get(&process_id)
            .cloned()
            .expect("inserted background process remains registered");
        Ok(background_response(&task.borrow()))
    }

    fn execute_read(
        &self,
        request: &ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        let arguments = parse_process_id(request)?;
        let task = self
            .registry
            .borrow()
            .processes
            .get(&arguments.process_id)
            .cloned()
            .ok_or_else(|| PluginError::domain(process_not_found(&arguments.process_id)))?;
        if arguments.release && task.borrow().terminal.is_none() {
            return Err(PluginError::domain(execution_failed(
                "process_still_running",
                "Only a terminal background process handle can be released",
            )));
        }
        let response = background_response(&task.borrow());
        if arguments.release {
            self.registry
                .borrow_mut()
                .processes
                .remove(&arguments.process_id);
        }
        Ok(response)
    }

    fn execute_cancel(
        &self,
        request: &ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        let arguments = parse_process_id(request)?;
        if arguments.release {
            return Err(PluginError::domain(ExecuteError::InvalidArguments));
        }
        let task = self
            .registry
            .borrow()
            .processes
            .get(&arguments.process_id)
            .cloned()
            .ok_or_else(|| PluginError::domain(process_not_found(&arguments.process_id)))?;
        {
            let mut task = task.borrow_mut();
            if task.terminal.is_none() {
                task.cancel_requested = true;
                task.cancellation.cancel();
            }
        }
        Ok(background_response(&task.borrow()))
    }

    fn execute_list(
        &self,
        request: &ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        let valid = serde_json::from_str::<serde_json::Value>(request.arguments_json.as_str())
            .ok()
            .and_then(|value| value.as_object().map(serde_json::Map::is_empty))
            .unwrap_or(false);
        if !valid {
            return Err(PluginError::domain(ExecuteError::InvalidArguments));
        }
        let processes = self
            .registry
            .borrow()
            .processes
            .values()
            .map(|task| background_snapshot(&task.borrow()))
            .collect::<Vec<_>>();
        let count = processes.len();
        Ok(json_response(
            &serde_json::json!({"processes": processes}),
            &serde_json::json!({"count": count}),
        ))
    }

    fn progress_catalog(
        &self,
        _context: Ctx,
        _request: ProgressCatalogRequest,
    ) -> impl std::future::Future<
        Output = PluginResult<ProgressCatalogResponse, progress_contract::ProgressCatalogError>,
    > {
        let available = self.state.borrow().is_some();
        futures::future::ready(if available {
            Ok(ProgressCatalogResponse {
                tools: vec![ToolProgressDefinition {
                    name: EXEC_TOOL.to_owned(),
                }],
            })
        } else {
            Err(PluginError::runtime(RuntimeFailure::Unavailable {
                capability: progress_contract::CAPABILITY_ID,
            }))
        })
    }

    async fn execute_progress(
        &self,
        context: Ctx,
        request: ExecuteOpen,
    ) -> PluginResult<
        ProviderStream<progress_contract::ToolProgressExecuteProgress>,
        ProgressExecuteError,
    > {
        if request.name != EXEC_TOOL {
            return Err(PluginError::domain(ProgressExecuteError::NotFound));
        }
        let Ok(arguments) = serde_json::from_str::<ExecArguments>(request.arguments_json.as_str())
        else {
            return Err(PluginError::domain(ProgressExecuteError::InvalidArguments));
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
                PluginError::runtime(RuntimeFailure::PluginFailure {
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
) -> PluginResult<(), ProgressExecuteError> {
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
    stream.close_send().await.map_err(PluginError::runtime)?;
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut completed = false;
    loop {
        match stream.receive().await.map_err(PluginError::runtime)? {
            StreamEvent::Message(_) if completed => {
                return Err(PluginError::runtime(RuntimeFailure::ProtocolViolation {
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
                        .map_err(PluginError::runtime)?;
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
                        .map_err(PluginError::runtime)?;
                }
                RunStreamResponseKind::Completed => {
                    completed = true;
                    let exit_code = message.exit_code.ok_or_else(|| {
                        PluginError::runtime(RuntimeFailure::ProtocolViolation {
                            capability: process_contract::CAPABILITY_ID,
                        })
                    })?;
                    let duration_ms = message.duration_ms.ok_or_else(|| {
                        PluginError::runtime(RuntimeFailure::ProtocolViolation {
                            capability: process_contract::CAPABILITY_ID,
                        })
                    })?;
                    let output = format_process_output(&exit_code, &stdout, &stderr);
                    if output.len() > MAX_TOOL_OUTPUT_BYTES {
                        return Err(PluginError::domain(
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
                        .map_err(PluginError::runtime)?;
                }
            },
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) if completed => return Ok(()),
            StreamEvent::Terminal(Ok(())) => {
                return Err(PluginError::runtime(RuntimeFailure::ProtocolViolation {
                    capability: process_contract::CAPABILITY_ID,
                }));
            }
            StreamEvent::Terminal(Err(error)) => {
                return Err(PluginError::domain(map_process_stream_error(error)));
            }
        }
    }
}

fn append_bounded_output(
    target: &mut String,
    content: &str,
    other_length: usize,
) -> PluginResult<(), ProgressExecuteError> {
    let within_limit = target
        .len()
        .checked_add(other_length)
        .and_then(|length| length.checked_add(content.len()))
        .is_some_and(|length| length <= MAX_TOOL_OUTPUT_BYTES);
    if !within_limit {
        return Err(PluginError::domain(
            ProgressExecuteError::OutputLimitExceeded,
        ));
    }
    target.push_str(content);
    Ok(())
}

fn map_process_stream_open_error(
    error: ProcessRunStreamInvocationError,
) -> PluginError<ProgressExecuteError> {
    match error {
        ProcessRunStreamInvocationError::Domain(error) => {
            PluginError::domain(map_process_stream_error(error))
        }
        ProcessRunStreamInvocationError::Runtime(error) => PluginError::runtime(error),
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

#[allow(
    clippy::too_many_arguments,
    reason = "the background runner owns one explicit process and persistence boundary"
)]
async fn run_background_process(
    process: Port<process_contract::ProcessClient>,
    session: Port<session_contract::SessionClient>,
    process_context: InvocationContext,
    persistence_context: InvocationContext,
    arguments: ExecArguments,
    timeout_ms: u64,
    max_log_bytes: usize,
    task: Rc<RefCell<BackgroundProcess>>,
) {
    let terminal = observe_background_process(
        &process,
        process_context,
        &arguments,
        timeout_ms,
        max_log_bytes,
        &task,
    )
    .await;
    task.borrow_mut().terminal = Some(terminal);
    let persisted = persist_background_terminal(&session, persistence_context, &task).await;
    let mut task = task.borrow_mut();
    match persisted {
        Ok(()) => task.durable_terminal = true,
        Err(error) => task.persistence_error = Some(error),
    }
}

async fn observe_background_process(
    process: &Port<process_contract::ProcessClient>,
    context: InvocationContext,
    arguments: &ExecArguments,
    timeout_ms: u64,
    max_log_bytes: usize,
    task: &Rc<RefCell<BackgroundProcess>>,
) -> BackgroundTerminal {
    let stream = match process
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
    {
        Ok(stream) => stream,
        Err(ProcessRunStreamInvocationError::Domain(error)) => {
            return BackgroundTerminal::Domain {
                reason_code: process_stream_reason(&error).to_owned(),
            };
        }
        Err(ProcessRunStreamInvocationError::Runtime(error)) => {
            return BackgroundTerminal::Runtime {
                reason_code: runtime_failure_code(&error).to_owned(),
            };
        }
    };
    if let Err(error) = stream.close_send().await {
        return BackgroundTerminal::Runtime {
            reason_code: runtime_failure_code(&error).to_owned(),
        };
    }
    let mut completed = None;
    loop {
        match stream.receive().await {
            Ok(StreamEvent::Message(_)) if completed.is_some() => {
                return BackgroundTerminal::Runtime {
                    reason_code: "process_protocol_violation".to_owned(),
                };
            }
            Ok(StreamEvent::Message(message)) => match message.kind {
                RunStreamResponseKind::Stdout => {
                    append_background_log(task, true, &message.content, max_log_bytes);
                }
                RunStreamResponseKind::Stderr => {
                    append_background_log(task, false, &message.content, max_log_bytes);
                }
                RunStreamResponseKind::Completed => {
                    let (Some(exit_code), Some(duration_ms)) =
                        (message.exit_code, message.duration_ms)
                    else {
                        return BackgroundTerminal::Runtime {
                            reason_code: "process_protocol_violation".to_owned(),
                        };
                    };
                    completed = Some(BackgroundTerminal::Completed {
                        exit_code,
                        duration_ms,
                    });
                }
            },
            Ok(StreamEvent::PeerHalfClosed) => {}
            Ok(StreamEvent::Terminal(Ok(()))) => {
                return completed.unwrap_or_else(|| BackgroundTerminal::Runtime {
                    reason_code: "process_protocol_violation".to_owned(),
                });
            }
            Ok(StreamEvent::Terminal(Err(error))) => {
                return BackgroundTerminal::Domain {
                    reason_code: process_stream_reason(&error).to_owned(),
                };
            }
            Err(error) => {
                return BackgroundTerminal::Runtime {
                    reason_code: runtime_failure_code(&error).to_owned(),
                };
            }
        }
    }
}

fn append_background_log(
    task: &Rc<RefCell<BackgroundProcess>>,
    stdout: bool,
    content: &str,
    max_log_bytes: usize,
) {
    let mut task = task.borrow_mut();
    let used = task.stdout.len().saturating_add(task.stderr.len());
    let remaining = max_log_bytes.saturating_sub(used);
    if remaining == 0 {
        task.logs_truncated = true;
        return;
    }
    let mut boundary = remaining.min(content.len());
    while !content.is_char_boundary(boundary) {
        boundary -= 1;
    }
    if stdout {
        task.stdout.push_str(&content[..boundary]);
    } else {
        task.stderr.push_str(&content[..boundary]);
    }
    task.logs_truncated |= boundary < content.len();
}

async fn persist_background_terminal(
    session: &Port<session_contract::SessionClient>,
    context: InvocationContext,
    task: &Rc<RefCell<BackgroundProcess>>,
) -> Result<(), String> {
    let (owner, payload) = {
        let task = task.borrow();
        let snapshot = background_snapshot(&task);
        let status = terminal_status(&task);
        let duration_ms = terminal_duration_ms(&task);
        let formatted_output = format_process_output(
            terminal_exit_code(&task).unwrap_or("unavailable"),
            &task.stdout,
            &task.stderr,
        );
        let metadata_json = serde_json::json!({
            "schema": "lenso.agent.background-process@1",
            "process": snapshot,
            "owner_tool_call_id": task.owner.tool_call_id,
        })
        .to_string();
        (
            task.owner.clone(),
            serde_json::json!({
                "call_id": format!("background-process:{}", task.process_id),
                "name": "background_process",
                "status": status,
                "content": formatted_output,
                "duration_ms": duration_ms,
                "metadata_json": metadata_json,
            }),
        )
    };
    let occurred_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| format!("failed to format process terminal timestamp: {error}"))?;
    let event = AppendSessionRequestEventsItem {
        event_id: uuid::Uuid::new_v4().to_string(),
        kind: AppendSessionRequestEventsItemKind::ToolResult,
        turn_id: Some(owner.turn_id),
        occurred_at,
        payload_json: payload
            .to_string()
            .try_into()
            .expect("background process terminal payload must be valid JSON"),
    };
    let mut revision = "0".to_owned();
    for _ in 0..TERMINAL_APPEND_ATTEMPTS {
        match session
            .append_with_context(
                context.clone(),
                AppendSessionRequest {
                    session_id: owner.session_id.clone(),
                    expected_revision: revision,
                    events: vec![event.clone()],
                },
            )
            .await
        {
            Ok(_) => return Ok(()),
            Err(SessionAppendInvocationError::Domain(SessionAppendError::RevisionConflict {
                payload,
            })) => revision = payload.current_revision,
            Err(error) => return Err(format!("Session terminal append failed: {error:?}")),
        }
    }
    Err("Session terminal append exceeded its conflict retry bound".to_owned())
}

fn detached_context(
    parent: &InvocationContext,
    cancellation: CancellationToken,
) -> Result<InvocationContext, RuntimeFailure> {
    let mut detached = InvocationContext::new(parent.request_id(), None, cancellation);
    for extension in parent.extensions() {
        if extension.key() == TOOL_TASK_OWNER_EXTENSION {
            continue;
        }
        detached = detached
            .with_extension(extension.key(), extension.value().to_vec())
            .map_err(|error| RuntimeFailure::Internal {
                detail: format!("failed to preserve background process context: {error}"),
            })?;
    }
    for extension in parent.sealed_extensions() {
        detached = detached
            .with_sealed_extension(extension.clone())
            .map_err(|error| RuntimeFailure::Internal {
                detail: format!("failed to preserve background process authority: {error}"),
            })?;
    }
    Ok(detached)
}

fn parse_process_id(request: &ExecuteRequest) -> PluginResult<ProcessIdArguments, ExecuteError> {
    let arguments = serde_json::from_str::<ProcessIdArguments>(request.arguments_json.as_str())
        .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))?;
    if uuid::Uuid::parse_str(&arguments.process_id).is_err() {
        return Err(PluginError::domain(ExecuteError::InvalidArguments));
    }
    Ok(arguments)
}

fn process_id_schema(allow_release: bool) -> tool_provider_contract::RawJson {
    let mut properties = serde_json::json!({
        "process_id": {"type": "string", "format": "uuid"}
    });
    if allow_release {
        properties["release"] = serde_json::json!({"type": "boolean", "default": false});
    }
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": ["process_id"]
    })
    .to_string()
    .try_into()
    .expect("process handle schema must be valid JSON")
}

fn empty_schema() -> tool_provider_contract::RawJson {
    "{\"type\":\"object\",\"additionalProperties\":false}"
        .to_owned()
        .try_into()
        .expect("empty Tool schema must be valid JSON")
}

fn background_response(task: &BackgroundProcess) -> ExecuteResponse {
    json_response(
        &background_snapshot(task),
        &serde_json::json!({
            "schema": "lenso.agent.background-process@1",
            "process_id": task.process_id,
            "status": terminal_status(task),
        }),
    )
}

fn json_response(content: &serde_json::Value, metadata: &serde_json::Value) -> ExecuteResponse {
    ExecuteResponse {
        content_blocks: None,
        content: content.to_string(),
        content_type: ContentType::Text,
        metadata_json: metadata
            .to_string()
            .try_into()
            .expect("process Tool metadata must be valid JSON"),
    }
}

fn background_snapshot(task: &BackgroundProcess) -> serde_json::Value {
    serde_json::json!({
        "process_id": task.process_id,
        "program": task.program,
        "status": terminal_status(task),
        "stdout": task.stdout,
        "stderr": task.stderr,
        "logs_truncated": task.logs_truncated,
        "cancel_requested": task.cancel_requested,
        "exit_code": terminal_exit_code(task),
        "duration_ms": terminal_duration_ms(task),
        "reason_code": terminal_reason(task),
        "durable_terminal": task.durable_terminal,
        "persistence_error": task.persistence_error,
    })
}

fn terminal_status(task: &BackgroundProcess) -> &'static str {
    match (&task.terminal, task.cancel_requested) {
        (None, true) => "cancelling",
        (None, false) => "running",
        (Some(BackgroundTerminal::Completed { .. }), _) => "completed",
        (Some(_), true) => "cancelled",
        (Some(_), false) => "failed",
    }
}

fn terminal_exit_code(task: &BackgroundProcess) -> Option<&str> {
    match &task.terminal {
        Some(BackgroundTerminal::Completed { exit_code, .. }) => Some(exit_code),
        _ => None,
    }
}

fn terminal_duration_ms(task: &BackgroundProcess) -> Option<u64> {
    match &task.terminal {
        Some(BackgroundTerminal::Completed { duration_ms, .. }) => duration_ms.parse().ok(),
        _ => None,
    }
}

fn terminal_reason(task: &BackgroundProcess) -> Option<&str> {
    match &task.terminal {
        Some(
            BackgroundTerminal::Domain { reason_code }
            | BackgroundTerminal::Runtime { reason_code },
        ) => Some(reason_code),
        _ => None,
    }
}

fn process_stream_reason(error: &RunStreamError) -> &str {
    match error {
        RunStreamError::InvalidRequest => "invalid_request",
        RunStreamError::ProgramNotAllowed => "program_not_allowed",
        RunStreamError::InvalidWorkingDirectory => "invalid_working_directory",
        RunStreamError::Timeout => "timeout",
        RunStreamError::OutputLimitExceeded => "output_limit_exceeded",
        RunStreamError::Terminated => "terminated",
        RunStreamError::Unknown(error) => &error.code,
    }
}

const fn runtime_failure_code(error: &RuntimeFailure) -> &'static str {
    match error {
        RuntimeFailure::Unavailable { .. } => "runtime_unavailable",
        RuntimeFailure::UnknownOperation { .. } => "runtime_unknown_operation",
        RuntimeFailure::AmbiguousBinding { .. } => "runtime_ambiguous_binding",
        RuntimeFailure::ProtocolViolation { .. } => "runtime_protocol_violation",
        RuntimeFailure::MissingPluginFactory { .. } => "runtime_missing_plugin_factory",
        RuntimeFailure::UnavailableExecutionClass { .. } => "runtime_execution_class_unavailable",
        RuntimeFailure::InvalidResolvedPlan { .. } => "runtime_invalid_plan",
        RuntimeFailure::AdmissionClosed => "runtime_admission_closed",
        RuntimeFailure::ResourceExhausted { .. } => "runtime_resource_exhausted",
        RuntimeFailure::DeadlineExceeded { .. } => "runtime_deadline_exceeded",
        RuntimeFailure::Cancelled { .. } => "runtime_cancelled",
        RuntimeFailure::Internal { .. } => "runtime_internal",
        RuntimeFailure::PluginFailure { .. } => "runtime_plugin_failure",
        RuntimeFailure::PluginRestartExhausted { .. } => "runtime_restart_exhausted",
    }
}

fn process_not_found(process_id: &str) -> ExecuteError {
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: "process_not_found".to_owned(),
            message: "Background process handle is unavailable".to_owned(),
            details_json: serde_json::json!({"process_id": process_id})
                .to_string()
                .try_into()
                .expect("process-not-found details must be valid JSON"),
        },
    }
}

impl Lifecycle for ProcessToolsPlugin {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        let process_catalog = self
            .process
            .catalog(ProcessCatalogRequest {})
            .await
            .map_err(|error| RuntimeFailure::PluginFailure {
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
        let input_schema_json: tool_provider_contract::RawJson = serde_json::json!({
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
                    tools: vec![
                        ToolDefinition {
                            name: EXEC_TOOL.to_owned(),
                            description: "Run one explicitly allowed executable without shell parsing. The command can still execute trusted project code and is not a sandbox.".to_owned(),
                            input_schema_json: input_schema_json.clone(),
                            execution: ToolExecutionClass::Exclusive,
                        },
                        ToolDefinition {
                            name: START_TOOL.to_owned(),
                            description: "Start one explicitly allowed process in the background and return a generation-owned handle immediately.".to_owned(),
                            input_schema_json,
                            execution: ToolExecutionClass::Exclusive,
                        },
                        ToolDefinition {
                            name: READ_TOOL.to_owned(),
                            description: "Read bounded logs and terminal status for one background process. Set release=true only after it is terminal.".to_owned(),
                            input_schema_json: process_id_schema(true),
                            execution: ToolExecutionClass::ParallelSafe,
                        },
                        ToolDefinition {
                            name: CANCEL_TOOL.to_owned(),
                            description: "Request cancellation of one background process without cancelling the owning Agent Turn.".to_owned(),
                            input_schema_json: process_id_schema(false),
                            execution: ToolExecutionClass::Exclusive,
                        },
                        ToolDefinition {
                            name: LIST_TOOL.to_owned(),
                            description: "List every retained background process handle in this App Generation.".to_owned(),
                            input_schema_json: empty_schema(),
                            execution: ToolExecutionClass::ParallelSafe,
                        },
                    ],
                },
            }));
        Ok(())
    }

    #[allow(clippy::unused_async_trait_impl)]
    async fn deactivate(&self, _context: DeactivateContext) -> Result<(), RuntimeFailure> {
        for task in self.registry.borrow().processes.values() {
            task.borrow().cancellation.cancel();
        }
        self.state.replace(None);
        Ok(())
    }
}

fn default_cwd() -> String {
    ".".to_owned()
}

const fn default_max_background_processes() -> usize {
    8
}

const fn default_max_background_log_bytes() -> usize {
    262_144
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

#[cfg(test)]
mod tests {
    use super::*;

    fn background_process() -> Rc<RefCell<BackgroundProcess>> {
        Rc::new(RefCell::new(BackgroundProcess {
            process_id: uuid::Uuid::nil().to_string(),
            owner: ToolTaskOwner {
                session_id: "session-1".to_owned(),
                turn_id: "turn-1".to_owned(),
                tool_call_id: "call-1".to_owned(),
            },
            program: "fixture".to_owned(),
            stdout: String::new(),
            stderr: String::new(),
            logs_truncated: false,
            cancel_requested: false,
            cancellation: CancellationToken::new(),
            terminal: None,
            durable_terminal: false,
            persistence_error: None,
        }))
    }

    #[test]
    fn background_logs_share_one_utf8_safe_bound() {
        let task = background_process();
        append_background_log(&task, true, "ab你", 4);
        append_background_log(&task, false, "stderr", 4);
        let task = task.borrow();
        assert_eq!(task.stdout, "ab");
        assert_eq!(task.stderr, "st");
        assert_eq!(task.stdout.len() + task.stderr.len(), 4);
        assert!(task.logs_truncated);
    }

    #[test]
    fn legacy_configuration_receives_bounded_background_defaults() {
        let config: ProcessToolsConfig =
            serde_json::from_value(serde_json::json!({"default_timeout_ms": 120_000})).unwrap();
        assert_eq!(config.max_background_processes, 8);
        assert_eq!(config.max_background_log_bytes, 262_144);
        validate_config(&config).unwrap();
    }
}

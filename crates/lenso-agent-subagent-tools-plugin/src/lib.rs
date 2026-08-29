//! Bounded Tool projection over one explicitly composed child Agent.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    task::{Poll, Waker},
};

use futures::future::{Either, ready, select};
use lenso::prelude::*;
use lenso_capability_agent::{
    self as agent_contract, AgentInvocationError, RunTurnError, RunTurnRequest, RunTurnResponseKind,
};
use lenso_capability_agent_tool_provider::{
    self as tool_provider_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
    ToolProviderProvider,
};
use lenso_capability_agent_turn_input::{
    self as turn_input_contract, SubmitError, SubmitRequest, TurnInputInvocationError,
};
use lenso_kernel::{
    CancellationToken, InvocationContext, NativeStream, RuntimeFailure, StreamEvent,
};

/// Stable model-visible Tool name.
pub const DELEGATE_TOOL: &str = "delegate";
/// Stable model-visible Tool name for starting generation-owned child work.
pub const SPAWN_SUBAGENT_TOOL: &str = "spawn_subagent";
/// Stable model-visible Tool name for joining one generation-owned child task.
pub const WAIT_SUBAGENT_TOOL: &str = "wait_subagent";
/// Stable model-visible Tool name for cancelling one generation-owned child task.
pub const CANCEL_SUBAGENT_TOOL: &str = "cancel_subagent";
/// Stable model-visible Tool name for adding input at the child Agent's next model boundary.
pub const SEND_SUBAGENT_TOOL: &str = "send_subagent";
/// Stable model-visible Tool name for non-destructive task discovery.
pub const LIST_SUBAGENTS_TOOL: &str = "list_subagents";
const RESULT_METADATA_SCHEMA: &str = "lenso.agent.subagent-result@1";
const TASK_METADATA_SCHEMA: &str = "lenso.agent.subagent-task@1";
const TASK_LIST_METADATA_SCHEMA: &str = "lenso.agent.subagent-task-list@1";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)]
struct SubagentToolsConfig {
    max_output_bytes: usize,
    max_task_bytes: usize,
    max_tasks: usize,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegateArguments {
    task: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskIdArguments {
    task_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArguments {
    task_id: String,
    input: String,
}

fn validate_config(config: &SubagentToolsConfig) -> Result<(), RuntimeFailure> {
    if config.max_output_bytes == 0
        || config.max_output_bytes > 1_048_576
        || config.max_task_bytes == 0
        || config.max_task_bytes > 262_144
        || config.max_tasks == 0
        || config.max_tasks > 64
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
    turn_input: Port<turn_input_contract::TurnInputClient>,
    registry: Rc<RefCell<SubagentTaskRegistry>>,
    #[tasks]
    managed_tasks: ManagedTasks,
}

#[lenso::provides(tool_provider_contract::ToolProvider)]
impl ToolProviderProvider for SubagentToolsPlugin {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_provider_contract::ToolProviderCatalog> {
        let task_schema = task_input_schema(self.config.max_task_bytes);
        let task_id_schema = task_id_input_schema();
        Box::pin(ready(Ok(Ok(CatalogResponse {
            tools: vec![
                ToolDefinition {
                    name: DELEGATE_TOOL.to_owned(),
                    description: "Delegate one bounded task to an independently composed child Agent and wait for its terminal result. The child has its own durable Session and only the Capabilities selected for it by App Composition.".to_owned(),
                    input_schema_json: task_schema.clone(),
                    execution: ToolExecutionClass::Exclusive,
                },
                ToolDefinition {
                    name: SPAWN_SUBAGENT_TOOL.to_owned(),
                    description: "Start one bounded child-Agent task and return its task ID immediately. Use wait_subagent to collect the terminal result.".to_owned(),
                    input_schema_json: task_schema,
                    execution: ToolExecutionClass::Exclusive,
                },
                ToolDefinition {
                    name: WAIT_SUBAGENT_TOOL.to_owned(),
                    description: "Wait for one spawned child-Agent task, consume its terminal result, and release its task slot.".to_owned(),
                    input_schema_json: task_id_schema.clone(),
                    execution: ToolExecutionClass::Exclusive,
                },
                ToolDefinition {
                    name: CANCEL_SUBAGENT_TOOL.to_owned(),
                    description: "Request cancellation of one spawned child-Agent task without cancelling the parent Turn.".to_owned(),
                    input_schema_json: task_id_schema,
                    execution: ToolExecutionClass::Exclusive,
                },
                ToolDefinition {
                    name: SEND_SUBAGENT_TOOL.to_owned(),
                    description: "Submit additional input to a running child-Agent task. Acceptance waits until the input is durably recorded for the child's next model request.".to_owned(),
                    input_schema_json: send_input_schema(self.config.max_task_bytes),
                    execution: ToolExecutionClass::Exclusive,
                },
                ToolDefinition {
                    name: LIST_SUBAGENTS_TOOL.to_owned(),
                    description: "List every child-Agent task still owned by this App Generation without waiting for or consuming terminal results.".to_owned(),
                    input_schema_json: empty_input_schema(),
                    execution: ToolExecutionClass::Exclusive,
                },
            ],
        }))))
    }

    fn execute(
        &self,
        context: InvocationContext,
        request: ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_provider_contract::ToolProviderExecute> {
        match request.name.as_str() {
            DELEGATE_TOOL => self.execute_delegate(context, &request),
            SPAWN_SUBAGENT_TOOL => self.execute_spawn(&context, &request),
            WAIT_SUBAGENT_TOOL => self.execute_wait(&request),
            CANCEL_SUBAGENT_TOOL => self.execute_cancel(&request),
            SEND_SUBAGENT_TOOL => self.execute_send(context, &request),
            LIST_SUBAGENTS_TOOL => self.execute_list(&request),
            _ => Box::pin(ready(Ok(Err(ExecuteError::NotFound)))),
        }
    }
}

fn task_input_schema(max_task_bytes: usize) -> tool_provider_contract::RawJson {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "task": {
                "type": "string",
                "minLength": 1,
                "maxLength": max_task_bytes
            }
        },
        "required": ["task"]
    })
    .to_string()
    .try_into()
    .expect("subagent task schema must be valid JSON")
}

fn task_id_input_schema() -> tool_provider_contract::RawJson {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "task_id": { "type": "string", "minLength": 1, "maxLength": 64 }
        },
        "required": ["task_id"]
    })
    .to_string()
    .try_into()
    .expect("subagent task ID schema must be valid JSON")
}

fn send_input_schema(max_input_bytes: usize) -> tool_provider_contract::RawJson {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "task_id": { "type": "string", "minLength": 1, "maxLength": 64 },
            "input": {
                "type": "string",
                "minLength": 1,
                "maxLength": max_input_bytes
            }
        },
        "required": ["task_id", "input"]
    })
    .to_string()
    .try_into()
    .expect("subagent send schema must be valid JSON")
}

fn empty_input_schema() -> tool_provider_contract::RawJson {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {}
    })
    .to_string()
    .try_into()
    .expect("subagent list schema must be valid JSON")
}

impl SubagentToolsPlugin {
    fn execute_delegate(
        &self,
        context: InvocationContext,
        request: &ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_provider_contract::ToolProviderExecute> {
        let Ok(arguments) = self.parse_task(request) else {
            return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
        };
        let task_bytes = arguments.task.len();
        Box::pin(execute_delegation(
            self.agent.clone(),
            context,
            arguments.task,
            task_bytes,
            self.config.max_output_bytes,
            None,
            None,
        ))
    }

    fn execute_spawn(
        &self,
        context: &InvocationContext,
        request: &ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_provider_contract::ToolProviderExecute> {
        let Ok(arguments) = self.parse_task(request) else {
            return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
        };
        let mut registry = self.registry.borrow_mut();
        if registry.tasks.len() >= self.config.max_tasks {
            return Box::pin(ready(Ok(Err(execution_failed(
                "subagent_task_capacity_exceeded",
                "The bounded subagent task registry is full",
                &task_metadata(None, "rejected"),
            )))));
        }
        let task_id = format!("subagent-{}", uuid::Uuid::new_v4());
        let task = Rc::new(RefCell::new(SubagentTask::new()));
        registry.tasks.insert(task_id.clone(), task.clone());
        drop(registry);

        let child_context = match detached_child_context(context) {
            Ok(context) => context,
            Err(error) => {
                self.registry.borrow_mut().tasks.remove(&task_id);
                return Box::pin(ready(Err(error)));
            }
        };
        task.borrow_mut()
            .attach_cancellation(child_context.cancellation());
        let agent = self.agent.clone();
        let task_bytes = arguments.task.len();
        let max_output_bytes = self.config.max_output_bytes;
        let background_task_id = task_id.clone();
        let background_task = task.clone();
        let parent_cancellation = context.cancellation();
        let spawned = self.managed_tasks.spawn_local(async move {
            let outcome = execute_delegation(
                agent,
                child_context,
                arguments.task,
                task_bytes,
                max_output_bytes,
                Some(background_task_id),
                Some((background_task.clone(), parent_cancellation)),
            )
            .await;
            background_task.borrow_mut().complete(outcome);
        });
        if let Err(error) = spawned {
            self.registry.borrow_mut().tasks.remove(&task_id);
            return Box::pin(ready(Err(RuntimeFailure::PluginFailure {
                detail: format!("subagent task failed to start: {error:?}"),
            })));
        }

        Box::pin(ready(Ok(Ok(ExecuteResponse {
            content: serde_json::json!({
                "task_id": task_id,
                "status": "running"
            })
            .to_string(),
            content_type: ContentType::Text,
            metadata_json: task_metadata(Some(&task_id), "running")
                .to_string()
                .try_into()
                .expect("subagent task metadata must be valid JSON"),
        }))))
    }

    fn execute_wait(
        &self,
        request: &ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_provider_contract::ToolProviderExecute> {
        let Ok(arguments) = parse_task_id(request) else {
            return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
        };
        let Some(task) = self
            .registry
            .borrow()
            .tasks
            .get(&arguments.task_id)
            .cloned()
        else {
            return Box::pin(ready(Ok(Err(task_not_found(&arguments.task_id)))));
        };
        let registry = self.registry.clone();
        Box::pin(async move {
            let terminal = wait_for_task(task.clone()).await;
            let mut registry = registry.borrow_mut();
            if registry
                .tasks
                .get(&arguments.task_id)
                .is_some_and(|registered| Rc::ptr_eq(registered, &task))
            {
                registry.tasks.remove(&arguments.task_id);
            }
            Ok(Ok(task_terminal_response(&arguments.task_id, terminal)))
        })
    }

    fn execute_cancel(
        &self,
        request: &ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_provider_contract::ToolProviderExecute> {
        let Ok(arguments) = parse_task_id(request) else {
            return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
        };
        let Some(task) = self
            .registry
            .borrow()
            .tasks
            .get(&arguments.task_id)
            .cloned()
        else {
            return Box::pin(ready(Ok(Err(task_not_found(&arguments.task_id)))));
        };
        let status = task.borrow_mut().request_cancel();
        Box::pin(ready(Ok(Ok(ExecuteResponse {
            content: serde_json::json!({
                "task_id": arguments.task_id,
                "status": status
            })
            .to_string(),
            content_type: ContentType::Text,
            metadata_json: task_metadata(Some(&arguments.task_id), status)
                .to_string()
                .try_into()
                .expect("subagent task metadata must be valid JSON"),
        }))))
    }

    fn execute_send(
        &self,
        context: InvocationContext,
        request: &ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_provider_contract::ToolProviderExecute> {
        let Ok(arguments) = parse_send(request, self.config.max_task_bytes) else {
            return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
        };
        let Some(task) = self
            .registry
            .borrow()
            .tasks
            .get(&arguments.task_id)
            .cloned()
        else {
            return Box::pin(ready(Ok(Err(task_not_found(&arguments.task_id)))));
        };
        let turn_input = self.turn_input.clone();
        Box::pin(async move {
            let Some(child_session_id) = wait_for_child_session(task).await else {
                return Ok(Err(execution_failed(
                    "subagent_task_not_running",
                    "The subagent task ended before it accepted additional input",
                    &task_metadata(Some(&arguments.task_id), "already_terminal"),
                )));
            };
            match turn_input
                .submit_with_context(
                    context,
                    SubmitRequest {
                        session_id: child_session_id.clone(),
                        input: arguments.input,
                    },
                )
                .await
            {
                Ok(response) => Ok(Ok(ExecuteResponse {
                    content: serde_json::json!({
                        "task_id": arguments.task_id,
                        "status": "input_accepted",
                        "child_session_id": response.session_id,
                        "accepted_revision": response.accepted_revision,
                    })
                    .to_string(),
                    content_type: ContentType::Text,
                    metadata_json: task_metadata(Some(&arguments.task_id), "input_accepted")
                        .to_string()
                        .try_into()
                        .expect("subagent task metadata must be valid JSON"),
                })),
                Err(error) => Ok(Err(map_turn_input_error(&error, &arguments.task_id))),
            }
        })
    }

    fn execute_list(
        &self,
        request: &ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_provider_contract::ToolProviderExecute> {
        let Ok(arguments) = serde_json::from_str::<BTreeMap<String, serde_json::Value>>(
            request.arguments_json.as_str(),
        ) else {
            return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
        };
        if !arguments.is_empty() {
            return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
        }
        let tasks = self
            .registry
            .borrow()
            .tasks
            .iter()
            .map(|(task_id, task)| task.borrow().snapshot(task_id))
            .collect::<Vec<_>>();
        Box::pin(ready(Ok(Ok(ExecuteResponse {
            content: serde_json::json!({
                "task_count": tasks.len(),
                "tasks": tasks,
            })
            .to_string(),
            content_type: ContentType::Text,
            metadata_json: serde_json::json!({
                "schema": TASK_LIST_METADATA_SCHEMA,
                "task_count": tasks.len(),
            })
            .to_string()
            .try_into()
            .expect("subagent task-list metadata must be valid JSON"),
        }))))
    }

    fn parse_task(&self, request: &ExecuteRequest) -> Result<DelegateArguments, ()> {
        let arguments = serde_json::from_str::<DelegateArguments>(request.arguments_json.as_str())
            .map_err(|_| ())?;
        if arguments.task.trim().is_empty() || arguments.task.len() > self.config.max_task_bytes {
            return Err(());
        }
        Ok(arguments)
    }
}

fn parse_task_id(request: &ExecuteRequest) -> Result<TaskIdArguments, ()> {
    let arguments =
        serde_json::from_str::<TaskIdArguments>(request.arguments_json.as_str()).map_err(|_| ())?;
    if arguments.task_id.trim().is_empty() || arguments.task_id.len() > 64 {
        return Err(());
    }
    Ok(arguments)
}

fn parse_send(request: &ExecuteRequest, max_input_bytes: usize) -> Result<SendArguments, ()> {
    let arguments =
        serde_json::from_str::<SendArguments>(request.arguments_json.as_str()).map_err(|_| ())?;
    if arguments.task_id.trim().is_empty()
        || arguments.task_id.len() > 64
        || arguments.input.trim().is_empty()
        || arguments.input.len() > max_input_bytes
    {
        return Err(());
    }
    Ok(arguments)
}

#[derive(Default, Debug)]
struct SubagentTaskRegistry {
    tasks: BTreeMap<String, Rc<RefCell<SubagentTask>>>,
}

#[derive(Debug)]
struct SubagentTask {
    child_session_id: Option<String>,
    cancel_requested: bool,
    cancellation: Option<CancellationToken>,
    stream: Option<Rc<NativeStream<agent_contract::Agent>>>,
    terminal: Option<SubagentTaskTerminal>,
    waiters: Vec<Waker>,
}

#[derive(Clone, Debug)]
enum SubagentTaskTerminal {
    Completed(ExecuteResponse),
    Domain(ExecuteError),
    Cancelled,
    Runtime(RuntimeFailure),
}

impl SubagentTask {
    fn new() -> Self {
        Self {
            child_session_id: None,
            cancel_requested: false,
            cancellation: None,
            stream: None,
            terminal: None,
            waiters: Vec::new(),
        }
    }

    fn attach_cancellation(&mut self, cancellation: CancellationToken) {
        if self.cancel_requested {
            cancellation.cancel();
        }
        self.cancellation = Some(cancellation);
    }

    fn attach_stream(&mut self, stream: Rc<NativeStream<agent_contract::Agent>>) {
        if self.cancel_requested {
            stream.cancel();
        }
        self.stream = Some(stream);
    }

    fn observe_session(&mut self, session_id: &str) {
        if self.child_session_id.is_none() {
            self.child_session_id = Some(session_id.to_owned());
            for waiter in self.waiters.drain(..) {
                waiter.wake();
            }
        }
    }

    fn complete(&mut self, outcome: Result<Result<ExecuteResponse, ExecuteError>, RuntimeFailure>) {
        self.cancellation = None;
        self.stream = None;
        self.terminal = Some(match outcome {
            Ok(Ok(response)) => SubagentTaskTerminal::Completed(response),
            Ok(Err(error)) => SubagentTaskTerminal::Domain(error),
            Err(RuntimeFailure::Cancelled { .. } | RuntimeFailure::AdmissionClosed)
                if self.cancel_requested =>
            {
                SubagentTaskTerminal::Cancelled
            }
            Err(error) => SubagentTaskTerminal::Runtime(error),
        });
        for waiter in self.waiters.drain(..) {
            waiter.wake();
        }
    }

    fn request_cancel(&mut self) -> &'static str {
        if self.terminal.is_some() {
            return "already_terminal";
        }
        self.cancel_requested = true;
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
        if let Some(stream) = &self.stream {
            stream.cancel();
        }
        "cancellation_requested"
    }

    fn snapshot(&self, task_id: &str) -> serde_json::Value {
        let status = match self.terminal {
            Some(SubagentTaskTerminal::Completed(_)) => "completed",
            Some(SubagentTaskTerminal::Domain(_) | SubagentTaskTerminal::Runtime(_)) => "failed",
            Some(SubagentTaskTerminal::Cancelled) => "cancelled",
            None if self.cancel_requested => "cancellation_requested",
            None => "running",
        };
        serde_json::json!({
            "task_id": task_id,
            "status": status,
            "child_session_id": self.child_session_id,
        })
    }
}

async fn wait_for_task(task: Rc<RefCell<SubagentTask>>) -> SubagentTaskTerminal {
    std::future::poll_fn(move |context| {
        let mut task = task.borrow_mut();
        if let Some(terminal) = task.terminal.clone() {
            return Poll::Ready(terminal);
        }
        if !task
            .waiters
            .iter()
            .any(|waiter| waiter.will_wake(context.waker()))
        {
            task.waiters.push(context.waker().clone());
        }
        Poll::Pending
    })
    .await
}

async fn wait_for_child_session(task: Rc<RefCell<SubagentTask>>) -> Option<String> {
    std::future::poll_fn(move |context| {
        let mut task = task.borrow_mut();
        if let Some(session_id) = &task.child_session_id {
            return Poll::Ready(Some(session_id.clone()));
        }
        if task.terminal.is_some() {
            return Poll::Ready(None);
        }
        if !task
            .waiters
            .iter()
            .any(|waiter| waiter.will_wake(context.waker()))
        {
            task.waiters.push(context.waker().clone());
        }
        Poll::Pending
    })
    .await
}

fn detached_child_context(parent: &InvocationContext) -> Result<InvocationContext, RuntimeFailure> {
    let mut child = InvocationContext::new(
        parent.request_id(),
        parent.deadline(),
        CancellationToken::new(),
    );
    for extension in parent.extensions() {
        child = child
            .with_extension(extension.key(), extension.value().to_vec())
            .map_err(|error| RuntimeFailure::Internal {
                detail: format!("failed to preserve child invocation context: {error}"),
            })?;
    }
    for extension in parent.sealed_extensions() {
        child = child
            .with_sealed_extension(extension.clone())
            .map_err(|error| RuntimeFailure::Internal {
                detail: format!("failed to preserve sealed child invocation context: {error}"),
            })?;
    }
    Ok(child)
}

fn task_metadata(task_id: Option<&str>, status: &str) -> serde_json::Value {
    serde_json::json!({
        "schema": TASK_METADATA_SCHEMA,
        "task_id": task_id,
        "status": status,
    })
}

fn task_not_found(task_id: &str) -> ExecuteError {
    execution_failed(
        "subagent_task_not_found",
        "The subagent task does not exist in this App Generation",
        &task_metadata(Some(task_id), "missing"),
    )
}

fn task_terminal_response(task_id: &str, terminal: SubagentTaskTerminal) -> ExecuteResponse {
    match terminal {
        SubagentTaskTerminal::Completed(response) => response,
        SubagentTaskTerminal::Domain(error) => task_domain_error_response(task_id, error),
        SubagentTaskTerminal::Cancelled => task_status_response(
            task_id,
            "cancelled",
            "subagent_cancelled",
            "The child-Agent task was cancelled",
        ),
        SubagentTaskTerminal::Runtime(error) => task_status_response(
            task_id,
            "failed",
            runtime_failure_code(&error),
            "The child-Agent task ended with a Runtime Failure",
        ),
    }
}

fn task_domain_error_response(task_id: &str, error: ExecuteError) -> ExecuteResponse {
    let (reason_code, message, metadata) = match error {
        ExecuteError::ExecutionFailed { payload } => {
            let mut metadata =
                serde_json::from_str::<serde_json::Value>(payload.details_json.as_str())
                    .unwrap_or_else(|_| task_metadata(Some(task_id), "failed"));
            if let Some(object) = metadata.as_object_mut() {
                object.insert(
                    "task_id".to_owned(),
                    serde_json::Value::String(task_id.to_owned()),
                );
                object.insert(
                    "status".to_owned(),
                    serde_json::Value::String("failed".to_owned()),
                );
            }
            (payload.reason_code, payload.message, metadata)
        }
        ExecuteError::InvalidArguments => (
            "child_invalid_arguments".to_owned(),
            "The child Agent rejected its arguments".to_owned(),
            task_metadata(Some(task_id), "failed"),
        ),
        ExecuteError::NotFound => (
            "child_not_found".to_owned(),
            "The child Agent operation was not found".to_owned(),
            task_metadata(Some(task_id), "failed"),
        ),
        ExecuteError::OutputLimitExceeded => (
            "child_output_limit_exceeded".to_owned(),
            "The child Agent exceeded its output limit".to_owned(),
            task_metadata(Some(task_id), "failed"),
        ),
        ExecuteError::PermissionDenied => (
            "child_permission_denied".to_owned(),
            "The child Agent was denied".to_owned(),
            task_metadata(Some(task_id), "failed"),
        ),
        ExecuteError::Unknown(error) => (
            error.code,
            "The child Agent returned an unknown Domain Error".to_owned(),
            task_metadata(Some(task_id), "failed"),
        ),
    };
    ExecuteResponse {
        content: serde_json::json!({
            "task_id": task_id,
            "status": "failed",
            "reason_code": reason_code,
            "message": message,
        })
        .to_string(),
        content_type: ContentType::Text,
        metadata_json: metadata
            .to_string()
            .try_into()
            .expect("subagent failure metadata must be valid JSON"),
    }
}

fn task_status_response(
    task_id: &str,
    status: &str,
    reason_code: &str,
    message: &str,
) -> ExecuteResponse {
    ExecuteResponse {
        content: serde_json::json!({
            "task_id": task_id,
            "status": status,
            "reason_code": reason_code,
            "message": message,
        })
        .to_string(),
        content_type: ContentType::Text,
        metadata_json: task_metadata(Some(task_id), status)
            .to_string()
            .try_into()
            .expect("subagent task metadata must be valid JSON"),
    }
}

const fn runtime_failure_code(error: &RuntimeFailure) -> &'static str {
    match error {
        RuntimeFailure::Unavailable { .. } => "child_runtime_unavailable",
        RuntimeFailure::UnknownOperation { .. } => "child_runtime_unknown_operation",
        RuntimeFailure::AmbiguousBinding { .. } => "child_runtime_ambiguous_binding",
        RuntimeFailure::ProtocolViolation { .. } => "child_runtime_protocol_violation",
        RuntimeFailure::MissingPluginFactory { .. } => "child_runtime_missing_plugin_factory",
        RuntimeFailure::UnavailableExecutionClass { .. } => {
            "child_runtime_unavailable_execution_class"
        }
        RuntimeFailure::InvalidResolvedPlan { .. } => "child_runtime_invalid_plan",
        RuntimeFailure::AdmissionClosed => "child_runtime_admission_closed",
        RuntimeFailure::ResourceExhausted { .. } => "child_runtime_resource_exhausted",
        RuntimeFailure::DeadlineExceeded { .. } => "child_runtime_deadline_exceeded",
        RuntimeFailure::Cancelled { .. } => "subagent_cancelled",
        RuntimeFailure::Internal { .. } => "child_runtime_internal",
        RuntimeFailure::PluginFailure { .. } => "child_runtime_plugin_failure",
        RuntimeFailure::PluginRestartExhausted { .. } => "child_runtime_restart_exhausted",
    }
}

async fn execute_delegation(
    agent: Port<agent_contract::AgentClient>,
    context: InvocationContext,
    task: String,
    task_bytes: usize,
    max_output_bytes: usize,
    task_id: Option<String>,
    background: Option<(Rc<RefCell<SubagentTask>>, CancellationToken)>,
) -> Result<Result<ExecuteResponse, ExecuteError>, RuntimeFailure> {
    let mut progress = ChildRunProgress {
        task_id,
        ..ChildRunProgress::default()
    };
    let parent_cancellation = background.as_ref().map(|(_, cancellation)| cancellation);
    let stream = match open_child_stream(&agent, context, task, parent_cancellation).await {
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
    let stream = Rc::new(stream);
    if let Some((background_task, _)) = &background {
        background_task.borrow_mut().attach_stream(stream.clone());
    }
    stream.close_send().await?;
    loop {
        match receive_child_event(&stream, parent_cancellation).await? {
            StreamEvent::Message(message) => {
                if let Err(error) = progress.observe_message(&message, task_bytes, max_output_bytes)
                {
                    return Ok(Err(error));
                }
                if let Some((background_task, _)) = &background
                    && let Some(session_id) = progress.child_session_id.as_deref()
                {
                    background_task.borrow_mut().observe_session(session_id);
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

async fn open_child_stream(
    agent: &Port<agent_contract::AgentClient>,
    context: InvocationContext,
    task: String,
    parent_cancellation: Option<&CancellationToken>,
) -> Result<NativeStream<agent_contract::Agent>, AgentInvocationError> {
    let child_cancellation = context.cancellation();
    let request_id = context.request_id();
    let open = Box::pin(agent.run_turn_with_context(
        context,
        RunTurnRequest {
            input: task,
            session_id: None,
        },
    ));
    let Some(parent_cancellation) = parent_cancellation else {
        return open.await;
    };
    let cancelled = Box::pin(parent_cancellation.cancelled());
    match select(open, cancelled).await {
        Either::Left((result, _)) => result,
        Either::Right(((), _)) => {
            child_cancellation.cancel();
            Err(AgentInvocationError::Runtime(RuntimeFailure::Cancelled {
                request_id,
            }))
        }
    }
}

async fn receive_child_event(
    stream: &Rc<NativeStream<agent_contract::Agent>>,
    parent_cancellation: Option<&CancellationToken>,
) -> Result<StreamEvent<agent_contract::RunTurnResponse, RunTurnError>, RuntimeFailure> {
    let Some(parent_cancellation) = parent_cancellation else {
        return stream.receive().await;
    };
    let receive = Box::pin(stream.receive());
    let cancelled = Box::pin(parent_cancellation.cancelled());
    match select(receive, cancelled).await {
        Either::Left((result, _)) => result,
        Either::Right(((), _)) => {
            stream.cancel();
            Err(RuntimeFailure::Cancelled {
                request_id: stream.request_id(),
            })
        }
    }
}

fn map_turn_input_error(error: &TurnInputInvocationError, task_id: &str) -> ExecuteError {
    let (reason_code, message) = match error {
        TurnInputInvocationError::Domain(SubmitError::InvalidInput) => (
            "subagent_input_invalid",
            "The additional child-Agent input is invalid",
        ),
        TurnInputInvocationError::Domain(SubmitError::TurnNotActive | SubmitError::InputClosed) => {
            (
                "subagent_task_not_running",
                "The child-Agent task no longer accepts input",
            )
        }
        TurnInputInvocationError::Domain(SubmitError::Unknown(_)) => (
            "subagent_input_rejected",
            "The child-Agent input provider rejected the input",
        ),
        TurnInputInvocationError::Runtime(_) => (
            "subagent_input_runtime_failure",
            "The child-Agent input provider failed before durable acceptance",
        ),
    };
    execution_failed(
        reason_code,
        message,
        &task_metadata(Some(task_id), "input_rejected"),
    )
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
    task_id: Option<String>,
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
            "task_id": self.task_id,
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
            task_id: None,
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
                max_tasks: 64,
            })
            .is_ok()
        );
        assert!(
            validate_config(&SubagentToolsConfig {
                max_output_bytes: 0,
                max_task_bytes: 262_144,
                max_tasks: 8,
            })
            .is_err()
        );
        assert!(
            validate_config(&SubagentToolsConfig {
                max_output_bytes: 1_048_577,
                max_task_bytes: 262_145,
                max_tasks: 65,
            })
            .is_err()
        );
    }

    #[test]
    fn task_snapshot_is_non_destructive_and_tracks_lifecycle() {
        let mut task = SubagentTask::new();
        assert_eq!(task.snapshot("task-1")["status"], "running");

        task.observe_session("session-1");
        assert_eq!(task.snapshot("task-1")["child_session_id"], "session-1");
        assert_eq!(task.request_cancel(), "cancellation_requested");
        assert_eq!(task.snapshot("task-1")["status"], "cancellation_requested");

        task.terminal = Some(SubagentTaskTerminal::Cancelled);
        assert_eq!(task.snapshot("task-1")["status"], "cancelled");
        assert!(matches!(
            task.terminal,
            Some(SubagentTaskTerminal::Cancelled)
        ));
    }

    #[test]
    fn detached_child_cancellation_does_not_cancel_parent() {
        let parent_cancellation = CancellationToken::new();
        let parent = InvocationContext::new(7, None, parent_cancellation.clone())
            .with_extension(
                "lenso.app.generation-spec-digest@1",
                b"sha256:test".to_vec(),
            )
            .unwrap();

        let child = detached_child_context(&parent).unwrap();
        child.cancellation().cancel();

        assert!(!parent_cancellation.is_cancelled());
        assert!(child.cancellation().is_cancelled());
        assert_eq!(
            child.extension("lenso.app.generation-spec-digest@1"),
            Some(b"sha256:test".as_slice())
        );
    }

    #[test]
    fn cancellation_before_child_stream_open_is_terminal_and_waitable() {
        let task = Rc::new(RefCell::new(SubagentTask::new()));
        let child_cancellation = CancellationToken::new();
        task.borrow_mut()
            .attach_cancellation(child_cancellation.clone());
        assert_eq!(task.borrow_mut().request_cancel(), "cancellation_requested");
        assert!(child_cancellation.is_cancelled());
        task.borrow_mut()
            .complete(Err(RuntimeFailure::Cancelled { request_id: 7 }));

        let terminal = futures::executor::block_on(wait_for_task(task));
        assert!(matches!(terminal, SubagentTaskTerminal::Cancelled));
    }
}

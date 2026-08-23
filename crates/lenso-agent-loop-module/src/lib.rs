//! Agent Loop Module.

use std::{cell::Cell, cell::RefCell, rc::Rc};

use futures::future::LocalBoxFuture;
use lenso_agent_native_support::FiniteOutputStream;
use lenso_capability_agent::{
    AgentEndpoint, AgentInvocationError, AgentProvider, CAPABILITY_ID, RunTurnError,
    RunTurnRequest, RunTurnResponse,
};
use lenso_capability_agent_model::{
    CompleteRequest, CompleteRequestMessagesItem, CompleteRequestMessagesItemRole,
    CompleteRequestToolsItem, CompleteResponse, CompleteResponseKind, ModelClient, ModelEvent,
};
use lenso_capability_agent_session::{
    AppendError, AppendRequest, AppendRequestEventsItem, AppendRequestEventsItemKind, OpenError,
    OpenRequest, SessionAppendInvocationError, SessionClient, SessionOpenInvocationError,
};
use lenso_capability_agent_tools::{
    CatalogRequest, ExecuteRequest, ToolsClient, ToolsExecuteInvocationError,
};
use lenso_kernel::{
    ActivateContext, InvocationContext, ModuleFuture, ModuleLifecycle, NativeStreamEndpoint,
    NativeStreamSession, RuntimeFailure, StreamEvent,
};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Runtime package identity selected by App Composition.
pub const PACKAGE_ID: &str = "lenso.agent.loop";
/// Exact linked package version.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentConfig {
    model: String,
    max_steps: u32,
    max_tool_calls: u32,
    max_output_tokens: i64,
}

/// Native factory for one Agent Loop generation.
#[derive(Clone, Debug, Default)]
pub struct AgentLoopFactory;

impl NativeModuleFactory for AgentLoopFactory {
    fn package_id(&self) -> &'static str {
        PACKAGE_ID
    }

    fn package_version(&self) -> &'static str {
        PACKAGE_VERSION
    }

    fn instantiate(
        &self,
        context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        if context.entrypoint() != "default" {
            return Err(invalid_plan("unsupported Agent Loop entrypoint"));
        }
        let config = serde_json::from_str::<AgentConfig>(context.configuration())
            .map_err(|error| invalid_plan(format!("invalid Agent Loop configuration: {error}")))?;
        if config.model.is_empty()
            || config.max_steps == 0
            || config.max_tool_calls == 0
            || config.max_output_tokens <= 0
        {
            return Err(invalid_plan("Agent Loop limits and model must be non-zero"));
        }
        let clients = Rc::new(RefCell::new(None));
        let active = Rc::new(Cell::new(false));
        let endpoint = Rc::new(AgentEndpoint::new(AgentLoop {
            config,
            clients: clients.clone(),
            active,
        })) as Rc<dyn NativeStreamEndpoint>;
        Ok(NativeModuleInstance::with_stream_endpoints(
            vec![endpoint],
            AgentLifecycle { clients },
        ))
    }
}

#[derive(Debug)]
struct AgentClients {
    model: ModelClient,
    tools: ToolsClient,
    session: SessionClient,
}

#[derive(Clone, Debug)]
struct AgentLoop {
    config: AgentConfig,
    clients: Rc<RefCell<Option<Rc<AgentClients>>>>,
    active: Rc<Cell<bool>>,
}

impl AgentProvider for AgentLoop {
    fn run_turn(
        &self,
        context: InvocationContext,
        request: RunTurnRequest,
    ) -> LocalBoxFuture<'static, Result<Box<dyn NativeStreamSession>, AgentInvocationError>> {
        if request.input.trim().is_empty() {
            return Box::pin(futures::future::ready(Err(AgentInvocationError::Domain(
                RunTurnError::ContextLimitExceeded,
            ))));
        }
        if self.active.replace(true) {
            return Box::pin(futures::future::ready(Err(AgentInvocationError::Domain(
                RunTurnError::ConcurrentTurn,
            ))));
        }
        let Some(clients) = self.clients.borrow().clone() else {
            self.active.set(false);
            return Box::pin(futures::future::ready(Err(AgentInvocationError::Runtime(
                RuntimeFailure::Unavailable {
                    capability: CAPABILITY_ID,
                },
            ))));
        };
        let config = self.config.clone();
        let active = self.active.clone();
        Box::pin(async move {
            let _turn = ActiveTurn(active);
            let messages = run_turn(&clients, &config, context, request).await?;
            Ok(
                Box::new(FiniteOutputStream::successful(CAPABILITY_ID, messages))
                    as Box<dyn NativeStreamSession>,
            )
        })
    }
}

#[derive(Debug)]
struct ActiveTurn(Rc<Cell<bool>>);

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

#[derive(Debug)]
struct AgentLifecycle {
    clients: Rc<RefCell<Option<Rc<AgentClients>>>>,
}

impl ModuleLifecycle for AgentLifecycle {
    fn activate(&self, context: ActivateContext) -> ModuleFuture {
        let clients = AgentClients {
            model: match ModelClient::from_dependencies(context.dependencies()) {
                Ok(client) => client,
                Err(error) => return Box::pin(futures::future::ready(Err(error))),
            },
            tools: match ToolsClient::from_dependencies(context.dependencies()) {
                Ok(client) => client,
                Err(error) => return Box::pin(futures::future::ready(Err(error))),
            },
            session: match SessionClient::from_dependencies(context.dependencies()) {
                Ok(client) => client,
                Err(error) => return Box::pin(futures::future::ready(Err(error))),
            },
        };
        self.clients.replace(Some(Rc::new(clients)));
        Box::pin(futures::future::ready(Ok(())))
    }
}

#[allow(clippy::too_many_lines)]
async fn run_turn(
    clients: &AgentClients,
    config: &AgentConfig,
    context: InvocationContext,
    request: RunTurnRequest,
) -> Result<Vec<RunTurnResponse>, AgentInvocationError> {
    if config.max_steps < 2 {
        return Err(AgentInvocationError::Domain(
            RunTurnError::StepLimitExceeded,
        ));
    }
    let opened = clients
        .session
        .open_with_context(
            context.clone(),
            OpenRequest {
                session_id: request.session_id,
            },
        )
        .await
        .map_err(map_session_open_error)?;
    let session_id = opened.session_id;
    let turn_id = uuid::Uuid::new_v4().to_string();
    let mut revision = opened.revision;
    let mut initial_events = Vec::new();
    if opened.created {
        initial_events.push(session_event(
            AppendRequestEventsItemKind::SessionCreated,
            None,
            &serde_json::json!({"session_id": session_id}),
        )?);
    }
    initial_events.push(session_event(
        AppendRequestEventsItemKind::TurnStarted,
        Some(&turn_id),
        &serde_json::json!({"input": request.input}),
    )?);
    revision = append_events(clients, &context, &session_id, revision, initial_events).await?;

    let catalog = clients
        .tools
        .catalog_with_context(context.clone(), CatalogRequest {})
        .await
        .map_err(|error| {
            AgentInvocationError::Runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Tool catalog failed: {error:?}"),
            })
        })?;
    let tools = catalog
        .tools
        .into_iter()
        .map(|tool| CompleteRequestToolsItem {
            name: tool.name,
            description: tool.description,
            input_schema_json: tool.input_schema_json,
        })
        .collect::<Vec<_>>();
    revision = append_events(
        clients,
        &context,
        &session_id,
        revision,
        vec![session_event(
            AppendRequestEventsItemKind::ModelRequested,
            Some(&turn_id),
            &serde_json::json!({"step": 1}),
        )?],
    )
    .await?;
    let first = collect_model(
        clients,
        &context,
        CompleteRequest {
            model: config.model.clone(),
            messages: vec![CompleteRequestMessagesItem {
                role: CompleteRequestMessagesItemRole::User,
                content: request.input,
                tool_call_id: None,
                tool_name: None,
                arguments_json: None,
            }],
            tools: tools.clone(),
            temperature: 0.0,
            max_output_tokens: config.max_output_tokens,
        },
    )
    .await?;
    let tool_call = first
        .iter()
        .find(|message| message.kind == CompleteResponseKind::ToolCall)
        .ok_or_else(|| {
            AgentInvocationError::Runtime(RuntimeFailure::ModuleFailure {
                detail: "fixture Model did not request a Tool".to_owned(),
            })
        })?;
    if config.max_tool_calls < 1 {
        return Err(AgentInvocationError::Domain(
            RunTurnError::ToolCallLimitExceeded,
        ));
    }
    revision = append_events(
        clients,
        &context,
        &session_id,
        revision,
        vec![session_event(
            AppendRequestEventsItemKind::ToolRequested,
            Some(&turn_id),
            &serde_json::json!({"name": tool_call.tool_name, "arguments_json": tool_call.arguments_json}),
        )?],
    )
    .await?;
    let tool_result = clients
        .tools
        .execute_with_context(
            context.clone(),
            ExecuteRequest {
                name: tool_call.tool_name.clone(),
                arguments_json: tool_call.arguments_json.clone(),
            },
        )
        .await
        .map_err(map_tools_error)?;
    revision = append_events(
        clients,
        &context,
        &session_id,
        revision,
        vec![session_event(
            AppendRequestEventsItemKind::ToolResult,
            Some(&turn_id),
            &serde_json::json!({"name": tool_call.tool_name, "metadata_json": tool_result.metadata_json}),
        )?],
    )
    .await?;
    let final_messages = collect_model(
        clients,
        &context,
        CompleteRequest {
            model: config.model.clone(),
            messages: vec![
                CompleteRequestMessagesItem {
                    role: CompleteRequestMessagesItemRole::Assistant,
                    content: String::new(),
                    tool_call_id: Some(tool_call.tool_call_id.clone()),
                    tool_name: Some(tool_call.tool_name.clone()),
                    arguments_json: Some(tool_call.arguments_json.clone()),
                },
                CompleteRequestMessagesItem {
                    role: CompleteRequestMessagesItemRole::Tool,
                    content: tool_result.content,
                    tool_call_id: Some(tool_call.tool_call_id.clone()),
                    tool_name: None,
                    arguments_json: None,
                },
            ],
            tools,
            temperature: 0.0,
            max_output_tokens: config.max_output_tokens,
        },
    )
    .await?;
    let responses = final_messages
        .into_iter()
        .filter(|message| message.kind == CompleteResponseKind::TextDelta)
        .enumerate()
        .map(|(index, message)| RunTurnResponse {
            sequence: (index + 1).to_string(),
            session_id: Some(session_id.clone()),
            text: message.text,
        })
        .collect::<Vec<_>>();
    let output = responses
        .iter()
        .map(|response| response.text.as_str())
        .collect::<String>();
    let _revision = append_events(
        clients,
        &context,
        &session_id,
        revision,
        vec![
            session_event(
                AppendRequestEventsItemKind::ModelOutput,
                Some(&turn_id),
                &serde_json::json!({"text": output}),
            )?,
            session_event(
                AppendRequestEventsItemKind::TurnCompleted,
                Some(&turn_id),
                &serde_json::json!({"output": output}),
            )?,
        ],
    )
    .await?;
    Ok(responses)
}

async fn collect_model(
    clients: &AgentClients,
    context: &InvocationContext,
    request: CompleteRequest,
) -> Result<Vec<CompleteResponse>, AgentInvocationError> {
    let stream = clients
        .model
        .complete_with_context(context.clone(), request)
        .await
        .map_err(|error| {
            AgentInvocationError::Runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Model completion failed: {error:?}"),
            })
        })?;
    stream
        .close_send()
        .await
        .map_err(AgentInvocationError::Runtime)?;
    let mut messages = Vec::new();
    loop {
        match stream
            .receive()
            .await
            .map_err(AgentInvocationError::Runtime)?
        {
            ModelEvent::Message(message) => messages.push(message),
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => return Ok(messages),
            StreamEvent::Terminal(Err(error)) => {
                return Err(AgentInvocationError::Runtime(
                    RuntimeFailure::ModuleFailure {
                        detail: format!("Model stream failed: {error:?}"),
                    },
                ));
            }
        }
    }
}

async fn append_events(
    clients: &AgentClients,
    context: &InvocationContext,
    session_id: &str,
    expected_revision: String,
    events: Vec<AppendRequestEventsItem>,
) -> Result<String, AgentInvocationError> {
    clients
        .session
        .append_with_context(
            context.clone(),
            AppendRequest {
                session_id: session_id.to_owned(),
                expected_revision,
                events,
            },
        )
        .await
        .map(|response| response.revision)
        .map_err(map_session_append_error)
}

fn session_event(
    kind: AppendRequestEventsItemKind,
    turn_id: Option<&str>,
    payload: &serde_json::Value,
) -> Result<AppendRequestEventsItem, AgentInvocationError> {
    Ok(AppendRequestEventsItem {
        event_id: uuid::Uuid::new_v4().to_string(),
        kind,
        turn_id: turn_id.map(ToOwned::to_owned),
        occurred_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| {
                AgentInvocationError::Runtime(RuntimeFailure::Internal {
                    detail: format!("failed to format event timestamp: {error}"),
                })
            })?,
        payload_json: payload.to_string(),
    })
}

fn map_session_open_error(error: SessionOpenInvocationError) -> AgentInvocationError {
    match error {
        SessionOpenInvocationError::Domain(OpenError::InvalidSessionId | OpenError::NotFound) => {
            AgentInvocationError::Domain(RunTurnError::InvalidSession)
        }
        SessionOpenInvocationError::Domain(error) => {
            AgentInvocationError::Runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Session open failed: {error:?}"),
            })
        }
        SessionOpenInvocationError::Runtime(error) => AgentInvocationError::Runtime(error),
    }
}

fn map_session_append_error(error: SessionAppendInvocationError) -> AgentInvocationError {
    match error {
        SessionAppendInvocationError::Domain(AppendError::RevisionConflict { .. }) => {
            AgentInvocationError::Domain(RunTurnError::ConcurrentTurn)
        }
        SessionAppendInvocationError::Domain(error) => {
            AgentInvocationError::Runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Session append failed: {error:?}"),
            })
        }
        SessionAppendInvocationError::Runtime(error) => AgentInvocationError::Runtime(error),
    }
}

fn map_tools_error(error: ToolsExecuteInvocationError) -> AgentInvocationError {
    match error {
        ToolsExecuteInvocationError::Domain(error) => {
            AgentInvocationError::Runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Tool execution failed: {error:?}"),
            })
        }
        ToolsExecuteInvocationError::Runtime(error) => AgentInvocationError::Runtime(error),
    }
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

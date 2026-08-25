//! Agent Loop Module.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fmt,
    rc::Rc,
};

use futures::{
    SinkExt, StreamExt,
    channel::mpsc,
    future::{LocalBoxFuture, ready},
    lock::Mutex,
};
use lenso_capability_agent::{
    AgentEndpoint, AgentInvocationError, AgentProvider, CAPABILITY_ID, RunTurnError,
    RunTurnRequest, RunTurnResponse,
};
use lenso_capability_agent_model::{
    CompleteRequest, CompleteRequestMessagesItem, CompleteRequestMessagesItemRole,
    CompleteRequestToolsItem, CompleteResponse, CompleteResponseKind, ModelClient, ModelEvent,
};
use lenso_capability_agent_prompt::{AssembleRequest, PromptClient, PromptInvocationError};
use lenso_capability_agent_session::{
    AppendError, AppendRequest, AppendRequestEventsItem, AppendRequestEventsItemKind, OpenError,
    OpenRequest, ReadError, ReadRequest, ReadResponseEventsItem, ReadResponseEventsItemKind,
    SessionAppendInvocationError, SessionClient, SessionOpenInvocationError,
    SessionReadInvocationError,
};
use lenso_capability_agent_tools::{
    CatalogRequest, ExecuteRequest, ToolsClient, ToolsExecuteInvocationError,
};
use lenso_kernel::{
    ActivateContext, CancellationToken, InvocationContext, ManagedTaskScope, ModuleFuture,
    ModuleLifecycle, NativeStreamEndpoint, NativeStreamItem, NativeStreamSession, RuntimeFailure,
    StreamEvent,
};
use lenso_module::Port;
use lenso_native_adapter::{NativeModuleFactoryContext, NativeModuleInstance};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Host-issued Invocation Context key for the leased App Generation identity.
pub const GENERATION_SPEC_DIGEST_EXTENSION: &str = "lenso.app.generation-spec-digest@1";

/// One validated Turn-to-Generation provenance reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnGenerationProvenance {
    /// Durable Session revision of the `turn_started` event.
    pub revision: u64,
    /// Stable Turn identity.
    pub turn_id: String,
    /// Exact content-addressed App Generation Spec digest.
    pub generation_spec_digest: String,
}

/// Interpret one `turn_started` payload owned by this Agent Loop.
pub fn inspect_turn_generation_provenance(
    revision: u64,
    turn_id: Option<&str>,
    payload_json: &str,
) -> Result<TurnGenerationProvenance, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TurnStartedPayload {
        generation_spec_digest: String,
        input: String,
    }
    let payload = serde_json::from_str::<TurnStartedPayload>(payload_json)
        .map_err(|error| format!("Turn provenance payload is invalid: {error}"))?;
    let _ = payload.input;
    if !canonical_generation_digest(&payload.generation_spec_digest) {
        return Err("Turn Generation Spec digest is invalid".to_owned());
    }
    Ok(TurnGenerationProvenance {
        revision,
        turn_id: turn_id
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "Turn provenance has no Turn ID".to_owned())?
            .to_owned(),
        generation_spec_digest: payload.generation_spec_digest,
    })
}

fn canonical_generation_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentConfig {
    model: String,
    max_steps: u32,
    max_tool_calls: u32,
    max_output_tokens: i64,
    max_history_events: i64,
}

/// Instantiates one Agent Loop generation.
#[lenso_native_adapter::module(
    descriptor = r#"{"provided_capabilities":[{"capability_id":"lenso.agent@1","descriptor_version":"1.1.0","operations":["run_turn"],"operation_kinds":{"run_turn":"stream"},"default_admission":{"queue_capacity":0,"max_concurrency":1},"operation_admissions":{},"event_admission":null,"cross_lane_transfer":false}],"required_capabilities":[{"capability_id":"lenso.agent.model@1","descriptor_version":"1.1.0","cardinality":"one"},{"capability_id":"lenso.agent.prompt@1","descriptor_version":"1.0.0","cardinality":"one"},{"capability_id":"lenso.agent.tools@1","descriptor_version":"1.0.0","cardinality":"one"},{"capability_id":"lenso.agent.session@1","descriptor_version":"1.1.0","cardinality":"one"}]}"#,
    configuration_schema = "config.schema.json"
)]
fn instantiate(
    context: NativeModuleFactoryContext<'_>,
) -> Result<NativeModuleInstance, RuntimeFailure> {
    if context.entrypoint() != "default" {
        return Err(invalid_plan("unsupported Agent Loop entrypoint"));
    }
    let config = serde_json::from_str::<AgentConfig>(context.configuration())
        .map_err(|error| invalid_plan(format!("invalid Agent Loop configuration: {error}")))?;
    if config.model.is_empty()
        || config.max_steps == 0
        || config.max_steps > 64
        || config.max_tool_calls > 64
        || config.max_output_tokens <= 0
        || !(1..=1000).contains(&config.max_history_events)
    {
        return Err(invalid_plan("Agent Loop model or limits are invalid"));
    }
    let clients = Rc::new(AgentClients::default());
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

#[derive(Debug)]
struct AgentClients {
    model: Port<ModelClient>,
    prompt: Port<PromptClient>,
    tools: Port<ToolsClient>,
    session: Port<SessionClient>,
    tasks: RefCell<Option<ManagedTaskScope>>,
}

impl Default for AgentClients {
    fn default() -> Self {
        Self {
            model: Port::new(),
            prompt: Port::new(),
            tools: Port::new(),
            session: Port::new(),
            tasks: RefCell::new(None),
        }
    }
}

impl AgentClients {
    fn connect(
        &self,
        dependencies: &lenso_kernel::ModuleDependencies,
    ) -> Result<(), RuntimeFailure> {
        self.model.connect(dependencies)?;
        self.prompt.connect(dependencies)?;
        self.tools.connect(dependencies)?;
        self.session.connect(dependencies)
    }
}

#[derive(Clone, Debug)]
struct AgentLoop {
    config: AgentConfig,
    clients: Rc<AgentClients>,
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
        let Some(tasks) = self.clients.tasks.borrow().clone() else {
            self.active.set(false);
            return Box::pin(futures::future::ready(Err(AgentInvocationError::Runtime(
                RuntimeFailure::Unavailable {
                    capability: CAPABILITY_ID,
                },
            ))));
        };
        let config = self.config.clone();
        let active = self.active.clone();
        let cancellation = context.cancellation();
        let (sender, receiver) = mpsc::channel(1);
        let clients = self.clients.clone();
        let task = tasks.spawn_local(Box::pin(async move {
            let _turn = ActiveTurn(active);
            produce_turn(clients, config, context, request, sender).await;
        }));
        match task {
            Ok(_) => Box::pin(ready(Ok(
                Box::new(AgentTurnStream::new(receiver, cancellation))
                    as Box<dyn NativeStreamSession>,
            ))),
            Err(error) => {
                self.active.set(false);
                Box::pin(ready(Err(AgentInvocationError::Runtime(
                    RuntimeFailure::ModuleFailure {
                        detail: format!("Agent turn task failed to start: {error:?}"),
                    },
                ))))
            }
        }
    }
}

#[derive(Debug)]
struct ActiveTurn(Rc<Cell<bool>>);

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

type AgentChannelItem = Result<NativeStreamItem, RuntimeFailure>;

struct AgentTurnStream {
    receiver: Rc<Mutex<mpsc::Receiver<AgentChannelItem>>>,
    cancellation: CancellationToken,
    cancelled: Rc<Cell<bool>>,
    send_closed: Rc<Cell<bool>>,
}

impl fmt::Debug for AgentTurnStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentTurnStream")
            .field("cancelled", &self.cancelled.get())
            .field("send_closed", &self.send_closed.get())
            .finish_non_exhaustive()
    }
}

impl AgentTurnStream {
    fn new(receiver: mpsc::Receiver<AgentChannelItem>, cancellation: CancellationToken) -> Self {
        Self {
            receiver: Rc::new(Mutex::new(receiver)),
            cancellation,
            cancelled: Rc::new(Cell::new(false)),
            send_closed: Rc::new(Cell::new(false)),
        }
    }
}

impl NativeStreamSession for AgentTurnStream {
    fn send(&self, _message: Box<dyn Any>) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        Box::pin(ready(Err(RuntimeFailure::ProtocolViolation {
            capability: CAPABILITY_ID,
        })))
    }

    fn receive(&self) -> LocalBoxFuture<'static, Result<NativeStreamItem, RuntimeFailure>> {
        let receiver = self.receiver.clone();
        let cancelled = self.cancelled.clone();
        Box::pin(async move {
            if cancelled.get() {
                return Err(RuntimeFailure::AdmissionClosed);
            }
            receiver.lock().await.next().await.unwrap_or_else(|| {
                Err(RuntimeFailure::ModuleFailure {
                    detail: "Agent turn ended without a terminal event".to_owned(),
                })
            })
        })
    }

    fn close_send(&self) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        let result = if self.send_closed.replace(true) {
            Err(RuntimeFailure::ProtocolViolation {
                capability: CAPABILITY_ID,
            })
        } else {
            Ok(())
        };
        Box::pin(ready(result))
    }

    fn cancel(&self) {
        self.cancellation.cancel();
        self.cancelled.set(true);
    }
}

#[derive(Debug)]
struct AgentLifecycle {
    clients: Rc<AgentClients>,
}

impl ModuleLifecycle for AgentLifecycle {
    fn activate(&self, context: ActivateContext) -> ModuleFuture {
        if let Err(error) = self.clients.connect(context.dependencies()) {
            return Box::pin(futures::future::ready(Err(error)));
        }
        self.clients.tasks.replace(Some(context.tasks().clone()));
        Box::pin(futures::future::ready(Ok(())))
    }
}

async fn produce_turn(
    clients: Rc<AgentClients>,
    config: AgentConfig,
    context: InvocationContext,
    request: RunTurnRequest,
    mut sender: mpsc::Sender<AgentChannelItem>,
) {
    match run_turn(&clients, &config, &context, request, &mut sender).await {
        Ok(()) => {
            let _ = sender.send(Ok(NativeStreamItem::PeerHalfClosed)).await;
            let _ = sender.send(Ok(NativeStreamItem::Terminal(Ok(())))).await;
        }
        Err(AgentInvocationError::Domain(error)) => {
            let _ = sender.send(Ok(NativeStreamItem::PeerHalfClosed)).await;
            let _ = sender
                .send(Ok(NativeStreamItem::Terminal(Err(
                    Box::new(error) as Box<dyn Any>
                ))))
                .await;
        }
        Err(AgentInvocationError::Runtime(error)) => {
            let _ = sender.send(Err(error)).await;
        }
    }
}

async fn run_turn(
    clients: &AgentClients,
    config: &AgentConfig,
    context: &InvocationContext,
    request: RunTurnRequest,
    sender: &mut mpsc::Sender<AgentChannelItem>,
) -> Result<(), AgentInvocationError> {
    let generation_spec_digest = generation_spec_digest(context)?;
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
    let mut messages = if opened.created {
        Vec::new()
    } else {
        read_history(clients, context, &session_id, &opened.revision, config).await?
    };
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
        &serde_json::json!({
            "generation_spec_digest": generation_spec_digest,
            "input": request.input
        }),
    )?);
    revision = append_events(clients, context, &session_id, revision, initial_events).await?;
    messages.push(user_message(request.input));

    let result = execute_steps(
        clients,
        config,
        context,
        &session_id,
        &turn_id,
        &mut revision,
        messages,
        sender,
    )
    .await;
    if let Err(error) = &result {
        record_turn_failure(clients, context, &session_id, &turn_id, revision, error).await;
    }
    result
}

fn generation_spec_digest(context: &InvocationContext) -> Result<&str, AgentInvocationError> {
    let digest = context
        .extension(GENERATION_SPEC_DIGEST_EXTENSION)
        .and_then(|value| std::str::from_utf8(value).ok())
        .filter(|value| {
            value.strip_prefix("sha256:").is_some_and(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        })
        .ok_or_else(|| {
            AgentInvocationError::Runtime(RuntimeFailure::ModuleFailure {
                detail: "Agent Turn is missing canonical Generation provenance".to_owned(),
            })
        })?;
    Ok(digest)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn execute_steps(
    clients: &AgentClients,
    config: &AgentConfig,
    context: &InvocationContext,
    session_id: &str,
    turn_id: &str,
    revision: &mut String,
    mut messages: Vec<CompleteRequestMessagesItem>,
    sender: &mut mpsc::Sender<AgentChannelItem>,
) -> Result<(), AgentInvocationError> {
    let prompt = clients
        .prompt
        .assemble_with_context(context.clone(), AssembleRequest {})
        .await
        .map_err(map_prompt_error)?;
    if !prompt.content.is_empty() {
        messages.insert(
            0,
            CompleteRequestMessagesItem {
                role: CompleteRequestMessagesItemRole::System,
                content: prompt.content,
                tool_call_id: None,
                tool_name: None,
                arguments_json: None,
            },
        );
    }
    let prompt_contributions = prompt.contributions;
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
    let mut tool_call_count = 0_u32;
    let mut sequence = 0_u64;
    let mut remaining_output_tokens = config.max_output_tokens;
    let mut turn_output = String::new();

    for step in 1..=config.max_steps {
        *revision = append_events(
            clients,
            context,
            session_id,
            revision.clone(),
            vec![session_event(
                AppendRequestEventsItemKind::ModelRequested,
                Some(turn_id),
                &serde_json::json!({
                    "step": step,
                    "prompt_contributions": prompt_contributions
                }),
            )?],
        )
        .await?;
        let completion = stream_model(
            clients,
            context,
            CompleteRequest {
                model: config.model.clone(),
                messages: messages.clone(),
                tools: tools.clone(),
                temperature: 0.0,
                max_output_tokens: remaining_output_tokens,
            },
            session_id,
            &mut sequence,
            sender,
        )
        .await?;
        if let Some(output_tokens) = completion.output_tokens {
            let used = i64::try_from(output_tokens).unwrap_or(i64::MAX);
            remaining_output_tokens = remaining_output_tokens.saturating_sub(used);
        }
        turn_output.push_str(&completion.text);
        if completion.tool_calls.is_empty() {
            if completion.text.is_empty() {
                return Err(AgentInvocationError::Runtime(
                    RuntimeFailure::ModuleFailure {
                        detail: "Model completed without text or a Tool call".to_owned(),
                    },
                ));
            }
            *revision = append_events(
                clients,
                context,
                session_id,
                revision.clone(),
                vec![
                    session_event(
                        AppendRequestEventsItemKind::ModelOutput,
                        Some(turn_id),
                        &serde_json::json!({"text": completion.text}),
                    )?,
                    session_event(
                        AppendRequestEventsItemKind::TurnCompleted,
                        Some(turn_id),
                        &serde_json::json!({"output": turn_output}),
                    )?,
                ],
            )
            .await?;
            return Ok(());
        }
        if !completion.text.is_empty() {
            *revision = append_events(
                clients,
                context,
                session_id,
                revision.clone(),
                vec![session_event(
                    AppendRequestEventsItemKind::ModelOutput,
                    Some(turn_id),
                    &serde_json::json!({"step": step, "text": completion.text}),
                )?],
            )
            .await?;
            messages.push(CompleteRequestMessagesItem {
                role: CompleteRequestMessagesItemRole::Assistant,
                content: completion.text.clone(),
                tool_call_id: None,
                tool_name: None,
                arguments_json: None,
            });
        }
        if step == config.max_steps {
            return Err(AgentInvocationError::Domain(
                RunTurnError::StepLimitExceeded,
            ));
        }
        let requested = u32::try_from(completion.tool_calls.len()).unwrap_or(u32::MAX);
        if tool_call_count.saturating_add(requested) > config.max_tool_calls {
            return Err(AgentInvocationError::Domain(
                RunTurnError::ToolCallLimitExceeded,
            ));
        }
        if remaining_output_tokens <= 0 {
            return Err(AgentInvocationError::Domain(
                RunTurnError::ContextLimitExceeded,
            ));
        }
        tool_call_count = tool_call_count.saturating_add(requested);
        for tool_call in completion.tool_calls {
            *revision = append_events(
                clients,
                context,
                session_id,
                revision.clone(),
                vec![session_event(
                    AppendRequestEventsItemKind::ToolRequested,
                    Some(turn_id),
                    &serde_json::json!({
                        "call_id": tool_call.tool_call_id,
                        "name": tool_call.tool_name,
                        "arguments_json": tool_call.arguments_json
                    }),
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
            *revision = append_events(
                clients,
                context,
                session_id,
                revision.clone(),
                vec![session_event(
                    AppendRequestEventsItemKind::ToolResult,
                    Some(turn_id),
                    &serde_json::json!({
                        "call_id": tool_call.tool_call_id,
                        "name": tool_call.tool_name,
                        "metadata_json": tool_result.metadata_json
                    }),
                )?],
            )
            .await?;
            messages.push(assistant_tool_message(&tool_call));
            messages.push(CompleteRequestMessagesItem {
                role: CompleteRequestMessagesItemRole::Tool,
                content: tool_result.content,
                tool_call_id: Some(tool_call.tool_call_id),
                tool_name: None,
                arguments_json: None,
            });
        }
    }
    Err(AgentInvocationError::Domain(
        RunTurnError::StepLimitExceeded,
    ))
}

#[derive(Debug)]
struct ModelStep {
    text: String,
    tool_calls: Vec<CompleteResponse>,
    output_tokens: Option<u64>,
}

async fn stream_model(
    clients: &AgentClients,
    context: &InvocationContext,
    request: CompleteRequest,
    session_id: &str,
    sequence: &mut u64,
    sender: &mut mpsc::Sender<AgentChannelItem>,
) -> Result<ModelStep, AgentInvocationError> {
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
    let mut completion = ModelStep {
        text: String::new(),
        tool_calls: Vec::new(),
        output_tokens: None,
    };
    loop {
        match stream
            .receive()
            .await
            .map_err(AgentInvocationError::Runtime)?
        {
            ModelEvent::Message(message) => match message.kind {
                CompleteResponseKind::TextDelta => {
                    completion.text.push_str(&message.text);
                    *sequence = sequence.saturating_add(1);
                    send_agent_message(
                        sender,
                        RunTurnResponse {
                            sequence: sequence.to_string(),
                            session_id: Some(session_id.to_owned()),
                            text: message.text,
                        },
                        context.request_id(),
                    )
                    .await?;
                }
                CompleteResponseKind::ToolCall => completion.tool_calls.push(message),
                CompleteResponseKind::Usage => {
                    completion.output_tokens =
                        Some(message.output_tokens.parse().map_err(|_| {
                            AgentInvocationError::Runtime(RuntimeFailure::ModuleFailure {
                                detail: "Model emitted invalid output token usage".to_owned(),
                            })
                        })?);
                }
            },
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => return Ok(completion),
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

async fn send_agent_message(
    sender: &mut mpsc::Sender<AgentChannelItem>,
    message: RunTurnResponse,
    request_id: u64,
) -> Result<(), AgentInvocationError> {
    sender
        .send(Ok(NativeStreamItem::Message(
            Box::new(message) as Box<dyn Any>
        )))
        .await
        .map_err(|_| AgentInvocationError::Runtime(RuntimeFailure::Cancelled { request_id }))
}

async fn read_history(
    clients: &AgentClients,
    context: &InvocationContext,
    session_id: &str,
    current_revision: &str,
    config: &AgentConfig,
) -> Result<Vec<CompleteRequestMessagesItem>, AgentInvocationError> {
    let current_revision = current_revision.parse::<u64>().map_err(|_| {
        AgentInvocationError::Runtime(RuntimeFailure::ModuleFailure {
            detail: "Session returned an invalid revision".to_owned(),
        })
    })?;
    if current_revision == 0 {
        return Ok(Vec::new());
    }
    let history_limit = u64::try_from(config.max_history_events).map_err(|_| {
        AgentInvocationError::Runtime(RuntimeFailure::Internal {
            detail: "Agent history limit conversion failed".to_owned(),
        })
    })?;
    let history = clients
        .session
        .read_with_context(
            context.clone(),
            ReadRequest {
                session_id: session_id.to_owned(),
                after_revision: current_revision.saturating_sub(history_limit).to_string(),
                limit: config.max_history_events,
            },
        )
        .await
        .map_err(map_session_read_error)?;
    reconstruct_history(&history.events)
}

#[derive(Default)]
struct HistoricalTurn {
    input: Option<String>,
    output: Option<String>,
}

fn reconstruct_history(
    events: &[ReadResponseEventsItem],
) -> Result<Vec<CompleteRequestMessagesItem>, AgentInvocationError> {
    let mut turns = BTreeMap::<String, HistoricalTurn>::new();
    let mut turn_order = Vec::new();
    for event in events {
        let Some(turn_id) = event.turn_id.as_ref() else {
            continue;
        };
        match event.kind {
            ReadResponseEventsItemKind::TurnStarted => {
                if !turns.contains_key(turn_id) {
                    turn_order.push(turn_id.clone());
                }
                turns.entry(turn_id.clone()).or_default().input =
                    Some(history_payload_text(event, "input")?);
            }
            ReadResponseEventsItemKind::TurnCompleted => {
                turns.entry(turn_id.clone()).or_default().output =
                    Some(history_payload_text(event, "output")?);
            }
            _ => {}
        }
    }
    let mut messages = Vec::new();
    for turn_id in turn_order {
        let Some(turn) = turns.remove(&turn_id) else {
            continue;
        };
        if let (Some(input), Some(output)) = (turn.input, turn.output) {
            messages.push(user_message(input));
            messages.push(CompleteRequestMessagesItem {
                role: CompleteRequestMessagesItemRole::Assistant,
                content: output,
                tool_call_id: None,
                tool_name: None,
                arguments_json: None,
            });
        }
    }
    Ok(messages)
}

fn history_payload_text(
    event: &ReadResponseEventsItem,
    field: &str,
) -> Result<String, AgentInvocationError> {
    serde_json::from_str::<serde_json::Value>(&event.payload_json)
        .ok()
        .and_then(|payload| payload.get(field)?.as_str().map(ToOwned::to_owned))
        .ok_or_else(|| {
            AgentInvocationError::Runtime(RuntimeFailure::ModuleFailure {
                detail: "Session history contains an invalid Agent event".to_owned(),
            })
        })
}

fn user_message(content: String) -> CompleteRequestMessagesItem {
    CompleteRequestMessagesItem {
        role: CompleteRequestMessagesItemRole::User,
        content,
        tool_call_id: None,
        tool_name: None,
        arguments_json: None,
    }
}

fn assistant_tool_message(tool_call: &CompleteResponse) -> CompleteRequestMessagesItem {
    CompleteRequestMessagesItem {
        role: CompleteRequestMessagesItemRole::Assistant,
        content: String::new(),
        tool_call_id: Some(tool_call.tool_call_id.clone()),
        tool_name: Some(tool_call.tool_name.clone()),
        arguments_json: Some(tool_call.arguments_json.clone()),
    }
}

async fn record_turn_failure(
    clients: &AgentClients,
    context: &InvocationContext,
    session_id: &str,
    turn_id: &str,
    revision: String,
    error: &AgentInvocationError,
) {
    let cancelled = context.is_cancelled();
    let kind = if cancelled {
        AppendRequestEventsItemKind::TurnCancelled
    } else {
        AppendRequestEventsItemKind::TurnFailed
    };
    let Ok(event) = session_event(
        kind,
        Some(turn_id),
        &serde_json::json!({"error": turn_error_code(error, cancelled)}),
    ) else {
        return;
    };
    let request = AppendRequest {
        session_id: session_id.to_owned(),
        expected_revision: revision,
        events: vec![event],
    };
    if cancelled {
        let _ = clients.session.append(request).await;
    } else {
        let _ = clients
            .session
            .append_with_context(context.clone(), request)
            .await;
    }
}

fn turn_error_code(error: &AgentInvocationError, cancelled: bool) -> &'static str {
    if cancelled {
        return "cancelled";
    }
    match error {
        AgentInvocationError::Domain(RunTurnError::ConcurrentTurn) => "concurrent_turn",
        AgentInvocationError::Domain(RunTurnError::ContextLimitExceeded) => {
            "context_limit_exceeded"
        }
        AgentInvocationError::Domain(RunTurnError::InvalidSession) => "invalid_session",
        AgentInvocationError::Domain(RunTurnError::StepLimitExceeded) => "step_limit_exceeded",
        AgentInvocationError::Domain(RunTurnError::ToolCallLimitExceeded) => {
            "tool_call_limit_exceeded"
        }
        AgentInvocationError::Domain(RunTurnError::Unknown(_)) => "unknown_domain_error",
        AgentInvocationError::Runtime(_) => "runtime_failure",
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

fn map_session_read_error(error: SessionReadInvocationError) -> AgentInvocationError {
    match error {
        SessionReadInvocationError::Domain(ReadError::InvalidCursor | ReadError::NotFound) => {
            AgentInvocationError::Domain(RunTurnError::InvalidSession)
        }
        SessionReadInvocationError::Domain(error) => {
            AgentInvocationError::Runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Session read failed: {error:?}"),
            })
        }
        SessionReadInvocationError::Runtime(error) => AgentInvocationError::Runtime(error),
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

fn map_prompt_error(error: PromptInvocationError) -> AgentInvocationError {
    match error {
        PromptInvocationError::Domain(error) => {
            AgentInvocationError::Runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Prompt assembly failed: {error:?}"),
            })
        }
        PromptInvocationError::Runtime(error) => AgentInvocationError::Runtime(error),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn history_event(
        revision: &str,
        kind: ReadResponseEventsItemKind,
        payload_json: &str,
    ) -> ReadResponseEventsItem {
        ReadResponseEventsItem {
            revision: revision.to_owned(),
            event_id: format!("event-{revision}"),
            kind,
            turn_id: Some("turn-1".to_owned()),
            occurred_at: "2026-08-24T00:00:00Z".to_owned(),
            payload_json: payload_json.to_owned(),
        }
    }

    #[test]
    fn completed_turns_reconstruct_as_model_history() {
        let messages = reconstruct_history(&[
            history_event(
                "1",
                ReadResponseEventsItemKind::TurnStarted,
                r#"{"input":"hello"}"#,
            ),
            history_event(
                "2",
                ReadResponseEventsItemKind::TurnCompleted,
                r#"{"output":"world"}"#,
            ),
        ])
        .unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, CompleteRequestMessagesItemRole::User);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, CompleteRequestMessagesItemRole::Assistant);
        assert_eq!(messages[1].content, "world");
    }

    #[test]
    fn stream_cancellation_propagates_to_the_invocation() {
        let (_sender, receiver) = mpsc::channel::<AgentChannelItem>(1);
        let cancellation = CancellationToken::new();
        let stream = AgentTurnStream::new(receiver, cancellation.clone());
        stream.cancel();
        assert!(cancellation.is_cancelled());
    }

    #[test]
    fn generation_provenance_requires_one_canonical_host_extension() {
        let missing = InvocationContext::new(1, None, CancellationToken::new());
        assert!(generation_spec_digest(&missing).is_err());

        let digest = format!("sha256:{}", "a".repeat(64));
        let present = InvocationContext::new(2, None, CancellationToken::new())
            .with_extension(GENERATION_SPEC_DIGEST_EXTENSION, digest.as_bytes().to_vec())
            .unwrap();
        assert_eq!(generation_spec_digest(&present).unwrap(), digest);

        let uppercase = InvocationContext::new(3, None, CancellationToken::new())
            .with_extension(
                GENERATION_SPEC_DIGEST_EXTENSION,
                format!("sha256:{}", "A".repeat(64)).into_bytes(),
            )
            .unwrap();
        assert!(generation_spec_digest(&uppercase).is_err());
    }

    #[test]
    fn turn_provenance_parser_owns_the_exact_agent_event_payload() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let payload = serde_json::json!({
            "generation_spec_digest": digest,
            "input": "hello"
        })
        .to_string();
        let provenance = inspect_turn_generation_provenance(2, Some("turn-1"), &payload).unwrap();
        assert_eq!(provenance.revision, 2);
        assert_eq!(provenance.turn_id, "turn-1");
        assert_eq!(provenance.generation_spec_digest, digest);

        let unknown = serde_json::json!({
            "generation_spec_digest": digest,
            "input": "hello",
            "unexpected": true
        })
        .to_string();
        assert!(inspect_turn_generation_provenance(2, Some("turn-1"), &unknown).is_err());
    }
}

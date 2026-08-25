//! Agent Loop Module.

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use lenso::prelude::*;
use lenso_capability_agent::{
    self as agent_capability, RunTurnError, RunTurnRequest, RunTurnResponse,
};
use lenso_capability_agent_model::{
    self as model_capability, CompleteError, CompleteMessage, CompleteMessageInput,
    CompleteMessageKind, CompleteMessageRole, CompleteOpen, CompleteTool, ModelEvent,
    ModelInvocationError,
};
use lenso_capability_agent_prompt::{
    self as prompt_capability, AssembleRequest, PromptInvocationError,
};
use lenso_capability_agent_session::{
    self as session_capability, AppendError, AppendSessionRequest, AppendSessionRequestEventsItem,
    AppendSessionRequestEventsItemKind, OpenError, OpenSessionRequest, ReadError,
    ReadSessionRequest, ReadSessionResponseEventsItem, ReadSessionResponseEventsItemKind,
    SessionAppendInvocationError, SessionOpenInvocationError, SessionReadInvocationError,
};
use lenso_capability_agent_tools::{
    self as tools_capability, CatalogRequest, ExecuteRequest, ToolsExecuteInvocationError,
};
use lenso_kernel::{InvocationContext, StreamEvent};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Host-issued Invocation Context key for the leased App Generation identity.
pub const GENERATION_SPEC_DIGEST_EXTENSION: &str = "lenso.app.generation-spec-digest@1";
/// Host-issued Invocation Context key for one Turn's narrowed Tool authority.
pub const RUN_SCOPE_EXTENSION: &str = "lenso.agent.run-scope@1";

/// One immutable Turn-local authority scope. Names must come from the Plan-bound Tool catalog.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunScope {
    /// Exact Tool names admitted for this Turn. An empty set disables Tools.
    pub allowed_tools: BTreeSet<String>,
}

impl RunScope {
    /// Creates a deterministic scope from requested Tool names.
    pub fn new(tools: impl IntoIterator<Item = impl Into<String>>) -> Result<Self, String> {
        let mut allowed_tools = BTreeSet::new();
        for tool in tools {
            let tool = tool.into();
            if tool.is_empty() || tool.len() > 128 {
                return Err("Run Scope contains an invalid Tool name".to_owned());
            }
            allowed_tools.insert(tool);
        }
        Ok(Self { allowed_tools })
    }

    /// Attaches this scope to one root Invocation Context.
    pub fn attach(self, context: InvocationContext) -> Result<InvocationContext, String> {
        context
            .with_typed_extension(&self)
            .map_err(|error| format!("failed to attach Run Scope: {error}"))
    }
}

impl TypedExtension for RunScope {
    const KEY: &'static str = RUN_SCOPE_EXTENSION;
}

type TurnFailure = ModuleError<RunTurnError, RuntimeFailure>;
const RECOVERY_EVENT_LIMIT: u64 = 512;

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
        #[serde(default)]
        run_scope: Option<RunScope>,
    }
    let payload = serde_json::from_str::<TurnStartedPayload>(payload_json)
        .map_err(|error| format!("Turn provenance payload is invalid: {error}"))?;
    let _ = payload.input;
    let _ = payload.run_scope;
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

#[derive(Clone, Debug, serde::Deserialize, lenso::ModuleConfig)]
#[serde(deny_unknown_fields)]
struct AgentConfig {
    model: String,
    max_steps: u32,
    max_tool_calls: u32,
    max_output_tokens: i64,
    max_history_events: i64,
}

#[lenso::module(validate = validate_agent_config)]
#[derive(Clone, Debug)]
struct AgentLoop {
    #[config]
    config: AgentConfig,
    model: Port<model_capability::ModelClient>,
    prompt: Port<prompt_capability::PromptClient>,
    tools: Port<tools_capability::ToolsClient>,
    session: Port<session_capability::SessionClient>,
    #[tasks]
    tasks: ManagedTasks,
    active: Rc<Cell<bool>>,
}

fn validate_agent_config(config: &AgentConfig) -> Result<(), RuntimeFailure> {
    if config.model.is_empty()
        || config.max_steps == 0
        || config.max_steps > 64
        || config.max_tool_calls > 64
        || config.max_output_tokens <= 0
        || !(1..=1000).contains(&config.max_history_events)
    {
        return Err(invalid_plan("Agent Loop model or limits are invalid"));
    }
    Ok(())
}

#[lenso::provides(agent_capability::Agent)]
impl AgentLoop {
    async fn run_turn(
        &self,
        context: Ctx,
        request: RunTurnRequest,
    ) -> ModuleResult<ProviderStream<agent_capability::Agent>, RunTurnError> {
        if request.input.trim().is_empty() {
            return Err(ModuleError::domain(RunTurnError::ContextLimitExceeded));
        }
        if self.active.replace(true) {
            return Err(ModuleError::domain(RunTurnError::ConcurrentTurn));
        }
        let active = self.active.clone();
        let (stream, channel) = ProviderStream::channel(&context, 1);
        let module = self.clone();
        let task = self.tasks.spawn_local(async move {
            let _turn = ActiveTurn(active);
            produce_turn(module, context, request, channel).await;
        });
        match task {
            Ok(_) => Ok(stream),
            Err(error) => {
                self.active.set(false);
                Err(ModuleError::runtime(RuntimeFailure::ModuleFailure {
                    detail: format!("Agent turn task failed to start: {error:?}"),
                }))
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

async fn produce_turn(
    module: AgentLoop,
    context: InvocationContext,
    request: RunTurnRequest,
    mut channel: ProviderStreamChannel<agent_capability::Agent>,
) {
    let result = run_turn(&module, &module.config, &context, request, &mut channel).await;
    let _ = channel.complete(result).await;
}

async fn run_turn(
    clients: &AgentLoop,
    config: &AgentConfig,
    context: &InvocationContext,
    request: RunTurnRequest,
    channel: &mut ProviderStreamChannel<agent_capability::Agent>,
) -> Result<(), TurnFailure> {
    let generation_spec_digest = generation_spec_digest(context)?;
    let run_scope = run_scope(context)?;
    let opened = clients
        .session
        .open_with_context(
            context.clone(),
            OpenSessionRequest {
                session_id: request.session_id,
            },
        )
        .await
        .map_err(map_session_open_error)?;
    let session_id = opened.session_id;
    let history = if opened.created {
        Vec::new()
    } else {
        read_session_tail(clients, context, &session_id, &opened.revision, config).await?
    };
    let history_event_count = usize::try_from(config.max_history_events).map_err(|_| {
        ModuleError::runtime(RuntimeFailure::Internal {
            detail: "Agent history limit conversion failed".to_owned(),
        })
    })?;
    let history_start = history.len().saturating_sub(history_event_count);
    let mut messages = reconstruct_history(&history[history_start..])?;
    let turn_id = uuid::Uuid::new_v4().to_string();
    let mut revision = opened.revision;
    let mut initial_events = Vec::new();
    if opened.created {
        initial_events.push(session_event(
            AppendSessionRequestEventsItemKind::SessionCreated,
            None,
            &serde_json::json!({"session_id": session_id}),
        )?);
    }
    initial_events.extend(interrupted_turn_events(&history)?);
    initial_events.push(session_event(
        AppendSessionRequestEventsItemKind::TurnStarted,
        Some(&turn_id),
        &serde_json::json!({
            "generation_spec_digest": generation_spec_digest,
            "input": request.input,
            "run_scope": run_scope.as_ref()
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
        run_scope.as_ref(),
        channel,
    )
    .await;
    if let Err(error) = &result {
        record_turn_failure(clients, context, &session_id, &turn_id, revision, error).await;
    }
    result
}

fn run_scope(context: &InvocationContext) -> Result<Option<RunScope>, TurnFailure> {
    context.typed_extension::<RunScope>().map_err(|error| {
        ModuleError::runtime(RuntimeFailure::ModuleFailure {
            detail: format!("Agent Turn has an invalid Run Scope: {error}"),
        })
    })
}

fn generation_spec_digest(context: &InvocationContext) -> Result<&str, TurnFailure> {
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
            ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: "Agent Turn is missing canonical Generation provenance".to_owned(),
            })
        })?;
    Ok(digest)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn execute_steps(
    clients: &AgentLoop,
    config: &AgentConfig,
    context: &InvocationContext,
    session_id: &str,
    turn_id: &str,
    revision: &mut String,
    mut messages: Vec<CompleteMessageInput>,
    run_scope: Option<&RunScope>,
    channel: &mut ProviderStreamChannel<agent_capability::Agent>,
) -> Result<(), TurnFailure> {
    let prompt = clients
        .prompt
        .assemble_with_context(context.clone(), AssembleRequest {})
        .await
        .map_err(map_prompt_error)?;
    if !prompt.content.is_empty() {
        messages.insert(
            0,
            CompleteMessageInput {
                role: CompleteMessageRole::System,
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
            ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Tool catalog failed: {error:?}"),
            })
        })?;
    let static_tools = catalog
        .tools
        .into_iter()
        .map(|tool| (tool.name.clone(), tool))
        .collect::<BTreeMap<_, _>>();
    if let Some(scope) = run_scope
        && let Some(unknown) = scope
            .allowed_tools
            .iter()
            .find(|name| !static_tools.contains_key(*name))
    {
        return Err(ModuleError::runtime(RuntimeFailure::ModuleFailure {
            detail: format!("Run Scope requests Tool `{unknown}` outside the Plan-bound catalog"),
        }));
    }
    let tools = static_tools
        .values()
        .filter(|tool| run_scope.is_none_or(|scope| scope.allowed_tools.contains(&tool.name)))
        .map(|tool| CompleteTool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema_json: tool.input_schema_json.clone(),
        })
        .collect::<Vec<_>>();
    let admitted_tools = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
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
                AppendSessionRequestEventsItemKind::ModelRequested,
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
            CompleteOpen {
                model: config.model.clone(),
                messages: messages.clone(),
                tools: tools.clone(),
                temperature: 0.0,
                max_output_tokens: remaining_output_tokens,
            },
            session_id,
            &mut sequence,
            channel,
        )
        .await?;
        if let Some(output_tokens) = completion.output_tokens {
            let used = i64::try_from(output_tokens).unwrap_or(i64::MAX);
            remaining_output_tokens = remaining_output_tokens.saturating_sub(used);
        }
        turn_output.push_str(&completion.text);
        if completion.tool_calls.is_empty() {
            if completion.text.is_empty() {
                return Err(ModuleError::runtime(RuntimeFailure::ModuleFailure {
                    detail: "Model completed without text or a Tool call".to_owned(),
                }));
            }
            *revision = append_events(
                clients,
                context,
                session_id,
                revision.clone(),
                vec![
                    session_event(
                        AppendSessionRequestEventsItemKind::ModelOutput,
                        Some(turn_id),
                        &serde_json::json!({"text": completion.text}),
                    )?,
                    session_event(
                        AppendSessionRequestEventsItemKind::TurnCompleted,
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
                    AppendSessionRequestEventsItemKind::ModelOutput,
                    Some(turn_id),
                    &serde_json::json!({"step": step, "text": completion.text}),
                )?],
            )
            .await?;
            messages.push(CompleteMessageInput {
                role: CompleteMessageRole::Assistant,
                content: completion.text.clone(),
                tool_call_id: None,
                tool_name: None,
                arguments_json: None,
            });
        }
        if step == config.max_steps {
            return Err(ModuleError::domain(RunTurnError::StepLimitExceeded));
        }
        let requested = u32::try_from(completion.tool_calls.len()).unwrap_or(u32::MAX);
        if tool_call_count.saturating_add(requested) > config.max_tool_calls {
            return Err(ModuleError::domain(RunTurnError::ToolCallLimitExceeded));
        }
        if remaining_output_tokens <= 0 {
            return Err(ModuleError::domain(RunTurnError::ContextLimitExceeded));
        }
        tool_call_count = tool_call_count.saturating_add(requested);
        for tool_call in completion.tool_calls {
            if !admitted_tools.contains(tool_call.tool_name.as_str()) {
                return Err(ModuleError::runtime(RuntimeFailure::ModuleFailure {
                    detail: format!(
                        "Model requested Tool `{}` outside the immutable Run Scope",
                        tool_call.tool_name
                    ),
                }));
            }
            *revision = append_events(
                clients,
                context,
                session_id,
                revision.clone(),
                vec![session_event(
                    AppendSessionRequestEventsItemKind::ToolRequested,
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
                    AppendSessionRequestEventsItemKind::ToolResult,
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
            messages.push(CompleteMessageInput {
                role: CompleteMessageRole::Tool,
                content: tool_result.content,
                tool_call_id: Some(tool_call.tool_call_id),
                tool_name: None,
                arguments_json: None,
            });
        }
    }
    Err(ModuleError::domain(RunTurnError::StepLimitExceeded))
}

#[derive(Debug)]
struct ModelStep {
    text: String,
    tool_calls: Vec<CompleteMessage>,
    output_tokens: Option<u64>,
}

async fn stream_model(
    clients: &AgentLoop,
    context: &InvocationContext,
    request: CompleteOpen,
    session_id: &str,
    sequence: &mut u64,
    channel: &mut ProviderStreamChannel<agent_capability::Agent>,
) -> Result<ModelStep, TurnFailure> {
    let stream = clients
        .model
        .complete_with_context(context.clone(), request)
        .await
        .map_err(map_model_error)?;
    stream.close_send().await.map_err(ModuleError::runtime)?;
    let mut completion = ModelStep {
        text: String::new(),
        tool_calls: Vec::new(),
        output_tokens: None,
    };
    loop {
        match stream.receive().await.map_err(ModuleError::runtime)? {
            ModelEvent::Message(message) => match message.kind {
                CompleteMessageKind::TextDelta => {
                    completion.text.push_str(&message.text);
                    *sequence = sequence.saturating_add(1);
                    send_agent_message(
                        channel,
                        RunTurnResponse {
                            sequence: sequence.to_string(),
                            session_id: Some(session_id.to_owned()),
                            text: message.text,
                        },
                        context.request_id(),
                    )
                    .await?;
                }
                CompleteMessageKind::ToolCall => completion.tool_calls.push(message),
                CompleteMessageKind::Usage => {
                    completion.output_tokens =
                        Some(message.output_tokens.parse().map_err(|_| {
                            ModuleError::runtime(RuntimeFailure::ModuleFailure {
                                detail: "Model emitted invalid output token usage".to_owned(),
                            })
                        })?);
                }
            },
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => return Ok(completion),
            StreamEvent::Terminal(Err(error)) => return Err(map_model_domain_error(error)),
        }
    }
}

async fn send_agent_message(
    channel: &mut ProviderStreamChannel<agent_capability::Agent>,
    message: RunTurnResponse,
    request_id: u64,
) -> Result<(), TurnFailure> {
    channel
        .send(message)
        .await
        .map_err(|_| ModuleError::runtime(RuntimeFailure::Cancelled { request_id }))
}

async fn read_session_tail(
    clients: &AgentLoop,
    context: &InvocationContext,
    session_id: &str,
    current_revision: &str,
    config: &AgentConfig,
) -> Result<Vec<ReadSessionResponseEventsItem>, TurnFailure> {
    let current_revision = current_revision.parse::<u64>().map_err(|_| {
        ModuleError::runtime(RuntimeFailure::ModuleFailure {
            detail: "Session returned an invalid revision".to_owned(),
        })
    })?;
    if current_revision == 0 {
        return Ok(Vec::new());
    }
    let configured_history_limit = u64::try_from(config.max_history_events).map_err(|_| {
        ModuleError::runtime(RuntimeFailure::Internal {
            detail: "Agent history limit conversion failed".to_owned(),
        })
    })?;
    let history_limit = configured_history_limit.max(RECOVERY_EVENT_LIMIT);
    let history = clients
        .session
        .read_with_context(
            context.clone(),
            ReadSessionRequest {
                session_id: session_id.to_owned(),
                after_revision: current_revision.saturating_sub(history_limit).to_string(),
                limit: i64::try_from(history_limit).map_err(|_| {
                    ModuleError::runtime(RuntimeFailure::Internal {
                        detail: "Agent recovery limit conversion failed".to_owned(),
                    })
                })?,
            },
        )
        .await
        .map_err(map_session_read_error)?;
    Ok(history.events)
}

fn interrupted_turn_events(
    events: &[ReadSessionResponseEventsItem],
) -> Result<Vec<AppendSessionRequestEventsItem>, TurnFailure> {
    let mut open_turns = BTreeSet::new();
    for event in events {
        let Some(turn_id) = event.turn_id.as_ref() else {
            continue;
        };
        match event.kind {
            ReadSessionResponseEventsItemKind::TurnStarted => {
                open_turns.insert(turn_id.clone());
            }
            ReadSessionResponseEventsItemKind::TurnCompleted
            | ReadSessionResponseEventsItemKind::TurnFailed
            | ReadSessionResponseEventsItemKind::TurnCancelled => {
                open_turns.remove(turn_id);
            }
            _ => {}
        }
    }
    open_turns
        .into_iter()
        .map(|turn_id| {
            session_event(
                AppendSessionRequestEventsItemKind::TurnFailed,
                Some(&turn_id),
                &serde_json::json!({"error": "host_interrupted"}),
            )
        })
        .collect()
}

#[derive(Default)]
struct HistoricalTurn {
    input: Option<String>,
    output: Option<String>,
}

fn reconstruct_history(
    events: &[ReadSessionResponseEventsItem],
) -> Result<Vec<CompleteMessageInput>, TurnFailure> {
    let mut turns = BTreeMap::<String, HistoricalTurn>::new();
    let mut turn_order = Vec::new();
    for event in events {
        let Some(turn_id) = event.turn_id.as_ref() else {
            continue;
        };
        match event.kind {
            ReadSessionResponseEventsItemKind::TurnStarted => {
                if !turns.contains_key(turn_id) {
                    turn_order.push(turn_id.clone());
                }
                turns.entry(turn_id.clone()).or_default().input =
                    Some(history_payload_text(event, "input")?);
            }
            ReadSessionResponseEventsItemKind::TurnCompleted => {
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
            messages.push(CompleteMessageInput {
                role: CompleteMessageRole::Assistant,
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
    event: &ReadSessionResponseEventsItem,
    field: &str,
) -> Result<String, TurnFailure> {
    serde_json::from_str::<serde_json::Value>(event.payload_json.as_str())
        .ok()
        .and_then(|payload| payload.get(field)?.as_str().map(ToOwned::to_owned))
        .ok_or_else(|| {
            ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: "Session history contains an invalid Agent event".to_owned(),
            })
        })
}

fn user_message(content: String) -> CompleteMessageInput {
    CompleteMessageInput {
        role: CompleteMessageRole::User,
        content,
        tool_call_id: None,
        tool_name: None,
        arguments_json: None,
    }
}

fn assistant_tool_message(tool_call: &CompleteMessage) -> CompleteMessageInput {
    CompleteMessageInput {
        role: CompleteMessageRole::Assistant,
        content: String::new(),
        tool_call_id: Some(tool_call.tool_call_id.clone()),
        tool_name: Some(tool_call.tool_name.clone()),
        arguments_json: Some(tool_call.arguments_json.clone()),
    }
}

async fn record_turn_failure(
    clients: &AgentLoop,
    context: &InvocationContext,
    session_id: &str,
    turn_id: &str,
    revision: String,
    error: &TurnFailure,
) {
    let cancelled = context.is_cancelled();
    let kind = if cancelled {
        AppendSessionRequestEventsItemKind::TurnCancelled
    } else {
        AppendSessionRequestEventsItemKind::TurnFailed
    };
    let Ok(event) = session_event(
        kind,
        Some(turn_id),
        &serde_json::json!({"error": turn_error_code(error, cancelled)}),
    ) else {
        return;
    };
    let request = AppendSessionRequest {
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

fn turn_error_code(error: &TurnFailure, cancelled: bool) -> &'static str {
    if cancelled {
        return "cancelled";
    }
    match error {
        ModuleError::Domain(RunTurnError::ConcurrentTurn) => "concurrent_turn",
        ModuleError::Domain(RunTurnError::ContextLimitExceeded) => "context_limit_exceeded",
        ModuleError::Domain(RunTurnError::InvalidSession) => "invalid_session",
        ModuleError::Domain(RunTurnError::StepLimitExceeded) => "step_limit_exceeded",
        ModuleError::Domain(RunTurnError::ToolCallLimitExceeded) => "tool_call_limit_exceeded",
        ModuleError::Domain(RunTurnError::Unknown(_)) => "unknown_domain_error",
        ModuleError::Runtime(_) => "runtime_failure",
    }
}

async fn append_events(
    clients: &AgentLoop,
    context: &InvocationContext,
    session_id: &str,
    expected_revision: String,
    events: Vec<AppendSessionRequestEventsItem>,
) -> Result<String, TurnFailure> {
    clients
        .session
        .append_with_context(
            context.clone(),
            AppendSessionRequest {
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
    kind: AppendSessionRequestEventsItemKind,
    turn_id: Option<&str>,
    payload: &serde_json::Value,
) -> Result<AppendSessionRequestEventsItem, TurnFailure> {
    Ok(AppendSessionRequestEventsItem {
        event_id: uuid::Uuid::new_v4().to_string(),
        kind,
        turn_id: turn_id.map(ToOwned::to_owned),
        occurred_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| {
                ModuleError::runtime(RuntimeFailure::Internal {
                    detail: format!("failed to format event timestamp: {error}"),
                })
            })?,
        payload_json: payload
            .to_string()
            .try_into()
            .expect("serde_json values must produce valid JSON"),
    })
}

fn map_session_open_error(error: SessionOpenInvocationError) -> TurnFailure {
    match error {
        SessionOpenInvocationError::Domain(OpenError::InvalidSessionId | OpenError::NotFound) => {
            ModuleError::domain(RunTurnError::InvalidSession)
        }
        SessionOpenInvocationError::Domain(error) => {
            ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Session open failed: {error:?}"),
            })
        }
        SessionOpenInvocationError::Runtime(error) => ModuleError::runtime(error),
    }
}

fn map_session_read_error(error: SessionReadInvocationError) -> TurnFailure {
    match error {
        SessionReadInvocationError::Domain(ReadError::InvalidCursor | ReadError::NotFound) => {
            ModuleError::domain(RunTurnError::InvalidSession)
        }
        SessionReadInvocationError::Domain(error) => {
            ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Session read failed: {error:?}"),
            })
        }
        SessionReadInvocationError::Runtime(error) => ModuleError::runtime(error),
    }
}

fn map_session_append_error(error: SessionAppendInvocationError) -> TurnFailure {
    match error {
        SessionAppendInvocationError::Domain(AppendError::RevisionConflict { .. }) => {
            ModuleError::domain(RunTurnError::ConcurrentTurn)
        }
        SessionAppendInvocationError::Domain(error) => {
            ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Session append failed: {error:?}"),
            })
        }
        SessionAppendInvocationError::Runtime(error) => ModuleError::runtime(error),
    }
}

fn map_prompt_error(error: PromptInvocationError) -> TurnFailure {
    match error {
        PromptInvocationError::Domain(error) => {
            ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Prompt assembly failed: {error:?}"),
            })
        }
        PromptInvocationError::Runtime(error) => ModuleError::runtime(error),
    }
}

fn map_model_error(error: ModelInvocationError) -> TurnFailure {
    match error {
        ModelInvocationError::Domain(error) => map_model_domain_error(error),
        ModelInvocationError::Runtime(error) => ModuleError::runtime(error),
    }
}

fn map_model_domain_error(error: CompleteError) -> TurnFailure {
    let detail = match error {
        CompleteError::ProviderFailure { payload } => payload.message,
        error => format!("Model completion failed: {error:?}"),
    };
    ModuleError::runtime(RuntimeFailure::ModuleFailure { detail })
}

fn map_tools_error(error: ToolsExecuteInvocationError) -> TurnFailure {
    match error {
        ToolsExecuteInvocationError::Domain(error) => {
            ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: format!("Tool execution failed: {error:?}"),
            })
        }
        ToolsExecuteInvocationError::Runtime(error) => ModuleError::runtime(error),
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
    use lenso_kernel::{CancellationToken, NativeStreamSession};

    #[test]
    fn struct_authoring_derives_the_complete_module_descriptor() {
        let descriptor: serde_json::Value = serde_json::from_str(MODULE_DESCRIPTOR_JSON).unwrap();
        assert_eq!(descriptor["package_id"], "lenso.agent.loop");
        assert_eq!(
            descriptor["provided_capabilities"][0]["capability_id"],
            "lenso.agent@1"
        );
        let requirements = descriptor["required_capabilities"]
            .as_array()
            .expect("requirements must be an array");
        assert_eq!(requirements.len(), 4);
        assert!(
            requirements
                .iter()
                .all(|requirement| requirement["cardinality"] == "one")
        );
        assert_eq!(
            descriptor["configuration_schema"]["required"],
            serde_json::json!([
                "model",
                "max_steps",
                "max_tool_calls",
                "max_output_tokens",
                "max_history_events"
            ])
        );
    }

    fn history_event(
        revision: &str,
        kind: ReadSessionResponseEventsItemKind,
        payload_json: &str,
    ) -> ReadSessionResponseEventsItem {
        ReadSessionResponseEventsItem {
            revision: revision.to_owned(),
            event_id: format!("event-{revision}"),
            kind,
            turn_id: Some("turn-1".to_owned()),
            occurred_at: "2026-08-24T00:00:00Z".to_owned(),
            payload_json: payload_json.to_owned().try_into().unwrap(),
        }
    }

    #[test]
    fn completed_turns_reconstruct_as_model_history() {
        let messages = reconstruct_history(&[
            history_event(
                "1",
                ReadSessionResponseEventsItemKind::TurnStarted,
                r#"{"input":"hello"}"#,
            ),
            history_event(
                "2",
                ReadSessionResponseEventsItemKind::TurnCompleted,
                r#"{"output":"world"}"#,
            ),
        ])
        .unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, CompleteMessageRole::User);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, CompleteMessageRole::Assistant);
        assert_eq!(messages[1].content, "world");
    }

    #[test]
    fn stream_cancellation_propagates_to_the_invocation() {
        let cancellation = CancellationToken::new();
        let context = InvocationContext::new(1, None, cancellation.clone());
        let (stream, _channel) = ProviderStream::<agent_capability::Agent>::channel(&context, 1);
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

    #[test]
    fn interrupted_turn_is_closed_before_a_resumed_turn_starts() {
        let events = [history_event(
            "1",
            ReadSessionResponseEventsItemKind::TurnStarted,
            r#"{"input":"hello"}"#,
        )];

        let recovery = interrupted_turn_events(&events).unwrap();

        assert_eq!(recovery.len(), 1);
        assert_eq!(
            recovery[0].kind,
            AppendSessionRequestEventsItemKind::TurnFailed
        );
        assert_eq!(recovery[0].turn_id.as_deref(), Some("turn-1"));
        assert!(
            recovery[0]
                .payload_json
                .as_str()
                .contains("host_interrupted")
        );
    }

    #[test]
    fn completed_turn_needs_no_recovery_fact() {
        let events = [
            history_event(
                "1",
                ReadSessionResponseEventsItemKind::TurnStarted,
                r#"{"input":"hello"}"#,
            ),
            history_event(
                "2",
                ReadSessionResponseEventsItemKind::TurnCompleted,
                r#"{"output":"world"}"#,
            ),
        ];

        assert!(interrupted_turn_events(&events).unwrap().is_empty());
    }

    #[test]
    fn run_scope_is_deterministic_and_rejects_invalid_names() {
        let scope = RunScope::new(["workspace.read", "text.echo", "workspace.read"]).unwrap();
        assert_eq!(
            scope.allowed_tools.into_iter().collect::<Vec<_>>(),
            vec!["text.echo", "workspace.read"]
        );
        assert!(RunScope::new([""]).is_err());
    }
}

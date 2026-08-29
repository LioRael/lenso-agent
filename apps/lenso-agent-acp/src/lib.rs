use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use agent_client_protocol::schema::{
    ProtocolVersion,
    v1::{
        AgentCapabilities, CancelNotification, ContentBlock, ContentChunk, Implementation,
        InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
        PermissionOption, PermissionOptionKind, PromptRequest, PromptResponse,
        RequestPermissionOutcome, RequestPermissionRequest, SessionNotification, SessionUpdate,
        StopReason, ToolCall, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
    },
};
use agent_client_protocol::{Agent, Client, ConnectTo, ConnectionTo, Stdio};
use lenso_agent_acp_plugin as _;
use lenso_agent_host::{AcpSurface, AgentHost, Profile, generation::TurnGeneration};
use lenso_agent_loop_plugin::RunScope;
use lenso_capability_agent::{
    RUN_TURN_OPERATION, RunTurnRequest, RunTurnResponse, RunTurnResponseKind,
};
use lenso_capability_agent_user_interaction::{InteractionAnswer, PendingInteraction};
use lenso_kernel::{CancellationToken, StreamEvent};
use tokio::sync::{mpsc, oneshot};

const COMMAND_CAPACITY: usize = 32;
const INTERACTION_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone, Debug)]
pub struct AgentAcpConfig {
    pub agent_home: Option<PathBuf>,
    pub allowed_tools: Option<Vec<String>>,
    pub plan: Option<PathBuf>,
    pub plugins: fn(),
    pub profile: Option<String>,
}

#[derive(Debug)]
enum RuntimeCommand {
    NewSession {
        request: NewSessionRequest,
        reply: oneshot::Sender<Result<NewSessionResponse, String>>,
    },
    Prompt {
        connection: ConnectionTo<Client>,
        request: PromptRequest,
        reply: oneshot::Sender<Result<PromptResponse, String>>,
    },
}

pub async fn run_stdio(config: AgentAcpConfig) -> Result<(), String> {
    let selected_profile = match (&config.plan, &config.profile) {
        (Some(plan), None) => Profile::resolved_plan(plan),
        (None, Some(profile)) => Profile::named(profile),
        (None, None) => Profile::Default,
        (Some(_), Some(_)) => {
            return Err("an exact Plan conflicts with a named Agent Profile".to_owned());
        }
    };
    let host = AgentHost::builder().plugins(config.plugins);
    let host = match config.agent_home {
        Some(home) => host.agent_home(home)?,
        None => host,
    }
    .surface(AcpSurface::stdio())
    .build()?;
    let app = host.run(selected_profile).await?;
    let (commands_tx, commands_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (cancellations_tx, cancellations_rx) = mpsc::unbounded_channel();
    let runtime = tokio::task::spawn_local(runtime_loop(
        app,
        commands_rx,
        config.allowed_tools,
        cancellations_rx,
    ));
    let busy = Arc::new(AtomicBool::new(false));
    let connection_result = build_agent(commands_tx, cancellations_tx, busy)
        .connect_to(Stdio::new())
        .await
        .map_err(|error| format!("ACP stdio connection failed: {error}"));
    let runtime_result = runtime
        .await
        .map_err(|error| format!("ACP runtime task failed: {error}"))?;
    connection_result.and(runtime_result)
}

fn build_agent(
    commands: mpsc::Sender<RuntimeCommand>,
    cancellations: mpsc::UnboundedSender<String>,
    busy: Arc<AtomicBool>,
) -> impl agent_client_protocol::ConnectTo<Client> {
    let new_session_commands = commands.clone();
    let prompt_commands = commands;
    Agent
        .builder()
        .name("lenso-agent-acp")
        .on_receive_request(
            async move |request: InitializeRequest, responder, _connection| {
                let _ = request;
                responder.respond(
                    InitializeResponse::new(ProtocolVersion::V1)
                        .agent_capabilities(AgentCapabilities::new())
                        .agent_info(
                            Implementation::new("lenso-agent-acp", env!("CARGO_PKG_VERSION"))
                                .title("Lenso Agent"),
                        ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: NewSessionRequest, responder, connection| {
                let commands = new_session_commands.clone();
                connection.spawn(async move {
                    let (reply, result) = oneshot::channel();
                    commands
                        .send(RuntimeCommand::NewSession { request, reply })
                        .await
                        .map_err(|_| agent_client_protocol::Error::internal_error())?;
                    match result.await {
                        Ok(Ok(response)) => responder.respond(response),
                        Ok(Err(detail)) => responder.respond_with_error(protocol_error(detail)),
                        Err(_) => responder
                            .respond_with_error(protocol_error("ACP runtime stopped".to_owned())),
                    }
                })?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: PromptRequest, responder, connection| {
                if busy.swap(true, Ordering::AcqRel) {
                    return responder.respond_with_error(
                        agent_client_protocol::Error::invalid_request()
                            .data("another ACP prompt is active"),
                    );
                }
                let commands = prompt_commands.clone();
                let release_busy = Arc::clone(&busy);
                let prompt_connection = connection.clone();
                connection.spawn(async move {
                    let (reply, result) = oneshot::channel();
                    let send = commands
                        .send(RuntimeCommand::Prompt {
                            connection: prompt_connection,
                            request,
                            reply,
                        })
                        .await;
                    let response = match send {
                        Ok(()) => result.await.map_err(|_| "ACP runtime stopped".to_owned()),
                        Err(_) => Err("ACP runtime stopped".to_owned()),
                    };
                    release_busy.store(false, Ordering::Release);
                    match response {
                        Ok(Ok(response)) => responder.respond(response),
                        Ok(Err(detail)) | Err(detail) => {
                            responder.respond_with_error(protocol_error(detail))
                        }
                    }
                })?;
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: CancelNotification, _connection| {
                let _ = cancellations.send(notification.session_id.to_string());
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
}

fn protocol_error(detail: String) -> agent_client_protocol::Error {
    agent_client_protocol::Error::internal_error().data(detail)
}

async fn runtime_loop(
    mut app: lenso_agent_host::generation::AgentApp,
    mut commands: mpsc::Receiver<RuntimeCommand>,
    allowed_tools: Option<Vec<String>>,
    mut cancellations: mpsc::UnboundedReceiver<String>,
) -> Result<(), String> {
    while let Some(command) = commands.recv().await {
        match command {
            RuntimeCommand::NewSession { request, reply } => {
                let result = open_session(&app, request).await;
                let _ = reply.send(result);
            }
            RuntimeCommand::Prompt {
                connection,
                request,
                reply,
            } => {
                let result = prompt(
                    &app,
                    request,
                    connection,
                    allowed_tools.as_deref(),
                    &mut cancellations,
                )
                .await;
                let _ = reply.send(result);
            }
        }
    }
    app.shutdown().await
}

async fn open_session(
    app: &lenso_agent_host::generation::AgentApp,
    request: NewSessionRequest,
) -> Result<NewSessionResponse, String> {
    validate_workspace_request(
        &request.cwd,
        &request.additional_directories,
        request.mcp_servers.len(),
    )?;
    let turn = app.lease_acp_turn().await?;
    turn.open_session().await.map(NewSessionResponse::new)
}

fn validate_workspace_request(
    cwd: &Path,
    additional_directories: &[PathBuf],
    mcp_server_count: usize,
) -> Result<(), String> {
    if !cwd.is_absolute() {
        return Err("ACP session cwd must be absolute".to_owned());
    }
    if !additional_directories.is_empty() {
        return Err("ACP additional workspace directories are not supported".to_owned());
    }
    if mcp_server_count != 0 {
        return Err("ACP-provided MCP servers are not supported".to_owned());
    }
    let requested = cwd
        .canonicalize()
        .map_err(|error| format!("failed to resolve ACP session cwd: {error}"))?;
    let workspace = std::env::current_dir()
        .and_then(|path| path.canonicalize())
        .map_err(|error| format!("failed to resolve Host Workspace: {error}"))?;
    if requested != workspace {
        return Err(format!(
            "ACP session cwd `{}` does not match Host Workspace `{}`",
            requested.display(),
            workspace.display()
        ));
    }
    Ok(())
}

async fn prompt(
    app: &lenso_agent_host::generation::AgentApp,
    request: PromptRequest,
    connection: ConnectionTo<Client>,
    allowed_tools: Option<&[String]>,
    cancellations: &mut mpsc::UnboundedReceiver<String>,
) -> Result<PromptResponse, String> {
    let session_id = request.session_id.to_string();
    let input = prompt_text(request.prompt)?;
    let turn = app.lease_acp_turn().await?;
    turn.read_session(session_id.clone(), 0, 1).await?;
    let cancellation = CancellationToken::new();
    invoke_turn(
        &turn,
        &session_id,
        input,
        connection,
        allowed_tools,
        cancellation,
        cancellations,
    )
    .await
}

fn prompt_text(prompt: Vec<ContentBlock>) -> Result<String, String> {
    let mut sections = Vec::with_capacity(prompt.len());
    for block in prompt {
        match block {
            ContentBlock::Text(text) => sections.push(text.text),
            ContentBlock::ResourceLink(resource) => sections.push(format!(
                "Referenced resource: {} ({})",
                resource.name, resource.uri
            )),
            ContentBlock::Image(_) | ContentBlock::Audio(_) | ContentBlock::Resource(_) => {
                return Err(
                    "this ACP entrypoint currently accepts text and resource links only".to_owned(),
                );
            }
            _ => return Err("this ACP content type is not supported".to_owned()),
        }
    }
    if sections.is_empty() {
        return Err("ACP prompt must contain at least one content block".to_owned());
    }
    Ok(sections.join("\n\n"))
}

async fn invoke_turn(
    turn: &TurnGeneration,
    session_id: &str,
    input: String,
    connection: ConnectionTo<Client>,
    allowed_tools: Option<&[String]>,
    cancellation: CancellationToken,
    cancellations: &mut mpsc::UnboundedReceiver<String>,
) -> Result<PromptResponse, String> {
    let mut context = turn.invocation_context_with_cancellation(cancellation.clone())?;
    if let Some(allowed_tools) = allowed_tools {
        context = RunScope::new(allowed_tools.iter().cloned())?.attach(context)?;
    }
    let stream = turn
        .handle()
        .open_with_context(
            RUN_TURN_OPERATION,
            context,
            RunTurnRequest {
                input,
                session_id: Some(session_id.to_owned()),
            },
        )
        .await
        .map_err(|error| format!("Agent stream failed to open: {error:?}"))?
        .map_err(|error| format!("Agent rejected the turn: {error:?}"))?;
    stream
        .close_send()
        .await
        .map_err(|error| format!("failed to half-close Agent input: {error:?}"))?;
    let mut seen_interactions = HashSet::new();
    let mut interaction_tick = tokio::time::interval(INTERACTION_POLL_INTERVAL);
    interaction_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            received = stream.receive() => {
                let event = match received {
                    Ok(event) => event,
                    Err(_) if cancellation.is_cancelled() => {
                        return Ok(PromptResponse::new(StopReason::Cancelled));
                    }
                    Err(error) => return Err(format!("Agent stream failed: {error:?}")),
                };
                match event {
                    StreamEvent::Message(message) => {
                        send_agent_update(&connection, session_id, message)?;
                    }
                    StreamEvent::PeerHalfClosed => {}
                    StreamEvent::Terminal(Ok(())) => {
                        return Ok(PromptResponse::new(StopReason::EndTurn));
                    }
                    StreamEvent::Terminal(Err(error)) if cancellation.is_cancelled() => {
                        let _ = error;
                        return Ok(PromptResponse::new(StopReason::Cancelled));
                    }
                    StreamEvent::Terminal(Err(error)) => {
                        return Err(format!("Agent turn failed: {error:?}"));
                    }
                }
            }
            _ = interaction_tick.tick() => {
                if cancellation.is_cancelled() {
                    continue;
                }
                for interaction in turn.pending_interactions().await? {
                    if seen_interactions.insert(interaction.interaction_id.clone()) {
                        answer_interaction(turn, session_id, &connection, interaction, &cancellation).await?;
                    }
                }
            }
            Some(cancelled) = cancellations.recv() => {
                if cancelled == session_id {
                    cancellation.cancel();
                }
            }
        }
    }
}

fn send_agent_update(
    connection: &ConnectionTo<Client>,
    session_id: &str,
    message: RunTurnResponse,
) -> Result<(), String> {
    let update = match message.kind.as_ref() {
        None | Some(RunTurnResponseKind::TextDelta) => {
            SessionUpdate::AgentMessageChunk(ContentChunk::new(message.text.into()))
        }
        Some(RunTurnResponseKind::ReasoningDelta) => {
            SessionUpdate::AgentThoughtChunk(ContentChunk::new(message.text.into()))
        }
        Some(RunTurnResponseKind::ReasoningCompleted) => return Ok(()),
        Some(RunTurnResponseKind::ToolStarted) => {
            SessionUpdate::ToolCall(tool_call_from_message(&message))
        }
        Some(RunTurnResponseKind::ToolProgress) => SessionUpdate::ToolCallUpdate(
            tool_call_update_from_message(&message, ToolCallStatus::InProgress),
        ),
        Some(RunTurnResponseKind::ToolCompleted) => SessionUpdate::ToolCallUpdate(
            tool_call_update_from_message(&message, ToolCallStatus::Completed),
        ),
        Some(RunTurnResponseKind::ToolFailed) => SessionUpdate::ToolCallUpdate(
            tool_call_update_from_message(&message, ToolCallStatus::Failed),
        ),
    };
    connection
        .send_notification(SessionNotification::new(session_id.to_owned(), update))
        .map_err(|error| format!("failed to send ACP Session update: {error}"))
}

fn tool_call_from_message(message: &RunTurnResponse) -> ToolCall {
    let id = message
        .tool_call_id
        .clone()
        .unwrap_or_else(|| "unknown-tool-call".to_owned());
    let title = message
        .tool_name
        .clone()
        .unwrap_or_else(|| "Tool call".to_owned());
    let mut tool = ToolCall::new(id, title).status(ToolCallStatus::InProgress);
    if let Some(arguments) = message.arguments_json.as_ref()
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments.as_ref())
    {
        tool = tool.raw_input(value);
    }
    tool
}

fn tool_call_update_from_message(
    message: &RunTurnResponse,
    status: ToolCallStatus,
) -> ToolCallUpdate {
    let mut fields = ToolCallUpdateFields::new().status(status);
    if !message.text.is_empty() {
        fields = fields.content(vec![message.text.clone().into()]);
    }
    if let Some(content) = message.content.as_ref() {
        fields = fields.raw_output(serde_json::Value::String(content.clone()));
    } else if let Some(error) = message.error.as_ref() {
        fields = fields.raw_output(serde_json::json!({"error": error}));
    }
    ToolCallUpdate::new(
        message
            .tool_call_id
            .clone()
            .unwrap_or_else(|| "unknown-tool-call".to_owned()),
        fields,
    )
}

async fn answer_interaction(
    turn: &TurnGeneration,
    session_id: &str,
    connection: &ConnectionTo<Client>,
    interaction: PendingInteraction,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    if interaction.questions.len() != 1 {
        cancellation.cancel();
        return Err("ACP v1 can project only one exact Tool approval interaction".to_owned());
    }
    let question = &interaction.questions[0];
    let option_ids = question
        .options
        .iter()
        .map(|option| option.option_id.as_str())
        .collect::<HashSet<_>>();
    if question.question_id != "approval"
        || question.multi_select
        || option_ids != HashSet::from(["approve", "deny"])
    {
        cancellation.cancel();
        return Err(
            "ACP v1 can project only the exact approve-or-deny Tool interaction".to_owned(),
        );
    }
    let options = question
        .options
        .iter()
        .map(|option| {
            let kind = if option.option_id == "approve" {
                PermissionOptionKind::AllowOnce
            } else {
                PermissionOptionKind::RejectOnce
            };
            PermissionOption::new(option.option_id.clone(), option.label.clone(), kind)
        })
        .collect::<Vec<_>>();
    let tool_call = ToolCall::new(interaction.interaction_id.clone(), question.header.clone())
        .status(ToolCallStatus::Pending)
        .raw_input(serde_json::json!({"prompt": question.prompt}));
    let response = connection
        .send_request(RequestPermissionRequest::new(
            session_id.to_owned(),
            tool_call.into(),
            options,
        ))
        .block_task()
        .await
        .map_err(|error| format!("ACP permission request failed: {error}"))?;
    let RequestPermissionOutcome::Selected(selected) = response.outcome else {
        cancellation.cancel();
        return Ok(());
    };
    turn.answer_interaction(
        interaction.interaction_id,
        vec![InteractionAnswer {
            question_id: question.question_id.clone(),
            selected_option_ids: vec![selected.option_id.to_string()],
            other: Some(None),
        }],
    )
    .await
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{ImageContent, ResourceLink};

    use super::*;

    #[test]
    fn text_and_resource_links_form_one_model_input() {
        let input = prompt_text(vec![
            "Fix the bug".into(),
            ContentBlock::ResourceLink(ResourceLink::new("log", "file:///tmp/error.log")),
        ])
        .unwrap();
        assert_eq!(
            input,
            "Fix the bug\n\nReferenced resource: log (file:///tmp/error.log)"
        );
    }

    #[test]
    fn unsupported_media_fails_before_agent_invocation() {
        let error = prompt_text(vec![ContentBlock::Image(ImageContent::new(
            "AA==",
            "image/png",
        ))])
        .unwrap_err();
        assert!(error.contains("text and resource links only"));
    }
}

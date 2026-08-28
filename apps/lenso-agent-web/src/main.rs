use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, RwLock},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response, sse::Event, sse::Sse},
    routing::{get, post},
};
use clap::{ArgAction, Parser};
use lenso_agent_host::{AgentHost, Profile, WebSurface, generation::AgentApp};
use lenso_agent_loop_plugin::RunScope;
use lenso_agent_session_inspection::{
    InspectedSession, InspectedSessionEvent, Trajectory, project_trajectory,
};
use lenso_agent_web_plugin as _;
use lenso_capability_agent::{RUN_TURN_OPERATION, RunTurnRequest, RunTurnResponse};
use lenso_capability_agent_session::{
    ListSessionsResponse, ReadSessionResponse, ReadSessionResponseEventsItemKind,
};
use lenso_capability_agent_user_interaction::{
    InteractionAnswer, InteractionOption, InteractionQuestion, PendingInteraction,
};
use lenso_kernel::{CancellationToken, StreamEvent};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

const MAX_REQUEST_BYTES: usize = 65_536;
const MAX_PROMPT_BYTES: usize = 65_536;
const TOOL_POLICY_SCHEMA: &str = "lenso.agent.tool-policy.v1";
const CONTROL_TOKEN_ENV: &str = "LENSO_AGENT_CONTROL_TOKEN";

#[derive(Debug, Parser)]
#[command(
    name = "lenso-agent-web",
    version,
    about = "Run the Lenso Agent Harness Web API"
)]
struct Args {
    /// Address used by the Agent Web API.
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,

    /// Exact immutable Resolved App Plan used by the Web surface.
    #[arg(long, value_name = "PATH")]
    plan: Option<PathBuf>,

    /// Select `profiles/<name>.toml` for this Web process.
    #[arg(long, value_name = "NAME", conflicts_with = "plan")]
    profile: Option<String>,

    /// Admit one Plan-bound Tool to every Console Agent Turn. Repeat to admit more.
    #[arg(long = "allow-tool", value_name = "NAME", action = ArgAction::Append)]
    allowed_tools: Vec<String>,

    /// Durable Tool policy file. Enabling mutation also requires `LENSO_AGENT_CONTROL_TOKEN`.
    #[arg(long, value_name = "PATH")]
    tool_policy: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct WebRuntime {
    available_tools: Vec<BootstrapTool>,
    commands: mpsc::Sender<RuntimeCommand>,
    control_token: Option<String>,
    policy: Arc<RwLock<ToolPolicyDocument>>,
    policy_path: Option<PathBuf>,
    profile: Option<String>,
}

#[derive(Debug)]
enum RuntimeCommand {
    AnswerInteraction {
        answers: Vec<InteractionAnswer>,
        interaction_id: String,
        reply: oneshot::Sender<Result<(), RuntimeInteractionError>>,
        request_id: String,
    },
    CancelTurn {
        reply: oneshot::Sender<bool>,
        request_id: String,
    },
    ListSessions {
        reply: oneshot::Sender<Result<WebSessionList, String>>,
    },
    PendingInteractions {
        reply: oneshot::Sender<Result<Vec<PendingInteraction>, RuntimeInteractionError>>,
        request_id: String,
    },
    ReadSession {
        reply: oneshot::Sender<Result<ReadSessionResponse, String>>,
        session_id: String,
    },
    ReadTrajectory {
        reply: oneshot::Sender<Result<Trajectory, String>>,
        session_id: String,
    },
    RunTurn {
        events: mpsc::Sender<Result<Event, Infallible>>,
        request: WebTurnRequest,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Debug)]
enum RuntimeInteractionError {
    Inactive,
    Rejected(String),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebTurnRequest {
    #[serde(default)]
    edit_turn_id: Option<String>,
    input: String,
    request_id: String,
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WebAnswerInteractionRequest {
    answers: Vec<WebInteractionAnswer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WebInteractionAnswer {
    question_id: String,
    selected_option_ids: Vec<String>,
    other: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebPendingInteractionsResponse {
    interactions: Vec<WebPendingInteraction>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebPendingInteraction {
    interaction_id: String,
    questions: Vec<WebInteractionQuestion>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebInteractionQuestion {
    header: String,
    multi_select: bool,
    options: Vec<WebInteractionOption>,
    prompt: String,
    question_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebInteractionOption {
    description: String,
    label: String,
    option_id: String,
    preview: Option<String>,
}

impl From<WebInteractionAnswer> for InteractionAnswer {
    fn from(answer: WebInteractionAnswer) -> Self {
        Self {
            question_id: answer.question_id,
            selected_option_ids: answer.selected_option_ids,
            other: Some(answer.other),
        }
    }
}

impl From<PendingInteraction> for WebPendingInteraction {
    fn from(interaction: PendingInteraction) -> Self {
        Self {
            interaction_id: interaction.interaction_id,
            questions: interaction
                .questions
                .into_iter()
                .map(WebInteractionQuestion::from)
                .collect(),
        }
    }
}

impl From<InteractionQuestion> for WebInteractionQuestion {
    fn from(question: InteractionQuestion) -> Self {
        Self {
            header: question.header,
            multi_select: question.multi_select,
            options: question
                .options
                .into_iter()
                .map(WebInteractionOption::from)
                .collect(),
            prompt: question.prompt,
            question_id: question.question_id,
        }
    }
}

impl From<InteractionOption> for WebInteractionOption {
    fn from(option: InteractionOption) -> Self {
        Self {
            description: option.description,
            label: option.label,
            option_id: option.option_id,
            preview: option.preview.and_then(|preview| preview),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapResponse {
    capabilities: BTreeMap<&'static str, bool>,
    mode: &'static str,
    profile: String,
    tools: BootstrapTools,
    trajectory: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebSessionList {
    sessions: Vec<WebSessionSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebSessionSummary {
    revision: String,
    session_id: String,
    title: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
struct BootstrapTools {
    allowed: Vec<String>,
    available: Vec<BootstrapTool>,
}

#[derive(Clone, Debug, Serialize)]
struct BootstrapTool {
    description: String,
    name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ToolPolicyDocument {
    allowed: Vec<String>,
    revision: u64,
    schema: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolPolicyResponse {
    allowed: Vec<String>,
    available: Vec<BootstrapTool>,
    revision: u64,
    schema: &'static str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UpdateToolPolicyRequest {
    allowed: Vec<String>,
    expected_revision: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WebStreamEvent<'a> {
    #[serde(rename = "turn_cancelled")]
    Cancelled { session_id: Option<&'a str> },
    #[serde(rename = "turn_completed")]
    Completed { session_id: Option<&'a str> },
    #[serde(rename = "turn_failed")]
    Failed { detail: &'a str },
    #[serde(rename = "turn_message")]
    Message { message: &'a RunTurnResponse },
}

#[derive(Debug, Serialize)]
struct Problem {
    detail: String,
}

#[derive(Debug)]
struct ApiProblem {
    detail: String,
    status: StatusCode,
}

impl ApiProblem {
    fn bad_request(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            status: StatusCode::BAD_REQUEST,
        }
    }

    fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            status: StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    fn not_found(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            status: StatusCode::NOT_FOUND,
        }
    }

    fn conflict(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            status: StatusCode::CONFLICT,
        }
    }

    fn forbidden(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            status: StatusCode::FORBIDDEN,
        }
    }
}

impl IntoResponse for ApiProblem {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(Problem {
                detail: self.detail,
            }),
        )
            .into_response()
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let local = tokio::task::LocalSet::new();
    match local.run_until(run(Args::parse())).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    let configured_tools = normalize_allowed_tools(args.allowed_tools)?;
    let profile = match (&args.plan, &args.profile) {
        (Some(plan), None) => Profile::resolved_plan(plan),
        (None, Some(profile)) => Profile::named(profile),
        (None, None) => Profile::Default,
        (Some(_), Some(_)) => unreachable!("clap rejects Plan/Profile conflicts"),
    };
    let host = AgentHost::builder()
        .plugins(lenso_agent_default_plugins::link)
        .surface(WebSurface::browser())
        .build()?;
    let app = host.run(profile).await?;
    let available_tools = resolve_tool_policy(&app, &configured_tools).await?;
    let configured_control_token = std::env::var(CONTROL_TOKEN_ENV)
        .ok()
        .filter(|value| !value.is_empty());
    if args.tool_policy.is_some() && configured_control_token.is_none() {
        return Err(format!("--tool-policy requires {CONTROL_TOKEN_ENV}"));
    }
    let control_token = args.tool_policy.as_ref().and(configured_control_token);
    let policy = load_tool_policy(
        args.tool_policy.as_deref(),
        configured_tools,
        &available_tools,
    )?;
    let runtime = WebRuntime::start(
        app,
        args.profile,
        policy,
        args.tool_policy,
        available_tools,
        control_token,
    );
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .map_err(|error| format!("failed to bind {}: {error}", args.listen))?;
    println!("Lenso Agent Web listening on http://{}", args.listen);
    axum::serve(listener, router(runtime.clone()))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("Agent Web server failed: {error}"))?;
    runtime.shutdown().await
}

fn router(runtime: WebRuntime) -> Router {
    Router::new()
        .route("/api/console/v1/agent/bootstrap", get(bootstrap))
        .route("/api/console/v1/agent/turns", post(run_turn))
        .route(
            "/api/console/v1/agent/turns/{request_id}/cancel",
            post(cancel_turn),
        )
        .route(
            "/api/console/v1/agent/turns/{request_id}/interactions",
            get(pending_interactions),
        )
        .route(
            "/api/console/v1/agent/turns/{request_id}/interactions/{interaction_id}/answer",
            post(answer_interaction),
        )
        .route("/api/console/v1/agent/sessions", get(list_sessions))
        .route(
            "/api/console/v1/agent/control/tool-policy",
            get(read_tool_policy).put(update_tool_policy),
        )
        .route(
            "/api/console/v1/agent/sessions/{session_id}",
            get(read_session),
        )
        .route(
            "/api/console/v1/agent/sessions/{session_id}/trajectory",
            get(read_trajectory),
        )
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .with_state(runtime)
}

async fn bootstrap(
    State(runtime): State<WebRuntime>,
) -> Result<Json<BootstrapResponse>, ApiProblem> {
    let policy = runtime.read_tool_policy()?;
    Ok(Json(BootstrapResponse {
        capabilities: [
            ("cancel", true),
            ("edit", true),
            ("sessionList", true),
            ("sessionRead", true),
            ("userInteraction", true),
        ]
        .into_iter()
        .collect(),
        mode: "console",
        profile: runtime.profile.unwrap_or_else(|| "default".to_owned()),
        tools: BootstrapTools {
            allowed: policy.allowed,
            available: runtime.available_tools,
        },
        trajectory: Trajectory::SCHEMA,
    }))
}

async fn read_tool_policy(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
) -> Result<Json<ToolPolicyResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    runtime.read_tool_policy().map(Json)
}

async fn update_tool_policy(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Json(request): Json<UpdateToolPolicyRequest>,
) -> Result<Json<ToolPolicyResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    runtime.update_tool_policy(request).map(Json)
}

async fn run_turn(
    State(runtime): State<WebRuntime>,
    Json(request): Json<WebTurnRequest>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, ApiProblem> {
    validate_turn_request(&request)?;
    let (events, receiver) = mpsc::channel(32);
    runtime
        .commands
        .send(RuntimeCommand::RunTurn { events, request })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    Ok(Sse::new(ReceiverStream::new(receiver)))
}

async fn cancel_turn(
    State(runtime): State<WebRuntime>,
    Path(request_id): Path<String>,
) -> Result<StatusCode, ApiProblem> {
    if !valid_session_id(&request_id) {
        return Err(ApiProblem::bad_request("Agent request ID is invalid"));
    }
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::CancelTurn { reply, request_id })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    if response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
    {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(ApiProblem::not_found("Agent Turn is not active"))
    }
}

async fn pending_interactions(
    State(runtime): State<WebRuntime>,
    Path(request_id): Path<String>,
) -> Result<Json<WebPendingInteractionsResponse>, ApiProblem> {
    validate_request_id(&request_id)?;
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::PendingInteractions { reply, request_id })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    let interactions = response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
        .map_err(interaction_problem)?;
    Ok(Json(WebPendingInteractionsResponse {
        interactions: interactions
            .into_iter()
            .map(WebPendingInteraction::from)
            .collect(),
    }))
}

async fn answer_interaction(
    State(runtime): State<WebRuntime>,
    Path((request_id, interaction_id)): Path<(String, String)>,
    Json(request): Json<WebAnswerInteractionRequest>,
) -> Result<StatusCode, ApiProblem> {
    validate_request_id(&request_id)?;
    validate_request_id(&interaction_id)?;
    let answers = request
        .answers
        .into_iter()
        .map(InteractionAnswer::from)
        .collect();
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::AnswerInteraction {
            answers,
            interaction_id,
            reply,
            request_id,
        })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
        .map_err(interaction_problem)?;
    Ok(StatusCode::NO_CONTENT)
}

fn interaction_problem(error: RuntimeInteractionError) -> ApiProblem {
    match error {
        RuntimeInteractionError::Inactive => ApiProblem::not_found("Agent Turn is not active"),
        RuntimeInteractionError::Rejected(detail) => ApiProblem::conflict(detail),
    }
}

async fn read_session(
    State(runtime): State<WebRuntime>,
    Path(session_id): Path<String>,
) -> Result<Json<ReadSessionResponse>, ApiProblem> {
    if !valid_session_id(&session_id) {
        return Err(ApiProblem::bad_request("Session ID is invalid"));
    }
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::ReadSession { reply, session_id })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
        .map(Json)
        .map_err(ApiProblem::unavailable)
}

async fn read_trajectory(
    State(runtime): State<WebRuntime>,
    Path(session_id): Path<String>,
) -> Result<Json<Trajectory>, ApiProblem> {
    if !valid_session_id(&session_id) {
        return Err(ApiProblem::bad_request("Session ID is invalid"));
    }
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::ReadTrajectory { reply, session_id })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
        .map(Json)
        .map_err(ApiProblem::unavailable)
}

async fn list_sessions(
    State(runtime): State<WebRuntime>,
) -> Result<Json<WebSessionList>, ApiProblem> {
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::ListSessions { reply })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
        .map(Json)
        .map_err(ApiProblem::unavailable)
}

fn validate_turn_request(request: &WebTurnRequest) -> Result<(), ApiProblem> {
    if request.input.trim().is_empty() {
        return Err(ApiProblem::bad_request("Agent input must not be empty"));
    }
    if request.input.len() > MAX_PROMPT_BYTES {
        return Err(ApiProblem::bad_request("Agent input is too large"));
    }
    if !valid_session_id(&request.request_id) {
        return Err(ApiProblem::bad_request("Agent request ID is invalid"));
    }
    if request
        .session_id
        .as_deref()
        .is_some_and(|session_id| !valid_session_id(session_id))
    {
        return Err(ApiProblem::bad_request("Session ID is invalid"));
    }
    if request.edit_turn_id.is_some() && request.session_id.is_none() {
        return Err(ApiProblem::bad_request(
            "Editing a message requires its source Session",
        ));
    }
    if request
        .edit_turn_id
        .as_deref()
        .is_some_and(|turn_id| !valid_session_id(turn_id))
    {
        return Err(ApiProblem::bad_request("Turn ID is invalid"));
    }
    Ok(())
}

fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn validate_request_id(value: &str) -> Result<(), ApiProblem> {
    if valid_session_id(value) {
        Ok(())
    } else {
        Err(ApiProblem::bad_request(
            "Agent interaction identity is invalid",
        ))
    }
}

fn normalize_allowed_tools(tools: Vec<String>) -> Result<Vec<String>, String> {
    Ok(RunScope::new(tools)?.allowed_tools.into_iter().collect())
}

async fn resolve_tool_policy(
    app: &AgentApp,
    allowed_tools: &[String],
) -> Result<Vec<BootstrapTool>, String> {
    let turn = app.lease_web_turn().await?;
    let mut available_tools = turn
        .tool_catalog()
        .await?
        .into_iter()
        .map(|tool| BootstrapTool {
            description: tool.description,
            name: tool.name,
        })
        .collect::<Vec<_>>();
    available_tools.sort_by(|left, right| left.name.cmp(&right.name));
    let available_names = available_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    if available_names.len() != available_tools.len() {
        return Err("active Plan-bound Tool catalog contains duplicate names".to_owned());
    }
    if let Some(unknown) = allowed_tools
        .iter()
        .find(|name| !available_names.contains(name.as_str()))
    {
        return Err(format!(
            "configured Console Agent Tool `{unknown}` is not in the active Plan-bound catalog"
        ));
    }
    Ok(available_tools)
}

impl WebRuntime {
    fn start(
        app: AgentApp,
        profile: Option<String>,
        policy: ToolPolicyDocument,
        policy_path: Option<PathBuf>,
        available_tools: Vec<BootstrapTool>,
        control_token: Option<String>,
    ) -> Self {
        let (commands, receiver) = mpsc::channel(16);
        let policy = Arc::new(RwLock::new(policy));
        tokio::task::spawn_local(runtime_actor(app, receiver, Arc::clone(&policy)));
        Self {
            available_tools,
            commands,
            control_token,
            policy,
            policy_path,
            profile,
        }
    }

    fn authorize_control(&self, headers: &HeaderMap) -> Result<(), ApiProblem> {
        let expected = self
            .control_token
            .as_deref()
            .ok_or_else(|| ApiProblem::not_found("Agent Tool policy control is not configured"))?;
        let supplied = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        if !supplied.is_some_and(|supplied| constant_time_eq(supplied, expected)) {
            return Err(ApiProblem::forbidden(
                "Agent Tool policy control token is invalid",
            ));
        }
        Ok(())
    }

    fn read_tool_policy(&self) -> Result<ToolPolicyResponse, ApiProblem> {
        let policy = self
            .policy
            .read()
            .map_err(|_| ApiProblem::unavailable("Agent Tool policy lock is poisoned"))?;
        Ok(policy_response(&policy, &self.available_tools))
    }

    fn update_tool_policy(
        &self,
        request: UpdateToolPolicyRequest,
    ) -> Result<ToolPolicyResponse, ApiProblem> {
        let mut policy = self
            .policy
            .write()
            .map_err(|_| ApiProblem::unavailable("Agent Tool policy lock is poisoned"))?;
        update_policy(
            &mut policy,
            self.policy_path.as_deref(),
            &self.available_tools,
            request,
        )
        .map_err(ApiProblem::conflict)
    }

    async fn shutdown(&self) -> Result<(), String> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::Shutdown { reply })
            .await
            .map_err(|_| "Agent runtime already stopped".to_owned())?;
        response
            .await
            .map_err(|_| "Agent runtime stopped without a shutdown result".to_owned())?
    }
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[expect(
    clippy::too_many_lines,
    reason = "the actor keeps every serialized runtime command in one auditable dispatch loop"
)]
async fn runtime_actor(
    mut app: AgentApp,
    mut commands: mpsc::Receiver<RuntimeCommand>,
    policy: Arc<RwLock<ToolPolicyDocument>>,
) {
    let mut pending = VecDeque::new();
    let mut pre_cancelled = BTreeSet::new();
    loop {
        let command = match pending.pop_front() {
            Some(command) => command,
            None => match commands.recv().await {
                Some(command) => command,
                None => break,
            },
        };
        match command {
            RuntimeCommand::AnswerInteraction { reply, .. } => {
                let _ = reply.send(Err(RuntimeInteractionError::Inactive));
            }
            RuntimeCommand::PendingInteractions { reply, .. } => {
                let _ = reply.send(Err(RuntimeInteractionError::Inactive));
            }
            RuntimeCommand::CancelTurn { reply, .. } => {
                let _ = reply.send(false);
            }
            RuntimeCommand::ListSessions { reply } => {
                let result = list_sessions_from_app(&app).await;
                let _ = reply.send(result);
            }
            RuntimeCommand::ReadSession { reply, session_id } => {
                let result = read_session_from_app(&app, session_id).await;
                let _ = reply.send(result);
            }
            RuntimeCommand::ReadTrajectory { reply, session_id } => {
                let result = read_session_from_app(&app, session_id)
                    .await
                    .and_then(|session| project_web_trajectory(&session));
                let _ = reply.send(result);
            }
            RuntimeCommand::RunTurn { events, request } => {
                let allowed_tools = match policy.read() {
                    Ok(policy) => policy.allowed.clone(),
                    Err(_) => {
                        send_stream_event(
                            &events,
                            "turn.failed",
                            None,
                            &WebStreamEvent::Failed {
                                detail: "Agent Tool policy lock is poisoned",
                            },
                        )
                        .await;
                        continue;
                    }
                };
                let request_id = request.request_id.clone();
                let cancellation = CancellationToken::new();
                if pre_cancelled.remove(&request_id) {
                    cancellation.cancel();
                }
                let turn = match app.lease_web_turn().await {
                    Ok(turn) => turn,
                    Err(error) => {
                        send_stream_event(
                            &events,
                            "turn.failed",
                            None,
                            &WebStreamEvent::Failed { detail: &error },
                        )
                        .await;
                        continue;
                    }
                };
                let shutdown = {
                    let running = run_turn_on_lease(
                        &turn,
                        request,
                        &events,
                        cancellation.clone(),
                        &allowed_tools,
                    );
                    tokio::pin!(running);
                    let mut shutdown = None;
                    loop {
                        tokio::select! {
                            () = &mut running => break,
                            command = commands.recv() => {
                                let Some(command) = command else {
                                    cancellation.cancel();
                                    break;
                                };
                                match command {
                                    RuntimeCommand::PendingInteractions { reply, request_id: target_id } => {
                                        let result = if target_id == request_id {
                                            turn.pending_interactions()
                                                .await
                                                .map_err(RuntimeInteractionError::Rejected)
                                        } else {
                                            Err(RuntimeInteractionError::Inactive)
                                        };
                                        let _ = reply.send(result);
                                    }
                                    RuntimeCommand::AnswerInteraction {
                                        answers,
                                        interaction_id,
                                        reply,
                                        request_id: target_id,
                                    } => {
                                        let result = if target_id == request_id {
                                            turn.answer_interaction(interaction_id, answers)
                                                .await
                                                .map_err(RuntimeInteractionError::Rejected)
                                        } else {
                                            Err(RuntimeInteractionError::Inactive)
                                        };
                                        let _ = reply.send(result);
                                    }
                                    RuntimeCommand::CancelTurn { reply, request_id: cancelled_id } => {
                                        let found = cancelled_id == request_id || pending.iter().any(|command| matches!(command, RuntimeCommand::RunTurn { request, .. } if request.request_id == cancelled_id));
                                        if cancelled_id == request_id {
                                            cancellation.cancel();
                                        } else if found {
                                            pre_cancelled.insert(cancelled_id);
                                        }
                                        let _ = reply.send(found);
                                    }
                                    RuntimeCommand::Shutdown { reply } => {
                                        cancellation.cancel();
                                        shutdown = Some(reply);
                                    }
                                    command => pending.push_back(command),
                                }
                            }
                        }
                    }
                    shutdown
                };
                if let Some(reply) = shutdown {
                    let _ = reply.send(app.shutdown().await);
                    return;
                }
            }
            RuntimeCommand::Shutdown { reply } => {
                let _ = reply.send(app.shutdown().await);
                return;
            }
        }
    }
    let _ = app.shutdown().await;
}

fn load_tool_policy(
    path: Option<&std::path::Path>,
    configured_tools: Vec<String>,
    available_tools: &[BootstrapTool],
) -> Result<ToolPolicyDocument, String> {
    let Some(path) = path else {
        return Ok(ToolPolicyDocument {
            allowed: configured_tools,
            revision: 0,
            schema: TOOL_POLICY_SCHEMA.to_owned(),
        });
    };
    if path.exists() {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("failed to read Agent Tool policy: {error}"))?;
        let mut document: ToolPolicyDocument = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Agent Tool policy is invalid: {error}"))?;
        if document.schema != TOOL_POLICY_SCHEMA {
            return Err("Agent Tool policy schema is unsupported".to_owned());
        }
        document.allowed = validate_policy_tools(document.allowed, available_tools)?;
        return Ok(document);
    }
    let document = ToolPolicyDocument {
        allowed: configured_tools,
        revision: 0,
        schema: TOOL_POLICY_SCHEMA.to_owned(),
    };
    persist_tool_policy(path, &document)?;
    Ok(document)
}

fn validate_policy_tools(
    tools: Vec<String>,
    available_tools: &[BootstrapTool],
) -> Result<Vec<String>, String> {
    let tools = normalize_allowed_tools(tools)?;
    let available = available_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    if let Some(unknown) = tools.iter().find(|tool| !available.contains(tool.as_str())) {
        return Err(format!(
            "Tool `{unknown}` is not in the active Plan-bound catalog"
        ));
    }
    Ok(tools)
}

fn update_policy(
    policy: &mut ToolPolicyDocument,
    path: Option<&std::path::Path>,
    available_tools: &[BootstrapTool],
    request: UpdateToolPolicyRequest,
) -> Result<ToolPolicyResponse, String> {
    if request.expected_revision != policy.revision {
        return Err("Agent Tool policy changed; reload before saving".to_owned());
    }
    let allowed = validate_policy_tools(request.allowed, available_tools)?;
    let next = ToolPolicyDocument {
        allowed,
        revision: policy
            .revision
            .checked_add(1)
            .ok_or_else(|| "Agent Tool policy revision overflowed".to_owned())?,
        schema: TOOL_POLICY_SCHEMA.to_owned(),
    };
    let path = path.ok_or_else(|| "Agent Tool policy persistence is not configured".to_owned())?;
    persist_tool_policy(path, &next)?;
    *policy = next;
    Ok(policy_response(policy, available_tools))
}

fn persist_tool_policy(path: &std::path::Path, policy: &ToolPolicyDocument) -> Result<(), String> {
    use std::io::Write as _;

    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create Agent Tool policy directory: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(policy)
        .map_err(|error| format!("failed to encode Agent Tool policy: {error}"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("failed to create Agent Tool policy: {error}"))?;
    file.write_all(&bytes)
        .map_err(|error| format!("failed to write Agent Tool policy: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("failed to sync Agent Tool policy: {error}"))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| format!("failed to commit Agent Tool policy: {error}"))?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("failed to sync Agent Tool policy directory: {error}"))?;
    Ok(())
}

fn policy_response(
    policy: &ToolPolicyDocument,
    available_tools: &[BootstrapTool],
) -> ToolPolicyResponse {
    ToolPolicyResponse {
        allowed: policy.allowed.clone(),
        available: available_tools.to_vec(),
        revision: policy.revision,
        schema: TOOL_POLICY_SCHEMA,
    }
}

async fn read_session_from_app(
    app: &AgentApp,
    session_id: String,
) -> Result<ReadSessionResponse, String> {
    let turn = app.lease_web_turn().await?;
    turn.read_session(session_id, 0, 1000).await
}

fn project_web_trajectory(session: &ReadSessionResponse) -> Result<Trajectory, String> {
    let revision = session
        .revision
        .parse::<u64>()
        .map_err(|_| "Agent Session revision is invalid".to_owned())?;
    let inspected = InspectedSession {
        session_id: session.session_id.clone(),
        revision,
        events: session
            .events
            .iter()
            .map(|event| {
                Ok(InspectedSessionEvent {
                    revision: event
                        .revision
                        .parse::<u64>()
                        .map_err(|_| "Agent Session event revision is invalid".to_owned())?,
                    event_id: event.event_id.clone(),
                    kind: session_event_kind_name(&event.kind).to_owned(),
                    turn_id: event.turn_id.clone(),
                    occurred_at: event.occurred_at.clone(),
                    payload_json: event.payload_json.as_ref().to_owned(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
    };
    project_trajectory(&inspected)
}

fn session_event_kind_name(kind: &ReadSessionResponseEventsItemKind) -> &'static str {
    match kind {
        ReadSessionResponseEventsItemKind::SessionCreated => "session_created",
        ReadSessionResponseEventsItemKind::SystemInstructionInstalled => {
            "system_instruction_installed"
        }
        ReadSessionResponseEventsItemKind::ContextCompactionStarted => "context_compaction_started",
        ReadSessionResponseEventsItemKind::ContextCompactionCommitted => {
            "context_compaction_committed"
        }
        ReadSessionResponseEventsItemKind::ContextCompactionFailed => "context_compaction_failed",
        ReadSessionResponseEventsItemKind::MemoryRecalled => "memory_recalled",
        ReadSessionResponseEventsItemKind::MemoryRecallFailed => "memory_recall_failed",
        ReadSessionResponseEventsItemKind::MemoryCommitted => "memory_committed",
        ReadSessionResponseEventsItemKind::MemoryCommitFailed => "memory_commit_failed",
        ReadSessionResponseEventsItemKind::TurnStarted => "turn_started",
        ReadSessionResponseEventsItemKind::ModelRequested => "model_requested",
        ReadSessionResponseEventsItemKind::ModelOutput => "model_output",
        ReadSessionResponseEventsItemKind::ToolRequested => "tool_requested",
        ReadSessionResponseEventsItemKind::ToolResult => "tool_result",
        ReadSessionResponseEventsItemKind::TurnCompleted => "turn_completed",
        ReadSessionResponseEventsItemKind::TurnFailed => "turn_failed",
        ReadSessionResponseEventsItemKind::TurnCancelled => "turn_cancelled",
    }
}

async fn list_sessions_from_app(app: &AgentApp) -> Result<WebSessionList, String> {
    let turn = app.lease_web_turn().await?;
    let listed = turn.list_sessions(50).await?;
    project_session_list(&turn, listed).await
}

async fn project_session_list(
    turn: &lenso_agent_host::generation::TurnGeneration,
    listed: ListSessionsResponse,
) -> Result<WebSessionList, String> {
    let mut sessions = Vec::with_capacity(listed.sessions.len());
    for summary in listed.sessions {
        let session = turn
            .read_session(summary.session_id.clone(), 0, 1000)
            .await?;
        let title = session
            .events
            .iter()
            .find(|event| event.kind == ReadSessionResponseEventsItemKind::TurnStarted)
            .and_then(|event| {
                serde_json::from_str::<serde_json::Value>(event.payload_json.as_ref()).ok()
            })
            .and_then(|payload| {
                payload
                    .get("input")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "New chat".to_owned());
        sessions.push(WebSessionSummary {
            revision: summary.revision,
            session_id: summary.session_id,
            title,
            updated_at: summary.updated_at,
        });
    }
    Ok(WebSessionList { sessions })
}

async fn run_turn_on_lease(
    turn: &lenso_agent_host::generation::TurnGeneration,
    request: WebTurnRequest,
    events: &mpsc::Sender<Result<Event, Infallible>>,
    cancellation: CancellationToken,
    allowed_tools: &[String],
) {
    if let Err(error) = invoke_turn(turn, request, events, cancellation, allowed_tools).await {
        send_stream_event(
            events,
            "turn.failed",
            None,
            &WebStreamEvent::Failed { detail: &error },
        )
        .await;
    }
}

async fn invoke_turn(
    turn: &lenso_agent_host::generation::TurnGeneration,
    request: WebTurnRequest,
    events: &mpsc::Sender<Result<Event, Infallible>>,
    cancellation: CancellationToken,
    allowed_tools: &[String],
) -> Result<(), String> {
    let requested_session_id = match (request.session_id, request.edit_turn_id) {
        (Some(session_id), Some(turn_id)) => {
            turn.fork_session_before_turn(session_id, turn_id).await?
        }
        (Some(session_id), None) => session_id,
        (None, None) => turn.open_session().await?,
        (None, Some(_)) => return Err("Editing a message requires its source Session".to_owned()),
    };
    let context = RunScope::new(allowed_tools.iter().cloned())?
        .attach(turn.invocation_context_with_cancellation(cancellation.clone())?)?;
    let stream = turn
        .handle()
        .open_with_context(
            RUN_TURN_OPERATION,
            context,
            RunTurnRequest {
                input: request.input,
                session_id: Some(requested_session_id.clone()),
            },
        )
        .await
        .map_err(|error| format!("Agent stream failed to open: {error:?}"))?
        .map_err(|error| format!("Agent rejected the turn: {error:?}"))?;
    stream
        .close_send()
        .await
        .map_err(|error| format!("failed to half-close Agent input: {error:?}"))?;
    let mut session_id = Some(requested_session_id);
    loop {
        let received = match stream.receive().await {
            Ok(received) => received,
            Err(_) if cancellation.is_cancelled() => {
                send_stream_event(
                    events,
                    "turn.cancelled",
                    None,
                    &WebStreamEvent::Cancelled {
                        session_id: session_id.as_deref(),
                    },
                )
                .await;
                return Ok(());
            }
            Err(error) => return Err(format!("Agent stream failed: {error:?}")),
        };
        match received {
            StreamEvent::Message(message) => {
                session_id = message.session_id.clone().or(session_id);
                let event_id = Some(message.sequence.as_str());
                if !send_stream_event(
                    events,
                    "turn.message",
                    event_id,
                    &WebStreamEvent::Message { message: &message },
                )
                .await
                {
                    return Ok(());
                }
            }
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => {
                send_stream_event(
                    events,
                    "turn.completed",
                    None,
                    &WebStreamEvent::Completed {
                        session_id: session_id.as_deref(),
                    },
                )
                .await;
                return Ok(());
            }
            StreamEvent::Terminal(Err(error)) => {
                if cancellation.is_cancelled() {
                    send_stream_event(
                        events,
                        "turn.cancelled",
                        None,
                        &WebStreamEvent::Cancelled {
                            session_id: session_id.as_deref(),
                        },
                    )
                    .await;
                    return Ok(());
                }
                return Err(format!("Agent turn failed: {error:?}"));
            }
        }
    }
}

async fn send_stream_event(
    events: &mpsc::Sender<Result<Event, Infallible>>,
    kind: &'static str,
    id: Option<&str>,
    payload: &WebStreamEvent<'_>,
) -> bool {
    let Ok(data) = serde_json::to_string(payload) else {
        return false;
    };
    let mut event = Event::default().event(kind).data(data);
    if let Some(id) = id {
        event = event.id(id);
    }
    events.send(Ok(event)).await.is_ok()
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn console_tool_policy_is_explicit_sorted_and_deduplicated() {
        let args = Args::try_parse_from([
            "lenso-agent-web",
            "--allow-tool",
            "workspace.read",
            "--allow-tool",
            "text.echo",
            "--allow-tool",
            "workspace.read",
        ])
        .unwrap();
        assert_eq!(
            normalize_allowed_tools(args.allowed_tools).unwrap(),
            ["text.echo", "workspace.read"]
        );
    }

    #[test]
    fn console_tool_policy_defaults_to_no_tools_and_rejects_invalid_names() {
        let args = Args::try_parse_from(["lenso-agent-web"]).unwrap();
        assert!(
            normalize_allowed_tools(args.allowed_tools)
                .unwrap()
                .is_empty()
        );
        assert!(normalize_allowed_tools(vec![String::new()]).is_err());
    }

    #[test]
    fn rejects_empty_and_oversized_turns() {
        assert!(
            validate_turn_request(&WebTurnRequest {
                edit_turn_id: None,
                input: "  ".to_owned(),
                request_id: "request-1".to_owned(),
                session_id: None,
            })
            .is_err()
        );
        assert!(
            validate_turn_request(&WebTurnRequest {
                edit_turn_id: None,
                input: "x".repeat(MAX_PROMPT_BYTES + 1),
                request_id: "request-2".to_owned(),
                session_id: None,
            })
            .is_err()
        );
    }

    #[test]
    fn validates_session_identity_before_runtime_access() {
        assert!(valid_session_id("session-123_test"));
        assert!(!valid_session_id("../session"));
        assert!(!valid_session_id(""));
    }
}

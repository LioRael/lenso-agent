use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    convert::Infallible,
    fmt,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response, sse::Event, sse::Sse},
    routing::{get, post},
};
use lenso_agent_host::{
    AgentDirectories, AgentHost, Profile, ProviderModelCatalog, WebSurface,
    generation::{AgentApp, RenameSessionFailure},
};
use lenso_agent_loop_plugin::RunScope;
use lenso_agent_session_inspection::{
    InspectedSession, InspectedSessionEvent, Trajectory, project_trajectory,
};
use lenso_agent_session_terminal_plugin as _;
use lenso_agent_web_plugin as _;
use lenso_app_authoring::PluginConfigurationAuthority;
use lenso_capability_agent::{RUN_TURN_OPERATION, RunTurnRequest, RunTurnResponse};
use lenso_capability_agent_context_source::{
    ContextRole, ReadResourceRequest, RenderPromptRequest,
    SnapshotResponse as ContextSnapshotResponse,
};
use lenso_capability_agent_session::{
    ListSessionsResponse, ReadSessionResponse, ReadSessionResponseEventsItemKind, RenameError,
    RenameSessionResponse,
};
use lenso_capability_agent_session_control::CompactSessionResponse;
use lenso_capability_agent_task_supervisor::SnapshotResponse as TaskSnapshotResponse;
use lenso_capability_agent_user_interaction::{
    InteractionAnswer, InteractionOption, InteractionQuestion, PendingInteraction,
};
use lenso_capability_terminal_command::{
    CatalogResponse as TerminalCatalogResponse, ContentType as TerminalContentType,
    ExecuteMessage as TerminalExecuteMessage, ExecuteOpen as TerminalExecuteOpen,
    OutputKind as TerminalOutputKind,
};
use lenso_kernel::{CancellationToken, InvocationContext, StreamEvent};
use lenso_terminal_cli_surface::{ParseOutcome, parse_line as parse_terminal_line};
use lenso_terminal_command_plugin as _;
use lenso_terminal_web_plugin as _;

#[cfg(test)]
pub(crate) fn configure_test_fixture_model(root: &FsPath) {
    let model = root.join("plugins/lenso.agent.model.fixture");
    std::fs::create_dir_all(&model).unwrap();
    std::fs::write(
        model.join("model.toml"),
        concat!(
            "model = \"fixture/readme-summary-v1\"\n",
            "allowed_models = [\"fixture/alternate-v1\"]\n",
        ),
    )
    .unwrap();
    let agent = root.join("plugins/lenso.agent.loop");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(
        agent.join("agent.toml"),
        "model = \"fixture/readme-summary-v1\"\n",
    )
    .unwrap();
}
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_stream::wrappers::ReceiverStream;

mod configuration_service;
mod configuration_store;
mod plugin_control;
mod plugin_control_api;
mod remote_configuration_authority;

pub use configuration_service::{
    CONFIGURATION_SERVICE_READ_TOKEN_ENV, CONFIGURATION_SERVICE_WRITE_TOKEN_ENV,
    PluginConfigurationService, PluginConfigurationServiceAccess,
    PluginConfigurationServiceResource,
};
pub use configuration_store::{
    PluginConfigurationHistoryAuthority, PluginConfigurationPublicationRecord,
    PluginConfigurationStoreConfig, SqlitePluginConfigurationAuthority,
};
pub use remote_configuration_authority::{
    RemotePluginConfigurationAuthority, RemotePluginConfigurationConfig,
    RemotePluginConfigurationResource,
};

use plugin_control::{PluginControl, PluginMutationCoordinator};
use plugin_control_api::{PluginRuntimeCommand, PluginRuntimeState};

const MAX_REQUEST_BYTES: usize = 65_536;
const MAX_DEFERRED_RUNTIME_COMMANDS: usize = 16;
const SESSION_READ_PAGE_LIMIT: i64 = 1000;
const REMOTE_CONFIGURATION_WATCH_WAIT: Duration = Duration::from_secs(5);
const REMOTE_CONFIGURATION_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_PROMPT_BYTES: usize = 65_536;
const TOOL_POLICY_SCHEMA: &str = "lenso.agent.tool-policy.v1";
/// Environment variable used by the standalone server for Tool policy control.
pub const CONTROL_TOKEN_ENV: &str = "LENSO_AGENT_CONTROL_TOKEN";
/// Environment variable used by the standalone server for Agent data-plane authorization.
pub const DATA_PLANE_TOKEN_ENV: &str = "LENSO_AGENT_WEB_TOKEN";

/// Selects which Host seam authorizes Tool policy control requests.
#[derive(Clone, Default)]
pub enum AgentWebControl {
    /// Policy mutation is not exposed by this Surface.
    #[default]
    Disabled,
    /// The standalone HTTP Adapter checks one exact bearer secret.
    Bearer(String),
    /// An embedding Host has already authorized requests before mounting the router.
    HostAuthorized,
}

impl fmt::Debug for AgentWebControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("Disabled"),
            Self::Bearer(_) => formatter.write_str("Bearer([REDACTED])"),
            Self::HostAuthorized => formatter.write_str("HostAuthorized"),
        }
    }
}

/// Selects which Host seam authorizes Agent Web data-plane requests.
#[derive(Clone, Default)]
pub enum AgentWebAccess {
    #[default]
    /// Data-plane routes are unavailable until the Host selects an authorization seam.
    Disabled,
    /// A Host-proven loopback listener accepts requests from the local user.
    Local,
    /// The standalone HTTP Adapter checks one exact bearer secret.
    Bearer(String),
    /// An embedding Host has already authorized requests before mounting the router.
    HostAuthorized,
}

impl fmt::Debug for AgentWebAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("Disabled"),
            Self::Local => formatter.write_str("Local"),
            Self::Bearer(_) => formatter.write_str("Bearer([REDACTED])"),
            Self::HostAuthorized => formatter.write_str("HostAuthorized"),
        }
    }
}

/// Host-owned configuration for one embedded Agent Web Surface.
#[derive(Clone, Debug)]
pub struct AgentWebConfig {
    /// Explicit Agent Home used instead of process-global environment discovery.
    pub agent_home: Option<PathBuf>,
    /// App root whose Plugin Root is managed by the Console surface.
    ///
    /// Omit to preserve the standalone behavior of managing the Agent Home.
    pub managed_app_root: Option<PathBuf>,
    /// Exact immutable Resolved App Plan used as a diagnostic override.
    pub plan: Option<PathBuf>,
    /// Named Agent Profile selected before App resolution.
    pub profile: Option<String>,
    /// Plan-bound Tools admitted to every Turn. Empty means no Tools.
    pub allowed_tools: Vec<String>,
    /// Durable Tool policy file. Omit to keep policy immutable for this process.
    pub tool_policy: Option<PathBuf>,
    /// Host-selected authorization seam for Agent data-plane routes.
    pub access: AgentWebAccess,
    /// Host-selected authorization seam for Tool policy control routes.
    pub control: AgentWebControl,
    /// Enables Host-authorized mutation of the visible App Plugin Root.
    pub plugin_control: bool,
    /// Optional Host-provided authority for Plugin configuration authoring.
    ///
    /// The authority must materialize its complete desired state through the
    /// managed Plugin Root before publication returns. Omit to use the local
    /// Plugin Root authority.
    pub plugin_configuration_authority: Option<Arc<dyn PluginConfigurationAuthority>>,
    /// Optional history and rollback capability paired with the selected authority.
    ///
    /// A remote authority may implement this port without exposing its storage
    /// implementation to the Console. It requires `plugin_configuration_authority`.
    pub plugin_configuration_history: Option<Arc<dyn PluginConfigurationHistoryAuthority>>,
    /// Optional durable managed authority selected by this Host.
    ///
    /// This conflicts with `plugin_configuration_authority`; it is a concrete
    /// standalone adapter for the same Host port.
    pub plugin_configuration_store: Option<PluginConfigurationStoreConfig>,
    /// Optional remote service selected as the Plugin configuration authority.
    ///
    /// This conflicts with both injected and SQLite authorities. The service
    /// owns desired-state CAS; the Host Plugin Root remains its exact materialized mirror.
    pub plugin_configuration_remote: Option<RemotePluginConfigurationConfig>,
    /// Exact linked Plugin inventory exposed by this Host build.
    pub plugins: fn(),
}

impl AgentWebConfig {
    /// Creates an embedded Surface configuration with an explicit Host Plugin inventory.
    pub fn new(plugins: fn()) -> Self {
        Self {
            access: AgentWebAccess::Disabled,
            agent_home: None,
            managed_app_root: None,
            plan: None,
            profile: None,
            allowed_tools: Vec::new(),
            tool_policy: None,
            control: AgentWebControl::Disabled,
            plugin_control: false,
            plugin_configuration_authority: None,
            plugin_configuration_history: None,
            plugin_configuration_store: None,
            plugin_configuration_remote: None,
            plugins,
        }
    }
}

/// A running Agent Web Surface that can be mounted into any Axum Host.
#[derive(Clone, Debug)]
pub struct AgentWebSurface {
    runtime: WebRuntime,
}

#[derive(Clone, Debug)]
struct WebRuntime {
    access: AgentWebAccessPolicy,
    available_tools: Vec<BootstrapTool>,
    commands: mpsc::Sender<RuntimeCommand>,
    control: AgentWebControlPolicy,
    policy: Arc<RwLock<ToolPolicyDocument>>,
    policy_path: Option<PathBuf>,
    profile: Option<String>,
    plugin_control: Option<PluginControl>,
    plugin_mutations: PluginMutationCoordinator,
}

#[derive(Clone)]
enum AgentWebAccessPolicy {
    Disabled,
    Local,
    Bearer([u8; 32]),
    HostAuthorized,
}

impl fmt::Debug for AgentWebAccessPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("Disabled"),
            Self::Local => formatter.write_str("Local"),
            Self::Bearer(_) => formatter.write_str("Bearer([REDACTED])"),
            Self::HostAuthorized => formatter.write_str("HostAuthorized"),
        }
    }
}

impl From<AgentWebAccess> for AgentWebAccessPolicy {
    fn from(access: AgentWebAccess) -> Self {
        match access {
            AgentWebAccess::Local => Self::Local,
            AgentWebAccess::Bearer(expected) if !expected.trim().is_empty() => {
                Self::Bearer(bearer_digest(&expected))
            }
            AgentWebAccess::Disabled | AgentWebAccess::Bearer(_) => Self::Disabled,
            AgentWebAccess::HostAuthorized => Self::HostAuthorized,
        }
    }
}

#[derive(Clone)]
enum AgentWebControlPolicy {
    Disabled,
    Bearer([u8; 32]),
    HostAuthorized,
}

impl fmt::Debug for AgentWebControlPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("Disabled"),
            Self::Bearer(_) => formatter.write_str("Bearer([REDACTED])"),
            Self::HostAuthorized => formatter.write_str("HostAuthorized"),
        }
    }
}

impl AgentWebControlPolicy {
    const fn is_enabled(&self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

impl From<AgentWebControl> for AgentWebControlPolicy {
    fn from(control: AgentWebControl) -> Self {
        match control {
            AgentWebControl::Bearer(expected) if !expected.trim().is_empty() => {
                Self::Bearer(bearer_digest(&expected))
            }
            AgentWebControl::Disabled | AgentWebControl::Bearer(_) => Self::Disabled,
            AgentWebControl::HostAuthorized => Self::HostAuthorized,
        }
    }
}

#[derive(Debug)]
struct WebRuntimeConfig {
    access: AgentWebAccess,
    available_tools: Vec<BootstrapTool>,
    control: AgentWebControl,
    plugin_control: Option<PluginControl>,
    policy: ToolPolicyDocument,
    policy_path: Option<PathBuf>,
    profile: Option<String>,
    remote_configuration: Option<Arc<RemotePluginConfigurationAuthority>>,
}

#[derive(Debug)]
enum RuntimeCommand {
    CompactSession {
        reply: oneshot::Sender<Result<CompactSessionResponse, String>>,
        session_id: String,
    },
    ModelCatalog {
        reply: oneshot::Sender<Result<ProviderModelCatalog, String>>,
    },
    ContextSources {
        reply: oneshot::Sender<Result<ContextSnapshotResponse, String>>,
    },
    TerminalCatalog {
        reply: oneshot::Sender<Result<TerminalCatalogResponse, String>>,
    },
    RunTerminal {
        events: mpsc::Sender<Result<Event, Infallible>>,
        request: WebTerminalRequest,
    },
    CancelTerminal {
        reply: oneshot::Sender<bool>,
        request_id: String,
    },
    TerminalFinished {
        request_id: String,
    },
    Plugin(PluginRuntimeCommand),
    RemoteConfigurationWatchDegraded {
        detail: String,
    },
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
    TaskSnapshot {
        reply: oneshot::Sender<Result<TaskSnapshotResponse, String>>,
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
    RenameSession {
        expected_title_revision: String,
        reply: oneshot::Sender<Result<RenameSessionResponse, RenameSessionFailure>>,
        session_id: String,
        title: String,
    },
    SelectProfile {
        profile: Option<String>,
        reply: oneshot::Sender<Result<SelectedProfileResponse, String>>,
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
struct RemoteConfigurationSyncRuntime {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
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
    allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    edit_turn_id: Option<String>,
    input: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    reasoning_enabled: Option<bool>,
    #[serde(default)]
    reasoning_budget_tokens: Option<u64>,
    request_id: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    service_tier: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WebTerminalRequest {
    command_line: String,
    request_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SelectProfileRequest {
    profile: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SelectedProfileResponse {
    profile: Option<String>,
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
    latest_preview: Option<String>,
    revision: String,
    session_id: String,
    title: String,
    title_revision: String,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RenameSessionRequest {
    expected_title_revision: String,
    title: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RenameSessionResult {
    title: String,
    title_revision: String,
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
#[serde(tag = "type", rename_all = "snake_case")]
enum WebTerminalEvent<'a> {
    #[serde(rename = "terminal_cancelled")]
    Cancelled,
    #[serde(rename = "terminal_completed")]
    Completed,
    #[serde(rename = "terminal_failed")]
    Failed { detail: &'a str },
    #[serde(rename = "terminal_message")]
    Message { message: &'a TerminalExecuteMessage },
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

impl AgentWebSurface {
    /// Resolves the selected App, starts its Generation, and creates an embeddable Web Surface.
    pub async fn start(config: AgentWebConfig) -> Result<Self, String> {
        let AgentWebConfig {
            access,
            agent_home,
            managed_app_root,
            plan,
            profile,
            allowed_tools,
            tool_policy,
            control,
            plugin_control,
            plugin_configuration_authority,
            plugin_configuration_history,
            plugin_configuration_store,
            plugin_configuration_remote,
            plugins,
        } = config;
        let configured_tools = normalize_allowed_tools(allowed_tools)?;
        let selected_profile = match (&plan, &profile) {
            (Some(plan), None) => Profile::resolved_plan(plan),
            (None, Some(profile)) => Profile::named(profile),
            (None, None) => Profile::Default,
            (Some(_), Some(_)) => {
                return Err("an exact Plan conflicts with a named Agent Profile".to_owned());
            }
        };
        let directories = match agent_home.as_ref() {
            Some(agent_home) => AgentDirectories::from_home(agent_home)?,
            None => AgentDirectories::resolve()?,
        };
        let authority_selection = plugin_configuration_authority_selection(
            plugin_configuration_authority.as_ref(),
            plugin_configuration_history.as_ref(),
            plugin_configuration_store.as_ref(),
            plugin_configuration_remote.as_ref(),
        )?;
        validate_plugin_control_configuration(
            plugin_control,
            &control,
            authority_selection,
            plan.is_some(),
            profile.is_some(),
        )?;
        PluginControl::validate_target(managed_app_root.as_deref(), directories.home())?;
        let host = AgentHost::builder().plugins(plugins);
        let host = match agent_home {
            Some(agent_home) => host.agent_home(agent_home)?,
            None => host,
        }
        .surface(WebSurface::browser())
        .build()?;
        host.prepare_authoring()?;
        let app_root = managed_app_root.as_deref().unwrap_or(directories.home());
        let authorities = resolve_configuration_authorities(
            app_root,
            plugin_configuration_authority,
            plugin_configuration_history,
            plugin_configuration_store,
            plugin_configuration_remote,
        )?;
        let app = host.run(selected_profile).await?;
        let ResolvedConfigurationAuthorities {
            authority: plugin_configuration_authority,
            history: plugin_configuration_history,
            remote,
        } = authorities;
        let plugin_control = PluginControl::resolve(
            plugin_control,
            managed_app_root.as_deref(),
            directories.home(),
            profile.clone(),
            plugin_configuration_authority,
            plugin_configuration_history,
        )?;
        let available_tools = resolve_tool_policy(&app, &configured_tools).await?;
        if tool_policy.is_some() && matches!(control, AgentWebControl::Disabled) {
            return Err(format!("a Tool policy requires {CONTROL_TOKEN_ENV}"));
        }
        let policy = load_tool_policy(tool_policy.as_deref(), configured_tools, &available_tools)?;
        let runtime = WebRuntime::start(
            app,
            WebRuntimeConfig {
                access,
                available_tools,
                control,
                plugin_control,
                policy,
                policy_path: tool_policy,
                profile,
                remote_configuration: remote,
            },
        );
        Ok(Self { runtime })
    }

    /// Returns the same-origin Agent HTTP/SSE routes for composition into a Host router.
    pub fn router(&self) -> Router {
        router(self.runtime.clone())
    }

    /// Gracefully stops the App Generation owned by this Web Surface.
    pub async fn shutdown(&self) -> Result<(), String> {
        self.runtime.shutdown().await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PluginConfigurationAuthoritySelection {
    Local,
    Injected,
    InjectedWithHistory,
    Store,
    Remote,
}

fn plugin_configuration_authority_selection(
    injected_authority: Option<&Arc<dyn PluginConfigurationAuthority>>,
    injected_history: Option<&Arc<dyn PluginConfigurationHistoryAuthority>>,
    store: Option<&PluginConfigurationStoreConfig>,
    remote: Option<&RemotePluginConfigurationConfig>,
) -> Result<PluginConfigurationAuthoritySelection, String> {
    let injected_authority = injected_authority.is_some();
    let injected_history = injected_history.is_some();
    let store = store.is_some();
    let remote = remote.is_some();
    if injected_authority && (store || remote) {
        return Err(
            "a concrete Plugin configuration authority conflicts with an injected configuration authority"
                .to_owned(),
        );
    }
    if store && remote {
        return Err(
            "a remote Plugin configuration authority conflicts with the SQLite configuration store"
                .to_owned(),
        );
    }
    if injected_history && !injected_authority {
        return Err(
            "Plugin configuration history requires an injected configuration authority".to_owned(),
        );
    }
    Ok(
        match (injected_authority, injected_history, store, remote) {
            (false, false, false, false) => PluginConfigurationAuthoritySelection::Local,
            (true, false, false, false) => PluginConfigurationAuthoritySelection::Injected,
            (true, true, false, false) => {
                PluginConfigurationAuthoritySelection::InjectedWithHistory
            }
            (false, false, true, false) => PluginConfigurationAuthoritySelection::Store,
            (false, false, false, true) => PluginConfigurationAuthoritySelection::Remote,
            _ => unreachable!("invalid authority combination should fail before selection"),
        },
    )
}

struct ResolvedConfigurationAuthorities {
    authority: Option<Arc<dyn PluginConfigurationAuthority>>,
    history: Option<Arc<dyn PluginConfigurationHistoryAuthority>>,
    remote: Option<Arc<RemotePluginConfigurationAuthority>>,
}

fn resolve_configuration_authorities(
    app_root: &FsPath,
    injected_authority: Option<Arc<dyn PluginConfigurationAuthority>>,
    injected_history: Option<Arc<dyn PluginConfigurationHistoryAuthority>>,
    store: Option<PluginConfigurationStoreConfig>,
    remote: Option<RemotePluginConfigurationConfig>,
) -> Result<ResolvedConfigurationAuthorities, String> {
    match (store, remote) {
        (Some(store), None) => {
            let authority = Arc::new(
                SqlitePluginConfigurationAuthority::open(app_root, store)
                    .map_err(|error| error.to_string())?,
            );
            Ok(ResolvedConfigurationAuthorities {
                authority: Some(Arc::clone(&authority) as Arc<dyn PluginConfigurationAuthority>),
                history: Some(authority as Arc<dyn PluginConfigurationHistoryAuthority>),
                remote: None,
            })
        }
        (None, Some(remote)) => {
            let authority = Arc::new(
                RemotePluginConfigurationAuthority::connect(app_root, remote)
                    .map_err(|error| error.to_string())?,
            );
            Ok(ResolvedConfigurationAuthorities {
                authority: Some(Arc::clone(&authority) as Arc<dyn PluginConfigurationAuthority>),
                history: Some(
                    Arc::clone(&authority) as Arc<dyn PluginConfigurationHistoryAuthority>
                ),
                remote: Some(authority),
            })
        }
        (None, None) => Ok(ResolvedConfigurationAuthorities {
            authority: injected_authority,
            history: injected_history,
            remote: None,
        }),
        (Some(_), Some(_)) => unreachable!("authority selection rejects this conflict"),
    }
}

fn validate_plugin_control_configuration(
    enabled: bool,
    control: &AgentWebControl,
    authority: PluginConfigurationAuthoritySelection,
    has_exact_plan: bool,
    has_named_profile: bool,
) -> Result<(), String> {
    if enabled && matches!(control, AgentWebControl::Disabled) {
        return Err("Plugin Root control requires an authorized Host control seam".to_owned());
    }
    if !enabled && authority != PluginConfigurationAuthoritySelection::Local {
        return Err("a Plugin configuration authority requires Plugin Root control".to_owned());
    }
    if enabled && has_exact_plan {
        return Err("Plugin Root control cannot mutate an exact diagnostic Plan".to_owned());
    }
    if enabled && has_named_profile && authority != PluginConfigurationAuthoritySelection::Local {
        return Err(
            "named Profile Plugin control requires the built-in local configuration authority"
                .to_owned(),
        );
    }
    Ok(())
}

fn router(runtime: WebRuntime) -> Router {
    let data_plane = Router::new()
        .route("/api/console/v1/agent/bootstrap", get(bootstrap))
        .route("/api/console/v1/agent/models", get(model_catalog))
        .route(
            "/api/console/v1/agent/context-sources",
            get(context_sources),
        )
        .route(
            "/api/console/v1/agent/terminal/commands",
            get(terminal_catalog),
        )
        .route(
            "/api/console/v1/agent/terminal/executions",
            post(run_terminal),
        )
        .route(
            "/api/console/v1/agent/terminal/executions/{request_id}/cancel",
            post(cancel_terminal),
        )
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
        .route("/api/console/v1/agent/tasks", get(task_snapshot))
        .route(
            "/api/console/v1/agent/sessions/{session_id}",
            get(read_session).patch(rename_session),
        )
        .route(
            "/api/console/v1/agent/sessions/{session_id}/trajectory",
            get(read_trajectory),
        )
        .route(
            "/api/console/v1/agent/sessions/{session_id}/compact",
            post(compact_session),
        )
        .route_layer(middleware::from_fn_with_state(
            runtime.clone(),
            authorize_data_plane,
        ));
    data_plane
        .route(
            "/api/console/v1/agent/control/tool-policy",
            get(read_tool_policy).put(update_tool_policy),
        )
        .route(
            "/api/console/v1/agent/control/profile",
            post(select_profile),
        )
        .merge(plugin_control::routes())
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .with_state(runtime)
}

async fn authorize_data_plane(
    State(runtime): State<WebRuntime>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, ApiProblem> {
    runtime.authorize_data_plane(request.headers())?;
    Ok(next.run(request).await)
}

async fn model_catalog(
    State(runtime): State<WebRuntime>,
) -> Result<Json<ProviderModelCatalog>, ApiProblem> {
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::ModelCatalog { reply })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
        .map(Json)
        .map_err(ApiProblem::unavailable)
}

async fn context_sources(
    State(runtime): State<WebRuntime>,
) -> Result<Json<ContextSnapshotResponse>, ApiProblem> {
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::ContextSources { reply })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
        .map(Json)
        .map_err(ApiProblem::unavailable)
}

async fn terminal_catalog(
    State(runtime): State<WebRuntime>,
) -> Result<Json<TerminalCatalogResponse>, ApiProblem> {
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::TerminalCatalog { reply })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
        .map(Json)
        .map_err(ApiProblem::unavailable)
}

async fn run_terminal(
    State(runtime): State<WebRuntime>,
    Json(request): Json<WebTerminalRequest>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, ApiProblem> {
    validate_terminal_request(&request)?;
    let (events, receiver) = mpsc::channel(32);
    runtime
        .commands
        .send(RuntimeCommand::RunTerminal { events, request })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    Ok(Sse::new(ReceiverStream::new(receiver)))
}

async fn cancel_terminal(
    State(runtime): State<WebRuntime>,
    Path(request_id): Path<String>,
) -> Result<StatusCode, ApiProblem> {
    validate_request_id(&request_id)?;
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::CancelTerminal { reply, request_id })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    if response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
    {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(ApiProblem::not_found("Terminal command is not active"))
    }
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
            ("sessionRename", true),
            ("sessionCompact", true),
            ("taskSnapshot", true),
            ("contextSources", true),
            ("terminalCommands", true),
            ("turnModelSelection", true),
            ("turnToolSelection", true),
            ("profileSelection", runtime.control.is_enabled()),
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

async fn select_profile(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Json(request): Json<SelectProfileRequest>,
) -> Result<Json<SelectedProfileResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    if request
        .profile
        .as_deref()
        .is_some_and(|profile| profile.is_empty() || profile.len() > 128)
    {
        return Err(ApiProblem::bad_request("Agent Profile is invalid"));
    }
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::SelectProfile {
            profile: request.profile,
            reply,
        })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
        .map(Json)
        .map_err(ApiProblem::conflict)
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

async fn compact_session(
    State(runtime): State<WebRuntime>,
    Path(session_id): Path<String>,
) -> Result<Json<CompactSessionResponse>, ApiProblem> {
    if !valid_session_id(&session_id) {
        return Err(ApiProblem::bad_request("Session ID is invalid"));
    }
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::CompactSession { reply, session_id })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
        .map(Json)
        .map_err(ApiProblem::conflict)
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

async fn task_snapshot(
    State(runtime): State<WebRuntime>,
) -> Result<Json<TaskSnapshotResponse>, ApiProblem> {
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::TaskSnapshot { reply })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
        .map(Json)
        .map_err(ApiProblem::unavailable)
}

async fn rename_session(
    State(runtime): State<WebRuntime>,
    Path(session_id): Path<String>,
    Json(request): Json<RenameSessionRequest>,
) -> Result<Json<RenameSessionResult>, ApiProblem> {
    if !valid_session_id(&session_id) {
        return Err(ApiProblem::bad_request("Session ID is invalid"));
    }
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::RenameSession {
            expected_title_revision: request.expected_title_revision,
            reply,
            session_id,
            title: request.title,
        })
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime is not available"))?;
    let renamed = response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped before replying"))?
        .map_err(rename_problem)?;
    Ok(Json(RenameSessionResult {
        title: renamed.title,
        title_revision: renamed.title_revision,
    }))
}

fn rename_problem(error: RenameSessionFailure) -> ApiProblem {
    match error {
        RenameSessionFailure::Domain(RenameError::InvalidSessionId) => {
            ApiProblem::bad_request("Session ID is invalid")
        }
        RenameSessionFailure::Domain(RenameError::InvalidTitle) => {
            ApiProblem::bad_request("Session title is invalid")
        }
        RenameSessionFailure::Domain(RenameError::InvalidRevision) => {
            ApiProblem::bad_request("Session title revision is invalid")
        }
        RenameSessionFailure::Domain(RenameError::NotFound) => {
            ApiProblem::not_found("Session was not found")
        }
        RenameSessionFailure::Domain(RenameError::RevisionConflict { .. }) => {
            ApiProblem::conflict("Session title changed; reload before saving")
        }
        RenameSessionFailure::Domain(RenameError::Unknown(_)) => {
            ApiProblem::unavailable("Session Plugin returned an unsupported rename error")
        }
        RenameSessionFailure::Runtime(detail) => ApiProblem::unavailable(detail),
    }
}

fn validate_turn_request(request: &WebTurnRequest) -> Result<(), ApiProblem> {
    let reasoning_selections = usize::from(request.reasoning_effort.is_some())
        + usize::from(request.reasoning_enabled.is_some())
        + usize::from(request.reasoning_budget_tokens.is_some());
    if reasoning_selections > 1 {
        return Err(ApiProblem::bad_request("Select only one reasoning control"));
    }
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

fn validate_terminal_request(request: &WebTerminalRequest) -> Result<(), ApiProblem> {
    if request.command_line.trim().is_empty() {
        return Err(ApiProblem::bad_request(
            "Terminal command line must not be empty",
        ));
    }
    if request.command_line.len() > MAX_PROMPT_BYTES {
        return Err(ApiProblem::bad_request(
            "Terminal command line is too large",
        ));
    }
    validate_request_id(&request.request_id)
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
    fn start(app: AgentApp, config: WebRuntimeConfig) -> Self {
        let WebRuntimeConfig {
            access,
            available_tools,
            control,
            plugin_control,
            policy,
            policy_path,
            profile,
            remote_configuration,
        } = config;
        let (commands, receiver) = mpsc::channel(16);
        let policy = Arc::new(RwLock::new(policy));
        let remote_sync = remote_configuration
            .map(|authority| start_remote_configuration_sync(authority, commands.clone()));
        let configuration_authority = plugin_control
            .as_ref()
            .map(PluginControl::configuration_authority_response);
        tokio::task::spawn_local(runtime_actor(
            app,
            receiver,
            commands.clone(),
            Arc::clone(&policy),
            configuration_authority,
            remote_sync,
        ));
        Self {
            access: access.into(),
            available_tools,
            commands,
            control: control.into(),
            policy,
            policy_path,
            profile,
            plugin_control,
            plugin_mutations: PluginMutationCoordinator::default(),
        }
    }

    fn authorize_control(&self, headers: &HeaderMap) -> Result<(), ApiProblem> {
        match &self.control {
            AgentWebControlPolicy::Disabled => {
                Err(ApiProblem::not_found("Agent control is not configured"))
            }
            AgentWebControlPolicy::HostAuthorized => Ok(()),
            AgentWebControlPolicy::Bearer(expected) => authorize_bearer(headers, expected),
        }
    }

    fn authorize_data_plane(&self, headers: &HeaderMap) -> Result<(), ApiProblem> {
        match &self.access {
            AgentWebAccessPolicy::Disabled => {
                Err(ApiProblem::not_found("Agent data plane is not configured"))
            }
            AgentWebAccessPolicy::Local | AgentWebAccessPolicy::HostAuthorized => Ok(()),
            AgentWebAccessPolicy::Bearer(expected) => authorize_bearer(headers, expected),
        }
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

fn authorize_bearer(headers: &HeaderMap, expected: &[u8; 32]) -> Result<(), ApiProblem> {
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if supplied.is_some_and(|supplied| constant_time_digest_eq(&bearer_digest(supplied), expected))
    {
        Ok(())
    } else {
        Err(ApiProblem::forbidden(
            "Agent authorization token is invalid",
        ))
    }
}

fn bearer_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn constant_time_digest_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right.iter())
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
    command_sender: mpsc::Sender<RuntimeCommand>,
    policy: Arc<RwLock<ToolPolicyDocument>>,
    configuration_authority: Option<plugin_control_api::PluginConfigurationAuthorityResponse>,
    mut remote_sync: Option<RemoteConfigurationSyncRuntime>,
) {
    let mut pending = VecDeque::new();
    let mut pre_cancelled = BTreeSet::new();
    let mut active_terminal_commands = BTreeMap::<String, CancellationToken>::new();
    let mut plugin_runtime = PluginRuntimeState::new(&app, configuration_authority);
    loop {
        let command = match pending.pop_front() {
            Some(command) => command,
            None => match commands.recv().await {
                Some(command) => command,
                None => break,
            },
        };
        match command {
            RuntimeCommand::CompactSession { reply, session_id } => {
                let result = match app.lease_web_turn().await {
                    Ok(turn) => turn.compact_session(session_id).await,
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
            RuntimeCommand::ModelCatalog { reply } => {
                let _ = reply.send(app.provider_model_catalog().await);
            }
            RuntimeCommand::ContextSources { reply } => {
                let _ = reply.send(app.web_context_sources().await);
            }
            RuntimeCommand::TerminalCatalog { reply } => {
                let result = match app.lease_web_terminal().await {
                    Ok(terminal) => terminal.catalog().await,
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
            RuntimeCommand::RunTerminal { events, request } => {
                if !active_terminal_commands.is_empty() {
                    send_terminal_event(
                        &events,
                        "terminal.failed",
                        &WebTerminalEvent::Failed {
                            detail: "A Terminal command is already active",
                        },
                    )
                    .await;
                    continue;
                }
                let terminal = match app.lease_web_terminal().await {
                    Ok(terminal) => terminal,
                    Err(error) => {
                        send_terminal_event(
                            &events,
                            "terminal.failed",
                            &WebTerminalEvent::Failed { detail: &error },
                        )
                        .await;
                        continue;
                    }
                };
                let catalog = match terminal.catalog().await {
                    Ok(catalog) => catalog,
                    Err(error) => {
                        send_terminal_event(
                            &events,
                            "terminal.failed",
                            &WebTerminalEvent::Failed { detail: &error },
                        )
                        .await;
                        continue;
                    }
                };
                let line = request
                    .command_line
                    .trim()
                    .strip_prefix('/')
                    .unwrap_or(request.command_line.trim());
                let parsed = match parse_terminal_line(&catalog.commands, "lenso-console", line) {
                    Ok(ParseOutcome::Command(command)) => command,
                    Ok(ParseOutcome::Help(help)) => {
                        let message = TerminalExecuteMessage {
                            content: help,
                            content_type: TerminalContentType::Text,
                            kind: TerminalOutputKind::Result,
                        };
                        send_terminal_event(
                            &events,
                            "terminal.message",
                            &WebTerminalEvent::Message { message: &message },
                        )
                        .await;
                        send_terminal_event(
                            &events,
                            "terminal.completed",
                            &WebTerminalEvent::Completed,
                        )
                        .await;
                        continue;
                    }
                    Ok(ParseOutcome::NoMatch) => {
                        send_terminal_event(
                            &events,
                            "terminal.failed",
                            &WebTerminalEvent::Failed {
                                detail: "Terminal command was not found",
                            },
                        )
                        .await;
                        continue;
                    }
                    Err(error) => {
                        let detail = error.to_string();
                        send_terminal_event(
                            &events,
                            "terminal.failed",
                            &WebTerminalEvent::Failed { detail: &detail },
                        )
                        .await;
                        continue;
                    }
                };
                let request_id = request.request_id;
                let cancellation = CancellationToken::new();
                active_terminal_commands.insert(request_id.clone(), cancellation.clone());
                let sender = command_sender.clone();
                tokio::task::spawn_local(async move {
                    run_terminal_on_lease(terminal, parsed, &events, cancellation).await;
                    let _ = sender
                        .send(RuntimeCommand::TerminalFinished { request_id })
                        .await;
                });
            }
            RuntimeCommand::CancelTerminal { reply, request_id } => {
                let found = active_terminal_commands
                    .get(&request_id)
                    .is_some_and(|cancellation| {
                        cancellation.cancel();
                        true
                    });
                let _ = reply.send(found);
            }
            RuntimeCommand::TerminalFinished { request_id } => {
                active_terminal_commands.remove(&request_id);
            }
            RuntimeCommand::Plugin(command) => plugin_runtime.dispatch(&app, command),
            RuntimeCommand::RemoteConfigurationWatchDegraded { detail } => {
                app.report_plugin_watch_degraded(detail);
            }
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
            RuntimeCommand::TaskSnapshot { reply } => {
                let _ = reply.send(app.web_task_snapshot().await);
            }
            RuntimeCommand::ReadSession { reply, session_id } => {
                handle_read_command(&app, session_id, reply).await;
            }
            RuntimeCommand::RenameSession {
                expected_title_revision,
                reply,
                session_id,
                title,
            } => {
                handle_rename_command(&app, session_id, title, expected_title_revision, reply)
                    .await;
            }
            RuntimeCommand::SelectProfile { profile, reply } => {
                let result = app
                    .select_profile(profile)
                    .await
                    .map(|()| SelectedProfileResponse {
                        profile: app.selected_profile(),
                    });
                let _ = reply.send(result);
            }
            RuntimeCommand::ReadTrajectory { reply, session_id } => {
                let result = read_session_from_app(&app, session_id)
                    .await
                    .and_then(|session| project_web_trajectory(&session));
                let _ = reply.send(result);
            }
            RuntimeCommand::RunTurn {
                events,
                mut request,
            } => {
                if !active_terminal_commands.is_empty() {
                    send_stream_event(
                        &events,
                        "turn.failed",
                        None,
                        &WebStreamEvent::Failed {
                            detail: "A Terminal command is active",
                        },
                    )
                    .await;
                    continue;
                }
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
                request.input = match compose_web_context(&app, &request.input).await {
                    Ok(input) => input,
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
                                    RuntimeCommand::ModelCatalog { reply } => {
                                        let _ = reply.send(app.provider_model_catalog().await);
                                    }
                                    RuntimeCommand::ContextSources { reply } => {
                                        let _ = reply.send(app.web_context_sources().await);
                                    }
                                    RuntimeCommand::TerminalCatalog { reply } => {
                                        let result = match app.lease_web_terminal().await {
                                            Ok(terminal) => terminal.catalog().await,
                                            Err(error) => Err(error),
                                        };
                                        let _ = reply.send(result);
                                    }
                                    RuntimeCommand::CancelTerminal { reply, request_id } => {
                                        let found = active_terminal_commands
                                            .get(&request_id)
                                            .is_some_and(|cancellation| {
                                                cancellation.cancel();
                                                true
                                            });
                                        let _ = reply.send(found);
                                    }
                                    RuntimeCommand::TerminalFinished { request_id } => {
                                        active_terminal_commands.remove(&request_id);
                                    }
                                    RuntimeCommand::RunTerminal { events, .. } => {
                                        send_terminal_event(
                                            &events,
                                            "terminal.failed",
                                            &WebTerminalEvent::Failed {
                                                detail: "An Agent Turn is active",
                                            },
                                        )
                                        .await;
                                    }
                                    RuntimeCommand::Plugin(command) => {
                                        plugin_runtime.dispatch(&app, command);
                                    }
                                    RuntimeCommand::RemoteConfigurationWatchDegraded { detail } => {
                                        app.report_plugin_watch_degraded(detail);
                                    }
                                    RuntimeCommand::TaskSnapshot { reply } => {
                                        let _ = reply.send(turn.task_snapshot().await);
                                    }
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
                                        let found = cancel_active_or_deferred_turn(
                                            &request_id,
                                            &cancelled_id,
                                            &pending,
                                            &mut pre_cancelled,
                                            &cancellation,
                                        );
                                        let _ = reply.send(found);
                                    }
                                    RuntimeCommand::Shutdown { reply } => {
                                        cancellation.cancel();
                                        for cancellation in active_terminal_commands.values() {
                                            cancellation.cancel();
                                        }
                                        shutdown = Some(reply);
                                    }
                                    command => defer_runtime_command(&mut pending, command),
                                }
                            }
                        }
                    }
                    shutdown
                };
                if let Some(reply) = shutdown {
                    let sync = stop_remote_configuration_sync(&mut remote_sync).await;
                    let app = app.shutdown().await;
                    let _ = reply.send(sync.and(app));
                    return;
                }
            }
            RuntimeCommand::Shutdown { reply } => {
                for cancellation in active_terminal_commands.values() {
                    cancellation.cancel();
                }
                let sync = stop_remote_configuration_sync(&mut remote_sync).await;
                let app = app.shutdown().await;
                let _ = reply.send(sync.and(app));
                return;
            }
        }
    }
    for cancellation in active_terminal_commands.values() {
        cancellation.cancel();
    }
    let _ = stop_remote_configuration_sync(&mut remote_sync).await;
    let _ = app.shutdown().await;
}

fn cancel_active_or_deferred_turn(
    active_request_id: &str,
    cancelled_request_id: &str,
    pending: &VecDeque<RuntimeCommand>,
    pre_cancelled: &mut BTreeSet<String>,
    cancellation: &CancellationToken,
) -> bool {
    if cancelled_request_id == active_request_id {
        cancellation.cancel();
        return true;
    }
    if pending.iter().any(|command| {
        matches!(command, RuntimeCommand::RunTurn { request, .. } if request.request_id == cancelled_request_id)
    }) {
        pre_cancelled.insert(cancelled_request_id.to_owned());
        return true;
    }
    false
}

fn defer_runtime_command(pending: &mut VecDeque<RuntimeCommand>, command: RuntimeCommand) {
    if pending.len() < MAX_DEFERRED_RUNTIME_COMMANDS {
        pending.push_back(command);
        return;
    }
    let detail = "Agent runtime deferred-command capacity is exhausted";
    match command {
        RuntimeCommand::ListSessions { reply } => {
            let _ = reply.send(Err(detail.to_owned()));
        }
        RuntimeCommand::ReadSession { reply, .. } => {
            let _ = reply.send(Err(detail.to_owned()));
        }
        RuntimeCommand::ReadTrajectory { reply, .. } => {
            let _ = reply.send(Err(detail.to_owned()));
        }
        RuntimeCommand::RenameSession { reply, .. } => {
            let _ = reply.send(Err(RenameSessionFailure::Runtime(detail.to_owned())));
        }
        RuntimeCommand::CompactSession { reply, .. } => {
            let _ = reply.send(Err(detail.to_owned()));
        }
        RuntimeCommand::SelectProfile { reply, .. } => {
            let _ = reply.send(Err(detail.to_owned()));
        }
        RuntimeCommand::RunTurn { events, .. } => {
            if let Some(event) =
                stream_event("turn.failed", None, &WebStreamEvent::Failed { detail })
            {
                let _ = events.try_send(Ok(event));
            }
        }
        RuntimeCommand::RunTerminal { events, .. } => {
            if let Some(event) =
                terminal_stream_event("terminal.failed", &WebTerminalEvent::Failed { detail })
            {
                let _ = events.try_send(Ok(event));
            }
        }
        RuntimeCommand::ModelCatalog { .. }
        | RuntimeCommand::ContextSources { .. }
        | RuntimeCommand::TerminalCatalog { .. }
        | RuntimeCommand::CancelTerminal { .. }
        | RuntimeCommand::TerminalFinished { .. }
        | RuntimeCommand::Plugin(_)
        | RuntimeCommand::RemoteConfigurationWatchDegraded { .. }
        | RuntimeCommand::AnswerInteraction { .. }
        | RuntimeCommand::CancelTurn { .. }
        | RuntimeCommand::TaskSnapshot { .. }
        | RuntimeCommand::PendingInteractions { .. }
        | RuntimeCommand::Shutdown { .. } => {
            unreachable!("active-Turn priority command reached the deferred queue")
        }
    }
}

fn start_remote_configuration_sync(
    authority: Arc<RemotePluginConfigurationAuthority>,
    commands: mpsc::Sender<RuntimeCommand>,
) -> RemoteConfigurationSyncRuntime {
    let (stop, receiver) = watch::channel(false);
    let task = tokio::spawn(remote_configuration_sync_actor(
        authority, commands, receiver,
    ));
    RemoteConfigurationSyncRuntime { stop, task }
}

async fn remote_configuration_sync_actor(
    authority: Arc<RemotePluginConfigurationAuthority>,
    commands: mpsc::Sender<RuntimeCommand>,
    mut stop: watch::Receiver<bool>,
) {
    let mut last_error = None::<String>;
    loop {
        if *stop.borrow() {
            break;
        }
        let target = Arc::clone(&authority);
        let result = tokio::task::spawn_blocking(move || {
            target.synchronize(REMOTE_CONFIGURATION_WATCH_WAIT)
        })
        .await
        .map_err(|error| format!("remote configuration synchronization task failed: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()));
        let changed = match result {
            Ok(changed) => {
                last_error = None;
                changed
            }
            Err(detail) => {
                if last_error.as_deref() != Some(&detail) {
                    if commands
                        .send(RuntimeCommand::RemoteConfigurationWatchDegraded {
                            detail: detail.clone(),
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                    last_error = Some(detail);
                }
                false
            }
        };
        if changed {
            continue;
        }
        tokio::select! {
            update = stop.changed() => {
                if update.is_err() || *stop.borrow() {
                    break;
                }
            }
            () = tokio::time::sleep(REMOTE_CONFIGURATION_RETRY_DELAY) => {}
        }
    }
}

async fn stop_remote_configuration_sync(
    runtime: &mut Option<RemoteConfigurationSyncRuntime>,
) -> Result<(), String> {
    let Some(runtime) = runtime.take() else {
        return Ok(());
    };
    runtime
        .stop
        .send(true)
        .map_err(|_| "remote configuration synchronizer already stopped".to_owned())?;
    runtime
        .task
        .await
        .map_err(|error| format!("remote configuration synchronizer failed: {error}"))
}

async fn handle_read_command(
    app: &AgentApp,
    session_id: String,
    reply: oneshot::Sender<Result<ReadSessionResponse, String>>,
) {
    let _ = reply.send(read_session_from_app(app, session_id).await);
}

async fn handle_rename_command(
    app: &AgentApp,
    session_id: String,
    title: String,
    expected_title_revision: String,
    reply: oneshot::Sender<Result<RenameSessionResponse, RenameSessionFailure>>,
) {
    let result = rename_session_from_app(app, session_id, title, expected_title_revision).await;
    let _ = reply.send(result);
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
    collect_session_pages(&session_id, |cursor| {
        turn.read_session(session_id.clone(), cursor, SESSION_READ_PAGE_LIMIT)
    })
    .await
}

async fn collect_session_pages<F, Fut>(
    session_id: &str,
    mut read: F,
) -> Result<ReadSessionResponse, String>
where
    F: FnMut(u64) -> Fut,
    Fut: std::future::Future<Output = Result<ReadSessionResponse, String>>,
{
    let mut cursor = 0_u64;
    let mut complete: Option<ReadSessionResponse> = None;
    let mut expected_revision = None;
    loop {
        let mut page = read(cursor).await?;
        if page.session_id != session_id {
            return Err("Session read returned a different Session ID".to_owned());
        }
        let revision = page
            .revision
            .parse::<u64>()
            .map_err(|_| "Session returned an invalid revision".to_owned())?;
        match expected_revision {
            Some(expected) if revision != expected => {
                return Err("Session changed while its history was being read".to_owned());
            }
            None => expected_revision = Some(revision),
            _ => {}
        }
        if let Some(first) = complete.as_ref()
            && (page.title != first.title || page.title_revision != first.title_revision)
        {
            return Err("Session metadata changed while its history was being read".to_owned());
        }
        for event in &page.events {
            let event_revision = event
                .revision
                .parse::<u64>()
                .map_err(|_| "Session returned an invalid event revision".to_owned())?;
            let next = cursor
                .checked_add(1)
                .ok_or_else(|| "Session revision overflowed".to_owned())?;
            if event_revision != next || event_revision > revision {
                return Err("Session read returned non-contiguous events".to_owned());
            }
            cursor = event_revision;
        }
        if page.events.is_empty() && cursor != revision {
            return Err("Session read ended before its advertised revision".to_owned());
        }
        match complete.as_mut() {
            Some(complete) => complete.events.append(&mut page.events),
            None => complete = Some(page),
        }
        if cursor == revision {
            return complete.ok_or_else(|| "Session read returned no page".to_owned());
        }
    }
}

fn project_web_trajectory(session: &ReadSessionResponse) -> Result<Trajectory, String> {
    let revision = session
        .revision
        .parse::<u64>()
        .map_err(|_| "Agent Session revision is invalid".to_owned())?;
    let title_revision = session
        .title_revision
        .as_ref()
        .map(|revision| {
            revision
                .parse::<u64>()
                .map_err(|_| "Agent Session title revision is invalid".to_owned())
        })
        .transpose()?
        .unwrap_or_default();
    let inspected = InspectedSession {
        title: (title_revision > 0)
            .then(|| session.title.clone())
            .flatten(),
        title_revision,
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
    Ok(project_session_list(listed))
}

async fn rename_session_from_app(
    app: &AgentApp,
    session_id: String,
    title: String,
    expected_title_revision: String,
) -> Result<RenameSessionResponse, RenameSessionFailure> {
    let turn = app
        .lease_web_turn()
        .await
        .map_err(RenameSessionFailure::Runtime)?;
    turn.rename_session(session_id, title, expected_title_revision)
        .await
}

fn project_session_list(listed: ListSessionsResponse) -> WebSessionList {
    let sessions = listed
        .sessions
        .into_iter()
        .map(|summary| WebSessionSummary {
            latest_preview: summary.latest_preview,
            revision: summary.revision,
            session_id: summary.session_id,
            title: summary.title.unwrap_or_else(|| "New chat".to_owned()),
            title_revision: summary.title_revision.unwrap_or_else(|| "0".to_owned()),
            updated_at: summary.updated_at,
        })
        .collect();
    WebSessionList { sessions }
}

async fn compose_web_context(app: &AgentApp, input: &str) -> Result<String, String> {
    if let Some((source, name, task)) = selected_context_prompt(input)? {
        let rendered = app
            .render_web_context_prompt(RenderPromptRequest {
                source: source.to_owned(),
                name: name.to_owned(),
                arguments_json: "{}"
                    .to_owned()
                    .try_into()
                    .expect("empty JSON object is valid"),
            })
            .await?;
        let messages = rendered
            .messages
            .into_iter()
            .map(|message| {
                let role = match message.role {
                    ContextRole::User => "user",
                    ContextRole::Assistant => "assistant",
                };
                format!("[{role}]\n{}", message.text)
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        return Ok(format!(
            "Selected Context Prompt: {source}/{name}\n{messages}\n\n---\n\nUser task:\n{}",
            task.trim()
        ));
    }
    if let Some((source, uri, task)) = selected_context_resource(input)? {
        let response = app
            .read_web_context_resource(ReadResourceRequest {
                source: source.to_owned(),
                uri: uri.to_owned(),
            })
            .await?;
        let contents = response
            .contents
            .into_iter()
            .map(|content| {
                let body = content.text.flatten().unwrap_or_else(|| {
                    format!(
                        "[binary resource available: {} bytes base64]",
                        content.data_base64.flatten().map_or(0, |data| data.len())
                    )
                });
                format!(
                    "URI: {}\nMIME: {}\n{}",
                    content.uri, content.mime_type, body
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        return Ok(format!(
            "Selected Context Resource: {source}/{uri}\n{contents}\n\n---\n\nUser task:\n{}",
            task.trim()
        ));
    }
    Ok(input.to_owned())
}

fn selected_context_prompt(input: &str) -> Result<Option<(&str, &str, &str)>, String> {
    let Some(selection) = input.strip_prefix("/mcp-prompt ") else {
        return Ok(None);
    };
    let (identity, task) = selection
        .split_once(char::is_whitespace)
        .ok_or_else(|| "an MCP Prompt selection must be followed by the user task".to_owned())?;
    let (source, name) = identity
        .split_once('/')
        .ok_or_else(|| "invalid MCP Prompt source/name".to_owned())?;
    let task = task.trim();
    if task.is_empty() {
        return Err("an MCP Prompt selection must be followed by the user task".to_owned());
    }
    Ok(Some((source, name, task)))
}

fn selected_context_resource(input: &str) -> Result<Option<(&str, &str, &str)>, String> {
    let Some(selection) = input.strip_prefix("/mcp-resource ") else {
        return Ok(None);
    };
    let (identity, task) = selection
        .split_once(char::is_whitespace)
        .ok_or_else(|| "an MCP Resource selection must be followed by the user task".to_owned())?;
    let (source, uri) = identity
        .split_once('=')
        .ok_or_else(|| "invalid MCP Resource source=URI".to_owned())?;
    let task = task.trim();
    if task.is_empty() {
        return Err("an MCP Resource selection must be followed by the user task".to_owned());
    }
    Ok(Some((source, uri, task)))
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

fn turn_invocation_context(
    turn: &lenso_agent_host::generation::TurnGeneration,
    request: &WebTurnRequest,
    cancellation: CancellationToken,
) -> Result<InvocationContext, String> {
    turn.invocation_context_for_model_controls_with_cancellation(
        request.model.as_deref(),
        request.reasoning_effort.as_deref(),
        request.reasoning_enabled,
        request.reasoning_budget_tokens,
        request.service_tier.as_deref(),
        cancellation,
    )
}

async fn invoke_turn(
    turn: &lenso_agent_host::generation::TurnGeneration,
    request: WebTurnRequest,
    events: &mpsc::Sender<Result<Event, Infallible>>,
    cancellation: CancellationToken,
    allowed_tools: &[String],
) -> Result<(), String> {
    let requested_tools = resolve_turn_tools(request.allowed_tools.as_deref(), allowed_tools)?;
    let context = RunScope::new(requested_tools)?.attach(turn_invocation_context(
        turn,
        &request,
        cancellation.clone(),
    )?)?;
    let requested_session_id = match (request.session_id, request.edit_turn_id) {
        (Some(session_id), Some(turn_id)) => {
            turn.fork_session_before_turn(session_id, turn_id).await?
        }
        (Some(session_id), None) => session_id,
        (None, None) => turn.open_session().await?,
        (None, Some(_)) => return Err("Editing a message requires its source Session".to_owned()),
    };
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

fn resolve_turn_tools(
    requested: Option<&[String]>,
    policy: &[String],
) -> Result<Vec<String>, String> {
    let requested = requested.unwrap_or(policy);
    if requested
        .iter()
        .any(|tool| !policy.iter().any(|allowed| allowed == tool))
    {
        return Err("Turn Tool selection exceeds the configured Agent Tool policy".to_owned());
    }
    Ok(requested.to_vec())
}

async fn run_terminal_on_lease(
    terminal: lenso_agent_host::generation::TerminalGeneration,
    command: lenso_terminal_cli_surface::ParsedCommand,
    events: &mpsc::Sender<Result<Event, Infallible>>,
    cancellation: CancellationToken,
) {
    let request = TerminalExecuteOpen {
        id: command.id,
        arguments_json: match command.arguments_json.try_into() {
            Ok(arguments) => arguments,
            Err(_) => {
                send_terminal_event(
                    events,
                    "terminal.failed",
                    &WebTerminalEvent::Failed {
                        detail: "Terminal command arguments are not valid JSON",
                    },
                )
                .await;
                return;
            }
        },
        output_format: command.output_format,
    };
    let stream = match terminal
        .execute_with_cancellation(request, cancellation.clone())
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            if cancellation.is_cancelled() {
                send_terminal_event(events, "terminal.cancelled", &WebTerminalEvent::Cancelled)
                    .await;
            } else {
                send_terminal_event(
                    events,
                    "terminal.failed",
                    &WebTerminalEvent::Failed { detail: &error },
                )
                .await;
            }
            return;
        }
    };
    if let Err(error) = stream.close_send().await {
        let detail = format!("failed to half-close terminal command input: {error:?}");
        send_terminal_event(
            events,
            "terminal.failed",
            &WebTerminalEvent::Failed { detail: &detail },
        )
        .await;
        return;
    }
    loop {
        match stream.receive().await {
            Ok(StreamEvent::Message(message)) => {
                if !send_terminal_event(
                    events,
                    "terminal.message",
                    &WebTerminalEvent::Message { message: &message },
                )
                .await
                {
                    cancellation.cancel();
                    return;
                }
            }
            Ok(StreamEvent::PeerHalfClosed) => {}
            Ok(StreamEvent::Terminal(Ok(()))) => {
                send_terminal_event(events, "terminal.completed", &WebTerminalEvent::Completed)
                    .await;
                return;
            }
            Ok(StreamEvent::Terminal(Err(error))) => {
                let detail = format!("terminal command failed: {error:?}");
                send_terminal_event(
                    events,
                    "terminal.failed",
                    &WebTerminalEvent::Failed { detail: &detail },
                )
                .await;
                return;
            }
            Err(_) if cancellation.is_cancelled() => {
                send_terminal_event(events, "terminal.cancelled", &WebTerminalEvent::Cancelled)
                    .await;
                return;
            }
            Err(error) => {
                let detail = format!("terminal command stream failed: {error:?}");
                send_terminal_event(
                    events,
                    "terminal.failed",
                    &WebTerminalEvent::Failed { detail: &detail },
                )
                .await;
                return;
            }
        }
    }
}

async fn send_terminal_event(
    events: &mpsc::Sender<Result<Event, Infallible>>,
    kind: &'static str,
    payload: &WebTerminalEvent<'_>,
) -> bool {
    let Some(event) = terminal_stream_event(kind, payload) else {
        return false;
    };
    events.send(Ok(event)).await.is_ok()
}

fn terminal_stream_event(kind: &'static str, payload: &WebTerminalEvent<'_>) -> Option<Event> {
    let data = serde_json::to_string(payload).ok()?;
    Some(Event::default().event(kind).data(data))
}

async fn send_stream_event(
    events: &mpsc::Sender<Result<Event, Infallible>>,
    kind: &'static str,
    id: Option<&str>,
    payload: &WebStreamEvent<'_>,
) -> bool {
    let Some(event) = stream_event(kind, id, payload) else {
        return false;
    };
    events.send(Ok(event)).await.is_ok()
}

fn stream_event(
    kind: &'static str,
    id: Option<&str>,
    payload: &WebStreamEvent<'_>,
) -> Option<Event> {
    let data = serde_json::to_string(payload).ok()?;
    let mut event = Event::default().event(kind).data(data);
    if let Some(id) = id {
        event = event.id(id);
    }
    Some(event)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::future::ready;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::http::Method;
    use lenso_app_authoring::{
        LocalPluginRootAuthority, PluginConfigurationAuthoritySource, PluginConfigurationProposal,
        PluginRootAuthoringState, PluginRootRevision,
    };
    use tower::ServiceExt as _;

    use super::*;

    #[derive(Debug)]
    struct RecordingConfigurationAuthority {
        inspections: Arc<AtomicUsize>,
        local: LocalPluginRootAuthority,
        source: PluginConfigurationAuthoritySource,
    }

    impl PluginConfigurationAuthority for RecordingConfigurationAuthority {
        fn source(&self) -> PluginConfigurationAuthoritySource {
            self.source.clone()
        }

        fn inspect(&self) -> anyhow::Result<PluginRootAuthoringState> {
            self.inspections.fetch_add(1, Ordering::Relaxed);
            self.local.inspect()
        }

        fn propose(
            &self,
            expected_revision: &PluginRootRevision,
            plugin_id: &str,
            instance: &str,
            bytes: &[u8],
        ) -> anyhow::Result<PluginConfigurationProposal> {
            self.local
                .propose(expected_revision, plugin_id, instance, bytes)
        }

        fn publish(
            &self,
            proposal: &PluginConfigurationProposal,
        ) -> anyhow::Result<lenso_app_authoring::PluginConfigurationPublication> {
            self.local.publish(proposal)
        }
    }

    fn runtime_with_access(access: AgentWebAccess) -> WebRuntime {
        let (commands, _receiver) = mpsc::channel(1);
        WebRuntime {
            access: access.into(),
            available_tools: Vec::new(),
            commands,
            control: AgentWebControl::Disabled.into(),
            policy: Arc::new(RwLock::new(ToolPolicyDocument {
                allowed: Vec::new(),
                revision: 0,
                schema: TOOL_POLICY_SCHEMA.to_owned(),
            })),
            policy_path: None,
            profile: None,
            plugin_control: None,
            plugin_mutations: PluginMutationCoordinator::default(),
        }
    }

    async fn bootstrap_status(access: AgentWebAccess, token: Option<&str>) -> StatusCode {
        let mut request = Request::builder().uri("/api/console/v1/agent/bootstrap");
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        router(runtime_with_access(access))
            .oneshot(request.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    async fn control_status(
        access: AgentWebAccess,
        control: AgentWebControl,
        token: Option<&str>,
    ) -> StatusCode {
        let mut runtime = runtime_with_access(access);
        runtime.control = control.into();
        let mut request = Request::builder().uri("/api/console/v1/agent/control/tool-policy");
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        router(runtime)
            .oneshot(request.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn data_plane_middleware_enforces_bearer_and_preserves_host_authorized_local_access() {
        assert_eq!(
            bootstrap_status(AgentWebAccess::Bearer("secret".to_owned()), None).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            bootstrap_status(AgentWebAccess::Bearer("secret".to_owned()), Some("wrong")).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            bootstrap_status(AgentWebAccess::Bearer("secret".to_owned()), Some("secret")).await,
            StatusCode::OK
        );
        assert_eq!(
            bootstrap_status(AgentWebAccess::Bearer("   ".to_owned()), None).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            bootstrap_status(AgentWebAccess::Local, None).await,
            StatusCode::OK
        );
        assert_eq!(
            bootstrap_status(AgentWebAccess::HostAuthorized, None).await,
            StatusCode::OK
        );
        assert_eq!(
            bootstrap_status(AgentWebAccess::Disabled, None).await,
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn embedded_web_config_disables_the_data_plane_by_default() {
        let config = AgentWebConfig::new(lenso_agent_console_plugins::link);
        assert!(matches!(config.access, AgentWebAccess::Disabled));
    }

    #[test]
    fn authorization_token_comparison_covers_equal_and_unequal_lengths() {
        let expected = bearer_digest("same-secret");
        assert!(constant_time_digest_eq(
            &bearer_digest("same-secret"),
            &expected
        ));
        assert!(!constant_time_digest_eq(
            &bearer_digest("same-secreu"),
            &expected
        ));
        assert!(!constant_time_digest_eq(
            &bearer_digest("a-different-length"),
            &bearer_digest("short")
        ));
    }

    #[test]
    fn authorization_debug_output_redacts_bearer_secrets() {
        let data_secret = "data-secret-must-not-leak";
        let control_secret = "control-secret-must-not-leak";
        let access = AgentWebAccess::Bearer(data_secret.to_owned());
        let control = AgentWebControl::Bearer(control_secret.to_owned());
        let mut config = AgentWebConfig::new(lenso_agent_console_plugins::link);
        config.access = access.clone();
        config.control = control.clone();
        let mut runtime = runtime_with_access(access.clone());
        runtime.control = control.clone().into();
        let surface = AgentWebSurface {
            runtime: runtime.clone(),
        };

        for rendered in [
            format!("{access:?}"),
            format!("{control:?}"),
            format!("{config:?}"),
            format!("{runtime:?}"),
            format!("{surface:?}"),
        ] {
            assert!(rendered.contains("REDACTED") || !rendered.contains("Bearer"));
            assert!(!rendered.contains(data_secret));
            assert!(!rendered.contains(control_secret));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn data_plane_authorization_is_independent_from_control_authorization() {
        assert_eq!(
            control_status(
                AgentWebAccess::Disabled,
                AgentWebControl::HostAuthorized,
                None,
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            control_status(
                AgentWebAccess::Bearer("data-secret".to_owned()),
                AgentWebControl::Bearer("control-secret".to_owned()),
                Some("data-secret"),
            )
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            control_status(
                AgentWebAccess::Bearer("data-secret".to_owned()),
                AgentWebControl::Bearer("control-secret".to_owned()),
                Some("control-secret"),
            )
            .await,
            StatusCode::OK
        );
    }

    #[test]
    fn console_tool_policy_is_explicit_sorted_and_deduplicated() {
        assert_eq!(
            normalize_allowed_tools(vec![
                "workspace.read".to_owned(),
                "text.echo".to_owned(),
                "workspace.read".to_owned(),
            ])
            .unwrap(),
            ["text.echo", "workspace.read"]
        );
    }

    #[test]
    fn console_tool_policy_defaults_to_no_tools_and_rejects_invalid_names() {
        assert!(normalize_allowed_tools(Vec::new()).unwrap().is_empty());
        assert!(normalize_allowed_tools(vec![String::new()]).is_err());
    }

    #[test]
    fn turn_tool_selection_can_only_narrow_the_host_policy() {
        let policy = vec!["read".to_owned(), "write".to_owned()];

        assert_eq!(
            resolve_turn_tools(Some(&["read".to_owned()]), &policy).unwrap(),
            ["read"]
        );
        assert!(resolve_turn_tools(Some(&[]), &policy).unwrap().is_empty());
        assert!(resolve_turn_tools(Some(&["shell".to_owned()]), &policy).is_err());
    }

    #[test]
    fn web_context_selection_requires_an_identity_and_user_task() {
        assert_eq!(selected_context_prompt("plain task").unwrap(), None);
        assert_eq!(
            selected_context_prompt("/mcp-prompt workspace/brief Review this").unwrap(),
            Some(("workspace", "brief", "Review this"))
        );
        assert_eq!(
            selected_context_resource("/mcp-resource workspace=file:///architecture.md Explain it")
                .unwrap(),
            Some(("workspace", "file:///architecture.md", "Explain it"))
        );
        assert!(selected_context_prompt("/mcp-prompt workspace/brief ").is_err());
        assert!(selected_context_resource("/mcp-resource workspace=file:///brief ").is_err());
        assert!(selected_context_prompt("/mcp-prompt invalid Review").is_err());
        assert!(selected_context_resource("/mcp-resource invalid Review").is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn active_turn_deferred_commands_are_bounded_and_overflow_fails_closed() {
        let mut pending = VecDeque::new();
        for index in 0..MAX_DEFERRED_RUNTIME_COMMANDS {
            let (events, receiver) = mpsc::channel(1);
            defer_runtime_command(
                &mut pending,
                RuntimeCommand::RunTurn {
                    events,
                    request: WebTurnRequest {
                        allowed_tools: None,
                        edit_turn_id: None,
                        input: "queued".to_owned(),
                        model: None,
                        reasoning_effort: None,
                        reasoning_enabled: None,
                        reasoning_budget_tokens: None,
                        request_id: format!("queued-{index}"),
                        session_id: None,
                        service_tier: None,
                    },
                },
            );
            drop(receiver);
        }
        let (events, mut receiver) = mpsc::channel(1);
        defer_runtime_command(
            &mut pending,
            RuntimeCommand::RunTurn {
                events,
                request: WebTurnRequest {
                    allowed_tools: None,
                    edit_turn_id: None,
                    input: "overflow".to_owned(),
                    model: None,
                    reasoning_effort: None,
                    reasoning_enabled: None,
                    reasoning_budget_tokens: None,
                    request_id: "overflow".to_owned(),
                    session_id: None,
                    service_tier: None,
                },
            },
        );

        assert_eq!(pending.len(), MAX_DEFERRED_RUNTIME_COMMANDS);
        assert!(matches!(receiver.recv().await, Some(Ok(_))));
    }

    #[test]
    fn deferred_turn_overflow_never_waits_for_a_slow_sse_consumer() {
        let mut pending = (0..MAX_DEFERRED_RUNTIME_COMMANDS)
            .map(|index| {
                let (events, _receiver) = mpsc::channel(1);
                RuntimeCommand::RunTurn {
                    events,
                    request: WebTurnRequest {
                        allowed_tools: None,
                        edit_turn_id: None,
                        input: "queued".to_owned(),
                        model: None,
                        reasoning_effort: None,
                        reasoning_enabled: None,
                        reasoning_budget_tokens: None,
                        request_id: format!("queued-{index}"),
                        session_id: None,
                        service_tier: None,
                    },
                }
            })
            .collect::<VecDeque<_>>();
        let (events, receiver) = mpsc::channel(1);
        events.try_send(Ok(Event::default())).unwrap();

        defer_runtime_command(
            &mut pending,
            RuntimeCommand::RunTurn {
                events,
                request: WebTurnRequest {
                    allowed_tools: None,
                    edit_turn_id: None,
                    input: "overflow".to_owned(),
                    model: None,
                    reasoning_effort: None,
                    reasoning_enabled: None,
                    reasoning_budget_tokens: None,
                    request_id: "overflow".to_owned(),
                    session_id: None,
                    service_tier: None,
                },
            },
        );

        assert_eq!(pending.len(), MAX_DEFERRED_RUNTIME_COMMANDS);
        assert_eq!(receiver.len(), 1);
    }

    #[test]
    fn deferred_turns_drain_in_admission_order() {
        let mut pending = VecDeque::new();
        for request_id in ["first", "second", "third"] {
            let (events, _receiver) = mpsc::channel(1);
            defer_runtime_command(
                &mut pending,
                RuntimeCommand::RunTurn {
                    events,
                    request: WebTurnRequest {
                        allowed_tools: None,
                        edit_turn_id: None,
                        input: "queued".to_owned(),
                        model: None,
                        reasoning_effort: None,
                        reasoning_enabled: None,
                        reasoning_budget_tokens: None,
                        request_id: request_id.to_owned(),
                        session_id: None,
                        service_tier: None,
                    },
                },
            );
        }

        let mut drained = Vec::new();
        while let Some(command) = pending.pop_front() {
            let RuntimeCommand::RunTurn { request, .. } = command else {
                panic!("unexpected deferred command")
            };
            drained.push(request.request_id);
        }
        assert_eq!(drained, ["first", "second", "third"]);
    }

    #[test]
    fn active_turn_cancellation_prioritizes_active_and_marks_admitted_deferred_turns() {
        let mut pending = VecDeque::new();
        for request_id in ["queued-a", "queued-b"] {
            let (events, _receiver) = mpsc::channel(1);
            pending.push_back(RuntimeCommand::RunTurn {
                events,
                request: WebTurnRequest {
                    allowed_tools: None,
                    edit_turn_id: None,
                    input: "queued".to_owned(),
                    model: None,
                    reasoning_effort: None,
                    reasoning_enabled: None,
                    reasoning_budget_tokens: None,
                    request_id: request_id.to_owned(),
                    session_id: None,
                    service_tier: None,
                },
            });
        }
        let cancellation = CancellationToken::new();
        let mut pre_cancelled = BTreeSet::new();

        assert!(cancel_active_or_deferred_turn(
            "active",
            "queued-b",
            &pending,
            &mut pre_cancelled,
            &cancellation,
        ));
        assert!(!cancellation.is_cancelled());
        assert_eq!(pre_cancelled, BTreeSet::from(["queued-b".to_owned()]));

        assert!(cancel_active_or_deferred_turn(
            "active",
            "active",
            &pending,
            &mut pre_cancelled,
            &cancellation,
        ));
        assert!(cancellation.is_cancelled());

        let unrelated = CancellationToken::new();
        assert!(!cancel_active_or_deferred_turn(
            "another-active",
            "missing",
            &pending,
            &mut pre_cancelled,
            &unrelated,
        ));
        assert!(!unrelated.is_cancelled());
    }

    fn session_page(
        revision: u64,
        title: &str,
        title_revision: u64,
        event_revisions: impl IntoIterator<Item = u64>,
    ) -> ReadSessionResponse {
        let events = event_revisions
            .into_iter()
            .map(|event_revision| {
                serde_json::json!({
                    "revision": event_revision.to_string(),
                    "event_id": format!("event-{event_revision}"),
                    "kind": "turn_started",
                    "turn_id": format!("turn-{event_revision}"),
                    "occurred_at": "2026-08-30T00:00:00Z",
                    "payload_json": "{}",
                })
            })
            .collect::<Vec<_>>();
        serde_json::from_value(serde_json::json!({
            "session_id": "session-a",
            "revision": revision.to_string(),
            "title": title,
            "title_revision": title_revision.to_string(),
            "events": events,
        }))
        .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_read_collects_more_than_one_bounded_page() {
        let mut pages = VecDeque::from([
            session_page(1001, "Stable title", 1, 1..=1000),
            session_page(1001, "Stable title", 1, 1001..=1001),
        ]);
        let cursors = Rc::new(RefCell::new(Vec::new()));
        let observed = Rc::clone(&cursors);

        let session = collect_session_pages("session-a", move |cursor| {
            observed.borrow_mut().push(cursor);
            ready(
                pages
                    .pop_front()
                    .ok_or_else(|| "unexpected extra Session page".to_owned()),
            )
        })
        .await
        .unwrap();

        assert_eq!(&*cursors.borrow(), &[0, 1000]);
        assert_eq!(session.events.len(), 1001);
        assert_eq!(session.events.last().unwrap().revision, "1001");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_read_rejects_metadata_changes_between_pages() {
        let mut pages = VecDeque::from([
            session_page(2, "First title", 1, 1..=1),
            session_page(2, "Changed title", 2, 2..=2),
        ]);

        let error = collect_session_pages("session-a", move |_| {
            ready(
                pages
                    .pop_front()
                    .ok_or_else(|| "unexpected extra Session page".to_owned()),
            )
        })
        .await
        .unwrap_err();

        assert!(error.contains("metadata changed"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_read_rejects_non_contiguous_event_revisions() {
        let mut pages = VecDeque::from([
            session_page(3, "Stable title", 1, 1..=1),
            session_page(3, "Stable title", 1, 3..=3),
        ]);

        let error = collect_session_pages("session-a", move |_| {
            ready(
                pages
                    .pop_front()
                    .ok_or_else(|| "unexpected extra Session page".to_owned()),
            )
        })
        .await
        .unwrap_err();

        assert!(error.contains("non-contiguous"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn minimal_console_plugin_inventory_reaches_readiness_and_shuts_down() {
        let root = tempfile::tempdir().unwrap();
        configure_test_fixture_model(root.path());
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut config = AgentWebConfig::new(lenso_agent_console_plugins::link);
                config.agent_home = Some(root.path().to_path_buf());
                config.access = AgentWebAccess::HostAuthorized;
                let surface = AgentWebSurface::start(config).await.unwrap();
                let inventory = plugin_control::plugin_inventory(
                    State(surface.runtime.clone()),
                    axum::extract::Query(plugin_control::PluginInventoryQuery::default()),
                )
                .await
                .unwrap()
                .0;
                let inventory = serde_json::to_value(inventory).unwrap();
                assert_eq!(inventory["schema"], "lenso.agent.plugin-inventory.v2");
                assert!(
                    inventory["active"]["plugins"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|plugin| plugin["instanceKey"] == "lenso.agent.loop/agent")
                );
                assert!(
                    inventory["active"]["plugins"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|plugin| plugin["instanceKey"] == "lenso.terminal.web/web")
                );
                let (reply, response) = oneshot::channel();
                surface
                    .runtime
                    .commands
                    .send(RuntimeCommand::TerminalCatalog { reply })
                    .await
                    .unwrap();
                let catalog = response.await.unwrap().unwrap();
                assert!(
                    catalog
                        .commands
                        .iter()
                        .any(|command| command.path == ["sessions", "list"])
                );
                let response = surface
                    .router()
                    .oneshot(
                        Request::builder()
                            .method(Method::POST)
                            .uri("/api/console/v1/agent/terminal/executions")
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(axum::body::Body::from(
                                serde_json::json!({
                                    "commandLine": "/sessions list",
                                    "requestId": "terminal-proof"
                                })
                                .to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let body = axum::body::to_bytes(response.into_body(), 1_048_576)
                    .await
                    .unwrap();
                let body = String::from_utf8(body.to_vec()).unwrap();
                assert!(body.contains("terminal_message"));
                assert!(body.contains("terminal_completed"));
                surface.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn removing_web_terminal_consumer_preserves_the_agent_web_app() {
        let root = tempfile::tempdir().unwrap();
        configure_test_fixture_model(root.path());
        let plugin_directory = root.path().join("plugins/lenso.terminal.web");
        std::fs::create_dir_all(&plugin_directory).unwrap();
        std::fs::write(plugin_directory.join("web.disabled"), "").unwrap();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut config = AgentWebConfig::new(lenso_agent_console_plugins::link);
                config.agent_home = Some(root.path().to_path_buf());
                let surface = AgentWebSurface::start(config).await.unwrap();
                let (reply, response) = oneshot::channel();
                surface
                    .runtime
                    .commands
                    .send(RuntimeCommand::TerminalCatalog { reply })
                    .await
                    .unwrap();
                let error = response.await.unwrap().unwrap_err();
                assert!(error.contains("lenso.terminal.web/web"));
                surface.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plugin_control_accepts_a_named_profile() {
        let root = tempfile::tempdir().unwrap();
        configure_test_fixture_model(root.path());
        std::fs::create_dir_all(root.path().join("profiles")).unwrap();
        std::fs::write(
            root.path().join("profiles/web.toml"),
            concat!(
                "instances = [\n",
                "  \"lenso.agent.loop/agent\",\n",
                "  \"lenso.agent.model.fixture/model\",\n",
                "]\n",
            ),
        )
        .unwrap();
        let mut config = AgentWebConfig::new(lenso_agent_console_plugins::link);
        config.agent_home = Some(root.path().to_path_buf());
        config.profile = Some("web".to_owned());
        config.control = AgentWebControl::HostAuthorized;
        config.plugin_control = true;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let surface = AgentWebSurface::start(config).await.unwrap();
                surface.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plugin_control_dispatches_through_a_host_supplied_configuration_authority() {
        let root = tempfile::tempdir().unwrap();
        configure_test_fixture_model(root.path());
        let inspections = Arc::new(AtomicUsize::new(0));
        let authority = RecordingConfigurationAuthority {
            inspections: Arc::clone(&inspections),
            local: LocalPluginRootAuthority::new(root.path()),
            source: PluginConfigurationAuthoritySource::new("remote_fixture", "tenant/app")
                .unwrap(),
        };
        let mut config = AgentWebConfig::new(lenso_agent_console_plugins::link);
        config.agent_home = Some(root.path().to_path_buf());
        config.control = AgentWebControl::HostAuthorized;
        config.plugin_control = true;
        config.plugin_configuration_authority = Some(Arc::new(authority));

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let surface = AgentWebSurface::start(config).await.unwrap();
                let inventory = plugin_control::plugin_inventory(
                    State(surface.runtime.clone()),
                    axum::extract::Query(plugin_control::PluginInventoryQuery::default()),
                )
                .await
                .unwrap()
                .0;
                let inventory = serde_json::to_value(inventory).unwrap();
                assert_eq!(
                    inventory["configurationAuthority"],
                    serde_json::json!({
                        "kind": "remote_fixture",
                        "publicationHistory": false,
                        "reference": "tenant/app",
                        "rollbackProposals": false,
                    })
                );

                let management = surface
                    .runtime
                    .plugin_control
                    .clone()
                    .unwrap()
                    .inspect()
                    .unwrap();
                let management = serde_json::to_value(management).unwrap();
                assert_eq!(
                    management["configurationAuthority"],
                    inventory["configurationAuthority"]
                );
                assert_eq!(inspections.load(Ordering::Relaxed), 1);
                surface.shutdown().await.unwrap();
            })
            .await;
    }

    #[test]
    fn observable_plugin_control_rejects_a_separate_managed_app_root_without_writing_it() {
        let agent_home = tempfile::tempdir().unwrap();
        let managed_app = tempfile::tempdir().unwrap();
        let before = std::fs::read_dir(managed_app.path()).unwrap().count();

        let error = PluginControl::resolve(
            true,
            Some(managed_app.path()),
            agent_home.path(),
            None,
            None,
            None,
        )
        .unwrap_err();

        assert!(error.contains("managed App root to be the Agent Home"));
        assert_eq!(
            std::fs::read_dir(managed_app.path()).unwrap().count(),
            before
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plugin_control_supports_a_first_run_agent_home() {
        let parent = tempfile::tempdir().unwrap();
        let agent_home = parent.path().join("new-agent-home");
        configure_test_fixture_model(&agent_home);
        let mut config = AgentWebConfig::new(lenso_agent_console_plugins::link);
        config.agent_home = Some(agent_home.clone());
        config.control = AgentWebControl::HostAuthorized;
        config.plugin_control = true;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let surface = AgentWebSurface::start(config).await.unwrap();
                assert!(agent_home.join(".lenso/host-catalog.json").is_file());
                surface.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn named_profile_rejects_an_injected_authority_that_spoofs_local_source() {
        let root = tempfile::tempdir().unwrap();
        let inspections = Arc::new(AtomicUsize::new(0));
        let authority = RecordingConfigurationAuthority {
            inspections: Arc::clone(&inspections),
            local: LocalPluginRootAuthority::new(root.path()),
            source: PluginConfigurationAuthoritySource::new("local_plugin_root", "app").unwrap(),
        };
        let mut config = AgentWebConfig::new(lenso_agent_console_plugins::link);
        config.agent_home = Some(root.path().to_path_buf());
        config.profile = Some("web".to_owned());
        config.control = AgentWebControl::HostAuthorized;
        config.plugin_control = true;
        config.plugin_configuration_authority = Some(Arc::new(authority));

        let error = AgentWebSurface::start(config).await.unwrap_err();

        assert!(error.contains("built-in local configuration authority"));
        assert_eq!(inspections.load(Ordering::Relaxed), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plugin_control_rejects_a_relative_managed_app_root() {
        let agent_home = tempfile::tempdir().unwrap();
        let mut config = AgentWebConfig::new(lenso_agent_console_plugins::link);
        config.agent_home = Some(agent_home.path().to_path_buf());
        config.managed_app_root = Some(PathBuf::from("relative/app"));
        config.control = AgentWebControl::HostAuthorized;
        config.plugin_control = true;

        let error = AgentWebSurface::start(config).await.unwrap_err();

        assert!(error.contains("managed App root must be an absolute path"));
    }

    #[test]
    fn rejects_empty_and_oversized_turns() {
        assert!(
            validate_turn_request(&WebTurnRequest {
                allowed_tools: None,
                edit_turn_id: None,
                input: "  ".to_owned(),
                model: None,
                reasoning_effort: None,
                reasoning_enabled: None,
                reasoning_budget_tokens: None,
                request_id: "request-1".to_owned(),
                session_id: None,
                service_tier: None,
            })
            .is_err()
        );
        assert!(
            validate_turn_request(&WebTurnRequest {
                allowed_tools: None,
                edit_turn_id: None,
                input: "x".repeat(MAX_PROMPT_BYTES + 1),
                model: None,
                reasoning_effort: None,
                reasoning_enabled: None,
                reasoning_budget_tokens: None,
                request_id: "request-2".to_owned(),
                session_id: None,
                service_tier: None,
            })
            .is_err()
        );
        assert!(
            validate_terminal_request(&WebTerminalRequest {
                command_line: "  ".to_owned(),
                request_id: "terminal-1".to_owned(),
            })
            .is_err()
        );
        assert!(
            validate_terminal_request(&WebTerminalRequest {
                command_line: "sessions list".to_owned(),
                request_id: "../terminal".to_owned(),
            })
            .is_err()
        );
    }

    #[test]
    fn rejects_ambiguous_reasoning_controls() {
        assert!(
            validate_turn_request(&WebTurnRequest {
                allowed_tools: None,
                edit_turn_id: None,
                input: "Summarize this".to_owned(),
                model: Some("reasoning-model".to_owned()),
                reasoning_effort: Some("high".to_owned()),
                reasoning_enabled: Some(true),
                reasoning_budget_tokens: None,
                request_id: "request-reasoning".to_owned(),
                session_id: None,
                service_tier: None,
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

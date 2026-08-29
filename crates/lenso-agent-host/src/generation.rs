use std::{
    any::Any,
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    env, fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::sync::oneshot;

use lenso::CtxExt;
use lenso::host::{Host as FrameworkHost, HostBuilder as FrameworkHostBuilder};
use lenso_app_plan::{
    RequestAdmissionPlan, ResolvedAppPlan,
    authoring::{
        HostBinding, HostCatalog, HostDefaultPlugin, HostPluginConfiguration, HostSlot,
        PluginInstanceId, PluginRootSnapshot, resolve_plugin_root,
    },
};
use lenso_bun_adapter::{BunAdapter, BunCapabilityCodec};
use lenso_capability_agent::{Agent, AgentJsonCodec, CAPABILITY_ID as AGENT_CAPABILITY_ID};
use lenso_capability_agent_context_compaction::ContextCompactionJsonCodec;
use lenso_capability_agent_context_source::{
    ContextSourceJsonCodec, ContextSourceReadResource, ContextSourceRenderPrompt,
    ContextSourceSnapshot, READ_RESOURCE_OPERATION, RENDER_PROMPT_OPERATION, ReadResourceError,
    ReadResourceRequest, ReadResourceResponse, RenderPromptError, RenderPromptRequest,
    RenderPromptResponse, SNAPSHOT_OPERATION as CONTEXT_SNAPSHOT_OPERATION,
    SnapshotError as ContextSnapshotError, SnapshotRequest as ContextSnapshotRequest,
    SnapshotResponse as ContextSnapshotResponse,
};
use lenso_capability_agent_http_fetch::HttpFetchJsonCodec;
use lenso_capability_agent_lifecycle::LifecycleJsonCodec;
use lenso_capability_agent_memory::MemoryJsonCodec;
use lenso_capability_agent_model::ModelJsonCodec;
use lenso_capability_agent_prompt::PromptJsonCodec;
use lenso_capability_agent_session::{
    APPEND_OPERATION, AppendSessionRequest, AppendSessionRequestEventsItem,
    AppendSessionRequestEventsItemKind, LIST_OPERATION, ListSessionsRequest, ListSessionsResponse,
    OPEN_OPERATION, OpenSessionRequest, READ_OPERATION, RENAME_OPERATION, ReadSessionRequest,
    ReadSessionResponse, ReadSessionResponseEventsItemKind, RenameError, RenameSessionRequest,
    RenameSessionResponse, SessionAppend, SessionJsonCodec, SessionList, SessionOpen, SessionRead,
    SessionRename,
};
use lenso_capability_agent_session_presentation::SessionPresentationJsonCodec;
use lenso_capability_agent_tool_hook::ToolHookJsonCodec;
use lenso_capability_agent_tool_provider::ToolProviderJsonCodec;
use lenso_capability_agent_tools::{
    CATALOG_OPERATION, CatalogRequest, CatalogResponseToolsItem, ToolsCatalog, ToolsJsonCodec,
};
use lenso_capability_agent_tui_contribution::{
    SNAPSHOT_OPERATION, SnapshotRequest, SnapshotResponsePanelsItem, TuiContribution,
    validate_snapshot_panels,
};
use lenso_capability_agent_tui_suggestion::{
    SNAPSHOT_OPERATION as SUGGESTION_SNAPSHOT_OPERATION,
    SnapshotRequest as SuggestionSnapshotRequest, Suggestion, TuiSuggestion,
    validate_snapshot_suggestions,
};
use lenso_capability_agent_turn_input::TurnInputJsonCodec;
use lenso_capability_agent_user_interaction::{
    ANSWER_OPERATION, AnswerRequest, CAPABILITY_ID as USER_INTERACTION_CAPABILITY_ID,
    InteractionAnswer, InteractiveSurface, PENDING_OPERATION, PendingInteraction, PendingRequest,
    UserInteractionAnswer, UserInteractionJsonCodec, UserInteractionPending,
};
use lenso_capability_agent_workspace_read::WorkspaceReadJsonCodec;
use lenso_kernel::{
    CancellationToken, ExecutionAdapterCatalog, InvocationContext, NativeApp, NativeRequestHandle,
    NativeStreamHandle,
};
use lenso_native_adapter::NativePluginRegistry;
use lenso_plugin_control_plane::{
    AdapterProfile, AppGenerationSpec, AppGenerationTransitionSpec, CanonicalDocument,
    CatalogFactory, ControlHealth, ControlLifecycle, ControlPlaneError, ControlStateStore,
    DurableControlState, DurableGenerationRoute, DurableTransitionOutcome, EmbeddedPlugin,
    GenerationControllerClient, GenerationControllerEvent, GenerationMaintenanceOutcome,
    HostBuildManifest, HostExecutionPolicy, KernelGenerationRuntime, MultiExecutionCatalogFactory,
    PlanGenerationInput, ReplacementMode, ResolvedGeneration, RolloutPolicy,
    resolve_plan_generation, sha256_digest,
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::runtime_state::{LedgerControlStateStore, RuntimeAttachment, RuntimeState};
use crate::{AgentDirectories, AgentSurfaceKind};

const APP_ID: &str = "lenso.agent.harness";
const GENERATION_SPEC_DIGEST_EXTENSION: &str = "lenso.app.generation-spec-digest@1";
const DEFAULT_MODEL: &str = "gpt-5.6-luna";
const NATIVE_EXECUTION_CLASS: &str = "lenso.native-rust@1";
const QUICKJS_EXECUTION_CLASS: &str = "lenso.quickjs@1";
const PROCESS_EXECUTION_CLASS: &str = "lenso.process@1";
const BUN_EXECUTION_CLASS: &str = "lenso.bun-process@1";
const WASM_EXECUTION_CLASS: &str = "lenso.wasm-component@1";
// Wasm component instantiation can legitimately cross ten seconds on a busy developer machine.
// Keep the gate bounded while avoiding spurious install and rollback failures under local load.
const READY_TIMEOUT_NANOS: u64 = 30_000_000_000;
const DRAIN_TIMEOUT_NANOS: u64 = 2_000_000_000;
const ONLINE_DRAIN_TIMEOUT_NANOS: u64 = 300_000_000_000;
const ONLINE_ROLLBACK_WINDOW_NANOS: u64 = 1_000_000_000;
const MAINTENANCE_INTERVAL: Duration = Duration::from_millis(10);
const RECONCILE_QUIET_PERIOD: Duration = Duration::from_millis(200);
const RECONCILE_SETTLE_LIMIT: Duration = Duration::from_secs(2);
const RECONCILE_CONSISTENCY_INTERVAL: Duration = Duration::from_secs(2);
const MAX_RECONCILE_EVENTS: usize = 32;
const TUI_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(2);
const CONTEXT_SOURCE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TUI_PANELS: usize = 64;
const MAX_TUI_PANEL_BYTES: usize = 262_144;
const MAX_TUI_SUGGESTIONS: usize = 2_112;
const MAX_TUI_SUGGESTION_BYTES: usize = 2_097_152;

static NEXT_ROOT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct HarnessCatalogFactory;

impl CatalogFactory for HarnessCatalogFactory {
    fn catalog(
        &self,
        generation: &ResolvedGeneration,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        let (registry, _) = native_host_build();
        let mut catalog =
            ExecutionAdapterCatalog::single(registry.with_resources(generation.resources.clone()));
        if generation
            .plan
            .plugin_instances()
            .iter()
            .any(|instance| instance.execution_class().as_str() == BUN_EXECUTION_CLASS)
        {
            catalog = catalog.with_adapter(bun_adapter()).map_err(|error| {
                ControlPlaneError::HostFailure {
                    detail: error.to_string(),
                }
            })?;
        }
        Ok(catalog)
    }
}

fn bun_adapter() -> BunAdapter {
    BunAdapter::production("bun")
        .with_codec(BunJsonCodec(AgentJsonCodec))
        .with_codec(BunJsonCodec(ContextCompactionJsonCodec))
        .with_codec(BunJsonCodec(ContextSourceJsonCodec))
        .with_codec(BunJsonCodec(MemoryJsonCodec))
        .with_codec(BunJsonCodec(HttpFetchJsonCodec))
        .with_codec(BunJsonCodec(LifecycleJsonCodec))
        .with_codec(BunJsonCodec(ModelJsonCodec))
        .with_codec(BunJsonCodec(PromptJsonCodec))
        .with_codec(BunJsonCodec(SessionJsonCodec))
        .with_codec(BunJsonCodec(SessionPresentationJsonCodec))
        .with_codec(BunJsonCodec(ToolHookJsonCodec))
        .with_codec(BunJsonCodec(ToolProviderJsonCodec))
        .with_codec(BunJsonCodec(TurnInputJsonCodec))
        .with_codec(BunJsonCodec(ToolsJsonCodec))
        .with_codec(BunJsonCodec(UserInteractionJsonCodec))
        .with_codec(BunJsonCodec(WorkspaceReadJsonCodec))
}

#[derive(Debug)]
struct BunJsonCodec<T>(T);

impl<T: lenso_runtime_codec::JsonCapabilityCodec> BunCapabilityCodec for BunJsonCodec<T> {
    fn capability_id(&self) -> &'static str {
        self.0.capability_id()
    }
    fn descriptor_version(&self) -> &'static str {
        self.0.descriptor_version()
    }
    fn operations(&self) -> &'static [&'static str] {
        self.0.request_operations()
    }
    fn stream_operations(&self) -> &'static [&'static str] {
        self.0.stream_operations()
    }
    fn encode_request(
        &self,
        operation: &str,
        request: &dyn Any,
    ) -> Result<serde_json::Value, lenso_kernel::RuntimeFailure> {
        self.0.encode_request(operation, request)
    }
    fn decode_response(
        &self,
        operation: &str,
        value: serde_json::Value,
    ) -> Result<Box<dyn Any>, lenso_kernel::RuntimeFailure> {
        self.0.decode_response(operation, value)
    }
    fn decode_domain_error(
        &self,
        operation: &str,
        value: serde_json::Value,
    ) -> Result<Box<dyn Any>, lenso_kernel::RuntimeFailure> {
        self.0.decode_domain_error(operation, value)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HostBuildIdentity {
    executable_digest: String,
}

/// One operator-visible outcome from the live Plugin Desired State reconciler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OnlineGenerationEvent {
    Switched {
        resolution_authority_digest: String,
        generation_spec_digest: String,
        previous_generation_spec_digest: String,
        routing_epoch: u64,
    },
    Rejected {
        resolution_authority_digest: Option<String>,
        detail: String,
    },
    RolledBack {
        failed_generation_spec_digest: String,
        restored_generation_spec_digest: String,
        routing_epoch: u64,
        detail: String,
    },
    Failed {
        generation_spec_digest: String,
        detail: String,
    },
    WatchDegraded {
        detail: String,
    },
}

#[derive(Debug)]
struct GenerationReconciler {
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Debug)]
enum FilesystemReconcileSignal {
    Changed,
    Error(String),
}

struct FilesystemReconcileWatcher {
    watcher: Option<RecommendedWatcher>,
    signals: tokio::sync::mpsc::UnboundedReceiver<FilesystemReconcileSignal>,
    _sender: tokio::sync::mpsc::UnboundedSender<FilesystemReconcileSignal>,
    recursive_path: Option<PathBuf>,
    recursive_watched: bool,
}

impl FilesystemReconcileWatcher {
    fn start(non_recursive: &[&Path], recursive_path: Option<PathBuf>) -> (Self, Vec<String>) {
        let (sender, signals) = tokio::sync::mpsc::unbounded_channel();
        let callback_sender = sender.clone();
        let mut errors = Vec::new();
        let watcher =
            match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                match event {
                    Ok(_) => {
                        let _ = callback_sender.send(FilesystemReconcileSignal::Changed);
                    }
                    Err(error) => {
                        let _ = callback_sender
                            .send(FilesystemReconcileSignal::Error(error.to_string()));
                    }
                }
            }) {
                Ok(mut watcher) => {
                    let mut watched_paths = BTreeSet::new();
                    for path in non_recursive
                        .iter()
                        .copied()
                        .filter(|path| watched_paths.insert((*path).to_path_buf()))
                    {
                        if let Err(error) = watcher.watch(path, RecursiveMode::NonRecursive) {
                            errors.push(format!(
                                "failed to watch Desired State path {}: {error}",
                                path.display()
                            ));
                        }
                    }
                    Some(watcher)
                }
                Err(error) => {
                    errors.push(format!("failed to start filesystem watcher: {error}"));
                    None
                }
            };
        let mut watcher = Self {
            watcher,
            signals,
            _sender: sender,
            recursive_path,
            recursive_watched: false,
        };
        if let Some(error) = watcher.refresh_recursive_watch() {
            errors.push(error);
        }
        (watcher, errors)
    }

    fn refresh_recursive_watch(&mut self) -> Option<String> {
        let path = self.recursive_path.as_ref()?;
        let watcher = self.watcher.as_mut()?;
        if path.is_dir() && !self.recursive_watched {
            match watcher.watch(path, RecursiveMode::Recursive) {
                Ok(()) => self.recursive_watched = true,
                Err(error) => {
                    return Some(format!(
                        "failed to watch Plugin discovery path {}: {error}",
                        path.display()
                    ));
                }
            }
        } else if !path.exists() && self.recursive_watched {
            let _ = watcher.unwatch(path);
            self.recursive_watched = false;
        }
        None
    }

    async fn changed(&mut self) -> Option<FilesystemReconcileSignal> {
        self.signals.recv().await
    }

    async fn settle_after(&mut self, initial: Option<FilesystemReconcileSignal>) -> Vec<String> {
        let mut errors = Vec::new();
        if let Some(FilesystemReconcileSignal::Error(error)) = initial {
            errors.push(error);
        }
        let deadline = tokio::time::Instant::now() + RECONCILE_SETTLE_LIMIT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let quiet_period = RECONCILE_QUIET_PERIOD.min(remaining);
            let Ok(Some(signal)) = tokio::time::timeout(quiet_period, self.signals.recv()).await
            else {
                break;
            };
            if let FilesystemReconcileSignal::Error(error) = signal {
                errors.push(error);
            }
        }
        errors
    }
}

impl HostBuildIdentity {
    pub(crate) fn current() -> Result<Self, String> {
        let executable = env::current_exe()
            .map_err(|error| format!("failed to locate Host executable: {error}"))?;
        let executable_bytes = fs::read(&executable).map_err(|error| {
            format!(
                "failed to read Host executable {}: {error}",
                executable.display()
            )
        })?;
        Ok(Self {
            executable_digest: sha256_digest(&executable_bytes),
        })
    }
}

fn framework_host_builder<F: CatalogFactory>(
    runtime: KernelGenerationRuntime<F>,
    store: LedgerControlStateStore,
) -> FrameworkHostBuilder<KernelGenerationRuntime<F>, LedgerControlStateStore> {
    FrameworkHostBuilder::new(APP_ID, runtime, store).maintenance_interval(MAINTENANCE_INTERVAL)
}

async fn recover_or_open_host<F: CatalogFactory>(
    plan_bytes: &[u8],
    store_root: &Path,
    host_build: &HostBuildIdentity,
    runtime: KernelGenerationRuntime<F>,
    store: LedgerControlStateStore,
    durable: DurableControlState,
) -> Result<FrameworkHost<NativeApp>, String> {
    let has_live_state = durable.generations.iter().any(|record| {
        matches!(
            record.lifecycle,
            ControlLifecycle::Staged
                | ControlLifecycle::Ready
                | ControlLifecycle::Active
                | ControlLifecycle::Draining
                | ControlLifecycle::Standby
        )
    });
    if !has_live_state {
        return framework_host_builder(runtime, store)
            .build()
            .map_err(control_error);
    }
    let live_digests = durable
        .generations
        .iter()
        .filter(|record| {
            matches!(
                record.lifecycle,
                ControlLifecycle::Active | ControlLifecycle::Standby
            )
        })
        .map(|record| record.generation_spec_digest.as_str())
        .collect::<BTreeSet<_>>();
    let recoverable = resolve_retained_generations(plan_bytes, store_root, host_build)?;
    let all_live_generations_are_recoverable = live_digests
        .iter()
        .all(|digest| recoverable.contains_key(*digest));
    if all_live_generations_are_recoverable {
        return framework_host_builder(runtime, store)
            .recover(&recoverable, now_unix_nanos()?)
            .await
            .map_err(control_error);
    }
    if durable.host_suspended {
        return framework_host_builder(runtime, store)
            .replace_suspended()
            .map_err(control_error);
    }

    // The process-lifetime Host lease proves the previous owner of this Controller
    // namespace has exited even if it could not commit a clean suspension.
    let revision = durable.revision;
    let mut exited_host = durable;
    exited_host.host_suspended = true;
    store
        .compare_and_swap(APP_ID, revision, exited_host)
        .map_err(control_error)?;
    framework_host_builder(runtime, store)
        .replace_suspended()
        .map_err(control_error)
}

#[derive(Debug)]
pub struct AgentApp {
    host: FrameworkHost<NativeApp>,
    resolved_plan: ResolvedAppPlan,
    runtime: RuntimeAttachment,
    session_database: PathBuf,
    profile_name: Option<String>,
    authoring_managed: bool,
    reconciler: Option<GenerationReconciler>,
    reconcile_events: Rc<RefCell<VecDeque<OnlineGenerationEvent>>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RenameSessionFailure {
    Domain(RenameError),
    Runtime(String),
}

impl AgentApp {
    pub(crate) async fn start_with_runtime_state_profile_and_host_build(
        plan_bytes: &[u8],
        store_root: &Path,
        session_database: PathBuf,
        surface: AgentSurfaceKind,
        profile_name: Option<String>,
        host_build: HostBuildIdentity,
    ) -> Result<Self, String> {
        let resolved_plan: ResolvedAppPlan = serde_json::from_slice(plan_bytes)
            .map_err(|error| format!("failed to decode the resolved App Plan: {error}"))?;
        let runtime_state = RuntimeState::open(store_root)?;
        let runtime_attachment = runtime_state.attach(surface)?;
        let _authority_fence = runtime_attachment.authority_snapshot()?;
        let (generation, _resolution_authority_digest) =
            resolve_and_record_current_generation(plan_bytes, store_root, &host_build)?;
        let store = runtime_attachment.control_store();
        let durable = store.load(APP_ID).map_err(control_error)?;
        let runtime = KernelGenerationRuntime::new(harness_catalog_factory());
        let mut host =
            recover_or_open_host(plan_bytes, store_root, &host_build, runtime, store, durable)
                .await?;
        let recovered_active = host
            .inspect()
            .await
            .map_err(control_error)?
            .active_generation_spec_digest;
        if recovered_active.as_deref() != Some(generation.spec.digest()) {
            let transition = if let Some(active) = recovered_active.as_deref() {
                let recoverable =
                    resolve_retained_generations(plan_bytes, store_root, &host_build)?;
                let previous = recoverable.get(active).ok_or_else(|| {
                    "recovered Generation lost its retained Plugin authority".to_owned()
                })?;
                maintenance_transition(previous, &generation).map_err(control_error)?
            } else {
                initial_transition(&generation).map_err(control_error)?
            };
            if let Err(error) = host
                .transition(transition, generation, BTreeMap::new())
                .await
            {
                let _ = host.shutdown().await;
                return Err(control_error(error));
            }
        }
        if let Err(error) = runtime_attachment.state().confirm_legacy_migration() {
            let _ = host.shutdown().await;
            return Err(error);
        }
        let client = host.controller();
        let reconcile_events = Rc::new(RefCell::new(VecDeque::new()));
        let authoring_managed =
            plan_is_authoring_managed(plan_bytes, store_root, profile_name.as_deref());
        let reconciler = start_generation_reconciler(
            client.clone(),
            store_root.to_path_buf(),
            host_build,
            profile_name.clone(),
            authoring_managed,
            reconcile_events.clone(),
        );
        Ok(Self {
            host,
            resolved_plan,
            runtime: runtime_attachment,
            session_database,
            profile_name,
            authoring_managed,
            reconciler: Some(reconciler),
            reconcile_events,
        })
    }

    /// Returns the immutable App Plan selected when this Host started.
    pub const fn resolved_plan(&self) -> &ResolvedAppPlan {
        &self.resolved_plan
    }

    /// Resolves the current author-owned Plugin Root without changing runtime authority.
    pub fn desired_plan(&self) -> Result<ResolvedAppPlan, String> {
        if !self.authoring_managed {
            return Err(
                "this Host runs an exact diagnostic Plan, not an author-managed Plugin Root"
                    .to_owned(),
            );
        }
        let directories = directories_for_store_root(self.runtime.state().root())?;
        let root = crate::plugin_root::snapshot(&directories.plugins())?;
        if let Some(profile_name) = self.profile_name.as_deref() {
            let profile = crate::profile::select(profile_name, &root, &directories.profiles())?;
            resolve_host_plan_for_agent_in(&directories, profile.root(), profile.agent())
        } else {
            resolve_host_plan_in(&directories, &root)
        }
    }

    /// Snapshots explicit disabled Instance markers for management surfaces.
    pub fn disabled_plugin_instances(&self) -> Result<Vec<PluginInstanceId>, String> {
        let directories = directories_for_store_root(self.runtime.state().root())?;
        Ok(crate::plugin_root::snapshot(&directories.plugins())?
            .disabled()
            .to_vec())
    }

    /// Returns Instances whose author-owned files may be enabled or disabled.
    pub fn disableable_plugin_instances(&self) -> Result<Vec<PluginInstanceId>, String> {
        let directories = directories_for_store_root(self.runtime.state().root())?;
        let root = crate::plugin_root::snapshot(&directories.plugins())?;
        let mut instances = root
            .instances()
            .iter()
            .map(|instance| instance.id().clone())
            .chain(root.disabled().iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        instances.sort_by_key(ToString::to_string);
        Ok(instances)
    }

    pub async fn lease_turn(&self) -> Result<TurnGeneration, String> {
        self.lease_turn_for("cli").await
    }

    /// Pins one TUI-submitted Agent Turn to the active App Generation.
    pub async fn lease_tui_turn(&self) -> Result<TurnGeneration, String> {
        self.lease_turn_for("tui").await
    }

    /// Pins one Telegram message to the active App Generation.
    pub async fn lease_telegram_turn(&self) -> Result<TurnGeneration, String> {
        self.lease_turn_for("telegram").await
    }

    /// Pins one Discord message to the active App Generation.
    pub async fn lease_discord_turn(&self) -> Result<TurnGeneration, String> {
        self.lease_turn_for("discord").await
    }

    /// Pins one browser-submitted Agent Turn to the active App Generation.
    pub async fn lease_web_turn(&self) -> Result<TurnGeneration, String> {
        self.lease_turn_for("web").await
    }

    /// Snapshots Prompt and Resource metadata explicitly visible to the CLI surface.
    pub async fn cli_context_sources(&self) -> Result<ContextSnapshotResponse, String> {
        self.context_sources("lenso.agent.cli/cli").await
    }

    /// Snapshots Prompt and Resource metadata explicitly visible to the TUI surface.
    pub async fn tui_context_sources(&self) -> Result<ContextSnapshotResponse, String> {
        self.context_sources("lenso.agent.tui/tui").await
    }

    /// Renders one user-selected Context Prompt for the CLI surface.
    pub async fn render_cli_context_prompt(
        &self,
        request: RenderPromptRequest,
    ) -> Result<RenderPromptResponse, String> {
        self.render_context_prompt("lenso.agent.cli/cli", request)
            .await
    }

    /// Reads one application-selected Context Resource for the CLI surface.
    pub async fn read_cli_context_resource(
        &self,
        request: ReadResourceRequest,
    ) -> Result<ReadResourceResponse, String> {
        self.read_context_resource("lenso.agent.cli/cli", request)
            .await
    }

    /// Renders one user-selected Context Prompt for the TUI surface.
    pub async fn render_tui_context_prompt(
        &self,
        request: RenderPromptRequest,
    ) -> Result<RenderPromptResponse, String> {
        self.render_context_prompt("lenso.agent.tui/tui", request)
            .await
    }

    /// Reads one application-selected Context Resource for the TUI surface.
    pub async fn read_tui_context_resource(
        &self,
        request: ReadResourceRequest,
    ) -> Result<ReadResourceResponse, String> {
        self.read_context_resource("lenso.agent.tui/tui", request)
            .await
    }

    async fn context_sources(
        &self,
        consumer_instance: &str,
    ) -> Result<ContextSnapshotResponse, String> {
        let route = self.host.route().await.map_err(control_error)?;
        let handle = route
            .target()
            .many_handle::<ContextSourceSnapshot>(consumer_instance)
            .map_err(|error| format!("Context Source snapshot route is unavailable: {error:?}"))?;
        let cancellation = CancellationToken::new();
        let context = route
            .target()
            .invocation_context_after(CONTEXT_SOURCE_TIMEOUT, cancellation.clone());
        let invocation = handle.invoke_many_with_context(
            CONTEXT_SNAPSHOT_OPERATION,
            context,
            ContextSnapshotRequest {},
        );
        let responses = match tokio::time::timeout(CONTEXT_SOURCE_TIMEOUT, invocation).await {
            Ok(result) => {
                result.map_err(|error| format!("Context Source snapshot failed: {error:?}"))?
            }
            Err(_) => {
                cancellation.cancel();
                return Err("Context Source snapshot timed out".to_owned());
            }
        };
        let mut prompts = Vec::new();
        let mut resources = Vec::new();
        let mut prompt_keys = BTreeSet::new();
        let mut resource_keys = BTreeSet::new();
        for response in responses {
            let response = match response {
                Ok(response) => response,
                Err(ContextSnapshotError::NotFound) => continue,
                Err(error) => {
                    return Err(format!("Context Source rejected its snapshot: {error:?}"));
                }
            };
            for prompt in response.prompts {
                if !prompt_keys.insert((prompt.source.clone(), prompt.name.clone())) {
                    return Err(format!(
                        "duplicate Context Prompt `{}/{}`",
                        prompt.source, prompt.name
                    ));
                }
                prompts.push(prompt);
            }
            for resource in response.resources {
                if !resource_keys.insert((resource.source.clone(), resource.uri.clone())) {
                    return Err(format!(
                        "duplicate Context Resource `{}/{}`",
                        resource.source, resource.uri
                    ));
                }
                resources.push(resource);
            }
        }
        if prompts.len() > lenso_capability_agent_context_source::MAX_PROMPTS
            || resources.len() > lenso_capability_agent_context_source::MAX_RESOURCES
        {
            return Err("aggregate Context Source catalog exceeded its limit".to_owned());
        }
        Ok(ContextSnapshotResponse { prompts, resources })
    }

    async fn render_context_prompt(
        &self,
        consumer_instance: &str,
        request: RenderPromptRequest,
    ) -> Result<RenderPromptResponse, String> {
        let route = self.host.route().await.map_err(control_error)?;
        let handle = route
            .target()
            .many_handle::<ContextSourceRenderPrompt>(consumer_instance)
            .map_err(|error| format!("Context Prompt route is unavailable: {error:?}"))?;
        let context = route
            .target()
            .invocation_context_after(CONTEXT_SOURCE_TIMEOUT, CancellationToken::new());
        let responses = handle
            .invoke_many_with_context(RENDER_PROMPT_OPERATION, context, request)
            .await
            .map_err(|error| format!("Context Prompt rendering failed: {error:?}"))?;
        let mut found = None;
        for response in responses {
            match response {
                Ok(response) if found.is_none() => found = Some(response),
                Ok(_) => return Err("multiple Context Sources rendered the Prompt".to_owned()),
                Err(RenderPromptError::NotFound) => {}
                Err(error) => return Err(format!("Context Source rejected the Prompt: {error:?}")),
            }
        }
        found.ok_or_else(|| "Context Prompt was not found".to_owned())
    }

    async fn read_context_resource(
        &self,
        consumer_instance: &str,
        request: ReadResourceRequest,
    ) -> Result<ReadResourceResponse, String> {
        let route = self.host.route().await.map_err(control_error)?;
        let handle = route
            .target()
            .many_handle::<ContextSourceReadResource>(consumer_instance)
            .map_err(|error| format!("Context Resource route is unavailable: {error:?}"))?;
        let context = route
            .target()
            .invocation_context_after(CONTEXT_SOURCE_TIMEOUT, CancellationToken::new());
        let responses = handle
            .invoke_many_with_context(READ_RESOURCE_OPERATION, context, request)
            .await
            .map_err(|error| format!("Context Resource read failed: {error:?}"))?;
        let mut found = None;
        for response in responses {
            match response {
                Ok(response) if found.is_none() => found = Some(response),
                Ok(_) => return Err("multiple Context Sources read the Resource".to_owned()),
                Err(ReadResourceError::NotFound) => {}
                Err(error) => {
                    return Err(format!("Context Source rejected the Resource: {error:?}"));
                }
            }
        }
        found.ok_or_else(|| "Context Resource was not found".to_owned())
    }

    async fn lease_turn_for(&self, consumer_instance: &str) -> Result<TurnGeneration, String> {
        let route = self.host.route().await.map_err(control_error)?;
        let consumer_instance = match consumer_instance {
            "cli" => "lenso.agent.cli/cli",
            "tui" => "lenso.agent.tui/tui",
            "telegram" => "lenso.agent.telegram/telegram",
            "discord" => "lenso.agent.discord/discord",
            "web" => "lenso.agent.web/web",
            other => return Err(format!("unknown Agent surface `{other}`")),
        };
        let handle = Rc::new(
            route
                .target()
                .stream_handle::<Agent>(consumer_instance)
                .map_err(|error| format!("leased Generation has no Agent route: {error:?}"))?,
        );
        let surface_dependencies = route
            .target()
            .dependencies(consumer_instance)
            .map_err(|error| format!("leased Generation has no Surface route: {error:?}"))?;
        let agent_provider = surface_dependencies
            .bindings()
            .iter()
            .find(|binding| binding.capability_id() == AGENT_CAPABILITY_ID)
            .map(|binding| binding.provider_instance().to_owned())
            .ok_or_else(|| "leased Generation Surface has no Agent provider binding".to_owned())?;
        let tools_catalog = route
            .target()
            .handle::<ToolsCatalog>(&agent_provider)
            .map_err(|error| {
                format!("leased Generation Agent has no Tool catalog route: {error:?}")
            })?;
        let interactive = surface_dependencies
            .bindings()
            .iter()
            .any(|binding| binding.capability_id() == USER_INTERACTION_CAPABILITY_ID);
        let interaction = if interactive {
            let pending = route
                .target()
                .handle::<UserInteractionPending>(consumer_instance)
                .map_err(|error| {
                    format!("leased Generation has no User Interaction pending route: {error:?}")
                })?;
            let answer = route
                .target()
                .handle::<UserInteractionAnswer>(consumer_instance)
                .map_err(|error| {
                    format!("leased Generation has no User Interaction answer route: {error:?}")
                })?;
            Some(UserInteractionSurfaceHandles {
                pending: Rc::new(pending),
                answer: Rc::new(answer),
            })
        } else {
            None
        };
        Ok(TurnGeneration {
            consumer_instance: consumer_instance.to_owned(),
            route,
            handle,
            interaction,
            interactive,
            tools_catalog: Rc::new(tools_catalog),
        })
    }

    /// Snapshots every TUI panel provider in deterministic resolved order.
    pub async fn tui_panels(&self) -> Result<Vec<SnapshotResponsePanelsItem>, String> {
        let route = self.host.route().await.map_err(control_error)?;
        let handle = route
            .target()
            .many_handle::<TuiContribution>("lenso.agent.tui/tui")
            .map_err(|error| {
                format!("leased Generation has no TUI contribution route: {error:?}")
            })?;
        let cancellation = CancellationToken::new();
        let context = route
            .target()
            .invocation_context_after(TUI_SNAPSHOT_TIMEOUT, cancellation.clone());
        let invocation =
            handle.invoke_many_with_context(SNAPSHOT_OPERATION, context, SnapshotRequest {});
        let responses = match tokio::time::timeout(TUI_SNAPSHOT_TIMEOUT, invocation).await {
            Ok(result) => {
                result.map_err(|error| format!("TUI contribution snapshot failed: {error:?}"))?
            }
            Err(_) => {
                cancellation.cancel();
                return Err("TUI contribution snapshot timed out".to_owned());
            }
        };
        let mut panels = Vec::new();
        for response in responses {
            let response = response
                .map_err(|error| format!("TUI contribution rejected its snapshot: {error:?}"))?;
            validate_snapshot_panels(&response.panels).map_err(|error| {
                format!("TUI contribution returned an invalid snapshot: {error}")
            })?;
            panels.extend(response.panels);
        }
        validate_tui_panels(&panels)?;
        Ok(panels)
    }

    /// Snapshots every composer suggestion provider in deterministic resolved order.
    pub async fn tui_suggestions(&self) -> Result<Vec<Suggestion>, String> {
        let route = self.host.route().await.map_err(control_error)?;
        let handle = route
            .target()
            .many_handle::<TuiSuggestion>("lenso.agent.tui/tui")
            .map_err(|error| format!("leased Generation has no TUI suggestion route: {error:?}"))?;
        let cancellation = CancellationToken::new();
        let context = route
            .target()
            .invocation_context_after(TUI_SNAPSHOT_TIMEOUT, cancellation.clone());
        let invocation = handle.invoke_many_with_context(
            SUGGESTION_SNAPSHOT_OPERATION,
            context,
            SuggestionSnapshotRequest {},
        );
        let responses = match tokio::time::timeout(TUI_SNAPSHOT_TIMEOUT, invocation).await {
            Ok(result) => {
                result.map_err(|error| format!("TUI suggestion snapshot failed: {error:?}"))?
            }
            Err(_) => {
                cancellation.cancel();
                return Err("TUI suggestion snapshot timed out".to_owned());
            }
        };
        let mut suggestions = Vec::new();
        for response in responses {
            let response = response.map_err(|error| {
                format!("TUI suggestion provider rejected its snapshot: {error:?}")
            })?;
            validate_snapshot_suggestions(&response.suggestions).map_err(|error| {
                format!("TUI suggestion provider returned an invalid snapshot: {error}")
            })?;
            suggestions.extend(response.suggestions);
        }
        validate_tui_suggestions(&suggestions)?;
        Ok(suggestions)
    }

    pub async fn shutdown(&mut self) -> Result<(), String> {
        if let Some(mut reconciler) = self.reconciler.take() {
            if let Some(stop) = reconciler.stop.take() {
                let _ = stop.send(());
            }
            reconciler
                .task
                .await
                .map_err(|error| format!("Generation Reconciler task failed: {error}"))?;
        }
        self.host.suspend().await.map_err(control_error)?;
        self.runtime.release();
        crate::provenance::try_apply_automatic_gc(
            self.runtime.state().root(),
            &self.session_database,
        )?;
        Ok(())
    }

    /// Drains bounded online-reconcile events for terminal or host presentation.
    pub fn take_online_generation_events(&self) -> Vec<OnlineGenerationEvent> {
        self.reconcile_events.borrow_mut().drain(..).collect()
    }
}

pub(crate) fn live_controller_generation_digests(
    store_root: &Path,
) -> Result<BTreeSet<String>, String> {
    Ok(existing_controller_states(store_root)?
        .into_iter()
        .flat_map(|(_, state)| state.generations)
        .filter(|record| record.lifecycle != ControlLifecycle::Retired)
        .map(|record| record.generation_spec_digest)
        .collect())
}

fn existing_controller_states(
    store_root: &Path,
) -> Result<Vec<(String, DurableControlState)>, String> {
    RuntimeState::open_existing(store_root)?.controller_states()
}

fn resolve_and_record_current_generation(
    plan_bytes: &[u8],
    store_root: &Path,
    host_build: &HostBuildIdentity,
) -> Result<(ResolvedGeneration, String), String> {
    let directories = directories_for_store_root(store_root)?;
    let authority = crate::generation_authority::load_generation_authority_unfenced(store_root);
    let generation = resolve_generation_with_authority(
        plan_bytes,
        &authority,
        host_build,
        &directories.plugins(),
    )?;
    record_generation_spec(store_root, &generation.spec)?;
    crate::generation_authority::record_resolved_generation_authority_unfenced(
        store_root, &authority,
    );
    Ok((generation, authority.resolution_authority_digest))
}

fn start_generation_reconciler(
    client: GenerationControllerClient<NativeApp>,
    store_root: PathBuf,
    host_build: HostBuildIdentity,
    profile_name: Option<String>,
    authoring_managed: bool,
    events: Rc<RefCell<VecDeque<OnlineGenerationEvent>>>,
) -> GenerationReconciler {
    let (stop, mut stopped) = oneshot::channel();
    let mut controller_events = client.subscribe();
    let directories = directories_for_store_root(&store_root)
        .expect("validated Agent runtime root must have an Agent Home parent");
    let plugin_root = directories.plugins();
    let plugin_parent = watch_parent(&plugin_root);
    let profile_directory = directories.profiles();
    let mut watched_paths = vec![store_root.as_path()];
    if authoring_managed {
        watched_paths.push(plugin_parent.as_path());
        if profile_name.is_some() {
            watched_paths.push(profile_directory.as_path());
        }
    }
    let (mut watcher, watcher_errors) =
        FilesystemReconcileWatcher::start(&watched_paths, Some(plugin_root.clone()));
    report_watcher_errors(&events, watcher_errors);
    let task = tokio::task::spawn_local(async move {
        let mut interval = tokio::time::interval(RECONCILE_CONSISTENCY_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_attempted_desired_state_digest = None;
        let mut last_rejection = None::<OnlineGenerationEvent>;
        loop {
            tokio::select! {
                biased;
                _ = &mut stopped => break,
                event = controller_events.recv() => {
                    match event {
                        Ok(event) => {
                            if let Some(event) = online_event_from_controller_event(event) {
                                push_reconcile_event(&events, event);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            push_reconcile_event(&events, OnlineGenerationEvent::Rejected {
                                resolution_authority_digest: None,
                                detail: format!(
                                    "Generation Controller event stream lagged by {skipped} events"
                                ),
                            });
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = interval.tick() => {
                    if authoring_managed && let Some(event) = reconcile_online_generation(
                        &client,
                        &store_root,
                        &plugin_root,
                        &host_build,
                        profile_name.as_deref(),
                        &mut last_attempted_desired_state_digest,
                    ).await {
                        if matches!(event, OnlineGenerationEvent::Switched { .. })
                            || last_rejection.as_ref() != Some(&event)
                        {
                            push_reconcile_event(&events, event.clone());
                        }
                        last_rejection = matches!(event, OnlineGenerationEvent::Rejected { .. })
                            .then_some(event);
                    }
                }
                signal = watcher.changed() => {
                    let errors = watcher.settle_after(signal).await;
                    report_watcher_errors(&events, errors);
                    if authoring_managed && let Some(event) = reconcile_online_generation(
                        &client,
                        &store_root,
                        &plugin_root,
                        &host_build,
                        profile_name.as_deref(),
                        &mut last_attempted_desired_state_digest,
                    ).await {
                        if matches!(event, OnlineGenerationEvent::Switched { .. })
                            || last_rejection.as_ref() != Some(&event)
                        {
                            push_reconcile_event(&events, event.clone());
                        }
                        last_rejection = matches!(event, OnlineGenerationEvent::Rejected { .. })
                            .then_some(event);
                    }
                }
            }
        }
    });
    GenerationReconciler {
        stop: Some(stop),
        task,
    }
}

fn plan_is_authoring_managed(
    plan_bytes: &[u8],
    store_root: &Path,
    profile_name: Option<&str>,
) -> bool {
    let Ok(directories) = directories_for_store_root(store_root) else {
        return false;
    };
    let Ok(root) = crate::plugin_root::snapshot(&directories.plugins()) else {
        return false;
    };
    let resolved = if let Some(profile_name) = profile_name {
        crate::profile::select(profile_name, &root, &directories.profiles()).and_then(|profile| {
            resolve_host_plan_for_agent_in(&directories, profile.root(), profile.agent())
        })
    } else {
        resolve_host_plan_in(&directories, &root)
    };
    resolved
        .and_then(|plan| {
            serde_json::to_vec(&plan)
                .map_err(|error| format!("failed to encode the derived App: {error}"))
        })
        .is_ok_and(|derived| derived == plan_bytes)
}

fn watch_parent(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn online_event_from_controller_event(
    event: GenerationControllerEvent,
) -> Option<OnlineGenerationEvent> {
    let GenerationControllerEvent::Maintained(GenerationMaintenanceOutcome::Failed(failure)) =
        event
    else {
        return None;
    };
    let detail = format!("terminal App Generation failure: {:?}", failure.failure);
    Some(match failure.automatic_rollback {
        Some(rollback) => OnlineGenerationEvent::RolledBack {
            failed_generation_spec_digest: failure.generation_spec_digest,
            restored_generation_spec_digest: rollback.active_generation_spec_digest,
            routing_epoch: rollback.routing_epoch,
            detail,
        },
        None => OnlineGenerationEvent::Failed {
            generation_spec_digest: failure.generation_spec_digest,
            detail,
        },
    })
}

async fn activate_online_candidate(
    client: &GenerationControllerClient<NativeApp>,
    state: &DurableControlState,
    previous_generation_spec_digest: &str,
    candidate: ResolvedGeneration,
) -> Result<Option<DurableTransitionOutcome>, String> {
    let candidate_digest = candidate.spec.digest();
    let retained_candidate = state.generations.iter().find(|record| {
        record.generation_spec_digest == candidate_digest
            && matches!(
                record.lifecycle,
                ControlLifecycle::Draining | ControlLifecycle::Standby
            )
            && record.health == ControlHealth::Healthy
    });
    if let Some(retained_candidate) = retained_candidate {
        let is_direct_predecessor = state.generations.iter().any(|record| {
            record.generation_spec_digest == previous_generation_spec_digest
                && record.transition_spec_digest == retained_candidate.transition_spec_digest
        });
        if !is_direct_predecessor {
            return Ok(None);
        }
        return client
            .rollback(candidate_digest)
            .await
            .map(Some)
            .map_err(control_error);
    }
    let transition = online_overlap_transition(previous_generation_spec_digest, &candidate)
        .map_err(control_error)?;
    client
        .transition(transition, candidate, BTreeMap::new())
        .await
        .map(Some)
        .map_err(control_error)
}

async fn reconcile_online_generation(
    client: &GenerationControllerClient<NativeApp>,
    store_root: &Path,
    plugin_root: &Path,
    host_build: &HostBuildIdentity,
    profile_name: Option<&str>,
    last_attempted_desired_state_digest: &mut Option<String>,
) -> Option<OnlineGenerationEvent> {
    let coordinator = match crate::authority::AuthorityCoordinator::prepare(store_root) {
        Ok(coordinator) => coordinator,
        Err(detail) => {
            return Some(OnlineGenerationEvent::Rejected {
                resolution_authority_digest: None,
                detail,
            });
        }
    };
    let _authority_fence = match coordinator.try_snapshot() {
        Ok(Some(fence)) => fence,
        Ok(None) => return None,
        Err(detail) => {
            return Some(OnlineGenerationEvent::Rejected {
                resolution_authority_digest: None,
                detail,
            });
        }
    };
    let (resolution_authority_digest, candidate) = match resolve_desired_generation(
        plugin_root,
        store_root,
        host_build,
        profile_name,
        last_attempted_desired_state_digest,
    ) {
        Ok(Some(candidate)) => candidate,
        Ok(None) => return None,
        Err(event) => return Some(event),
    };
    let state = match client.inspect().await.map_err(control_error) {
        Ok(state) => state,
        Err(detail) => {
            return Some(OnlineGenerationEvent::Rejected {
                resolution_authority_digest: Some(resolution_authority_digest),
                detail,
            });
        }
    };
    let Some(previous_generation_spec_digest) = state.active_generation_spec_digest.as_deref()
    else {
        return Some(OnlineGenerationEvent::Rejected {
            resolution_authority_digest: Some(resolution_authority_digest),
            detail: "online reconcile requires one active App Generation".to_owned(),
        });
    };
    if previous_generation_spec_digest == candidate.spec.digest() {
        return None;
    }
    if let Err(detail) = record_generation_spec(store_root, &candidate.spec) {
        return Some(OnlineGenerationEvent::Rejected {
            resolution_authority_digest: Some(resolution_authority_digest),
            detail,
        });
    }
    match activate_online_candidate(client, &state, previous_generation_spec_digest, candidate)
        .await
    {
        Ok(Some(outcome)) => Some(OnlineGenerationEvent::Switched {
            resolution_authority_digest,
            generation_spec_digest: outcome.active_generation_spec_digest,
            previous_generation_spec_digest: previous_generation_spec_digest.to_owned(),
            routing_epoch: outcome.routing_epoch,
        }),
        Ok(None) => {
            *last_attempted_desired_state_digest = None;
            None
        }
        Err(detail) => Some(OnlineGenerationEvent::Rejected {
            resolution_authority_digest: Some(resolution_authority_digest),
            detail,
        }),
    }
}

fn resolve_desired_generation(
    plugin_root: &Path,
    store_root: &Path,
    host_build: &HostBuildIdentity,
    profile_name: Option<&str>,
    last_attempted_desired_state_digest: &mut Option<String>,
) -> Result<Option<(String, ResolvedGeneration)>, OnlineGenerationEvent> {
    let directories = directories_for_store_root(store_root).map_err(|detail| {
        OnlineGenerationEvent::Rejected {
            resolution_authority_digest: None,
            detail,
        }
    })?;
    let authority = crate::generation_authority::load_generation_authority_unfenced(store_root);
    let rejected = |detail| OnlineGenerationEvent::Rejected {
        resolution_authority_digest: Some(authority.resolution_authority_digest.clone()),
        detail,
    };
    let root = crate::plugin_root::snapshot(plugin_root).map_err(rejected)?;
    let plan = if let Some(profile_name) = profile_name {
        let profile = crate::profile::select(profile_name, &root, &directories.profiles())
            .map_err(rejected)?;
        resolve_host_plan_for_agent_in(&directories, profile.root(), profile.agent())
            .map_err(rejected)?
    } else {
        resolve_host_plan_in(&directories, &root).map_err(rejected)?
    };
    let resources = crate::plugin_root::plan_resources(plugin_root, &plan).map_err(rejected)?;
    let resource_identity = resources
        .iter()
        .map(|(instance, snapshot)| (instance, snapshot.digest()))
        .collect::<Vec<_>>();
    let desired_state_digest = sha256_digest(
        &serde_json::to_vec(&(
            &authority.resolution_authority_digest,
            &plan,
            resource_identity,
        ))
        .map_err(|error| rejected(format!("failed to identify desired Plugin state: {error}")))?,
    );
    if last_attempted_desired_state_digest.as_deref() == Some(&desired_state_digest) {
        return Ok(None);
    }
    *last_attempted_desired_state_digest = Some(desired_state_digest);
    let candidate =
        resolve_generation_from_plan(&plan, &authority, host_build, plugin_root, resources)
            .map_err(rejected)?;
    Ok(Some((authority.resolution_authority_digest, candidate)))
}

fn push_reconcile_event(
    events: &Rc<RefCell<VecDeque<OnlineGenerationEvent>>>,
    event: OnlineGenerationEvent,
) {
    let mut events = events.borrow_mut();
    if events.len() == MAX_RECONCILE_EVENTS {
        events.pop_front();
    }
    events.push_back(event);
}

fn report_watcher_errors(
    events: &Rc<RefCell<VecDeque<OnlineGenerationEvent>>>,
    errors: impl IntoIterator<Item = String>,
) {
    for detail in errors {
        let event = OnlineGenerationEvent::WatchDegraded { detail };
        if events.borrow().back() != Some(&event) {
            push_reconcile_event(events, event);
        }
    }
}

fn validate_tui_panels(panels: &[SnapshotResponsePanelsItem]) -> Result<(), String> {
    if panels.len() > MAX_TUI_PANELS {
        return Err(format!(
            "TUI contributions exceed the {MAX_TUI_PANELS}-panel aggregate limit"
        ));
    }
    let mut ids = BTreeSet::new();
    let mut total_bytes = 0usize;
    for panel in panels {
        if !ids.insert(panel.id.as_str()) {
            return Err(format!("duplicate TUI panel id `{}`", panel.id));
        }
        total_bytes = total_bytes
            .checked_add(panel.id.len())
            .and_then(|total| total.checked_add(panel.title.len()))
            .and_then(|total| total.checked_add(panel.body.len()))
            .ok_or_else(|| "TUI contribution size overflowed".to_owned())?;
        if total_bytes > MAX_TUI_PANEL_BYTES {
            return Err(format!(
                "TUI contributions exceed the {MAX_TUI_PANEL_BYTES}-byte aggregate limit"
            ));
        }
    }
    Ok(())
}

fn validate_tui_suggestions(suggestions: &[Suggestion]) -> Result<(), String> {
    if suggestions.len() > MAX_TUI_SUGGESTIONS {
        return Err(format!(
            "TUI suggestions exceed the {MAX_TUI_SUGGESTIONS}-item aggregate limit"
        ));
    }
    let mut ids = BTreeSet::new();
    let mut total_bytes = 0usize;
    for suggestion in suggestions {
        if !ids.insert(suggestion.id.as_str()) {
            return Err(format!("duplicate TUI suggestion id `{}`", suggestion.id));
        }
        total_bytes = total_bytes
            .checked_add(suggestion.id.len())
            .and_then(|total| total.checked_add(suggestion.label.len()))
            .and_then(|total| total.checked_add(suggestion.insert_text.len()))
            .and_then(|total| total.checked_add(suggestion.description.len()))
            .ok_or_else(|| "TUI suggestion size overflowed".to_owned())?;
        if total_bytes > MAX_TUI_SUGGESTION_BYTES {
            return Err(format!(
                "TUI suggestions exceed the {MAX_TUI_SUGGESTION_BYTES}-byte aggregate limit"
            ));
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct TurnGeneration {
    consumer_instance: String,
    route: DurableGenerationRoute<NativeApp>,
    handle: Rc<NativeStreamHandle<Agent>>,
    interaction: Option<UserInteractionSurfaceHandles>,
    interactive: bool,
    tools_catalog: Rc<NativeRequestHandle<ToolsCatalog>>,
}

#[derive(Debug)]
struct UserInteractionSurfaceHandles {
    pending: Rc<NativeRequestHandle<UserInteractionPending>>,
    answer: Rc<NativeRequestHandle<UserInteractionAnswer>>,
}

impl TurnGeneration {
    pub fn handle(&self) -> &NativeStreamHandle<Agent> {
        &self.handle
    }

    pub fn invocation_context(&self) -> Result<InvocationContext, String> {
        self.invocation_context_with_cancellation(CancellationToken::new())
    }

    /// Reads the exact Tool catalog bound to this Turn's Agent provider.
    pub async fn tool_catalog(&self) -> Result<Vec<CatalogResponseToolsItem>, String> {
        self.tools_catalog
            .invoke_with_context(
                CATALOG_OPERATION,
                self.invocation_context()?,
                CatalogRequest {},
            )
            .await
            .map_err(|error| format!("Tool catalog snapshot failed: {error:?}"))?
            .map(|response| response.tools)
            .map_err(|error| format!("Tool catalog snapshot was rejected: {error:?}"))
    }

    /// Creates a root invocation context whose lifetime can be controlled by
    /// the owning Surface.
    pub fn invocation_context_with_cancellation(
        &self,
        cancellation: CancellationToken,
    ) -> Result<InvocationContext, String> {
        let mut request_id = NEXT_ROOT_REQUEST_ID.load(Ordering::Relaxed);
        loop {
            let next = request_id
                .checked_add(1)
                .ok_or_else(|| "Host root request identity space is exhausted".to_owned())?;
            match NEXT_ROOT_REQUEST_ID.compare_exchange_weak(
                request_id,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => request_id = current,
            }
        }
        let context = InvocationContext::new(request_id, None, cancellation)
            .with_extension(
                GENERATION_SPEC_DIGEST_EXTENSION,
                self.generation_spec_digest().as_bytes().to_vec(),
            )
            .map_err(|error| format!("failed to attach Generation provenance: {error}"))?;
        if self.interactive {
            context
                .with_typed_extension(&InteractiveSurface)
                .map_err(|error| format!("failed to attach interactive Surface scope: {error}"))
        } else {
            Ok(context)
        }
    }

    pub async fn pending_interactions(&self) -> Result<Vec<PendingInteraction>, String> {
        let interaction = self
            .interaction
            .as_ref()
            .ok_or_else(|| "this Agent surface is not interactive".to_owned())?;
        interaction
            .pending
            .invoke_with_context(
                PENDING_OPERATION,
                self.invocation_context()?,
                PendingRequest {},
            )
            .await
            .map_err(|error| format!("User Interaction snapshot failed: {error:?}"))?
            .map(|response| response.interactions)
            .map_err(|error| format!("User Interaction snapshot was rejected: {error:?}"))
    }

    pub async fn answer_interaction(
        &self,
        interaction_id: String,
        answers: Vec<InteractionAnswer>,
    ) -> Result<(), String> {
        let interaction = self
            .interaction
            .as_ref()
            .ok_or_else(|| "this Agent surface is not interactive".to_owned())?;
        interaction
            .answer
            .invoke_with_context(
                ANSWER_OPERATION,
                self.invocation_context()?,
                AnswerRequest {
                    interaction_id,
                    answers,
                },
            )
            .await
            .map_err(|error| format!("User Interaction answer failed: {error:?}"))?
            .map(|_| ())
            .map_err(|error| format!("User Interaction answer was rejected: {error:?}"))
    }

    /// Reads durable Session events through the selected Session Plugin.
    pub async fn read_session(
        &self,
        session_id: String,
        after_revision: u64,
        limit: i64,
    ) -> Result<ReadSessionResponse, String> {
        let handle = self
            .route
            .target()
            .handle::<SessionRead>(&self.consumer_instance)
            .map_err(|error| format!("leased Generation has no Session route: {error:?}"))?;
        handle
            .invoke_with_context(
                READ_OPERATION,
                self.invocation_context()?,
                ReadSessionRequest {
                    after_revision: after_revision.to_string(),
                    limit,
                    session_id,
                },
            )
            .await
            .map_err(|error| format!("Session read failed: {error:?}"))?
            .map_err(|error| format!("Session read was rejected: {error:?}"))
    }

    /// Lists durable Sessions through the selected Session Plugin.
    pub async fn list_sessions(&self, limit: i64) -> Result<ListSessionsResponse, String> {
        let handle = self
            .route
            .target()
            .handle::<SessionList>(&self.consumer_instance)
            .map_err(|error| format!("leased Generation has no Session route: {error:?}"))?;
        handle
            .invoke_with_context(
                LIST_OPERATION,
                self.invocation_context()?,
                ListSessionsRequest { limit },
            )
            .await
            .map_err(|error| format!("Session list failed: {error:?}"))?
            .map_err(|error| format!("Session list was rejected: {error:?}"))
    }

    /// Replaces the user-owned Session title without changing the event-log revision.
    pub async fn rename_session(
        &self,
        session_id: String,
        title: String,
        expected_title_revision: String,
    ) -> Result<RenameSessionResponse, RenameSessionFailure> {
        let handle = self
            .route
            .target()
            .handle::<SessionRename>(&self.consumer_instance)
            .map_err(|error| {
                RenameSessionFailure::Runtime(format!(
                    "leased Generation has no Session route: {error:?}"
                ))
            })?;
        handle
            .invoke_with_context(
                RENAME_OPERATION,
                self.invocation_context()
                    .map_err(RenameSessionFailure::Runtime)?,
                RenameSessionRequest {
                    expected_title_revision,
                    session_id,
                    title,
                },
            )
            .await
            .map_err(|error| {
                RenameSessionFailure::Runtime(format!("Session rename failed: {error:?}"))
            })?
            .map_err(RenameSessionFailure::Domain)
    }

    /// Opens one durable Session through the selected Session Plugin.
    pub async fn open_session(&self) -> Result<String, String> {
        self.route
            .target()
            .handle::<SessionOpen>(&self.consumer_instance)
            .map_err(|error| format!("leased Generation has no Session route: {error:?}"))?
            .invoke_with_context(
                OPEN_OPERATION,
                self.invocation_context()?,
                OpenSessionRequest { session_id: None },
            )
            .await
            .map_err(|error| format!("Session open failed: {error:?}"))?
            .map(|response| response.session_id)
            .map_err(|error| format!("Session open was rejected: {error:?}"))
    }

    /// Creates an immutable branch containing all events before one selected Turn.
    pub async fn fork_session_before_turn(
        &self,
        source_session_id: String,
        turn_id: String,
    ) -> Result<String, String> {
        let events = self.read_all_session_events(source_session_id).await?;
        let target = events
            .iter()
            .position(|event| {
                event.turn_id.as_deref() == Some(turn_id.as_str())
                    && event.kind == ReadSessionResponseEventsItemKind::TurnStarted
            })
            .ok_or_else(|| "edited Turn was not found in the Session".to_owned())?;
        let branch_session_id = self.open_session().await?;
        let prefix = events
            .into_iter()
            .take(target)
            .map(|event| {
                let payload_json =
                    if event.kind == ReadSessionResponseEventsItemKind::SessionCreated {
                        serde_json::to_string(&serde_json::json!({
                            "session_id": branch_session_id,
                        }))
                        .map_err(|error| format!("failed to encode branch identity: {error}"))?
                        .try_into()
                        .map_err(|_| "failed to encode branch identity".to_owned())?
                    } else {
                        event.payload_json
                    };
                Ok(AppendSessionRequestEventsItem {
                    event_id: uuid::Uuid::new_v4().to_string(),
                    kind: append_event_kind(&event.kind),
                    occurred_at: event.occurred_at,
                    payload_json,
                    turn_id: event.turn_id,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        if !prefix.is_empty() {
            self.route
                .target()
                .handle::<SessionAppend>(&self.consumer_instance)
                .map_err(|error| format!("leased Generation has no Session route: {error:?}"))?
                .invoke_with_context(
                    APPEND_OPERATION,
                    self.invocation_context()?,
                    AppendSessionRequest {
                        events: prefix,
                        expected_revision: "0".to_owned(),
                        session_id: branch_session_id.clone(),
                    },
                )
                .await
                .map_err(|error| format!("Session branch append failed: {error:?}"))?
                .map_err(|error| format!("Session branch append was rejected: {error:?}"))?;
        }
        Ok(branch_session_id)
    }

    async fn read_all_session_events(
        &self,
        session_id: String,
    ) -> Result<Vec<lenso_capability_agent_session::ReadSessionResponseEventsItem>, String> {
        let mut cursor = 0_u64;
        let mut events = Vec::new();
        loop {
            let page = self.read_session(session_id.clone(), cursor, 1000).await?;
            if page.events.is_empty() {
                if cursor.to_string() == page.revision {
                    return Ok(events);
                }
                return Err("Session read ended before its advertised revision".to_owned());
            }
            cursor = page
                .events
                .last()
                .expect("non-empty page has a last event")
                .revision
                .parse::<u64>()
                .map_err(|_| "Session returned an invalid revision".to_owned())?;
            events.extend(page.events);
            if cursor.to_string() == page.revision {
                return Ok(events);
            }
        }
    }

    fn generation_spec_digest(&self) -> &str {
        self.route.generation_spec_digest()
    }
}

fn append_event_kind(
    kind: &ReadSessionResponseEventsItemKind,
) -> AppendSessionRequestEventsItemKind {
    match kind {
        ReadSessionResponseEventsItemKind::SessionCreated => {
            AppendSessionRequestEventsItemKind::SessionCreated
        }
        ReadSessionResponseEventsItemKind::SystemInstructionInstalled => {
            AppendSessionRequestEventsItemKind::SystemInstructionInstalled
        }
        ReadSessionResponseEventsItemKind::ContextCompactionStarted => {
            AppendSessionRequestEventsItemKind::ContextCompactionStarted
        }
        ReadSessionResponseEventsItemKind::ContextCompactionCommitted => {
            AppendSessionRequestEventsItemKind::ContextCompactionCommitted
        }
        ReadSessionResponseEventsItemKind::ContextCompactionFailed => {
            AppendSessionRequestEventsItemKind::ContextCompactionFailed
        }
        ReadSessionResponseEventsItemKind::MemoryRecalled => {
            AppendSessionRequestEventsItemKind::MemoryRecalled
        }
        ReadSessionResponseEventsItemKind::MemoryRecallFailed => {
            AppendSessionRequestEventsItemKind::MemoryRecallFailed
        }
        ReadSessionResponseEventsItemKind::MemoryCommitted => {
            AppendSessionRequestEventsItemKind::MemoryCommitted
        }
        ReadSessionResponseEventsItemKind::MemoryCommitFailed => {
            AppendSessionRequestEventsItemKind::MemoryCommitFailed
        }
        ReadSessionResponseEventsItemKind::TurnStarted => {
            AppendSessionRequestEventsItemKind::TurnStarted
        }
        ReadSessionResponseEventsItemKind::ModelRequested => {
            AppendSessionRequestEventsItemKind::ModelRequested
        }
        ReadSessionResponseEventsItemKind::ModelOutput => {
            AppendSessionRequestEventsItemKind::ModelOutput
        }
        ReadSessionResponseEventsItemKind::ToolRequested => {
            AppendSessionRequestEventsItemKind::ToolRequested
        }
        ReadSessionResponseEventsItemKind::ToolResult => {
            AppendSessionRequestEventsItemKind::ToolResult
        }
        ReadSessionResponseEventsItemKind::TurnCompleted => {
            AppendSessionRequestEventsItemKind::TurnCompleted
        }
        ReadSessionResponseEventsItemKind::TurnFailed => {
            AppendSessionRequestEventsItemKind::TurnFailed
        }
        ReadSessionResponseEventsItemKind::TurnCancelled => {
            AppendSessionRequestEventsItemKind::TurnCancelled
        }
    }
}

fn record_generation_spec(
    store_root: &Path,
    spec: &CanonicalDocument<AppGenerationSpec>,
) -> Result<(), String> {
    RuntimeState::open(store_root)?.record_generation(spec)
}

#[cfg(test)]
pub(crate) fn resolve_initial_generation(
    plan_bytes: &[u8],
    store_root: &Path,
) -> Result<ResolvedGeneration, String> {
    let host_build = HostBuildIdentity::current()?;
    resolve_initial_generation_for_host(plan_bytes, store_root, &host_build)
}

#[cfg(test)]
fn resolve_initial_generation_for_host(
    plan_bytes: &[u8],
    store_root: &Path,
    host_build: &HostBuildIdentity,
) -> Result<ResolvedGeneration, String> {
    let directories = directories_for_store_root(store_root)?;
    let authority = crate::generation_authority::load_generation_authority(store_root)?;
    resolve_generation_with_authority(plan_bytes, &authority, host_build, &directories.plugins())
}

fn resolve_retained_generations(
    plan_bytes: &[u8],
    store_root: &Path,
    host_build: &HostBuildIdentity,
) -> Result<BTreeMap<String, ResolvedGeneration>, String> {
    let directories = directories_for_store_root(store_root)?;
    crate::generation_authority::recovery_generation_authorities(store_root)
        .into_iter()
        .map(|authority| {
            let generation = resolve_generation_with_authority(
                plan_bytes,
                &authority,
                host_build,
                &directories.plugins(),
            )?;
            Ok((generation.spec.digest().to_owned(), generation))
        })
        .collect()
}

fn directories_for_store_root(store_root: &Path) -> Result<AgentDirectories, String> {
    let home = store_root.parent().ok_or_else(|| {
        format!(
            "Agent runtime root must have an Agent Home parent: {}",
            store_root.display()
        )
    })?;
    AgentDirectories::from_home(home)
}

fn resolve_generation_with_authority(
    plan_bytes: &[u8],
    authority: &crate::generation_authority::GenerationAuthority,
    host_build: &HostBuildIdentity,
    plugin_root: &Path,
) -> Result<ResolvedGeneration, String> {
    let plan = serde_json::from_slice::<ResolvedAppPlan>(plan_bytes)
        .map_err(|error| format!("resolved Plan is invalid JSON: {error}"))?;
    let resources = crate::plugin_root::plan_resources(plugin_root, &plan)?;
    resolve_generation_from_plan(&plan, authority, host_build, plugin_root, resources)
}

fn resolve_generation_from_plan(
    plan: &ResolvedAppPlan,
    authority: &crate::generation_authority::GenerationAuthority,
    host_build: &HostBuildIdentity,
    plugin_root: &Path,
    resources: lenso_runtime_codec::InstanceResourceCatalog,
) -> Result<ResolvedGeneration, String> {
    plan.validate()
        .map_err(|error| format!("resolved Plan is invalid: {error}"))?;
    if plan.execution_lanes().len() != 1 || plan.execution_lanes()[0].id().as_str() != "main" {
        return Err(
            "Plugin control-plane bootstrap currently supports the `main` execution lane only"
                .to_owned(),
        );
    }

    let target = format!("{}-unknown-{}", env::consts::ARCH, env::consts::OS);
    let (_, embedded_plugins) = native_host_build();
    let execution_classes = [
        (NATIVE_EXECUTION_CLASS, "lenso-native-adapter@0.1.2"),
        (QUICKJS_EXECUTION_CLASS, "lenso-quickjs-adapter@0.1.0"),
        (PROCESS_EXECUTION_CLASS, "lenso-process-adapter@0.1.0"),
        (BUN_EXECUTION_CLASS, "lenso-bun-adapter@0.1.3"),
        (WASM_EXECUTION_CLASS, "lenso-wasm-component-adapter@0.1.0"),
    ];
    let adapter_profiles = execution_classes
        .iter()
        .map(|(execution_class, build_identity)| AdapterProfile {
            execution_class: (*execution_class).to_owned(),
            adapter_build_identity: (*build_identity).to_owned(),
            targets: vec![target.clone()],
            profiles: Vec::new(),
        })
        .collect();
    let host_build = CanonicalDocument::from_value(
        "lenso-host-build.json",
        HostBuildManifest {
            schema_version: 1,
            app_id: APP_ID.to_owned(),
            host_executable_digest: host_build.executable_digest.clone(),
            target: target.clone(),
            embedded_plugins,
            adapter_profiles,
        },
    )
    .map_err(control_error)?;
    let policy = CanonicalDocument::from_value(
        "lenso-host-execution-policy.json",
        HostExecutionPolicy {
            schema_version: 1,
            app_id: APP_ID.to_owned(),
            host_build_manifest_digest: host_build.digest().to_owned(),
            target,
            preference: execution_classes
                .iter()
                .map(|(execution_class, _)| (*execution_class).to_owned())
                .collect(),
        },
    )
    .map_err(control_error)?;
    resolve_plan_generation(PlanGenerationInput {
        app_id: APP_ID,
        authority_digest: &authority.resolution_authority_digest,
        plan,
        host_build: &host_build,
        policy: &policy,
        artifacts: crate::plugin_root::plan_artifacts(plugin_root, plan)?,
        resources,
    })
    .map_err(control_error)
}

fn initial_transition(
    generation: &ResolvedGeneration,
) -> Result<CanonicalDocument<AppGenerationTransitionSpec>, ControlPlaneError> {
    CanonicalDocument::from_value(
        "lenso-generation-transition.json",
        AppGenerationTransitionSpec {
            schema_version: 1,
            app_id: APP_ID.to_owned(),
            from_generation_spec_digest: None,
            to_generation_spec_digest: generation.spec.digest().to_owned(),
            replacement_mode: ReplacementMode::Initial,
            state_compatibility_receipt_digests: Vec::new(),
            rollout_policy: RolloutPolicy {
                ready_timeout_nanos: READY_TIMEOUT_NANOS.to_string(),
                drain_timeout_nanos: DRAIN_TIMEOUT_NANOS.to_string(),
                rollback_window_nanos: "0".to_owned(),
                automatic_rollback_on_generation_failure: false,
            },
        },
    )
}

fn maintenance_transition(
    current: &ResolvedGeneration,
    candidate: &ResolvedGeneration,
) -> Result<CanonicalDocument<AppGenerationTransitionSpec>, ControlPlaneError> {
    CanonicalDocument::from_value(
        "lenso-generation-transition.json",
        AppGenerationTransitionSpec {
            schema_version: 1,
            app_id: APP_ID.to_owned(),
            from_generation_spec_digest: Some(current.spec.digest().to_owned()),
            to_generation_spec_digest: candidate.spec.digest().to_owned(),
            replacement_mode: ReplacementMode::Maintenance,
            state_compatibility_receipt_digests: Vec::new(),
            rollout_policy: RolloutPolicy {
                ready_timeout_nanos: READY_TIMEOUT_NANOS.to_string(),
                drain_timeout_nanos: DRAIN_TIMEOUT_NANOS.to_string(),
                rollback_window_nanos: "0".to_owned(),
                automatic_rollback_on_generation_failure: false,
            },
        },
    )
}

fn online_overlap_transition(
    current_generation_spec_digest: &str,
    candidate: &ResolvedGeneration,
) -> Result<CanonicalDocument<AppGenerationTransitionSpec>, ControlPlaneError> {
    CanonicalDocument::from_value(
        "lenso-generation-transition.json",
        AppGenerationTransitionSpec {
            schema_version: 1,
            app_id: APP_ID.to_owned(),
            from_generation_spec_digest: Some(current_generation_spec_digest.to_owned()),
            to_generation_spec_digest: candidate.spec.digest().to_owned(),
            replacement_mode: ReplacementMode::Overlap,
            state_compatibility_receipt_digests: Vec::new(),
            rollout_policy: RolloutPolicy {
                ready_timeout_nanos: READY_TIMEOUT_NANOS.to_string(),
                drain_timeout_nanos: ONLINE_DRAIN_TIMEOUT_NANOS.to_string(),
                rollback_window_nanos: ONLINE_ROLLBACK_WINDOW_NANOS.to_string(),
                automatic_rollback_on_generation_failure: true,
            },
        },
    )
}

fn native_host_build() -> (NativePluginRegistry, Vec<EmbeddedPlugin>) {
    let registry = NativePluginRegistry::new().with_linked_factories();
    let mut built_in_plugins = registry
        .factories()
        .map(|factory| EmbeddedPlugin {
            package_id: factory.package_id().to_owned(),
            factory_identity: factory.factory_identity(),
            execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        })
        .collect::<Vec<_>>();
    built_in_plugins.sort_by(|left, right| left.factory_identity.cmp(&right.factory_identity));
    (registry, built_in_plugins)
}

/// Returns the Host Catalog derived from the Plugin factories linked into this executable.
pub fn linked_host_catalog() -> Result<HostCatalog, String> {
    let directories = AgentDirectories::resolve()?;
    linked_host_catalog_in(&directories)
}

pub(crate) fn linked_host_catalog_in(
    directories: &AgentDirectories,
) -> Result<HostCatalog, String> {
    linked_host_catalog_for_agent_in(
        directories,
        &PluginInstanceId::new("lenso.agent.loop", "agent"),
    )
}

fn linked_host_catalog_for_agent_in(
    directories: &AgentDirectories,
    root_agent: &PluginInstanceId,
) -> Result<HostCatalog, String> {
    let registry = NativePluginRegistry::new().with_linked_factories();
    let available = registry
        .factories()
        .map(|factory| factory.package_id().to_owned())
        .collect::<BTreeSet<_>>();
    let defaults = host_catalog_defaults(directories, &available)
        .into_iter()
        .filter(|plugin| available.contains(plugin.id().plugin_id()))
        .collect::<Vec<_>>();
    let configurations = host_catalog_configurations(directories)
        .into_iter()
        .filter(|configuration| available.contains(configuration.id().plugin_id()))
        .collect::<Vec<_>>();
    NativePluginRegistry::host_catalog(host_catalog_slots(), defaults)
        .map(|catalog| {
            catalog
                .with_configurations(configurations)
                .with_bindings(host_catalog_bindings(root_agent, &available))
        })
        .map_err(|error| format!("linked Host Catalog is invalid: {error:?}"))
}

fn host_catalog_slots() -> Vec<HostSlot> {
    vec![
        HostSlot::many("agents"),
        HostSlot::optional("auth"),
        HostSlot::optional("console"),
        HostSlot::one("context-compactor").replaceable(),
        HostSlot::one("memory").replaceable(),
        HostSlot::many("surfaces"),
        HostSlot::many("tool-providers"),
        HostSlot::many("tool-hooks"),
        HostSlot::many("lifecycle-hooks"),
        HostSlot::one("http-fetch"),
        HostSlot::one("model").replaceable(),
        HostSlot::optional("process"),
        HostSlot::many("prompt-providers"),
        HostSlot::one("prompt-runtime"),
        HostSlot::one("root-tools-runtime"),
        HostSlot::optional("secrets"),
        HostSlot::one("session").replaceable(),
        HostSlot::optional("session-presentation").replaceable(),
        HostSlot::optional("restricted-tools-runtime"),
        HostSlot::many("tui-contributions"),
        HostSlot::many("tui-suggestions"),
        HostSlot::one("user-interaction").replaceable(),
        HostSlot::optional("workspace-import-read"),
    ]
}

fn host_catalog_defaults(
    directories: &AgentDirectories,
    available: &BTreeSet<String>,
) -> Vec<HostDefaultPlugin> {
    let mut defaults = agent_defaults(available);
    defaults.extend(default_interactive_plugins());
    defaults.extend([
        HostDefaultPlugin::new("lenso.agent.cli", "cli"),
        HostDefaultPlugin::new("lenso.agent.discord", "discord"),
        default_context_compaction_plugin(),
        default_memory_plugin(directories),
        default_session_presentation_plugin(),
        default_plugin(
            "lenso.agent.http-fetch",
            "http-fetch",
            serde_json::json!({
                "allowed_origins": [], "timeout_ms": 30000
            }),
        ),
        default_plugin(
            "lenso.agent.lifecycle.audit",
            "local-audit",
            serde_json::json!({"path": directories.lifecycle_events()}),
        ),
        HostDefaultPlugin::new("lenso.agent.prompt", "prompt"),
        default_plugin(
            "lenso.agent.prompt.static",
            "summary-skill",
            serde_json::json!({
                "contributions": [{
                    "id": "workspace.summary",
                    "version": "1.0.0",
                    "kind": "skill",
                    "content": "When summarizing a workspace file, ground the answer in the Tool result."
                }]
            }),
        ),
        default_plugin(
            "lenso.agent.session.sqlite",
            "sessions",
            serde_json::json!({
                "database": directories.session_database()
            }),
        ),
        default_skills_plugin(),
        HostDefaultPlugin::new("lenso.agent.telegram", "telegram"),
        HostDefaultPlugin::new("lenso.agent.tools", "tools"),
        HostDefaultPlugin::new("lenso.agent.ask-user-tools", "ask-user"),
        default_plugin(
            "lenso.agent.user-interaction.local",
            "local-interaction",
            serde_json::json!({"max_pending": 16, "timeout_ms": 300_000}),
        ),
        HostDefaultPlugin::new("lenso.agent.tui", "tui"),
        HostDefaultPlugin::new("lenso.agent.web", "web"),
        default_plugin(
            "lenso.agent.tui.static",
            "tui-help",
            serde_json::json!({
                "panels": [{
                    "id": "agent.help",
                    "title": "Help",
                    "body": "Enter sends a message.\nEsc cancels the active Turn or exits while idle.\nCtrl-C exits immediately.\nTab cycles contributed panels."
                }]
            }),
        ),
        default_plugin(
            "lenso.agent.tui-command-suggestions",
            "tui-commands",
            serde_json::json!({
                "commands": [
                    {"id": "agent.command.help", "label": "/help", "insert_text": "/help", "description": "Show keyboard shortcuts"},
                    {"id": "agent.command.clear", "label": "/clear", "insert_text": "/clear", "description": "Clear the visible conversation"},
                    {"id": "agent.command.new", "label": "/new", "insert_text": "/new", "description": "Start a new session"},
                    {"id": "agent.command.rename", "label": "/rename", "insert_text": "/rename ", "description": "Rename the current session"}
                ]
            }),
        ),
        default_plugin(
            "lenso.agent.tui-workspace-suggestions",
            "tui-workspace-suggestions",
            serde_json::json!({
                "root": "."
            }),
        ),
        default_plugin(
            "lenso.agent.workspace-import-read",
            "workspace-import-read",
            serde_json::json!({
                "root": "."
            }),
        ),
        default_plugin(
            "lenso.agent.workspace-read",
            "workspace-read",
            serde_json::json!({
                "root": "."
            }),
        ),
        HostDefaultPlugin::new("lenso.agent.workspace-read-tools", "restricted-read-tools"),
    ]);
    defaults
}

fn default_context_compaction_plugin() -> HostDefaultPlugin {
    default_plugin(
        "lenso.agent.context-compaction",
        "context-compactor",
        serde_json::json!({
            "max_input_characters": 1_048_576,
            "max_summary_characters": 8_192,
            "retain_recent_turns": 8
        }),
    )
}

fn default_memory_plugin(directories: &AgentDirectories) -> HostDefaultPlugin {
    default_plugin(
        "lenso.agent.memory.sqlite",
        "memory",
        serde_json::json!({
            "database": directories.memory_database(),
            "scope": "default",
            "max_records": 10_000,
            "max_item_characters": 16_384,
            "max_recall_items": 8,
            "max_recall_characters": 16_384
        }),
    )
}

fn default_session_presentation_plugin() -> HostDefaultPlugin {
    default_plugin(
        "lenso.agent.session-presentation",
        "presentation",
        serde_json::json!({
            "max_input_characters": 524_288,
            "max_title_characters": 80,
            "max_preview_characters": 240
        }),
    )
}

fn default_interactive_plugins() -> [HostDefaultPlugin; 3] {
    [
        default_plugin(
            "lenso.agent.auth.openai-codex",
            "auth",
            serde_json::json!({
                "issuer": "https://auth.openai.com",
                "profile": "default",
                "refresh_margin_seconds": 60
            }),
        ),
        default_plugin(
            "lenso.agent.model.openai-codex-direct",
            "model",
            serde_json::json!({
                "base_url": "https://chatgpt.com/backend-api",
                "max_event_bytes": 1_048_576,
                "model": DEFAULT_MODEL,
                "reasoning_effort": "medium"
            }),
        ),
        default_plugin(
            "lenso.agent.prompt.static",
            "default-instructions",
            serde_json::json!({
                "contributions": [{
                    "id": "harness.default",
                    "version": "1.0.0",
                    "kind": "instruction",
                    "content": "Be concise, follow explicit user instructions, and use only the Tools supplied by this App."
                }]
            }),
        ),
    ]
}

fn agent_defaults(available: &BTreeSet<String>) -> Vec<HostDefaultPlugin> {
    std::iter::once("agent")
        .chain(
            available
                .contains("lenso.agent.subagent-tools")
                .then_some("researcher"),
        )
        .chain(
            available
                .contains("lenso.agent.subagent-tools")
                .then_some("reviewer"),
        )
        .map(|instance_key| {
            default_plugin(
                "lenso.agent.loop",
                instance_key,
                serde_json::json!({
                    "model": DEFAULT_MODEL,
                    "max_steps": 8,
                    "max_tool_calls": 4,
                    "max_parallel_tool_calls": 4,
                    "max_output_tokens": 1024,
                    "max_history_events": 200,
                    "max_compaction_summary_characters": 8192,
                    "max_memory_items": 8,
                    "max_memory_characters": 16384
                }),
            )
        })
        .collect()
}

fn default_skills_plugin() -> HostDefaultPlugin {
    default_plugin(
        "lenso.agent.skills.filesystem",
        "skills",
        serde_json::json!({
            "catalog_contribution_id": "agents.skills.catalog",
            "max_catalog_bytes": 262_144,
            "max_file_bytes": 262_144,
            "max_prompt_catalog_bytes": 8_000,
            "max_resource_entries": 8_192,
            "max_resource_file_bytes": 262_144,
            "max_resource_manifest_bytes": 524_288,
            "max_resource_total_bytes": 16_777_216,
            "max_skills": 256,
            "max_total_bytes": 8_388_608,
            "root": "~/.agents/skills"
        }),
    )
}

fn host_catalog_configurations(directories: &AgentDirectories) -> Vec<HostPluginConfiguration> {
    let mut configurations = local_tool_configurations(directories);
    configurations.extend(model_and_auth_configurations(directories));
    configurations.push(host_plugin_configuration(
        "lenso.agent.session-presentation.model",
        serde_json::json!({
            "model": DEFAULT_MODEL,
            "instruction": "Create concise Session display metadata grounded only in the completed Turn.",
            "temperature": 0.0,
            "max_output_tokens": 256,
            "max_input_characters": 524_288,
            "max_title_characters": 80,
            "max_preview_characters": 240
        }),
    ));
    configurations
}

fn model_and_auth_configurations(directories: &AgentDirectories) -> Vec<HostPluginConfiguration> {
    vec![
        host_plugin_configuration(
            "lenso.agent.auth.openai-codex",
            serde_json::json!({
                "issuer": "https://auth.openai.com",
                "profile": "default",
                "credential_file": directories.auth(),
                "refresh_margin_seconds": 60
            }),
        ),
        host_plugin_configuration(
            "lenso.agent.model.openai-codex-direct",
            serde_json::json!({
                "base_url": "https://chatgpt.com/backend-api",
                "max_event_bytes": 1_048_576,
                "model": "gpt-5.6-luna",
                "reasoning_effort": "medium"
            }),
        ),
        host_plugin_configuration(
            "lenso.agent.model.openai-compatible",
            serde_json::json!({
                "api_key_ref": "model/openai-api-key",
                "base_url": "https://api.openai.com/v1",
                "model": "gpt-4o-mini"
            }),
        ),
    ]
}

fn local_tool_configurations(directories: &AgentDirectories) -> Vec<HostPluginConfiguration> {
    vec![
        host_plugin_configuration(
            "lenso.agent.approval-hook",
            serde_json::json!({
                "allow_tools": ["read_text"],
                "ask_tools": [],
                "default_decision": "ask",
                "deny_tools": [],
                "directory": directories.approvals(),
                "max_records": 10_000
            }),
        ),
        host_plugin_configuration(
            "lenso.agent.interactive-approval-hook",
            serde_json::json!({
                "allow_tools": [
                    "read_text", "skill_list", "skill", "skill_resources", "skill_resource",
                    "ask_user", "git_status", "git_diff", "git_log", "list_subagents",
                    "checkpoint_create", "checkpoint_review"
                ],
                "ask_tools": [],
                "default_decision": "ask",
                "deny_tools": [],
                "max_preview_bytes": 16_384
            }),
        ),
        host_plugin_configuration(
            "lenso.agent.code-mode-tools",
            serde_json::json!({
                "max_code_bytes": 32_768,
                "max_instructions": 1_000_000,
                "max_memory_bytes": 8_388_608,
                "max_output_bytes": 262_144,
                "max_parallel_subcalls": 4,
                "max_subcalls": 16
            }),
        ),
        host_plugin_configuration(
            "lenso.agent.git-tools",
            serde_json::json!({
                "default_timeout_ms": 30_000,
                "max_log_entries": 50,
                "max_commit_message_bytes": 4_096
            }),
        ),
        host_plugin_configuration(
            "lenso.agent.process.native",
            serde_json::json!({
                "allowed_programs": ["cargo", "git", "rg"],
                "program_presets": ["rust", "javascript", "python", "go", "build"],
                "environment_allowlist": [
                    "PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "TMPDIR", "LANG", "LC_ALL"
                ],
                "max_argument_bytes": 131_072,
                "max_output_bytes": 262_144,
                "max_timeout_ms": 600_000,
                "root": "."
            }),
        ),
        sandbox_process_configuration(directories),
        host_plugin_configuration(
            "lenso.agent.process-tools",
            serde_json::json!({"default_timeout_ms": 120_000}),
        ),
        host_plugin_configuration(
            "lenso.agent.subagent-tools",
            serde_json::json!({
                "max_output_bytes": 1_048_576,
                "max_task_bytes": 262_144,
                "max_tasks": 8
            }),
        ),
        host_plugin_configuration(
            "lenso.agent.workspace-edit",
            serde_json::json!({
                "checkpoint_directory": directories.runtime().join("workspace-checkpoints"),
                "max_checkpoints": 100,
                "max_edit_bytes": 131_072,
                "max_file_bytes": 1_048_576,
                "max_review_bytes": 262_144,
                "require_checkpoint": false,
                "root": "."
            }),
        ),
        host_plugin_configuration(
            "lenso.agent.workspace-instructions",
            serde_json::json!({
                "working_directory": ".",
                "file_name": "AGENTS.md",
                "max_ancestor_depth": 32,
                "max_file_bytes": 262_144,
                "max_total_bytes": 1_048_576
            }),
        ),
    ]
}

fn sandbox_process_configuration(directories: &AgentDirectories) -> HostPluginConfiguration {
    host_plugin_configuration(
        "lenso.agent.process.sandbox",
        serde_json::json!({
            "allow_network": false,
            "allowed_programs": ["cargo", "git", "rg"],
            "backend": "auto",
            "environment_allowlist": [
                "PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "LANG", "LC_ALL"
            ],
            "max_argument_bytes": 131_072,
            "max_output_bytes": 262_144,
            "max_timeout_ms": 600_000,
            "program_presets": ["rust", "javascript", "python", "go", "build"],
            "root": ".",
            "temporary_directory": directories.runtime().join("process-sandbox")
        }),
    )
}

fn host_plugin_configuration(
    plugin_id: &str,
    configuration: serde_json::Value,
) -> HostPluginConfiguration {
    HostPluginConfiguration::new(plugin_id, "default", configuration)
}

fn host_catalog_bindings(
    selected_agent: &PluginInstanceId,
    available: &BTreeSet<String>,
) -> Vec<HostBinding> {
    let root_tools = PluginInstanceId::new("lenso.agent.tools", "tools");
    let restricted_tools =
        PluginInstanceId::new("lenso.agent.workspace-read-tools", "restricted-read-tools");
    let root_agent = PluginInstanceId::new("lenso.agent.loop", "agent");
    let child_agents = [
        PluginInstanceId::new("lenso.agent.loop", "researcher"),
        PluginInstanceId::new("lenso.agent.loop", "reviewer"),
    ];
    let tool_admission = RequestAdmissionPlan::new(0, 4);
    let mut bindings = vec![
        HostBinding::to_instance(root_agent.clone(), "lenso.agent.tools@2", root_tools)
            .with_admission(tool_admission),
    ];
    if available.contains("lenso.agent.subagent-tools")
        && available.contains("lenso.agent.workspace-read-tools")
    {
        bindings.extend(child_agents.iter().cloned().map(|child_agent| {
            HostBinding::to_instance(child_agent, "lenso.agent.tools@2", restricted_tools.clone())
                .with_admission(tool_admission)
        }));
    }
    if selected_agent != &root_agent {
        bindings.push(
            HostBinding::to_instance(
                selected_agent.clone(),
                "lenso.agent.tools@2",
                PluginInstanceId::new("lenso.agent.tools", "tools"),
            )
            .with_admission(tool_admission),
        );
    }
    for surface in [
        PluginInstanceId::new("lenso.agent.cli", "cli"),
        PluginInstanceId::new("lenso.agent.discord", "discord"),
        PluginInstanceId::new("lenso.agent.telegram", "telegram"),
        PluginInstanceId::new("lenso.agent.tui", "tui"),
        PluginInstanceId::new("lenso.agent.web", "web"),
    ]
    .into_iter()
    .filter(|surface| available.contains(surface.plugin_id()))
    {
        bindings.push(HostBinding::to_instance(
            surface,
            "lenso.agent@3",
            selected_agent.clone(),
        ));
    }
    if available.contains("lenso.agent.code-mode-tools")
        && available.contains("lenso.agent.workspace-read-tools")
    {
        bindings.push(HostBinding::to_instance(
            PluginInstanceId::new("lenso.agent.code-mode-tools", "default"),
            "lenso.agent.tools@2",
            restricted_tools,
        ));
    }
    if available.contains("lenso.agent.subagent-tools") {
        let subagent_tools = PluginInstanceId::new("lenso.agent.subagent-tools", "default");
        bindings.push(HostBinding::to_instances(
            subagent_tools.clone(),
            "lenso.agent@3",
            child_agents.iter().cloned(),
        ));
        bindings.push(HostBinding::to_instances(
            subagent_tools,
            "lenso.agent.turn-input@1",
            child_agents,
        ));
    }
    bindings
}

fn default_plugin(
    plugin_id: &str,
    instance_key: &str,
    configuration: serde_json::Value,
) -> HostDefaultPlugin {
    HostDefaultPlugin::new(plugin_id, instance_key).with_configuration(configuration)
}

#[cfg(test)]
pub(crate) fn resolve_host_plan(root: &PluginRootSnapshot) -> Result<ResolvedAppPlan, String> {
    let directories = AgentDirectories::resolve()?;
    resolve_host_plan_in(&directories, root)
}

pub(crate) fn resolve_host_plan_in(
    directories: &AgentDirectories,
    root: &PluginRootSnapshot,
) -> Result<ResolvedAppPlan, String> {
    let host = linked_host_catalog_in(directories)?;
    resolve_plugin_root(&host, root)
        .map(|app| app.plan().clone())
        .map_err(|error| format!("failed to resolve Host Plugins: {error}"))
}

#[cfg(test)]
pub(crate) fn resolve_host_plan_for_agent(
    root: &PluginRootSnapshot,
    agent: &PluginInstanceId,
) -> Result<ResolvedAppPlan, String> {
    let directories = AgentDirectories::resolve()?;
    resolve_host_plan_for_agent_in(&directories, root, agent)
}

pub(crate) fn resolve_host_plan_for_agent_in(
    directories: &AgentDirectories,
    root: &PluginRootSnapshot,
    agent: &PluginInstanceId,
) -> Result<ResolvedAppPlan, String> {
    let host = linked_host_catalog_for_agent_in(directories, agent)?;
    resolve_plugin_root(&host, root)
        .map(|app| app.plan().clone())
        .map_err(|error| format!("failed to resolve Host Plugins for Agent `{agent}`: {error}"))
}

fn harness_catalog_factory() -> MultiExecutionCatalogFactory<HarnessCatalogFactory> {
    MultiExecutionCatalogFactory::new(HarnessCatalogFactory)
        .with_wasm_codec(AgentJsonCodec)
        .with_wasm_codec(ContextCompactionJsonCodec)
        .with_wasm_codec(ContextSourceJsonCodec)
        .with_wasm_codec(MemoryJsonCodec)
        .with_wasm_codec(HttpFetchJsonCodec)
        .with_wasm_codec(LifecycleJsonCodec)
        .with_wasm_codec(ModelJsonCodec)
        .with_wasm_codec(PromptJsonCodec)
        .with_wasm_codec(SessionJsonCodec)
        .with_wasm_codec(SessionPresentationJsonCodec)
        .with_wasm_codec(ToolHookJsonCodec)
        .with_wasm_codec(ToolProviderJsonCodec)
        .with_wasm_codec(TurnInputJsonCodec)
        .with_wasm_codec(ToolsJsonCodec)
        .with_wasm_codec(UserInteractionJsonCodec)
        .with_wasm_codec(WorkspaceReadJsonCodec)
        .with_quickjs_codec(AgentJsonCodec)
        .with_quickjs_codec(ContextCompactionJsonCodec)
        .with_quickjs_codec(ContextSourceJsonCodec)
        .with_quickjs_codec(MemoryJsonCodec)
        .with_quickjs_codec(LifecycleJsonCodec)
        .with_quickjs_codec(ModelJsonCodec)
        .with_quickjs_codec(PromptJsonCodec)
        .with_quickjs_codec(SessionJsonCodec)
        .with_quickjs_codec(ToolHookJsonCodec)
        .with_quickjs_codec(TurnInputJsonCodec)
        .with_quickjs_codec(ToolsJsonCodec)
        .with_quickjs_codec(UserInteractionJsonCodec)
        .with_quickjs_codec(WorkspaceReadJsonCodec)
        .with_process_codec(AgentJsonCodec)
        .with_process_codec(ContextCompactionJsonCodec)
        .with_process_codec(ContextSourceJsonCodec)
        .with_process_codec(MemoryJsonCodec)
        .with_process_codec(HttpFetchJsonCodec)
        .with_process_codec(LifecycleJsonCodec)
        .with_process_codec(ModelJsonCodec)
        .with_process_codec(PromptJsonCodec)
        .with_process_codec(SessionJsonCodec)
        .with_process_codec(ToolHookJsonCodec)
        .with_process_codec(ToolProviderJsonCodec)
        .with_process_codec(TurnInputJsonCodec)
        .with_process_codec(ToolsJsonCodec)
        .with_process_codec(UserInteractionJsonCodec)
        .with_process_codec(WorkspaceReadJsonCodec)
}

fn now_unix_nanos() -> Result<u128, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|error| format!("system clock precedes Unix epoch: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn control_error(error: ControlPlaneError) -> String {
    format!("Plugin control plane failed: {error:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_is_an_optional_host_slot() {
        let slot = host_catalog_slots()
            .into_iter()
            .find(|slot| slot.id() == "console")
            .unwrap();

        assert_eq!(
            slot.cardinality(),
            lenso_app_plan::authoring::HostSlotCardinality::Optional
        );
    }

    #[test]
    fn tui_panel_limits_reject_oversized_snapshots() {
        let panels = (0..=MAX_TUI_PANELS)
            .map(|index| SnapshotResponsePanelsItem {
                id: format!("agent.panel-{index}"),
                title: format!("Panel {index}"),
                body: "Content".to_owned(),
            })
            .collect::<Vec<_>>();
        assert!(validate_tui_panels(&panels).is_err());
    }

    #[test]
    fn relative_plugin_root_watches_the_current_directory() {
        assert_eq!(watch_parent(Path::new("plugins")), Path::new("."));
    }

    #[test]
    fn empty_plugin_root_selects_direct_codex_with_auth() {
        let plan = resolve_host_plan(&PluginRootSnapshot::default()).unwrap();
        let plan_json = serde_json::to_value(&plan).unwrap();
        let instances = plan
            .plugin_instances()
            .iter()
            .map(lenso_app_plan::PluginInstancePlan::instance_key)
            .collect::<BTreeSet<_>>();

        assert!(instances.contains("lenso.agent.auth.openai-codex/auth"));
        assert!(instances.contains("lenso.agent.model.openai-codex-direct/model"));
        assert!(instances.contains("lenso.agent.context-compaction/context-compactor"));
        assert!(instances.contains("lenso.agent.memory.sqlite/memory"));
        assert!(instances.contains("lenso.agent.session-presentation/presentation"));
        assert!(instances.contains("lenso.agent.skills.filesystem/skills"));
        assert!(!instances.contains("lenso.agent.model.fixture/model"));
        assert!(
            plan_json["capability_bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|binding| {
                    binding["consumer_instance"] == "lenso.agent.loop/agent"
                        && binding["provider_instance"]
                            == "lenso.agent.context-compaction/context-compactor"
                        && binding["capability_id"] == "lenso.agent.context-compaction@1"
                })
        );
        assert!(
            plan_json["capability_bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|binding| {
                    binding["consumer_instance"] == "lenso.agent.loop/agent"
                        && binding["provider_instance"]
                            == "lenso.agent.session-presentation/presentation"
                        && binding["capability_id"] == "lenso.agent.session-presentation@1"
                })
        );
        assert!(
            plan_json["capability_bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|binding| {
                    binding["consumer_instance"] == "lenso.agent.loop/agent"
                        && binding["provider_instance"] == "lenso.agent.memory.sqlite/memory"
                        && binding["capability_id"] == "lenso.agent.memory@1"
                })
        );
    }

    #[test]
    fn optional_session_presentation_can_be_removed_without_changing_the_agent_loop() {
        let directories = AgentDirectories::resolve().unwrap();
        let registry = NativePluginRegistry::new().with_linked_factories();
        let available = registry
            .factories()
            .map(|factory| factory.package_id().to_owned())
            .collect::<BTreeSet<_>>();
        let defaults = host_catalog_defaults(&directories, &available)
            .into_iter()
            .filter(|plugin| plugin.id().plugin_id() != "lenso.agent.session-presentation")
            .filter(|plugin| available.contains(plugin.id().plugin_id()))
            .collect::<Vec<_>>();
        let configurations = host_catalog_configurations(&directories)
            .into_iter()
            .filter(|configuration| available.contains(configuration.id().plugin_id()))
            .collect::<Vec<_>>();
        let root_agent = PluginInstanceId::new("lenso.agent.loop", "agent");
        let catalog = NativePluginRegistry::host_catalog(host_catalog_slots(), defaults)
            .unwrap()
            .with_configurations(configurations)
            .with_bindings(host_catalog_bindings(&root_agent, &available));
        let app = resolve_plugin_root(&catalog, &PluginRootSnapshot::default()).unwrap();
        let plan = app.plan();

        assert!(plan.plugin_instances().iter().all(|plugin| {
            plugin.instance_key() != "lenso.agent.session-presentation/presentation"
        }));
        let plan_json = serde_json::to_value(plan).unwrap();
        assert!(
            plan_json["capability_bindings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|binding| binding["capability_id"] != "lenso.agent.session-presentation@1")
        );
    }

    #[test]
    fn profile_configuration_can_replace_local_presentation_with_model_projection() {
        let root = PluginRootSnapshot::new(
            [],
            [
                lenso_app_plan::authoring::PluginRootInstance::new(
                    "lenso.agent.model.fixture",
                    "model",
                )
                .with_configuration(serde_json::json!({
                    "model": "fixture/readme-summary-v1",
                    "allowed_models": ["fixture/session-presentation-v1"]
                })),
                lenso_app_plan::authoring::PluginRootInstance::new(
                    "lenso.agent.session-presentation.model",
                    "semantic",
                )
                .with_configuration(serde_json::json!({
                    "model": "fixture/session-presentation-v1",
                    "instruction": "Create concise Session display metadata.",
                    "temperature": 0.0,
                    "max_output_tokens": 256,
                    "max_input_characters": 524_288,
                    "max_title_characters": 80,
                    "max_preview_characters": 240
                })),
            ],
            [],
        );
        let plan = resolve_host_plan(&root).unwrap();
        let instances = plan
            .plugin_instances()
            .iter()
            .map(lenso_app_plan::PluginInstancePlan::instance_key)
            .collect::<BTreeSet<_>>();
        assert!(instances.contains("lenso.agent.session-presentation.model/semantic"));
        assert!(!instances.contains("lenso.agent.session-presentation/presentation"));

        let plan_json = serde_json::to_value(&plan).unwrap();
        let bindings = plan_json["capability_bindings"].as_array().unwrap();
        assert!(bindings.iter().any(|binding| {
            binding["consumer_instance"] == "lenso.agent.session-presentation.model/semantic"
                && binding["provider_instance"] == "lenso.agent.model.fixture/model"
                && binding["capability_id"] == "lenso.agent.model@2"
        }));
        assert!(bindings.iter().any(|binding| {
            binding["consumer_instance"] == "lenso.agent.loop/agent"
                && binding["provider_instance"] == "lenso.agent.session-presentation.model/semantic"
                && binding["capability_id"] == "lenso.agent.session-presentation@1"
        }));
    }

    #[test]
    fn profile_can_select_a_distinct_agent_loop_and_model_instance() {
        let root = PluginRootSnapshot::new(
            [],
            [
                lenso_app_plan::authoring::PluginRootInstance::new("lenso.agent.loop", "game")
                    .with_configuration(serde_json::json!({
                        "model": "fixture/readme-summary-v1",
                        "max_steps": 12,
                        "max_tool_calls": 6,
                        "max_parallel_tool_calls": 2,
                        "max_output_tokens": 2048,
                        "max_history_events": 100,
                        "max_compaction_summary_characters": 8192,
                        "max_memory_items": 8,
                        "max_memory_characters": 16384
                    })),
                lenso_app_plan::authoring::PluginRootInstance::new(
                    "lenso.agent.model.fixture",
                    "game-model",
                )
                .with_configuration(serde_json::json!({
                    "model": "fixture/readme-summary-v1"
                })),
                lenso_app_plan::authoring::PluginRootInstance::new(
                    "lenso.agent.memory.sqlite",
                    "game-memory",
                )
                .with_configuration(serde_json::json!({
                    "database": ".lenso/memory/game.sqlite3",
                    "scope": "game",
                    "max_records": 1_000,
                    "max_item_characters": 8_192,
                    "max_recall_items": 4,
                    "max_recall_characters": 8_192
                })),
            ],
            [],
        );
        let selected_agent = PluginInstanceId::new("lenso.agent.loop", "game");
        let plan = resolve_host_plan_for_agent(&root, &selected_agent).unwrap();
        let plan = serde_json::to_value(plan).unwrap();

        assert!(
            plan["plugin_instances"]
                .as_array()
                .unwrap()
                .iter()
                .any(|plugin| { plugin["instance_key"] == "lenso.agent.model.fixture/game-model" })
        );
        assert!(
            plan["capability_bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|binding| {
                    binding["consumer_instance"] == "lenso.agent.cli/cli"
                        && binding["provider_instance"] == "lenso.agent.loop/game"
                })
        );
        assert!(
            plan["capability_bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|binding| {
                    binding["consumer_instance"] == "lenso.agent.loop/game"
                        && binding["provider_instance"] == "lenso.agent.memory.sqlite/game-memory"
                        && binding["capability_id"] == "lenso.agent.memory@1"
                })
        );
        let memory = plan["plugin_instances"]
            .as_array()
            .unwrap()
            .iter()
            .find(|plugin| plugin["instance_key"] == "lenso.agent.memory.sqlite/game-memory")
            .unwrap();
        let configuration: serde_json::Value =
            serde_json::from_str(memory["configuration"].as_str().unwrap()).unwrap();
        assert_eq!(configuration["scope"], "game");
    }

    #[test]
    fn initial_transition_preserves_the_resolved_generation() {
        let directory = tempfile::tempdir().unwrap();
        let generation =
            resolve_initial_generation(crate::test_support::headless_plan(), directory.path())
                .unwrap();
        let transition = initial_transition(&generation).unwrap();
        assert_eq!(
            transition.value().to_generation_spec_digest,
            generation.spec.digest()
        );
        assert_eq!(
            transition.value().replacement_mode,
            ReplacementMode::Initial
        );
    }

    #[test]
    fn plugin_root_edits_derive_a_new_generation_and_reject_invalid_state() {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        let store_root = directory.path().join("state");
        fs::create_dir_all(&store_root).unwrap();
        let host_build = HostBuildIdentity::current().unwrap();
        let mut last_attempted = None;

        let (_, base) = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap();

        let text_tools = plugin_root.join("lenso.agent.text-tools");
        fs::create_dir_all(&text_tools).unwrap();
        fs::write(text_tools.join("default.toml"), "").unwrap();
        let (_, configured) = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap();
        assert_ne!(configured.spec.digest(), base.spec.digest());
        assert!(
            configured
                .plan
                .plugin_instances()
                .iter()
                .any(|plugin| { plugin.instance_key() == "lenso.agent.text-tools/default" })
        );

        fs::write(text_tools.join("default.toml"), "not valid = [").unwrap();
        let rejected = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap_err();
        assert!(matches!(rejected, OnlineGenerationEvent::Rejected { .. }));

        fs::remove_dir_all(text_tools).unwrap();
        let (_, restored) = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap();
        assert_eq!(restored.spec.digest(), base.spec.digest());
    }

    #[test]
    fn resource_only_edits_create_a_generation_and_retain_old_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        let store_root = directory.path().join("state");
        let text_tools = plugin_root.join("lenso.agent.text-tools");
        let resource_directory = text_tools.join("default/prompts");
        fs::create_dir_all(&resource_directory).unwrap();
        fs::create_dir_all(&store_root).unwrap();
        fs::write(text_tools.join("default.toml"), "").unwrap();
        let resource = resource_directory.join("system.md");
        fs::write(&resource, "generation one").unwrap();
        let host_build = HostBuildIdentity::current().unwrap();
        let mut last_attempted = None;

        let (_, first) = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap();
        let retained_resources = first.resources.clone();

        fs::write(&resource, "generation two").unwrap();
        let (_, second) = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap();

        assert_ne!(second.spec.digest(), first.spec.digest());
        assert_eq!(
            retained_resources
                .for_instance("lenso.agent.text-tools/default")
                .read_text("prompts/system.md")
                .unwrap(),
            "generation one"
        );
        assert_eq!(
            second
                .resources
                .for_instance("lenso.agent.text-tools/default")
                .read_text("prompts/system.md")
                .unwrap(),
            "generation two"
        );
    }

    #[test]
    fn optional_linked_plugin_uses_host_configuration_only_after_it_is_added() {
        let empty = resolve_host_plan(&PluginRootSnapshot::default()).unwrap();
        assert!(
            !empty
                .plugin_instances()
                .iter()
                .any(|plugin| { plugin.instance_key() == "lenso.agent.workspace-edit/default" })
        );

        let directory = tempfile::tempdir().unwrap();
        let plugin_directory = directory.path().join("lenso.agent.workspace-edit");
        fs::create_dir_all(&plugin_directory).unwrap();
        fs::write(plugin_directory.join("default.toml"), "").unwrap();
        let root = crate::plugin_root::snapshot(directory.path()).unwrap();
        let configured = resolve_host_plan(&root).unwrap();
        let plugin = configured
            .plugin_instances()
            .iter()
            .find(|plugin| plugin.instance_key() == "lenso.agent.workspace-edit/default")
            .unwrap();
        let configuration: serde_json::Value =
            serde_json::from_str(plugin.configuration()).unwrap();
        assert_eq!(configuration["root"], ".");
        assert_eq!(configuration["max_file_bytes"], 1_048_576);
        assert_eq!(configuration["max_edit_bytes"], 131_072);
        assert_eq!(configuration["max_checkpoints"], 100);
        assert_eq!(configuration["max_review_bytes"], 262_144);
        assert_eq!(configuration["require_checkpoint"], false);
        assert!(
            configuration["checkpoint_directory"]
                .as_str()
                .is_some_and(|path| path.ends_with("runtime/workspace-checkpoints"))
        );
    }

    #[test]
    fn subagent_tasks_are_optional_bounded_and_bound_to_named_child_agents() {
        let empty = resolve_host_plan(&PluginRootSnapshot::default()).unwrap();
        assert!(
            !empty
                .plugin_instances()
                .iter()
                .any(|plugin| { plugin.instance_key() == "lenso.agent.subagent-tools/default" })
        );

        let directory = tempfile::tempdir().unwrap();
        let plugin_directory = directory.path().join("lenso.agent.subagent-tools");
        fs::create_dir_all(&plugin_directory).unwrap();
        fs::write(plugin_directory.join("default.toml"), "").unwrap();
        let root = crate::plugin_root::snapshot(directory.path()).unwrap();
        let configured = resolve_host_plan(&root).unwrap();
        let plugin = configured
            .plugin_instances()
            .iter()
            .find(|plugin| plugin.instance_key() == "lenso.agent.subagent-tools/default")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(plugin.configuration()).unwrap(),
            serde_json::json!({
                "max_output_bytes": 1_048_576,
                "max_task_bytes": 262_144,
                "max_tasks": 8
            })
        );
        let plan_json = serde_json::to_value(&configured).unwrap();
        let bindings = plan_json["capability_bindings"].as_array().unwrap();
        for child in ["researcher", "reviewer"] {
            for capability in ["lenso.agent@3", "lenso.agent.turn-input@1"] {
                assert!(bindings.iter().any(|binding| {
                    binding["consumer_instance"] == "lenso.agent.subagent-tools/default"
                        && binding["provider_instance"] == format!("lenso.agent.loop/{child}")
                        && binding["capability_id"] == capability
                }));
            }
        }
    }

    #[test]
    fn linked_secret_providers_use_only_the_selected_instance_configuration() {
        let providers = [
            (
                "lenso.secrets.env",
                serde_json::json!({
                    "references": {"model/openai-api-key": "OPENAI_API_KEY"}
                }),
            ),
            (
                "lenso.secrets.keychain",
                serde_json::json!({
                    "service": "com.lenso.agent.code",
                    "references": {"model/openai-api-key": "openai-api-key"}
                }),
            ),
            (
                "lenso.secrets.encrypted-file",
                serde_json::json!({
                    "path": ".lenso/secrets.age",
                    "key_environment_variable": "LENSO_SECRETS_FILE_PASSPHRASE",
                    "references": {"model/openai-api-key": "openai"},
                    "max_file_bytes": 1_048_576,
                    "max_plaintext_bytes": 1_048_576,
                    "max_records": 100
                }),
            ),
            (
                "lenso.secrets.command",
                serde_json::json!({
                    "program": "/opt/homebrew/bin/op",
                    "arguments": ["read", "--no-newline", "{source}"],
                    "environment_allowlist": ["OP_SERVICE_ACCOUNT_TOKEN", "HOME"],
                    "references": {"model/openai-api-key": "op://agent/openai/password"},
                    "timeout_ms": 30_000,
                    "max_output_bytes": 65_536
                }),
            ),
        ];

        for (plugin_id, configuration) in providers {
            let root = PluginRootSnapshot::new(
                [],
                [
                    lenso_app_plan::authoring::PluginRootInstance::new(plugin_id, "code")
                        .with_configuration(configuration.clone()),
                ],
                [],
            );
            let plan = resolve_host_plan(&root).unwrap();
            let selected = plan
                .plugin_instances()
                .iter()
                .find(|plugin| plugin.instance_key() == format!("{plugin_id}/code"))
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(selected.configuration()).unwrap(),
                configuration
            );
        }
    }

    #[test]
    fn git_tools_are_optional_and_bind_to_the_selected_process_provider() {
        let empty = resolve_host_plan(&PluginRootSnapshot::default()).unwrap();
        assert!(
            !empty
                .plugin_instances()
                .iter()
                .any(|plugin| { plugin.instance_key() == "lenso.agent.git-tools/default" })
        );

        let directory = tempfile::tempdir().unwrap();
        for plugin_id in ["lenso.agent.process.native", "lenso.agent.git-tools"] {
            let plugin_directory = directory.path().join(plugin_id);
            fs::create_dir_all(&plugin_directory).unwrap();
            fs::write(plugin_directory.join("default.toml"), "").unwrap();
        }
        let root = crate::plugin_root::snapshot(directory.path()).unwrap();
        let configured = resolve_host_plan(&root).unwrap();
        let plugin = configured
            .plugin_instances()
            .iter()
            .find(|plugin| plugin.instance_key() == "lenso.agent.git-tools/default")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(plugin.configuration()).unwrap(),
            serde_json::json!({
                "default_timeout_ms": 30_000,
                "max_log_entries": 50,
                "max_commit_message_bytes": 4_096
            })
        );
        let plan_json = serde_json::to_value(&configured).unwrap();
        assert!(
            plan_json["capability_bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|binding| {
                    binding["consumer_instance"] == "lenso.agent.git-tools/default"
                        && binding["provider_instance"] == "lenso.agent.process.native/default"
                        && binding["capability_id"] == "lenso.agent.process@1"
                })
        );

        let git_only_directory = tempfile::tempdir().unwrap();
        let plugin_directory = git_only_directory.path().join("lenso.agent.git-tools");
        fs::create_dir_all(&plugin_directory).unwrap();
        fs::write(plugin_directory.join("default.toml"), "").unwrap();
        let root = crate::plugin_root::snapshot(git_only_directory.path()).unwrap();
        let error = resolve_host_plan(&root).unwrap_err();
        assert!(error.contains("lenso.agent.process@1"), "{error}");
    }

    #[test]
    fn mcp_client_is_linked_opt_in_and_uses_one_plugin_root_configuration() {
        let empty = resolve_host_plan(&PluginRootSnapshot::default()).unwrap();
        assert!(
            !empty
                .plugin_instances()
                .iter()
                .any(|plugin| plugin.instance_key() == "lenso.agent.mcp-client/filesystem")
        );

        let configuration = serde_json::json!({
            "transport": "stdio",
            "program": "/usr/bin/env",
            "arguments": ["node", "/opt/mcp/filesystem.js", "/workspace"],
            "working_directory": "/workspace",
            "environment_allowlist": ["PATH", "HOME"],
            "protocol": "auto",
            "tool_namespace": "filesystem",
            "startup_timeout_ms": 5_000,
            "request_timeout_ms": 30_000
        });
        let root = PluginRootSnapshot::new(
            [],
            [lenso_app_plan::authoring::PluginRootInstance::new(
                "lenso.agent.mcp-client",
                "filesystem",
            )
            .with_configuration(configuration.clone())],
            [],
        );
        let plan = resolve_host_plan(&root).unwrap();
        let selected = plan
            .plugin_instances()
            .iter()
            .find(|plugin| plugin.instance_key() == "lenso.agent.mcp-client/filesystem")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(selected.configuration()).unwrap(),
            configuration
        );
        let plan_json = serde_json::to_value(&plan).unwrap();
        assert!(
            plan_json["capability_bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|binding| {
                    binding["provider_instance"] == "lenso.agent.mcp-client/filesystem"
                        && binding["capability_id"] == "lenso.agent.tool-provider@2"
                })
        );
    }
}

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::sync::oneshot;

use lenso_app_plan::{
    RequestAdmissionPlan, ResolvedAppPlan,
    authoring::{
        HostBinding, HostCatalog, HostDefaultPlugin, HostPluginConfiguration, HostSlot,
        PluginInstanceId, PluginRootSnapshot, resolve_plugin_root,
    },
};
use lenso_capability_agent::{Agent, AgentJsonCodec};
use lenso_capability_agent_http_fetch::HttpFetchJsonCodec;
use lenso_capability_agent_model::ModelJsonCodec;
use lenso_capability_agent_prompt::PromptJsonCodec;
use lenso_capability_agent_session::SessionJsonCodec;
use lenso_capability_agent_tool_hook::ToolHookJsonCodec;
use lenso_capability_agent_tool_provider::ToolProviderJsonCodec;
use lenso_capability_agent_tools::ToolsJsonCodec;
use lenso_capability_agent_tui_contribution::{
    SNAPSHOT_OPERATION, SnapshotRequest, SnapshotResponsePanelsItem, TuiContribution,
    validate_snapshot_panels,
};
use lenso_capability_agent_tui_suggestion::{
    SNAPSHOT_OPERATION as SUGGESTION_SNAPSHOT_OPERATION,
    SnapshotRequest as SuggestionSnapshotRequest, Suggestion, TuiSuggestion,
    validate_snapshot_suggestions,
};
use lenso_capability_agent_workspace_read::WorkspaceReadJsonCodec;
use lenso_kernel::{
    CancellationToken, ExecutionAdapterCatalog, InvocationContext, NativeApp, NativeStreamHandle,
};
use lenso_native_adapter::NativePluginRegistry;
use lenso_plugin_control_plane::{
    AdapterProfile, AppGenerationSpec, AppGenerationTransitionSpec, CanonicalDocument,
    CatalogFactory, ControlHealth, ControlLifecycle, ControlPlaneError, ControlStateStore,
    DurableControlState, DurableGenerationRoute, DurableGenerationSupervisor,
    DurableTransitionOutcome, EmbeddedPlugin, FileControlStateStore, GenerationController,
    GenerationControllerClient, GenerationControllerEvent, GenerationMaintenanceOutcome,
    HostBuildManifest, HostExecutionPolicy, KernelGenerationRuntime, MultiExecutionCatalogFactory,
    PlanGenerationInput, ReplacementMode, ResolvedGeneration, RolloutPolicy,
    resolve_plan_generation, sha256_digest,
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

const APP_ID: &str = "lenso.agent.harness";
const GENERATION_SPEC_DIGEST_EXTENSION: &str = "lenso.app.generation-spec-digest@1";
const DEFAULT_MODEL: &str = "gpt-5.6-luna";
const NATIVE_EXECUTION_CLASS: &str = "lenso.native-rust@1";
const QUICKJS_EXECUTION_CLASS: &str = "lenso.quickjs@1";
const PROCESS_EXECUTION_CLASS: &str = "lenso.process@1";
const WASM_EXECUTION_CLASS: &str = "lenso.wasm-component@1";
// Wasm component instantiation can legitimately cross ten seconds on a busy developer machine.
// Keep the gate bounded while avoiding spurious install and rollback failures under local load.
const READY_TIMEOUT_NANOS: u64 = 30_000_000_000;
const DRAIN_TIMEOUT_NANOS: u64 = 2_000_000_000;
const ONLINE_DRAIN_TIMEOUT_NANOS: u64 = 300_000_000_000;
const ONLINE_ROLLBACK_WINDOW_NANOS: u64 = 1_000_000_000;
const GENERATION_DIRECTORY: &str = "generations";
const CONTROL_DIRECTORY: &str = "generation-control";
const TUI_CONTROL_DIRECTORY: &str = "tui-generation-control";
const TELEGRAM_CONTROL_DIRECTORY: &str = "telegram-generation-control";
const DISCORD_CONTROL_DIRECTORY: &str = "discord-generation-control";
const CHANNEL_CONTROL_DIRECTORY: &str = "channel-generation-control";
const MAINTENANCE_INTERVAL: Duration = Duration::from_millis(10);
const RECONCILE_QUIET_PERIOD: Duration = Duration::from_millis(200);
const RECONCILE_SETTLE_LIMIT: Duration = Duration::from_secs(2);
const RECONCILE_CONSISTENCY_INTERVAL: Duration = Duration::from_secs(2);
const MAX_RECONCILE_EVENTS: usize = 32;
const TUI_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(2);
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
        Ok(ExecutionAdapterCatalog::single(
            registry.with_resources(generation.resources.clone()),
        ))
    }
}

#[derive(Clone, Debug)]
struct HostBuildIdentity {
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
    fn current() -> Result<Self, String> {
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

async fn recover_or_open_supervisor<F: CatalogFactory>(
    plan_bytes: &[u8],
    store_root: &Path,
    host_build: &HostBuildIdentity,
    runtime: KernelGenerationRuntime<F>,
    store: FileControlStateStore,
    durable: DurableControlState,
) -> Result<DurableGenerationSupervisor<KernelGenerationRuntime<F>, FileControlStateStore>, String>
{
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
        return DurableGenerationSupervisor::open(APP_ID, runtime, store).map_err(control_error);
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
        return DurableGenerationSupervisor::recover(
            APP_ID,
            runtime,
            store,
            &recoverable,
            now_unix_nanos()?,
        )
        .await
        .map_err(control_error);
    }
    if durable.host_suspended {
        return DurableGenerationSupervisor::replace_suspended_host(APP_ID, runtime, store)
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
    DurableGenerationSupervisor::replace_suspended_host(APP_ID, runtime, store)
        .map_err(control_error)
}

#[derive(Debug)]
pub struct AgentApp {
    client: GenerationControllerClient<NativeApp>,
    controller: Option<tokio::task::JoinHandle<Result<DurableControlState, ControlPlaneError>>>,
    reconciler: Option<GenerationReconciler>,
    reconcile_events: Rc<RefCell<VecDeque<OnlineGenerationEvent>>>,
    host_lease: Option<crate::authority::AuthorityFence>,
    generation_gc_lease: Option<crate::authority::AuthorityFence>,
}

impl AgentApp {
    pub async fn start(plan_bytes: &[u8]) -> Result<Self, String> {
        Self::start_with_profile(plan_bytes, None).await
    }

    pub async fn start_with_profile(
        plan_bytes: &[u8],
        profile_name: Option<String>,
    ) -> Result<Self, String> {
        Self::start_with_store_and_profile(plan_bytes, Path::new(".lenso/runtime"), profile_name)
            .await
    }

    pub async fn start_tui(plan_bytes: &[u8]) -> Result<Self, String> {
        Self::start_tui_with_profile(plan_bytes, None).await
    }

    pub async fn start_tui_with_profile(
        plan_bytes: &[u8],
        profile_name: Option<String>,
    ) -> Result<Self, String> {
        Self::start_tui_with_store_and_profile(
            plan_bytes,
            Path::new(".lenso/runtime"),
            profile_name,
        )
        .await
    }

    /// Starts the Telegram surface with an independent durable Controller lineage.
    pub async fn start_telegram(plan_bytes: &[u8]) -> Result<Self, String> {
        Self::start_with_store_and_control_directory(
            plan_bytes,
            Path::new(".lenso/runtime"),
            TELEGRAM_CONTROL_DIRECTORY,
        )
        .await
    }

    /// Starts the Discord surface with an independent durable Controller lineage.
    pub async fn start_discord(plan_bytes: &[u8]) -> Result<Self, String> {
        Self::start_with_store_and_control_directory(
            plan_bytes,
            Path::new(".lenso/runtime"),
            DISCORD_CONTROL_DIRECTORY,
        )
        .await
    }

    /// Starts all configured messaging surfaces in one durable Controller lineage.
    pub async fn start_channels(plan_bytes: &[u8]) -> Result<Self, String> {
        Self::start_with_store_and_control_directory(
            plan_bytes,
            Path::new(".lenso/runtime"),
            CHANNEL_CONTROL_DIRECTORY,
        )
        .await
    }

    async fn start_with_store_and_profile(
        plan_bytes: &[u8],
        store_root: &Path,
        profile_name: Option<String>,
    ) -> Result<Self, String> {
        Self::start_with_store_control_directory_profile_and_host_build(
            plan_bytes,
            store_root,
            CONTROL_DIRECTORY,
            profile_name,
            HostBuildIdentity::current()?,
        )
        .await
    }

    async fn start_tui_with_store_and_profile(
        plan_bytes: &[u8],
        store_root: &Path,
        profile_name: Option<String>,
    ) -> Result<Self, String> {
        Self::start_with_store_control_directory_profile_and_host_build(
            plan_bytes,
            store_root,
            TUI_CONTROL_DIRECTORY,
            profile_name,
            HostBuildIdentity::current()?,
        )
        .await
    }

    async fn start_with_store_and_control_directory(
        plan_bytes: &[u8],
        store_root: &Path,
        control_directory: &str,
    ) -> Result<Self, String> {
        let host_build = HostBuildIdentity::current()?;
        Self::start_with_store_control_directory_and_host_build(
            plan_bytes,
            store_root,
            control_directory,
            host_build,
        )
        .await
    }

    async fn start_with_store_control_directory_and_host_build(
        plan_bytes: &[u8],
        store_root: &Path,
        control_directory: &str,
        host_build: HostBuildIdentity,
    ) -> Result<Self, String> {
        Self::start_with_store_control_directory_profile_and_host_build(
            plan_bytes,
            store_root,
            control_directory,
            None,
            host_build,
        )
        .await
    }

    async fn start_with_store_control_directory_profile_and_host_build(
        plan_bytes: &[u8],
        store_root: &Path,
        control_directory: &str,
        profile_name: Option<String>,
        host_build: HostBuildIdentity,
    ) -> Result<Self, String> {
        let authority = crate::authority::AuthorityCoordinator::prepare(store_root)?;
        let generation_gc_lease = authority.generation_gc_snapshot()?;
        let host_lease = authority.host_lease(control_directory)?;
        let _authority_fence = authority.snapshot()?;
        let (generation, _resolution_authority_digest) =
            resolve_and_record_current_generation(plan_bytes, store_root, &host_build)?;
        let store = FileControlStateStore::open(store_root.join(control_directory))
            .map_err(control_error)?;
        let durable = store.load(APP_ID).map_err(control_error)?;
        let runtime = KernelGenerationRuntime::new(harness_catalog_factory());
        let supervisor = recover_or_open_supervisor(
            plan_bytes,
            store_root,
            &host_build,
            runtime,
            store,
            durable,
        )
        .await?;
        let recovered_active = supervisor.state().active_generation_spec_digest.clone();
        let (controller, client) =
            GenerationController::new(supervisor, MAINTENANCE_INTERVAL).map_err(control_error)?;
        let task = tokio::task::spawn_local(controller.run());
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
            if let Err(error) = client
                .transition(transition, generation, BTreeMap::new())
                .await
            {
                drop(client);
                let _ = task.await;
                return Err(control_error(error));
            }
        }
        let reconcile_events = Rc::new(RefCell::new(VecDeque::new()));
        let authoring_managed = plan_is_authoring_managed(plan_bytes, profile_name.as_deref());
        let reconciler = start_generation_reconciler(
            client.clone(),
            store_root.to_path_buf(),
            host_build,
            profile_name,
            authoring_managed,
            reconcile_events.clone(),
        );
        Ok(Self {
            client,
            controller: Some(task),
            reconciler: Some(reconciler),
            reconcile_events,
            host_lease: Some(host_lease),
            generation_gc_lease: Some(generation_gc_lease),
        })
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

    async fn lease_turn_for(&self, consumer_instance: &str) -> Result<TurnGeneration, String> {
        let route = self.client.route().await.map_err(control_error)?;
        let consumer_instance = match consumer_instance {
            "cli" => "lenso.agent.cli/cli",
            "tui" => "lenso.agent.tui/tui",
            "telegram" => "lenso.agent.telegram/telegram",
            "discord" => "lenso.agent.discord/discord",
            other => return Err(format!("unknown Agent surface `{other}`")),
        };
        let handle = Rc::new(
            route
                .target()
                .stream_handle::<Agent>(consumer_instance)
                .map_err(|error| format!("leased Generation has no Agent route: {error:?}"))?,
        );
        Ok(TurnGeneration { route, handle })
    }

    /// Snapshots every TUI panel provider in deterministic resolved order.
    pub async fn tui_panels(&self) -> Result<Vec<SnapshotResponsePanelsItem>, String> {
        let route = self.client.route().await.map_err(control_error)?;
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
        let route = self.client.route().await.map_err(control_error)?;
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
        let expected = self.client.suspend().await.map_err(control_error)?;
        let task = self
            .controller
            .take()
            .ok_or_else(|| "Generation Controller is already stopped".to_owned())?;
        let actual = task
            .await
            .map_err(|error| format!("Generation Controller task failed: {error}"))?
            .map_err(control_error)?;
        if actual != expected {
            return Err("Generation Controller returned inconsistent durable state".to_owned());
        }
        self.host_lease.take();
        self.generation_gc_lease.take();
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
) -> Result<Vec<(&'static str, DurableControlState)>, String> {
    let mut states = Vec::new();
    for (surface, directory) in [
        ("headless", CONTROL_DIRECTORY),
        ("tui", TUI_CONTROL_DIRECTORY),
        ("telegram", TELEGRAM_CONTROL_DIRECTORY),
        ("discord", DISCORD_CONTROL_DIRECTORY),
        ("channels", CHANNEL_CONTROL_DIRECTORY),
    ] {
        let path = store_root.join(directory);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "failed to inspect `{surface}` Generation Controller: {error}"
                ));
            }
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "`{surface}` Generation Controller state is not a regular directory"
            ));
        }
        let store = FileControlStateStore::open(path).map_err(control_error)?;
        let state = store.load(APP_ID).map_err(control_error)?;
        states.push((surface, state));
    }
    Ok(states)
}

fn resolve_and_record_current_generation(
    plan_bytes: &[u8],
    store_root: &Path,
    host_build: &HostBuildIdentity,
) -> Result<(ResolvedGeneration, String), String> {
    let authority = crate::generation_authority::load_generation_authority_unfenced(store_root);
    let generation = resolve_generation_with_authority(
        plan_bytes,
        &authority,
        host_build,
        &crate::plugin_root_path(),
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
    let plugin_root = crate::plugin_root_path();
    let plugin_parent = watch_parent(&plugin_root);
    let profile_directory = crate::profile::directory();
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

fn plan_is_authoring_managed(plan_bytes: &[u8], profile_name: Option<&str>) -> bool {
    let Ok(root) = crate::plugin_root::snapshot(&crate::plugin_root_path()) else {
        return false;
    };
    let resolved = if let Some(profile_name) = profile_name {
        crate::profile::select(profile_name, &root)
            .and_then(|profile| resolve_host_plan_for_agent(profile.root(), profile.agent()))
    } else {
        resolve_host_plan(&root)
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
    let authority = crate::generation_authority::load_generation_authority_unfenced(store_root);
    let rejected = |detail| OnlineGenerationEvent::Rejected {
        resolution_authority_digest: Some(authority.resolution_authority_digest.clone()),
        detail,
    };
    let root = crate::plugin_root::snapshot(plugin_root).map_err(rejected)?;
    let plan = if let Some(profile_name) = profile_name {
        let profile = crate::profile::select(profile_name, &root).map_err(rejected)?;
        resolve_host_plan_for_agent(profile.root(), profile.agent()).map_err(rejected)?
    } else {
        resolve_host_plan(&root).map_err(rejected)?
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
    route: DurableGenerationRoute<NativeApp>,
    handle: Rc<NativeStreamHandle<Agent>>,
}

impl TurnGeneration {
    pub fn handle(&self) -> &NativeStreamHandle<Agent> {
        &self.handle
    }

    pub fn invocation_context(&self) -> Result<InvocationContext, String> {
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
        InvocationContext::new(request_id, None, CancellationToken::new())
            .with_extension(
                GENERATION_SPEC_DIGEST_EXTENSION,
                self.generation_spec_digest().as_bytes().to_vec(),
            )
            .map_err(|error| format!("failed to attach Generation provenance: {error}"))
    }

    fn generation_spec_digest(&self) -> &str {
        self.route.generation_spec_digest()
    }
}

fn record_generation_spec(
    store_root: &Path,
    spec: &CanonicalDocument<AppGenerationSpec>,
) -> Result<(), String> {
    let digest = spec
        .digest()
        .strip_prefix("sha256:")
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "Generation Spec digest is not canonical SHA-256".to_owned())?;
    let directory = store_root.join(GENERATION_DIRECTORY);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("failed to create Generation provenance directory: {error}"))?;
    let metadata = fs::symlink_metadata(&directory)
        .map_err(|error| format!("failed to inspect Generation provenance directory: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Generation provenance path is not a regular directory".to_owned());
    }

    let destination = directory.join(format!("{digest}.json"));
    match fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err("Generation provenance record is not a regular file".to_owned());
            }
            let existing = fs::read(&destination)
                .map_err(|error| format!("failed to read Generation provenance: {error}"))?;
            if existing != spec.bytes() {
                return Err("Generation provenance record does not match its digest".to_owned());
            }
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "failed to inspect Generation provenance record: {error}"
            ));
        }
    }

    let temporary = directory.join(format!(".{digest}.{}.tmp", uuid::Uuid::new_v4()));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("failed to create Generation provenance: {error}"))?;
        file.write_all(spec.bytes())
            .map_err(|error| format!("failed to write Generation provenance: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("failed to sync Generation provenance: {error}"))?;
        fs::rename(&temporary, &destination)
            .map_err(|error| format!("failed to commit Generation provenance: {error}"))?;
        OpenOptions::new()
            .read(true)
            .open(&directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("failed to sync Generation provenance directory: {error}"))
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    write_result
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
    let authority = crate::generation_authority::load_generation_authority(store_root)?;
    resolve_generation_with_authority(
        plan_bytes,
        &authority,
        host_build,
        &crate::plugin_root_path(),
    )
}

fn resolve_retained_generations(
    plan_bytes: &[u8],
    store_root: &Path,
    host_build: &HostBuildIdentity,
) -> Result<BTreeMap<String, ResolvedGeneration>, String> {
    crate::generation_authority::recovery_generation_authorities(store_root)
        .into_iter()
        .map(|authority| {
            let generation = resolve_generation_with_authority(
                plan_bytes,
                &authority,
                host_build,
                &crate::plugin_root_path(),
            )?;
            Ok((generation.spec.digest().to_owned(), generation))
        })
        .collect()
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
    linked_host_catalog_for_agent(&PluginInstanceId::new("lenso.agent.loop", "agent"))
}

fn linked_host_catalog_for_agent(root_agent: &PluginInstanceId) -> Result<HostCatalog, String> {
    let registry = NativePluginRegistry::new().with_linked_factories();
    let available = registry
        .factories()
        .map(|factory| factory.package_id().to_owned())
        .collect::<BTreeSet<_>>();
    let defaults = host_catalog_defaults()
        .into_iter()
        .filter(|plugin| available.contains(plugin.id().plugin_id()))
        .collect::<Vec<_>>();
    let configurations = host_catalog_configurations()
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
        HostSlot::many("surfaces"),
        HostSlot::many("tool-providers"),
        HostSlot::many("tool-hooks"),
        HostSlot::one("http-fetch"),
        HostSlot::one("model").replaceable(),
        HostSlot::optional("process"),
        HostSlot::many("prompt-providers"),
        HostSlot::one("prompt-runtime"),
        HostSlot::one("root-tools-runtime"),
        HostSlot::optional("secrets"),
        HostSlot::one("session"),
        HostSlot::one("restricted-tools-runtime"),
        HostSlot::many("tui-contributions"),
        HostSlot::many("tui-suggestions"),
        HostSlot::one("workspace-import-read"),
    ]
}

fn host_catalog_defaults() -> Vec<HostDefaultPlugin> {
    let mut defaults = agent_defaults();
    defaults.extend(default_interactive_plugins());
    defaults.extend([
        HostDefaultPlugin::new("lenso.agent.cli", "cli"),
        HostDefaultPlugin::new("lenso.agent.discord", "discord"),
        default_plugin(
            "lenso.agent.http-fetch",
            "http-fetch",
            serde_json::json!({
                "allowed_origins": [], "timeout_ms": 30000
            }),
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
            "lenso.agent.session.file",
            "sessions",
            serde_json::json!({
                "directory": ".lenso/sessions"
            }),
        ),
        default_skills_plugin(),
        HostDefaultPlugin::new("lenso.agent.telegram", "telegram"),
        HostDefaultPlugin::new("lenso.agent.tools", "tools"),
        HostDefaultPlugin::new("lenso.agent.tui", "tui"),
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
                    {"id": "agent.command.new", "label": "/new", "insert_text": "/new", "description": "Start a new session"}
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

fn agent_defaults() -> Vec<HostDefaultPlugin> {
    ["agent", "subagent-agent"]
        .into_iter()
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
                    "max_history_events": 200
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

fn host_catalog_configurations() -> Vec<HostPluginConfiguration> {
    let mut configurations = local_tool_configurations();
    configurations.extend(model_and_auth_configurations());
    configurations
}

fn model_and_auth_configurations() -> Vec<HostPluginConfiguration> {
    vec![
        host_plugin_configuration(
            "lenso.agent.auth.openai-codex",
            serde_json::json!({
                "issuer": "https://auth.openai.com",
                "profile": "default",
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
        host_plugin_configuration(
            "lenso.secrets.env",
            serde_json::json!({
                "references": {"model/openai-api-key": "OPENAI_API_KEY"}
            }),
        ),
    ]
}

fn local_tool_configurations() -> Vec<HostPluginConfiguration> {
    vec![
        host_plugin_configuration(
            "lenso.agent.approval-hook",
            serde_json::json!({
                "allow_tools": ["read_text"],
                "ask_tools": [],
                "default_decision": "ask",
                "deny_tools": [],
                "directory": ".lenso/approvals",
                "max_records": 10_000
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
            "lenso.agent.process.native",
            serde_json::json!({
                "allowed_programs": ["cargo", "git", "rg"],
                "environment_allowlist": [
                    "PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "TMPDIR", "LANG", "LC_ALL"
                ],
                "max_argument_bytes": 131_072,
                "max_output_bytes": 262_144,
                "max_timeout_ms": 600_000,
                "root": "."
            }),
        ),
        host_plugin_configuration(
            "lenso.agent.process-tools",
            serde_json::json!({"default_timeout_ms": 120_000}),
        ),
        host_plugin_configuration(
            "lenso.agent.subagent-tools",
            serde_json::json!({
                "max_output_bytes": 1_048_576,
                "max_task_bytes": 262_144
            }),
        ),
        host_plugin_configuration(
            "lenso.agent.workspace-edit",
            serde_json::json!({
                "max_edit_bytes": 131_072,
                "max_file_bytes": 1_048_576,
                "root": "."
            }),
        ),
    ]
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
    let child_agent = PluginInstanceId::new("lenso.agent.loop", "subagent-agent");
    let tool_admission = RequestAdmissionPlan::new(0, 4);
    let mut bindings = vec![
        HostBinding::to_instance(root_agent.clone(), "lenso.agent.tools@2", root_tools)
            .with_admission(tool_admission),
        HostBinding::to_instance(
            child_agent.clone(),
            "lenso.agent.tools@2",
            restricted_tools.clone(),
        )
        .with_admission(tool_admission),
    ];
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
    if available.contains("lenso.agent.code-mode-tools") {
        bindings.push(HostBinding::to_instance(
            PluginInstanceId::new("lenso.agent.code-mode-tools", "default"),
            "lenso.agent.tools@2",
            restricted_tools,
        ));
    }
    if available.contains("lenso.agent.subagent-tools") {
        bindings.push(HostBinding::to_instance(
            PluginInstanceId::new("lenso.agent.subagent-tools", "default"),
            "lenso.agent@3",
            child_agent,
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

pub(crate) fn resolve_host_plan(root: &PluginRootSnapshot) -> Result<ResolvedAppPlan, String> {
    let host = linked_host_catalog()?;
    resolve_plugin_root(&host, root)
        .map(|app| app.plan().clone())
        .map_err(|error| format!("failed to resolve Host Plugins: {error}"))
}

pub(crate) fn resolve_host_plan_for_agent(
    root: &PluginRootSnapshot,
    agent: &PluginInstanceId,
) -> Result<ResolvedAppPlan, String> {
    let host = linked_host_catalog_for_agent(agent)?;
    resolve_plugin_root(&host, root)
        .map(|app| app.plan().clone())
        .map_err(|error| format!("failed to resolve Host Plugins for Agent `{agent}`: {error}"))
}

fn harness_catalog_factory() -> MultiExecutionCatalogFactory<HarnessCatalogFactory> {
    MultiExecutionCatalogFactory::new(HarnessCatalogFactory)
        .with_wasm_codec(AgentJsonCodec)
        .with_wasm_codec(HttpFetchJsonCodec)
        .with_wasm_codec(ModelJsonCodec)
        .with_wasm_codec(PromptJsonCodec)
        .with_wasm_codec(SessionJsonCodec)
        .with_wasm_codec(ToolHookJsonCodec)
        .with_wasm_codec(ToolProviderJsonCodec)
        .with_wasm_codec(ToolsJsonCodec)
        .with_wasm_codec(WorkspaceReadJsonCodec)
        .with_quickjs_codec(AgentJsonCodec)
        .with_quickjs_codec(ModelJsonCodec)
        .with_quickjs_codec(PromptJsonCodec)
        .with_quickjs_codec(SessionJsonCodec)
        .with_quickjs_codec(ToolHookJsonCodec)
        .with_quickjs_codec(ToolsJsonCodec)
        .with_quickjs_codec(WorkspaceReadJsonCodec)
        .with_process_codec(AgentJsonCodec)
        .with_process_codec(HttpFetchJsonCodec)
        .with_process_codec(ModelJsonCodec)
        .with_process_codec(PromptJsonCodec)
        .with_process_codec(SessionJsonCodec)
        .with_process_codec(ToolHookJsonCodec)
        .with_process_codec(ToolProviderJsonCodec)
        .with_process_codec(ToolsJsonCodec)
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
        let instances = plan
            .plugin_instances()
            .iter()
            .map(lenso_app_plan::PluginInstancePlan::instance_key)
            .collect::<BTreeSet<_>>();

        assert!(instances.contains("lenso.agent.auth.openai-codex/auth"));
        assert!(instances.contains("lenso.agent.model.openai-codex-direct/model"));
        assert!(instances.contains("lenso.agent.skills.filesystem/skills"));
        assert!(!instances.contains("lenso.agent.model.fixture/model"));
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
                        "max_history_events": 100
                    })),
                lenso_app_plan::authoring::PluginRootInstance::new(
                    "lenso.agent.model.fixture",
                    "game-model",
                )
                .with_configuration(serde_json::json!({
                    "model": "fixture/readme-summary-v1"
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
        assert_eq!(
            plugin.configuration(),
            r#"{"max_edit_bytes":131072,"max_file_bytes":1048576,"root":"."}"#
        );
    }
}

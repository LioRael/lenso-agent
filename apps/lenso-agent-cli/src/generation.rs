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

use lenso_agent_approval_hook_module as _;
use lenso_agent_auth_openai_codex_module as _;
use lenso_agent_cli_module as _;
use lenso_agent_code_mode_tools_module as _;
use lenso_agent_discord_module as _;
use lenso_agent_http_fetch_module as _;
use lenso_agent_loop_module::GENERATION_SPEC_DIGEST_EXTENSION;
use lenso_agent_model_fixture_module as _;
use lenso_agent_model_openai_codex_direct_module as _;
use lenso_agent_model_openai_compatible_module as _;
use lenso_agent_process_native_module as _;
use lenso_agent_process_tools_module as _;
use lenso_agent_prompt_filesystem_module as _;
use lenso_agent_prompt_module as _;
use lenso_agent_prompt_static_module as _;
use lenso_agent_session_file_module as _;
use lenso_agent_skills_filesystem_module as _;
use lenso_agent_subagent_tools_module as _;
use lenso_agent_telegram_module as _;
use lenso_agent_tools_module as _;
use lenso_agent_tui_command_suggestions_module as _;
use lenso_agent_tui_module as _;
use lenso_agent_tui_static_module as _;
use lenso_agent_tui_workspace_suggestions_module as _;
use lenso_agent_workspace_edit_module as _;
use lenso_agent_workspace_import_read_module as _;
use lenso_agent_workspace_read_module as _;
use lenso_agent_workspace_read_tools_module as _;
use lenso_app_plan::{ResolvedAppPlan, authoring::ModuleCatalog};
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
use lenso_native_adapter::NativeModuleRegistry;
use lenso_plugin_control_plane::{
    AdapterProfile, AppGenerationSpec, AppGenerationTransitionSpec, BuiltInModule,
    CanonicalDocument, CatalogFactory, ClassPolicy, ControlHealth, ControlLifecycle,
    ControlPlaneError, ControlStateStore, DurableControlState, DurableGenerationRoute,
    DurableGenerationSupervisor, DurableTransitionOutcome, FileControlStateStore,
    GenerationController, GenerationControllerClient, GenerationControllerEvent,
    GenerationMaintenanceOutcome, HostBuildManifest, HostExecutionPolicy, KernelGenerationRuntime,
    MemoryControlStateStore, MultiExecutionCatalogFactory, ReplacementMode, ResolutionInput,
    ResolvedGeneration, RolloutPolicy, resolve_generation, sha256_digest,
};
use lenso_secrets_env_module as _;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::plugin_profiles::{
    NATIVE_EXECUTION_CLASS, QUICKJS_EXECUTION_CLASS, WASM_EXECUTION_CLASS, harness_plugin_profiles,
};

const APP_ID: &str = "lenso.agent.harness";
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
        _generation: &ResolvedGeneration,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        let (registry, _) = native_host_build();
        Ok(ExecutionAdapterCatalog::single(registry))
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
        active_set_digest: String,
        generation_spec_digest: String,
        previous_generation_spec_digest: String,
        routing_epoch: u64,
    },
    Rejected {
        active_set_digest: Option<String>,
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
        if let Some(snapshot) = crate::plugins::current_source_generation_snapshot()? {
            return Self::start_with_source_snapshot(plan_bytes, snapshot).await;
        }
        Self::start_with_store(plan_bytes, Path::new(".lenso/plugins")).await
    }

    pub async fn start_tui(plan_bytes: &[u8]) -> Result<Self, String> {
        if let Some(snapshot) = crate::plugins::current_source_generation_snapshot()? {
            return Self::start_with_durable_source_snapshot(
                plan_bytes,
                snapshot,
                TUI_CONTROL_DIRECTORY,
            )
            .await;
        }
        Self::start_tui_with_store(plan_bytes, Path::new(".lenso/plugins")).await
    }

    /// Starts the Telegram surface with an independent durable Controller lineage.
    pub async fn start_telegram(plan_bytes: &[u8]) -> Result<Self, String> {
        if let Some(snapshot) = crate::plugins::current_source_generation_snapshot()? {
            return Self::start_with_durable_source_snapshot(
                plan_bytes,
                snapshot,
                TELEGRAM_CONTROL_DIRECTORY,
            )
            .await;
        }
        Self::start_with_store_and_control_directory(
            plan_bytes,
            Path::new(".lenso/plugins"),
            TELEGRAM_CONTROL_DIRECTORY,
        )
        .await
    }

    /// Starts the Discord surface with an independent durable Controller lineage.
    pub async fn start_discord(plan_bytes: &[u8]) -> Result<Self, String> {
        if let Some(snapshot) = crate::plugins::current_source_generation_snapshot()? {
            return Self::start_with_durable_source_snapshot(
                plan_bytes,
                snapshot,
                DISCORD_CONTROL_DIRECTORY,
            )
            .await;
        }
        Self::start_with_store_and_control_directory(
            plan_bytes,
            Path::new(".lenso/plugins"),
            DISCORD_CONTROL_DIRECTORY,
        )
        .await
    }

    /// Starts all configured messaging surfaces in one durable Controller lineage.
    pub async fn start_channels(plan_bytes: &[u8]) -> Result<Self, String> {
        if let Some(snapshot) = crate::plugins::current_source_generation_snapshot()? {
            return Self::start_with_durable_source_snapshot(
                plan_bytes,
                snapshot,
                CHANNEL_CONTROL_DIRECTORY,
            )
            .await;
        }
        Self::start_with_store_and_control_directory(
            plan_bytes,
            Path::new(".lenso/plugins"),
            CHANNEL_CONTROL_DIRECTORY,
        )
        .await
    }

    pub(crate) async fn start_with_store(
        plan_bytes: &[u8],
        store_root: &Path,
    ) -> Result<Self, String> {
        Self::start_with_store_and_control_directory(plan_bytes, store_root, CONTROL_DIRECTORY)
            .await
    }

    async fn start_tui_with_store(plan_bytes: &[u8], store_root: &Path) -> Result<Self, String> {
        Self::start_with_store_and_control_directory(plan_bytes, store_root, TUI_CONTROL_DIRECTORY)
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

    async fn start_with_source_snapshot(
        plan_bytes: &[u8],
        snapshot: crate::plugins::SourceGenerationSnapshot,
    ) -> Result<Self, String> {
        let generation_gc_lease = if snapshot.store_root.exists() {
            Some(
                crate::authority::AuthorityCoordinator::prepare(&snapshot.store_root)?
                    .generation_gc_snapshot()?,
            )
        } else {
            None
        };
        let host_build = HostBuildIdentity::current()?;
        let generation =
            resolve_generation_with_authority(plan_bytes, &snapshot.authority, &host_build)?;
        let runtime = KernelGenerationRuntime::new(harness_catalog_factory());
        let supervisor =
            DurableGenerationSupervisor::open(APP_ID, runtime, MemoryControlStateStore::default())
                .map_err(control_error)?;
        let (controller, client) =
            GenerationController::new(supervisor, MAINTENANCE_INTERVAL).map_err(control_error)?;
        let task = tokio::task::spawn_local(controller.run());
        let transition = initial_transition(&generation).map_err(control_error)?;
        if let Err(error) = client
            .transition(transition, generation, BTreeMap::new())
            .await
        {
            drop(client);
            let _ = task.await;
            return Err(control_error(error));
        }
        let reconcile_events = Rc::new(RefCell::new(VecDeque::new()));
        if !snapshot.blocked.is_empty() {
            push_reconcile_event(
                &reconcile_events,
                OnlineGenerationEvent::Rejected {
                    active_set_digest: Some(snapshot.authority.active_set_digest.clone()),
                    detail: blocked_discovery_detail(&snapshot.blocked),
                },
            );
        }
        let reconciler = start_source_generation_reconciler(
            client.clone(),
            plan_bytes.to_vec(),
            snapshot.definition_path,
            snapshot.store_root,
            host_build,
            snapshot.desired_state_digest,
            reconcile_events.clone(),
        );
        Ok(Self {
            client,
            controller: Some(task),
            reconciler: Some(reconciler),
            reconcile_events,
            host_lease: None,
            generation_gc_lease,
        })
    }

    async fn start_with_durable_source_snapshot(
        plan_bytes: &[u8],
        snapshot: crate::plugins::SourceGenerationSnapshot,
        control_directory: &str,
    ) -> Result<Self, String> {
        let host_build = HostBuildIdentity::current()?;
        Self::start_with_durable_source_snapshot_and_host_build(
            plan_bytes,
            snapshot,
            control_directory,
            host_build,
        )
        .await
    }

    async fn start_with_durable_source_snapshot_and_host_build(
        plan_bytes: &[u8],
        snapshot: crate::plugins::SourceGenerationSnapshot,
        control_directory: &str,
        host_build: HostBuildIdentity,
    ) -> Result<Self, String> {
        let coordinator = crate::authority::AuthorityCoordinator::prepare(&snapshot.store_root)?;
        let generation_gc_lease = coordinator.generation_gc_snapshot()?;
        let host_lease = coordinator.host_lease(control_directory)?;
        let _authority_fence = coordinator.snapshot()?;
        let generation =
            resolve_generation_with_authority(plan_bytes, &snapshot.authority, &host_build)?;
        record_generation_spec(&snapshot.store_root, &generation.spec)?;
        crate::plugins::record_resolved_generation_authority_unfenced(
            &snapshot.store_root,
            &snapshot.authority,
        )?;
        let store = FileControlStateStore::open(snapshot.store_root.join(control_directory))
            .map_err(control_error)?;
        let durable = store.load(APP_ID).map_err(control_error)?;
        let runtime = KernelGenerationRuntime::new(harness_catalog_factory());
        let supervisor = recover_or_open_supervisor(
            plan_bytes,
            &snapshot.store_root,
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
                online_overlap_transition(active, &generation).map_err(control_error)?
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
        if !snapshot.blocked.is_empty() {
            push_reconcile_event(
                &reconcile_events,
                OnlineGenerationEvent::Rejected {
                    active_set_digest: Some(snapshot.authority.active_set_digest.clone()),
                    detail: blocked_discovery_detail(&snapshot.blocked),
                },
            );
        }
        let reconciler = start_source_generation_reconciler(
            client.clone(),
            plan_bytes.to_vec(),
            snapshot.definition_path,
            snapshot.store_root,
            host_build,
            snapshot.desired_state_digest,
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

    async fn start_with_store_control_directory_and_host_build(
        plan_bytes: &[u8],
        store_root: &Path,
        control_directory: &str,
        host_build: HostBuildIdentity,
    ) -> Result<Self, String> {
        let authority = crate::authority::AuthorityCoordinator::prepare(store_root)?;
        let generation_gc_lease = authority.generation_gc_snapshot()?;
        let host_lease = authority.host_lease(control_directory)?;
        let _authority_fence = authority.snapshot()?;
        let (generation, active_set_digest) =
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
        let reconciler = start_generation_reconciler(
            client.clone(),
            plan_bytes.to_vec(),
            store_root.to_path_buf(),
            host_build,
            active_set_digest,
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
            .many_handle::<TuiContribution>("tui")
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
            .many_handle::<TuiSuggestion>("tui")
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

pub(crate) fn source_controller_status(store_root: &Path) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    for (surface, state) in existing_controller_states(store_root)? {
        lines.push(format!(
            "controller: {surface} revision={} suspended={} active={}",
            state.revision,
            state.host_suspended,
            state
                .active_generation_spec_digest
                .as_deref()
                .unwrap_or("none")
        ));
        let mut generations = state.generations;
        generations.sort_by(|left, right| {
            left.generation_spec_digest
                .cmp(&right.generation_spec_digest)
        });
        for generation in generations {
            lines.push(format!(
                "generation: {surface} {:?} health={:?} direction={:?} rollback-deadline={} retirement={:?} {}",
                generation.lifecycle,
                generation.health,
                generation.activation_direction,
                generation
                    .rollback_deadline_unix_nanos
                    .as_deref()
                    .unwrap_or("none"),
                generation.retirement_reason,
                generation.generation_spec_digest,
            ));
        }
    }
    Ok(lines)
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
    let authority = crate::plugins::load_generation_authority_unfenced(store_root)?;
    let generation = resolve_generation_with_authority(plan_bytes, &authority, host_build)?;
    record_generation_spec(store_root, &generation.spec)?;
    crate::plugins::record_resolved_generation_authority_unfenced(store_root, &authority)?;
    Ok((generation, authority.active_set_digest))
}

fn start_generation_reconciler(
    client: GenerationControllerClient<NativeApp>,
    plan_bytes: Vec<u8>,
    store_root: PathBuf,
    host_build: HostBuildIdentity,
    initial_active_set_digest: String,
    events: Rc<RefCell<VecDeque<OnlineGenerationEvent>>>,
) -> GenerationReconciler {
    let (stop, mut stopped) = oneshot::channel();
    let mut controller_events = client.subscribe();
    let (mut watcher, watcher_errors) =
        FilesystemReconcileWatcher::start(&[store_root.as_path()], None);
    report_watcher_errors(&events, watcher_errors);
    let task = tokio::task::spawn_local(async move {
        let mut interval = tokio::time::interval(RECONCILE_CONSISTENCY_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_attempted_active_set_digest = Some(initial_active_set_digest);
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
                                active_set_digest: None,
                                detail: format!(
                                    "Generation Controller event stream lagged by {skipped} events"
                                ),
                            });
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = interval.tick() => {
                    if let Some(event) = reconcile_online_generation(
                        &client,
                        &plan_bytes,
                        &store_root,
                        &host_build,
                        &mut last_attempted_active_set_digest,
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
                    if let Some(event) = reconcile_online_generation(
                        &client,
                        &plan_bytes,
                        &store_root,
                        &host_build,
                        &mut last_attempted_active_set_digest,
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

fn start_source_generation_reconciler(
    client: GenerationControllerClient<NativeApp>,
    plan_bytes: Vec<u8>,
    definition_path: PathBuf,
    store_root: PathBuf,
    host_build: HostBuildIdentity,
    initial_desired_state_digest: String,
    events: Rc<RefCell<VecDeque<OnlineGenerationEvent>>>,
) -> GenerationReconciler {
    let (stop, mut stopped) = oneshot::channel();
    let mut controller_events = client.subscribe();
    let app_root = definition_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let discovery_root = app_root.join("plugins");
    let (mut watcher, watcher_errors) = FilesystemReconcileWatcher::start(
        &[app_root.as_path(), store_root.as_path()],
        Some(discovery_root),
    );
    report_watcher_errors(&events, watcher_errors);
    let task = tokio::task::spawn_local(async move {
        let mut interval = tokio::time::interval(RECONCILE_CONSISTENCY_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_attempted_desired_state_digest = Some(initial_desired_state_digest);
        let mut last_event = None::<OnlineGenerationEvent>;
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
                                active_set_digest: None,
                                detail: format!(
                                    "Generation Controller event stream lagged by {skipped} events"
                                ),
                            });
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = interval.tick() => {
                    let errors = watcher.settle_after(None).await;
                    report_watcher_errors(&events, errors);
                    if let Some(error) = watcher.refresh_recursive_watch() {
                        report_watcher_errors(&events, [error]);
                    }
                    for event in reconcile_source_generation(
                        &client,
                        &plan_bytes,
                        &definition_path,
                        &store_root,
                        &host_build,
                        &mut last_attempted_desired_state_digest,
                    ).await {
                        if matches!(event, OnlineGenerationEvent::Switched { .. })
                            || last_event.as_ref() != Some(&event)
                        {
                            push_reconcile_event(&events, event.clone());
                        }
                        last_event = Some(event);
                    }
                }
                signal = watcher.changed() => {
                    let mut errors = watcher.settle_after(signal).await;
                    if let Some(error) = watcher.refresh_recursive_watch() {
                        errors.push(error);
                    }
                    report_watcher_errors(&events, errors);
                    for event in reconcile_source_generation(
                        &client,
                        &plan_bytes,
                        &definition_path,
                        &store_root,
                        &host_build,
                        &mut last_attempted_desired_state_digest,
                    ).await {
                        if matches!(event, OnlineGenerationEvent::Switched { .. })
                            || last_event.as_ref() != Some(&event)
                        {
                            push_reconcile_event(&events, event.clone());
                        }
                        last_event = Some(event);
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
            // A still-live standby from an older edit cannot be used as the
            // rollback target for the current edge. Let bounded maintenance
            // retire it, then retry this exact Desired State as a fresh stage.
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

async fn reconcile_source_generation(
    client: &GenerationControllerClient<NativeApp>,
    plan_bytes: &[u8],
    definition_path: &Path,
    store_root: &Path,
    host_build: &HostBuildIdentity,
    last_attempted_desired_state_digest: &mut Option<String>,
) -> Vec<OnlineGenerationEvent> {
    let rejected = |active_set_digest, detail| OnlineGenerationEvent::Rejected {
        active_set_digest,
        detail,
    };
    let _authority_fence = if store_root.exists() {
        let coordinator = match crate::authority::AuthorityCoordinator::prepare(store_root) {
            Ok(coordinator) => coordinator,
            Err(detail) => return vec![rejected(None, detail)],
        };
        match coordinator.try_snapshot() {
            Ok(Some(fence)) => Some(fence),
            Ok(None) => return Vec::new(),
            Err(detail) => return vec![rejected(None, detail)],
        }
    } else {
        None
    };
    let snapshot = match crate::plugins::source_generation_snapshot_at(definition_path) {
        Ok(snapshot) => snapshot,
        Err(detail) => return vec![rejected(None, detail)],
    };
    if snapshot.store_root != store_root {
        return vec![rejected(
            None,
            "source-backed Plugin authority root changed while the Host was running".to_owned(),
        )];
    }
    if last_attempted_desired_state_digest.as_deref() == Some(&snapshot.desired_state_digest) {
        return Vec::new();
    }
    *last_attempted_desired_state_digest = Some(snapshot.desired_state_digest.clone());
    let active_set_digest = snapshot.authority.active_set_digest.clone();
    let mut events = Vec::new();
    if !snapshot.blocked.is_empty() {
        events.push(rejected(
            Some(active_set_digest.clone()),
            blocked_discovery_detail(&snapshot.blocked),
        ));
    }
    let candidate =
        match resolve_generation_with_authority(plan_bytes, &snapshot.authority, host_build) {
            Ok(candidate) => candidate,
            Err(detail) => {
                events.push(rejected(Some(active_set_digest), detail));
                return events;
            }
        };
    let state = match client.inspect().await.map_err(control_error) {
        Ok(state) => state,
        Err(detail) => {
            events.push(rejected(Some(active_set_digest), detail));
            return events;
        }
    };
    let Some(previous_generation_spec_digest) = state.active_generation_spec_digest.as_deref()
    else {
        events.push(rejected(
            Some(active_set_digest),
            "online reconcile requires one active App Generation".to_owned(),
        ));
        return events;
    };
    if previous_generation_spec_digest == candidate.spec.digest() {
        return events;
    }
    if let Err(detail) = record_generation_spec(store_root, &candidate.spec).and_then(|()| {
        crate::plugins::record_resolved_generation_authority_unfenced(
            store_root,
            &snapshot.authority,
        )
    }) {
        events.push(rejected(Some(active_set_digest), detail));
        return events;
    }
    match activate_online_candidate(client, &state, previous_generation_spec_digest, candidate)
        .await
    {
        Ok(Some(outcome)) => events.push(OnlineGenerationEvent::Switched {
            active_set_digest,
            generation_spec_digest: outcome.active_generation_spec_digest,
            previous_generation_spec_digest: previous_generation_spec_digest.to_owned(),
            routing_epoch: outcome.routing_epoch,
        }),
        Ok(None) => *last_attempted_desired_state_digest = None,
        Err(detail) => events.push(rejected(Some(active_set_digest), detail)),
    }
    events
}

fn blocked_discovery_detail(blocked: &[crate::plugins::BlockedDiscoveredPlugin]) -> String {
    blocked
        .iter()
        .map(|blocked| format!("{}: {}", blocked.entry, blocked.detail))
        .collect::<Vec<_>>()
        .join("; ")
}

async fn reconcile_online_generation(
    client: &GenerationControllerClient<NativeApp>,
    plan_bytes: &[u8],
    store_root: &Path,
    host_build: &HostBuildIdentity,
    last_attempted_active_set_digest: &mut Option<String>,
) -> Option<OnlineGenerationEvent> {
    let coordinator = match crate::authority::AuthorityCoordinator::prepare(store_root) {
        Ok(coordinator) => coordinator,
        Err(detail) => {
            return Some(OnlineGenerationEvent::Rejected {
                active_set_digest: None,
                detail,
            });
        }
    };
    let _authority_fence = match coordinator.try_snapshot() {
        Ok(Some(fence)) => fence,
        Ok(None) => return None,
        Err(detail) => {
            return Some(OnlineGenerationEvent::Rejected {
                active_set_digest: None,
                detail,
            });
        }
    };
    let authority = match crate::plugins::load_generation_authority_unfenced(store_root) {
        Ok(authority) => authority,
        Err(detail) => {
            return Some(OnlineGenerationEvent::Rejected {
                active_set_digest: None,
                detail,
            });
        }
    };
    if last_attempted_active_set_digest.as_deref() == Some(&authority.active_set_digest) {
        return None;
    }
    *last_attempted_active_set_digest = Some(authority.active_set_digest.clone());
    let active_set_digest = authority.active_set_digest.clone();
    let candidate = match resolve_generation_with_authority(plan_bytes, &authority, host_build) {
        Ok(candidate) => candidate,
        Err(detail) => {
            return Some(OnlineGenerationEvent::Rejected {
                active_set_digest: Some(active_set_digest),
                detail,
            });
        }
    };
    let state = match client.inspect().await.map_err(control_error) {
        Ok(state) => state,
        Err(detail) => {
            return Some(OnlineGenerationEvent::Rejected {
                active_set_digest: Some(active_set_digest),
                detail,
            });
        }
    };
    let Some(previous_generation_spec_digest) = state.active_generation_spec_digest.as_deref()
    else {
        return Some(OnlineGenerationEvent::Rejected {
            active_set_digest: Some(active_set_digest),
            detail: "online reconcile requires one active App Generation".to_owned(),
        });
    };
    if previous_generation_spec_digest == candidate.spec.digest() {
        return None;
    }
    if let Err(detail) = record_generation_spec(store_root, &candidate.spec).and_then(|()| {
        crate::plugins::record_resolved_generation_authority_unfenced(store_root, &authority)
    }) {
        return Some(OnlineGenerationEvent::Rejected {
            active_set_digest: Some(active_set_digest),
            detail,
        });
    }
    match activate_online_candidate(client, &state, previous_generation_spec_digest, candidate)
        .await
    {
        Ok(Some(outcome)) => Some(OnlineGenerationEvent::Switched {
            active_set_digest,
            generation_spec_digest: outcome.active_generation_spec_digest,
            previous_generation_spec_digest: previous_generation_spec_digest.to_owned(),
            routing_epoch: outcome.routing_epoch,
        }),
        Ok(None) => {
            *last_attempted_active_set_digest = None;
            None
        }
        Err(detail) => Some(OnlineGenerationEvent::Rejected {
            active_set_digest: Some(active_set_digest),
            detail,
        }),
    }
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
    let authority = crate::plugins::load_generation_authority(store_root)?;
    resolve_generation_with_authority(plan_bytes, &authority, host_build)
}

fn resolve_retained_generations(
    plan_bytes: &[u8],
    store_root: &Path,
    host_build: &HostBuildIdentity,
) -> Result<BTreeMap<String, ResolvedGeneration>, String> {
    crate::plugins::recovery_generation_authorities(store_root)?
        .into_iter()
        .map(|authority| {
            let generation = resolve_generation_with_authority(plan_bytes, &authority, host_build)?;
            Ok((generation.spec.digest().to_owned(), generation))
        })
        .collect()
}

fn resolve_generation_with_authority(
    plan_bytes: &[u8],
    authority: &crate::plugins::GenerationPluginAuthority,
    host_build: &HostBuildIdentity,
) -> Result<ResolvedGeneration, String> {
    let plan = serde_json::from_slice::<ResolvedAppPlan>(plan_bytes)
        .map_err(|error| format!("resolved Plan is invalid JSON: {error}"))?;
    plan.validate()
        .map_err(|error| format!("resolved Plan is invalid: {error}"))?;
    if plan.execution_lanes().len() != 1 || plan.execution_lanes()[0].id().as_str() != "main" {
        return Err(
            "Plugin control-plane bootstrap currently supports the `main` execution lane only"
                .to_owned(),
        );
    }

    let target = crate::plugins::host_target();
    let (_, built_in_modules) = native_host_build();
    let plugin_profiles = harness_plugin_profiles()?;
    let execution_classes = [
        (NATIVE_EXECUTION_CLASS, "lenso-native-adapter@0.1.2"),
        (QUICKJS_EXECUTION_CLASS, "lenso-quickjs-adapter@0.1.0"),
        (WASM_EXECUTION_CLASS, "lenso-wasm-component-adapter@0.1.0"),
    ];
    let adapter_profiles = execution_classes
        .iter()
        .map(|(execution_class, build_identity)| AdapterProfile {
            execution_class: (*execution_class).to_owned(),
            adapter_build_identity: (*build_identity).to_owned(),
            targets: vec![target.clone()],
            profiles: plugin_profiles.profiles_for_execution_class(execution_class),
        })
        .collect();
    let host_build = CanonicalDocument::from_value(
        "lenso-host-build.json",
        HostBuildManifest {
            schema_version: 1,
            app_id: APP_ID.to_owned(),
            host_executable_digest: host_build.executable_digest.clone(),
            target: target.clone(),
            built_in_modules,
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
            classes: execution_classes
                .iter()
                .map(|(execution_class, _)| ClassPolicy {
                    execution_class: (*execution_class).to_owned(),
                    support_channels: plugin_profiles
                        .support_channels_for_execution_class(execution_class),
                    trust_levels: plugin_profiles.trust_levels_for_execution_class(execution_class),
                    profiles: plugin_profiles.profiles_for_execution_class(execution_class),
                })
                .collect(),
            preference: execution_classes
                .iter()
                .map(|(execution_class, _)| (*execution_class).to_owned())
                .collect(),
            instance_overrides: Vec::new(),
        },
    )
    .map_err(control_error)?;
    let composition = crate::plugins::generation_composition(authority, &plan)?;
    let generation = resolve_generation(&ResolutionInput {
        lock: &authority.lock,
        manifests: &authority.manifests,
        admission_receipts: &authority.admission_receipts,
        host_build: &host_build,
        policy: &policy,
        artifact_source: authority.artifact_source.as_ref(),
        base_instances: composition.base_instances,
        bindings: composition.bindings,
    })
    .map_err(control_error)?;
    close_over_base_binding_order(generation, &composition.preserved_base_bindings)
}

pub(crate) async fn ready_check_maintenance_transition(
    plan_bytes: &[u8],
    current_authority: crate::plugins::GenerationPluginAuthority,
    candidate_authority: crate::plugins::GenerationPluginAuthority,
    store_root: &Path,
) -> Result<String, String> {
    ready_check_transition(
        plan_bytes,
        current_authority,
        candidate_authority,
        Some(store_root),
    )
    .await
}

pub(crate) async fn ready_check_source_transition(
    plan_bytes: &[u8],
    current_authority: crate::plugins::GenerationPluginAuthority,
    candidate_authority: crate::plugins::GenerationPluginAuthority,
) -> Result<String, String> {
    ready_check_transition(plan_bytes, current_authority, candidate_authority, None).await
}

async fn ready_check_transition(
    plan_bytes: &[u8],
    current_authority: crate::plugins::GenerationPluginAuthority,
    candidate_authority: crate::plugins::GenerationPluginAuthority,
    record_root: Option<&Path>,
) -> Result<String, String> {
    let host_build = HostBuildIdentity::current()?;
    let current = resolve_generation_with_authority(plan_bytes, &current_authority, &host_build)?;
    let candidate =
        resolve_generation_with_authority(plan_bytes, &candidate_authority, &host_build)?;
    if current.spec.digest() == candidate.spec.digest() {
        return Err("candidate Plugin authority resolves to the current Generation".to_owned());
    }
    let runtime = KernelGenerationRuntime::new(harness_catalog_factory());
    let supervisor =
        DurableGenerationSupervisor::open(APP_ID, runtime, MemoryControlStateStore::default())
            .map_err(control_error)?;
    let (controller, client) =
        GenerationController::new(supervisor, MAINTENANCE_INTERVAL).map_err(control_error)?;
    let task = tokio::task::spawn_local(controller.run());
    let ready_result = async {
        let initial = initial_transition(&current).map_err(control_error)?;
        client
            .transition(initial, current.clone(), BTreeMap::new())
            .await
            .map_err(control_error)?;
        let maintenance = maintenance_transition(&current, &candidate).map_err(control_error)?;
        client
            .transition(maintenance, candidate.clone(), BTreeMap::new())
            .await
            .map_err(control_error)?;
        Ok::<(), String>(())
    }
    .await;
    let cleanup_result = async {
        client.shutdown().await.map_err(control_error)?;
        task.await
            .map_err(|error| format!("Generation Controller task failed: {error}"))?
            .map_err(control_error)?;
        Ok::<(), String>(())
    }
    .await;
    match (ready_result, cleanup_result) {
        (Ok(()), Ok(())) => {}
        (Err(error), Ok(())) | (Ok(()), Err(error)) => return Err(error),
        (Err(error), Err(cleanup)) => {
            return Err(format!(
                "{error}; Generation cleanup also failed: {cleanup}"
            ));
        }
    }
    if let Some(root) = record_root {
        record_generation_spec(root, &candidate.spec)?;
    }
    Ok(candidate.spec.digest().to_owned())
}

fn close_over_base_binding_order(
    mut generation: ResolvedGeneration,
    preserved_base_bindings: &[lenso_app_plan::CapabilityBinding],
) -> Result<ResolvedGeneration, String> {
    let base_keys = preserved_base_bindings
        .iter()
        .map(binding_key)
        .collect::<BTreeSet<_>>();
    let mut bindings = preserved_base_bindings
        .iter()
        .map(|binding| serde_json::to_value(binding).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut next_orders = BTreeMap::<(String, String), usize>::new();
    for binding in preserved_base_bindings {
        let group = (
            binding.consumer_instance().to_owned(),
            binding.capability_id().to_owned(),
        );
        next_orders
            .entry(group)
            .and_modify(|order| *order = (*order).max(binding.provider_order() + 1))
            .or_insert(binding.provider_order() + 1);
    }
    for binding in generation
        .plan
        .capability_bindings()
        .iter()
        .filter(|binding| !base_keys.contains(&binding_key(binding)))
    {
        let group = (
            binding.consumer_instance().to_owned(),
            binding.capability_id().to_owned(),
        );
        let order = next_orders.entry(group).or_default();
        let mut value = serde_json::to_value(binding).map_err(|error| error.to_string())?;
        value["provider_order"] = (*order).into();
        *order += 1;
        bindings.push(value);
    }
    bindings.sort_by(|left, right| {
        binding_value_key(left)
            .cmp(&binding_value_key(right))
            .then_with(|| {
                left["provider_order"]
                    .as_u64()
                    .cmp(&right["provider_order"].as_u64())
            })
    });
    let mut plan_value =
        serde_json::to_value(&generation.plan).map_err(|error| error.to_string())?;
    plan_value["capability_bindings"] = bindings.into();
    let plan = serde_json::from_value::<ResolvedAppPlan>(plan_value)
        .map_err(|error| format!("failed to close Plugin bindings into the Plan: {error}"))?;
    plan.validate()
        .map_err(|error| format!("Plugin-resolved Plan is invalid: {error}"))?;
    let resolved_plan_digest = sha256_digest(
        &serde_json::to_vec(&plan)
            .map_err(|error| format!("failed to encode Plugin-resolved Plan: {error}"))?,
    );
    generation.plan = plan;
    generation.spec = CanonicalDocument::from_value(
        "lenso-generation.json",
        AppGenerationSpec {
            schema_version: generation.spec.value().schema_version,
            app_id: generation.spec.value().app_id.clone(),
            host_build_manifest_digest: generation.spec.value().host_build_manifest_digest.clone(),
            host_execution_policy_digest: generation
                .spec
                .value()
                .host_execution_policy_digest
                .clone(),
            resolved_plan_digest,
            plugin_set_lock_digest: generation.spec.value().plugin_set_lock_digest.clone(),
            resolved_artifact_set_digest: generation
                .spec
                .value()
                .resolved_artifact_set_digest
                .clone(),
            effective_host_grant_set_digest: generation
                .spec
                .value()
                .effective_host_grant_set_digest
                .clone(),
        },
    )
    .map_err(control_error)?;
    Ok(generation)
}

fn binding_key(binding: &lenso_app_plan::CapabilityBinding) -> (String, String, String) {
    (
        binding.consumer_instance().to_owned(),
        binding.capability_id().to_owned(),
        binding.provider_instance().to_owned(),
    )
}

fn binding_value_key(value: &serde_json::Value) -> (&str, &str) {
    (
        value["consumer_instance"].as_str().unwrap_or_default(),
        value["capability_id"].as_str().unwrap_or_default(),
    )
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

fn native_host_build() -> (NativeModuleRegistry, Vec<BuiltInModule>) {
    let registry = NativeModuleRegistry::new().with_linked_factories();
    let mut built_in_modules = registry
        .factories()
        .map(|factory| BuiltInModule {
            package_id: factory.package_id().to_owned(),
            factory_identity: factory.factory_identity(),
            execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        })
        .collect::<Vec<_>>();
    built_in_modules.sort_by(|left, right| left.factory_identity.cmp(&right.factory_identity));
    (registry, built_in_modules)
}

pub(crate) fn linked_module_catalog() -> Result<ModuleCatalog, String> {
    let descriptor_json = [
        lenso_agent_cli_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_discord_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_http_fetch_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_loop_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_model_fixture_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_prompt_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_prompt_static_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_session_file_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_telegram_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_tools_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_tui_command_suggestions_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_tui_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_tui_static_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_tui_workspace_suggestions_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_workspace_import_read_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_workspace_read_module::MODULE_DESCRIPTOR_JSON,
        lenso_agent_workspace_read_tools_module::MODULE_DESCRIPTOR_JSON,
    ];
    let descriptors = descriptor_json
        .into_iter()
        .map(|json| {
            serde_json::from_str(json)
                .map_err(|error| format!("linked Module has an invalid Descriptor: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    ModuleCatalog::new(descriptors)
        .map_err(|error| format!("linked Module catalog is invalid: {error}"))
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
    use lenso_capability_agent::{RUN_TURN_OPERATION, RunTurnRequest};
    use lenso_kernel::StreamEvent;
    use lenso_plugin_bundle::{SourcePluginBuild, build_source_plugin_bundle};

    #[derive(Debug)]
    struct TerminalFailureRuntime {
        failed: Rc<RefCell<BTreeSet<String>>>,
    }

    impl lenso_plugin_control_plane::GenerationRuntime for TerminalFailureRuntime {
        type Handle = String;
        type Route = String;

        fn stage<'a>(
            &'a mut self,
            generation: &'a ResolvedGeneration,
            _ready_timeout_nanos: u64,
        ) -> futures::future::LocalBoxFuture<'a, Result<Self::Handle, ControlPlaneError>> {
            Box::pin(async move { Ok(generation.spec.digest().to_owned()) })
        }

        fn shutdown(
            &mut self,
            _handle: Self::Handle,
            _drain_timeout_nanos: u64,
        ) -> futures::future::LocalBoxFuture<'_, Result<(), ControlPlaneError>> {
            Box::pin(async { Ok(()) })
        }

        fn terminal_failure(&self, handle: &Self::Handle) -> Option<ControlPlaneError> {
            self.failed
                .borrow()
                .contains(handle)
                .then(|| ControlPlaneError::HostFailure {
                    detail: "terminal fixture".to_owned(),
                })
        }

        fn route(&self, handle: &Self::Handle) -> Self::Route {
            handle.clone()
        }
    }

    async fn next_online_event(app: &AgentApp) -> OnlineGenerationEvent {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(event) = app.take_online_generation_events().into_iter().next() {
                    return event;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("online Generation event timed out")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn filesystem_watcher_wakes_for_discovery_creation_and_nested_changes() {
        let directory = tempfile::tempdir().unwrap();
        let discovery_root = directory.path().join("plugins");
        let (mut watcher, errors) =
            FilesystemReconcileWatcher::start(&[directory.path()], Some(discovery_root.clone()));
        assert!(errors.is_empty(), "watcher setup failed: {errors:?}");

        fs::create_dir(&discovery_root).unwrap();
        tokio::time::timeout(Duration::from_secs(2), watcher.changed())
            .await
            .expect("discovery directory creation did not wake the watcher")
            .expect("watcher signal channel closed");
        assert_eq!(watcher.refresh_recursive_watch(), None);
        watcher.settle_after(None).await;

        let bundle = discovery_root.join("example");
        fs::create_dir(&bundle).unwrap();
        fs::write(bundle.join("lenso-plugin.json"), b"{}").unwrap();
        tokio::time::timeout(Duration::from_secs(2), watcher.changed())
            .await
            .expect("nested discovery change did not wake the watcher")
            .expect("watcher signal channel closed");
    }

    #[test]
    fn repeated_watcher_degradation_is_reported_once() {
        let events = Rc::new(RefCell::new(VecDeque::new()));
        report_watcher_errors(&events, ["fixture failure".to_owned()]);
        report_watcher_errors(&events, ["fixture failure".to_owned()]);
        assert_eq!(
            events.borrow().iter().collect::<Vec<_>>(),
            vec![&OnlineGenerationEvent::WatchDegraded {
                detail: "fixture failure".to_owned(),
            }]
        );
    }

    #[test]
    fn initial_generation_preserves_the_approved_plan() {
        let directory = tempfile::tempdir().unwrap();
        let plan = crate::test_support::headless_plan();
        let generation = resolve_initial_generation(plan, directory.path()).unwrap();
        let approved: ResolvedAppPlan = serde_json::from_slice(plan).unwrap();
        assert_eq!(generation.plan, approved);
        assert!(generation.artifact_set.value().releases.is_empty());
        assert!(generation.artifact_set.value().instances.is_empty());
    }

    #[test]
    fn online_source_transition_authorizes_one_bounded_automatic_rollback() {
        let directory = tempfile::tempdir().unwrap();
        let definition = source_definition(directory.path());
        let host_build = HostBuildIdentity::current().unwrap();
        let current_snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
        let current = resolve_generation_with_authority(
            crate::test_support::headless_plan(),
            &current_snapshot.authority,
            &host_build,
        )
        .unwrap();
        copy_text_tool_bundle(&directory.path().join("plugins/text-tools"));
        let candidate_snapshot =
            crate::plugins::source_generation_snapshot_at(&definition).unwrap();
        let candidate = resolve_generation_with_authority(
            crate::test_support::headless_plan(),
            &candidate_snapshot.authority,
            &host_build,
        )
        .unwrap();

        let transition = online_overlap_transition(current.spec.digest(), &candidate).unwrap();
        assert_eq!(
            transition.value().from_generation_spec_digest.as_deref(),
            Some(current.spec.digest())
        );
        assert_eq!(
            transition.value().rollout_policy.rollback_window_nanos,
            ONLINE_ROLLBACK_WINDOW_NANOS.to_string()
        );
        assert!(
            transition
                .value()
                .rollout_policy
                .automatic_rollback_on_generation_failure
        );
    }

    #[test]
    fn terminal_controller_failure_is_presented_as_an_exact_automatic_rollback() {
        let failure = lenso_plugin_control_plane::GenerationFailureOutcome {
            generation_spec_digest: "sha256:failed".to_owned(),
            failure: ControlPlaneError::HostFailure {
                detail: "terminal fixture".to_owned(),
            },
            automatic_rollback: Some(lenso_plugin_control_plane::DurableTransitionOutcome {
                active_generation_spec_digest: "sha256:restored".to_owned(),
                supervisor_epoch: 3,
                routing_epoch: 7,
                draining_generation_spec_digest: Some("sha256:failed".to_owned()),
                activation_direction: lenso_plugin_control_plane::ActivationDirection::Rollback,
            }),
        };

        assert_eq!(
            online_event_from_controller_event(GenerationControllerEvent::Maintained(
                GenerationMaintenanceOutcome::Failed(failure)
            )),
            Some(OnlineGenerationEvent::RolledBack {
                failed_generation_spec_digest: "sha256:failed".to_owned(),
                restored_generation_spec_digest: "sha256:restored".to_owned(),
                routing_epoch: 7,
                detail:
                    "terminal App Generation failure: HostFailure { detail: \"terminal fixture\" }"
                        .to_owned(),
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_source_generation_failure_restores_the_exact_predecessor_route() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let definition = source_definition(directory.path());
                let host_build = HostBuildIdentity::current().unwrap();
                let current_snapshot =
                    crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let current = resolve_generation_with_authority(
                    crate::test_support::headless_plan(),
                    &current_snapshot.authority,
                    &host_build,
                )
                .unwrap();
                copy_text_tool_bundle(&directory.path().join("plugins/text-tools"));
                let candidate_snapshot =
                    crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let candidate = resolve_generation_with_authority(
                    crate::test_support::headless_plan(),
                    &candidate_snapshot.authority,
                    &host_build,
                )
                .unwrap();
                let current_digest = current.spec.digest().to_owned();
                let candidate_digest = candidate.spec.digest().to_owned();
                let failed = Rc::new(RefCell::new(BTreeSet::new()));
                let runtime = TerminalFailureRuntime {
                    failed: failed.clone(),
                };
                let store_root = directory.path().join(".lenso/plugins");
                let supervisor = DurableGenerationSupervisor::open(
                    APP_ID,
                    runtime,
                    FileControlStateStore::open(store_root.join(CONTROL_DIRECTORY)).unwrap(),
                )
                .unwrap();
                let (controller, client) =
                    GenerationController::new(supervisor, MAINTENANCE_INTERVAL).unwrap();
                let mut events = client.subscribe();
                let task = tokio::task::spawn_local(controller.run());
                client
                    .transition(
                        initial_transition(&current).unwrap(),
                        current,
                        BTreeMap::new(),
                    )
                    .await
                    .unwrap();
                client
                    .transition(
                        online_overlap_transition(&current_digest, &candidate).unwrap(),
                        candidate,
                        BTreeMap::new(),
                    )
                    .await
                    .unwrap();
                failed.borrow_mut().insert(candidate_digest.clone());

                let rollback = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if let GenerationControllerEvent::Maintained(
                            GenerationMaintenanceOutcome::Failed(failure),
                        ) = events.recv().await.unwrap()
                            && failure.generation_spec_digest == candidate_digest
                        {
                            break failure.automatic_rollback.unwrap();
                        }
                    }
                })
                .await
                .expect("terminal source Generation did not roll back");
                assert_eq!(rollback.active_generation_spec_digest, current_digest);
                assert_eq!(client.route().await.unwrap().target(), &current_digest);
                let status = source_controller_status(&store_root).unwrap();
                assert!(
                    status.iter().any(|line| {
                        line.starts_with(
                            "generation: headless Active health=Healthy direction=Rollback",
                        ) && line.ends_with(&current_digest)
                    }),
                    "{status:?}"
                );
                assert!(
                    status.iter().any(|line| {
                        line.starts_with("generation: headless Retired health=Failed")
                            && line.contains("retirement=Some(TerminalFailure)")
                            && line.ends_with(&candidate_digest)
                    }),
                    "{status:?}"
                );

                client.shutdown().await.unwrap();
                task.await.unwrap().unwrap();
            })
            .await;
    }

    #[test]
    fn generation_spec_is_content_addressed_and_tampering_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let generation =
            resolve_initial_generation(crate::test_support::headless_plan(), directory.path())
                .unwrap();
        record_generation_spec(directory.path(), &generation.spec).unwrap();
        let digest = generation.spec.digest().strip_prefix("sha256:").unwrap();
        let record = directory
            .path()
            .join(GENERATION_DIRECTORY)
            .join(format!("{digest}.json"));
        assert_eq!(fs::read(&record).unwrap(), generation.spec.bytes());

        fs::write(&record, b"{}").unwrap();
        let error = record_generation_spec(directory.path(), &generation.spec).unwrap_err();
        assert!(error.contains("does not match its digest"));
    }

    #[test]
    fn duplicate_tui_panel_ids_fail_closed() {
        let panel = SnapshotResponsePanelsItem {
            id: "agent.help".to_owned(),
            title: "Help".to_owned(),
            body: "Esc quits".to_owned(),
        };
        let error = validate_tui_panels(&[panel.clone(), panel]).unwrap_err();
        assert_eq!(error, "duplicate TUI panel id `agent.help`");
    }

    #[test]
    fn aggregate_tui_panels_are_bounded() {
        let panels = (0..=MAX_TUI_PANELS)
            .map(|index| SnapshotResponsePanelsItem {
                id: format!("agent.panel-{index}"),
                title: format!("Panel {index}"),
                body: "Content".to_owned(),
            })
            .collect::<Vec<_>>();
        let error = validate_tui_panels(&panels).unwrap_err();
        assert!(error.contains("aggregate limit"));

        let oversized = SnapshotResponsePanelsItem {
            id: "agent.large".to_owned(),
            title: "Large".to_owned(),
            body: "x".repeat(MAX_TUI_PANEL_BYTES),
        };
        let error = validate_tui_panels(&[oversized]).unwrap_err();
        assert!(error.contains("byte aggregate limit"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tui_composition_snapshots_panels_and_streams_one_turn() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let mut app = AgentApp::start_with_store(
                    crate::test_support::headless_plan(),
                    directory.path(),
                )
                .await
                .unwrap();
                let panels = app.tui_panels().await.unwrap();
                assert_eq!(panels.len(), 1);
                assert_eq!(panels[0].id, "agent.help");
                let suggestions = app.tui_suggestions().await.unwrap();
                assert!(suggestions.iter().any(|item| item.label == "/help"));
                assert!(suggestions.iter().any(|item| item.label == "Cargo.toml"));

                let turn = app.lease_tui_turn().await.unwrap();
                let stream = turn
                    .handle()
                    .open_with_context(
                        RUN_TURN_OPERATION,
                        turn.invocation_context().unwrap(),
                        RunTurnRequest {
                            input: "Answer directly: hello".to_owned(),
                            session_id: None,
                        },
                    )
                    .await
                    .unwrap()
                    .unwrap();
                stream.close_send().await.unwrap();
                let mut output = String::new();
                loop {
                    match stream.receive().await.unwrap() {
                        StreamEvent::Message(message) if message.is_text_delta() => {
                            output.push_str(&message.text);
                        }
                        StreamEvent::Message(_) | StreamEvent::PeerHalfClosed => {}
                        StreamEvent::Terminal(Ok(())) => break,
                        StreamEvent::Terminal(Err(error)) => {
                            panic!("TUI Agent Turn failed: {error:?}")
                        }
                    }
                }
                assert_eq!(output, "Direct answer.");
                drop(stream);
                drop(turn);
                app.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn committed_plugin_authority_switches_online_while_an_old_turn_is_leased() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let plan_path = directory.path().join("resolved-plan.json");
                fs::write(&plan_path, crate::test_support::headless_plan()).unwrap();
                let mut app = AgentApp::start_with_store(
                    crate::test_support::headless_plan(),
                    directory.path(),
                )
                .await
                .unwrap();
                let old_turn = app.lease_turn().await.unwrap();
                let old_digest = old_turn.generation_spec_digest().to_owned();

                let command = crate::plugins::parse_command(&[
                    "enable".to_owned(),
                    "text-tools".to_owned(),
                    "--evidence".to_owned(),
                    "reviewed by online reconcile test".to_owned(),
                    "--plan".to_owned(),
                    plan_path.display().to_string(),
                    "--root".to_owned(),
                    directory.path().display().to_string(),
                ])
                .unwrap();
                crate::plugins::run(command).await.unwrap();

                let event = next_online_event(&app).await;
                let OnlineGenerationEvent::Switched {
                    generation_spec_digest,
                    previous_generation_spec_digest,
                    ..
                } = event
                else {
                    panic!("expected an online Generation switch")
                };
                assert_eq!(previous_generation_spec_digest, old_digest);
                assert_ne!(generation_spec_digest, old_digest);

                let new_turn = app.lease_turn().await.unwrap();
                assert_eq!(new_turn.generation_spec_digest(), generation_spec_digest);

                let stream = old_turn
                    .handle()
                    .open_with_context(
                        RUN_TURN_OPERATION,
                        old_turn.invocation_context().unwrap(),
                        RunTurnRequest {
                            input: "Answer directly: old Generation remains live".to_owned(),
                            session_id: None,
                        },
                    )
                    .await
                    .unwrap()
                    .unwrap();
                stream.close_send().await.unwrap();
                while !matches!(stream.receive().await.unwrap(), StreamEvent::Terminal(_)) {}

                drop(new_turn);
                drop(old_turn);
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let state = app.client.inspect().await.unwrap();
                        if state.generations.iter().any(|record| {
                            record.generation_spec_digest == old_digest
                                && record.lifecycle == ControlLifecycle::Standby
                                && record.health == ControlHealth::Healthy
                        }) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("old Generation did not become the healthy rollback standby");
                app.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_desired_state_keeps_the_current_generation_routable() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let mut app = AgentApp::start_with_store(
                    crate::test_support::headless_plan(),
                    directory.path(),
                )
                .await
                .unwrap();
                let before = app.lease_turn().await.unwrap();
                let before_digest = before.generation_spec_digest().to_owned();
                drop(before);

                fs::write(directory.path().join("active-set.json"), b"{}").unwrap();
                let event = next_online_event(&app).await;
                let OnlineGenerationEvent::Rejected { detail, .. } = event else {
                    panic!("expected the invalid Desired State to be rejected")
                };
                assert!(detail.contains("active-set.json"));

                let after = app.lease_turn().await.unwrap();
                assert_eq!(after.generation_spec_digest(), before_digest);
                drop(after);
                app.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn source_discovery_switches_online_and_removal_restores_the_base_generation() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let definition = source_definition(directory.path());
                let snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let mut app = AgentApp::start_with_durable_source_snapshot(
                    crate::test_support::headless_plan(),
                    snapshot,
                    CONTROL_DIRECTORY,
                )
                .await
                .unwrap();
                let old_turn = app.lease_turn().await.unwrap();
                let base_digest = old_turn.generation_spec_digest().to_owned();

                copy_text_tool_bundle(&directory.path().join("plugins/text-tools"));
                let event = next_online_event(&app).await;
                let OnlineGenerationEvent::Switched {
                    generation_spec_digest,
                    previous_generation_spec_digest,
                    ..
                } = event
                else {
                    panic!("expected source discovery to switch the Generation")
                };
                assert_eq!(previous_generation_spec_digest, base_digest);
                assert_ne!(generation_spec_digest, base_digest);
                assert_eq!(
                    run_turn_text(&app, "Use the text Plugin to uppercase Lenso plugin.").await,
                    "Text Plugin result: LENSO PLUGIN"
                );
                assert_eq!(old_turn.generation_spec_digest(), base_digest);
                drop(old_turn);

                fs::remove_dir_all(directory.path().join("plugins/text-tools")).unwrap();
                let event = next_online_event(&app).await;
                let OnlineGenerationEvent::Switched {
                    generation_spec_digest,
                    ..
                } = event
                else {
                    panic!("expected source removal to switch the Generation")
                };
                assert_eq!(generation_spec_digest, base_digest);
                app.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gradual_bundle_copy_settles_before_discovery_reconciles() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let definition = source_definition(directory.path());
                let snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let mut app = AgentApp::start_with_durable_source_snapshot(
                    crate::test_support::headless_plan(),
                    snapshot,
                    CONTROL_DIRECTORY,
                )
                .await
                .unwrap();

                let bundle = directory.path().join("plugins/text-tools");
                fs::create_dir_all(&bundle).unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../examples/plugins/text-tools/lenso-plugin.json");
                fs::copy(source, bundle.join("lenso-plugin.json")).unwrap();

                let event = next_online_event(&app).await;
                assert!(
                    matches!(event, OnlineGenerationEvent::Switched { .. }),
                    "gradual copy produced an intermediate reconcile event: {event:?}"
                );
                app.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hidden_staging_bundle_is_inert_until_atomic_publish() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let definition = source_definition(directory.path());
                let snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let mut app = AgentApp::start_with_durable_source_snapshot(
                    crate::test_support::headless_plan(),
                    snapshot,
                    CONTROL_DIRECTORY,
                )
                .await
                .unwrap();
                let before = app.lease_turn().await.unwrap();
                let base_digest = before.generation_spec_digest().to_owned();
                drop(before);

                let staging = directory.path().join("plugins/.staging-text-tools");
                copy_text_tool_bundle(&staging);
                tokio::time::sleep(RECONCILE_CONSISTENCY_INTERVAL + Duration::from_millis(500))
                    .await;
                assert!(app.take_online_generation_events().is_empty());
                let staged = app.lease_turn().await.unwrap();
                assert_eq!(staged.generation_spec_digest(), base_digest);
                drop(staged);

                fs::rename(staging, directory.path().join("plugins/text-tools")).unwrap();
                let event = next_online_event(&app).await;
                assert!(matches!(event, OnlineGenerationEvent::Switched { .. }));
                app.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn packaged_bundle_drops_in_replaces_and_unloads_without_extraction() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let definition = source_definition(directory.path());
                let snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let mut app = AgentApp::start_with_durable_source_snapshot(
                    crate::test_support::headless_plan(),
                    snapshot,
                    CONTROL_DIRECTORY,
                )
                .await
                .unwrap();
                let before = app.lease_turn().await.unwrap();
                let base_digest = before.generation_spec_digest().to_owned();
                drop(before);

                fs::create_dir(directory.path().join("plugins")).unwrap();
                let source = directory.path().join("bundle-source");
                copy_text_tool_bundle(&source);
                let package = directory.path().join("plugins/text-tools.lenso-plugin");
                crate::plugins::pack_bundle(&source, &package).unwrap();

                let event = next_online_event(&app).await;
                let OnlineGenerationEvent::Switched {
                    generation_spec_digest,
                    ..
                } = event
                else {
                    panic!("expected packaged Bundle drop-in to switch the Generation")
                };
                assert_ne!(generation_spec_digest, base_digest);
                assert_eq!(
                    run_turn_text(&app, "Use the text Plugin to uppercase Lenso plugin.").await,
                    "Text Plugin result: LENSO PLUGIN"
                );
                assert!(!directory.path().join("plugins/text-tools").exists());

                let manifest_path = source.join("lenso-plugin.json");
                let mut manifest: serde_json::Value =
                    serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
                manifest["release_version"] = "2.0.0".into();
                fs::write(
                    &manifest_path,
                    serde_json::to_vec_pretty(&manifest).unwrap(),
                )
                .unwrap();
                crate::plugins::pack_bundle(&source, &package).unwrap();

                let event = next_online_event(&app).await;
                let OnlineGenerationEvent::Switched {
                    generation_spec_digest: replacement_digest,
                    ..
                } = event
                else {
                    panic!("expected packaged Bundle replacement to switch the Generation")
                };
                assert_ne!(replacement_digest, generation_spec_digest);
                assert_ne!(replacement_digest, base_digest);
                assert_eq!(
                    run_turn_text(&app, "Use the text Plugin to uppercase Lenso plugin.").await,
                    "Text Plugin result: LENSO PLUGIN"
                );

                fs::remove_file(package).unwrap();
                let event = next_online_event(&app).await;
                let OnlineGenerationEvent::Switched {
                    generation_spec_digest,
                    ..
                } = &event
                else {
                    panic!("expected packaged Bundle removal to switch the Generation: {event:?}")
                };
                assert_eq!(generation_spec_digest, &base_digest);
                app.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_source_bundle_keeps_routing_and_does_not_block_a_safe_bundle() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let definition = source_definition(directory.path());
                let snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let mut app = AgentApp::start_with_durable_source_snapshot(
                    crate::test_support::headless_plan(),
                    snapshot,
                    CONTROL_DIRECTORY,
                )
                .await
                .unwrap();
                let before = app.lease_turn().await.unwrap();
                let base_digest = before.generation_spec_digest().to_owned();
                drop(before);

                fs::create_dir_all(directory.path().join("plugins/malformed")).unwrap();
                let event = next_online_event(&app).await;
                let OnlineGenerationEvent::Rejected { detail, .. } = event else {
                    panic!("expected malformed discovery Bundle to be quarantined")
                };
                assert!(detail.contains("Plugin Bundle is missing `lenso-plugin.json`"));
                let after_rejection = app.lease_turn().await.unwrap();
                assert_eq!(after_rejection.generation_spec_digest(), base_digest);
                drop(after_rejection);

                copy_text_tool_bundle(&directory.path().join("plugins/text-tools"));
                let switched = tokio::time::timeout(Duration::from_secs(15), async {
                    loop {
                        for event in app.take_online_generation_events() {
                            if let OnlineGenerationEvent::Switched { .. } = event {
                                return event;
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("safe discovered Bundle did not switch while another Bundle was blocked");
                let OnlineGenerationEvent::Switched {
                    generation_spec_digest,
                    ..
                } = switched
                else {
                    unreachable!()
                };
                assert_ne!(generation_spec_digest, base_digest);
                assert_eq!(
                    run_turn_text(&app, "Use the text Plugin to uppercase Lenso plugin.").await,
                    "Text Plugin result: LENSO PLUGIN"
                );
                app.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn source_candidate_that_cannot_become_ready_keeps_the_current_generation() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let definition = source_definition(directory.path());
                let snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let mut app = AgentApp::start_with_durable_source_snapshot(
                    crate::test_support::headless_plan(),
                    snapshot,
                    CONTROL_DIRECTORY,
                )
                .await
                .unwrap();
                let before = app.lease_turn().await.unwrap();
                let base_digest = before.generation_spec_digest().to_owned();
                drop(before);

                build_invalid_wasm_tool_bundle(directory.path());
                let event = next_online_event(&app).await;
                let OnlineGenerationEvent::Rejected { detail, .. } = event else {
                    panic!("expected an unstartable source candidate to be rejected")
                };
                assert!(
                    detail.contains("Wasm")
                        || detail.contains("wasm")
                        || detail.contains("Ready")
                        || detail.contains("ready"),
                    "{detail}"
                );
                let after = app.lease_turn().await.unwrap();
                assert_eq!(after.generation_spec_digest(), base_digest);
                drop(after);
                assert_eq!(
                    run_turn_text(&app, "Answer directly: current Generation is healthy").await,
                    "Direct answer."
                );
                app.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn durable_source_generation_recovers_after_graceful_restart() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let definition = source_definition(directory.path());
                copy_text_tool_bundle(&directory.path().join("plugins/text-tools"));

                let snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let mut first = AgentApp::start_with_durable_source_snapshot(
                    crate::test_support::headless_plan(),
                    snapshot,
                    TUI_CONTROL_DIRECTORY,
                )
                .await
                .unwrap();
                let first_turn = first.lease_tui_turn().await.unwrap();
                let first_digest = first_turn.generation_spec_digest().to_owned();
                drop(first_turn);
                first.shutdown().await.unwrap();

                assert!(
                    directory
                        .path()
                        .join(".lenso/plugins/tui-generation-control")
                        .is_dir()
                );
                assert!(
                    directory
                        .path()
                        .join(".lenso/plugins/generation-authorities")
                        .is_dir()
                );
                let status =
                    source_controller_status(&directory.path().join(".lenso/plugins")).unwrap();
                assert!(status.iter().any(|line| {
                    line.starts_with("controller: tui ") && line.contains("suspended=true")
                }));
                assert!(status.iter().any(|line| {
                    line.starts_with("generation: tui Active ")
                        || line.starts_with("generation: tui Standby ")
                }));
                let snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let mut recovered = AgentApp::start_with_durable_source_snapshot(
                    crate::test_support::headless_plan(),
                    snapshot,
                    TUI_CONTROL_DIRECTORY,
                )
                .await
                .unwrap();
                let recovered_turn = recovered.lease_tui_turn().await.unwrap();
                assert_eq!(recovered_turn.generation_spec_digest(), first_digest);
                drop(recovered_turn);
                recovered.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn durable_source_generation_recovers_after_unclean_host_exit() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let definition = source_definition(directory.path());
                copy_text_tool_bundle(&directory.path().join("plugins/text-tools"));
                let snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let mut first = AgentApp::start_with_durable_source_snapshot(
                    crate::test_support::headless_plan(),
                    snapshot,
                    CHANNEL_CONTROL_DIRECTORY,
                )
                .await
                .unwrap();
                let turn = first.lease_turn().await.unwrap();
                let active_digest = turn.generation_spec_digest().to_owned();
                drop(turn);
                first.controller.take().unwrap().abort();
                drop(first);

                let snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let mut recovered = AgentApp::start_with_durable_source_snapshot(
                    crate::test_support::headless_plan(),
                    snapshot,
                    CHANNEL_CONTROL_DIRECTORY,
                )
                .await
                .unwrap();
                let recovered_turn = recovered.lease_turn().await.unwrap();
                assert_eq!(recovered_turn.generation_spec_digest(), active_digest);
                drop(recovered_turn);
                recovered.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn durable_source_controller_state_is_namespaced_per_surface() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let definition = source_definition(directory.path());
                let snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let mut headless = AgentApp::start_with_durable_source_snapshot(
                    crate::test_support::headless_plan(),
                    snapshot,
                    CONTROL_DIRECTORY,
                )
                .await
                .unwrap();
                let snapshot = crate::plugins::source_generation_snapshot_at(&definition).unwrap();
                let mut tui = AgentApp::start_with_durable_source_snapshot(
                    crate::test_support::headless_plan(),
                    snapshot,
                    TUI_CONTROL_DIRECTORY,
                )
                .await
                .unwrap();
                assert!(
                    directory
                        .path()
                        .join(".lenso/plugins/generation-control")
                        .is_dir()
                );
                assert!(
                    directory
                        .path()
                        .join(".lenso/plugins/tui-generation-control")
                        .is_dir()
                );
                let headless_turn = headless.lease_turn().await.unwrap();
                let tui_turn = tui.lease_tui_turn().await.unwrap();
                assert_eq!(
                    headless_turn.generation_spec_digest(),
                    tui_turn.generation_spec_digest()
                );
                drop(headless_turn);
                drop(tui_turn);
                headless.shutdown().await.unwrap();
                tui.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_turn_is_pinned_to_the_active_generation() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let mut app = AgentApp::start_with_store(
                    crate::test_support::headless_plan(),
                    directory.path(),
                )
                .await
                .unwrap();
                let turn = app.lease_turn().await.unwrap();
                assert_eq!(turn.handle().binding_count(), 1);
                assert!(!turn.generation_spec_digest().is_empty());
                let context = turn.invocation_context().unwrap();
                assert_eq!(
                    context.extension(GENERATION_SPEC_DIGEST_EXTENSION),
                    Some(turn.generation_spec_digest().as_bytes())
                );
                assert!(app.shutdown().await.is_err());
                drop(turn);
                app.shutdown().await.unwrap();
                drop(app);

                let mut recovered = AgentApp::start_with_store(
                    crate::test_support::headless_plan(),
                    directory.path(),
                )
                .await
                .unwrap();
                let recovered_turn = recovered.lease_turn().await.unwrap();
                assert_eq!(recovered_turn.handle().binding_count(), 1);
                drop(recovered_turn);
                recovered.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tui_start_does_not_recover_the_headless_controller_lineage() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let mut headless = AgentApp::start_with_store(
                    crate::test_support::headless_plan(),
                    directory.path(),
                )
                .await
                .unwrap();
                headless.shutdown().await.unwrap();

                let mut tui = AgentApp::start_tui_with_store(
                    crate::test_support::headless_plan(),
                    directory.path(),
                )
                .await
                .unwrap();
                tui.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn channel_host_leases_telegram_and_discord_from_one_generation() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let mut app = AgentApp::start_with_store_and_control_directory(
                    crate::test_support::headless_plan(),
                    directory.path(),
                    CHANNEL_CONTROL_DIRECTORY,
                )
                .await
                .unwrap();

                let telegram = app.lease_telegram_turn().await.unwrap();
                let discord = app.lease_discord_turn().await.unwrap();
                assert_eq!(
                    telegram.generation_spec_digest(),
                    discord.generation_spec_digest()
                );
                assert_eq!(telegram.handle().binding_count(), 1);
                assert_eq!(discord.handle().binding_count(), 1);
                drop(telegram);
                drop(discord);
                app.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn clean_host_upgrade_replaces_an_unrecoverable_suspended_generation() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let first_host = HostBuildIdentity {
                    executable_digest: sha256_digest(b"host build A"),
                };
                let second_host = HostBuildIdentity {
                    executable_digest: sha256_digest(b"host build B"),
                };
                let mut first = AgentApp::start_with_store_control_directory_and_host_build(
                    crate::test_support::headless_plan(),
                    directory.path(),
                    TUI_CONTROL_DIRECTORY,
                    first_host,
                )
                .await
                .unwrap();
                let first_turn = first.lease_tui_turn().await.unwrap();
                let first_digest = first_turn.generation_spec_digest().to_owned();
                drop(first_turn);
                first.shutdown().await.unwrap();

                let mut upgraded = AgentApp::start_with_store_control_directory_and_host_build(
                    crate::test_support::headless_plan(),
                    directory.path(),
                    TUI_CONTROL_DIRECTORY,
                    second_host,
                )
                .await
                .unwrap();
                let upgraded_turn = upgraded.lease_tui_turn().await.unwrap();
                assert_ne!(upgraded_turn.generation_spec_digest(), first_digest);
                drop(upgraded_turn);
                upgraded.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_host_upgrade_still_fails_closed() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let mut first = AgentApp::start_with_store_control_directory_and_host_build(
                    crate::test_support::headless_plan(),
                    directory.path(),
                    TUI_CONTROL_DIRECTORY,
                    HostBuildIdentity {
                        executable_digest: sha256_digest(b"live host build"),
                    },
                )
                .await
                .unwrap();

                let error = AgentApp::start_with_store_control_directory_and_host_build(
                    crate::test_support::headless_plan(),
                    directory.path(),
                    TUI_CONTROL_DIRECTORY,
                    HostBuildIdentity {
                        executable_digest: sha256_digest(b"concurrent replacement build"),
                    },
                )
                .await
                .unwrap_err();
                assert!(error.contains("another Host owns"));
                first.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exited_unclean_host_is_replaced_without_deleting_plugin_state() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let mut first = AgentApp::start_with_store_control_directory_and_host_build(
                    crate::test_support::headless_plan(),
                    directory.path(),
                    TUI_CONTROL_DIRECTORY,
                    HostBuildIdentity {
                        executable_digest: sha256_digest(b"crashed host build"),
                    },
                )
                .await
                .unwrap();
                first.controller.take().unwrap().abort();
                drop(first);

                let mut replacement = AgentApp::start_with_store_control_directory_and_host_build(
                    crate::test_support::headless_plan(),
                    directory.path(),
                    TUI_CONTROL_DIRECTORY,
                    HostBuildIdentity {
                        executable_digest: sha256_digest(b"replacement host build"),
                    },
                )
                .await
                .unwrap();
                let turn = replacement.lease_tui_turn().await.unwrap();
                assert!(!turn.generation_spec_digest().is_empty());
                drop(turn);
                replacement.shutdown().await.unwrap();
            })
            .await;
    }

    fn source_definition(directory: &Path) -> PathBuf {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut definition: serde_json::Value =
            serde_json::from_slice(&fs::read(repository.join("lenso.app.json")).unwrap()).unwrap();
        definition["manifest"] = repository.join("Cargo.toml").display().to_string().into();
        let path = directory.join("lenso.app.json");
        fs::write(&path, serde_json::to_vec_pretty(&definition).unwrap()).unwrap();
        path
    }

    fn copy_text_tool_bundle(destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/plugins/text-tools/lenso-plugin.json");
        fs::create_dir_all(destination).unwrap();
        fs::copy(source, destination.join("lenso-plugin.json")).unwrap();
    }

    fn build_invalid_wasm_tool_bundle(directory: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/external-plugins/wasm-text-tools");
        let target = directory.join("wasm-target");
        let build = std::process::Command::new(env!("CARGO"))
            .args([
                "build",
                "--locked",
                "--release",
                "--target",
                "wasm32-unknown-unknown",
                "--manifest-path",
            ])
            .arg(source.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let artifact =
            target.join("wasm32-unknown-unknown/release/dev_example_wasm_text_tools.wasm");
        let output = directory.join("plugins/invalid-wasm");
        build_source_plugin_bundle(&SourcePluginBuild {
            package_manifest: source.join("Cargo.toml"),
            wasm_module: artifact,
            output: output.clone(),
        })
        .unwrap();
        let invalid = b"not a Wasm Component";
        fs::write(output.join("plugin.wasm"), invalid).unwrap();
    }

    async fn run_turn_text(app: &AgentApp, input: &str) -> String {
        let turn = app.lease_turn().await.unwrap();
        let stream = turn
            .handle()
            .open_with_context(
                RUN_TURN_OPERATION,
                turn.invocation_context().unwrap(),
                RunTurnRequest {
                    input: input.to_owned(),
                    session_id: None,
                },
            )
            .await
            .unwrap()
            .unwrap();
        stream.close_send().await.unwrap();
        let mut output = String::new();
        loop {
            match stream.receive().await.unwrap() {
                StreamEvent::Message(message) if message.is_text_delta() => {
                    output.push_str(&message.text);
                }
                StreamEvent::Message(_) | StreamEvent::PeerHalfClosed => {}
                StreamEvent::Terminal(Ok(())) => break,
                StreamEvent::Terminal(Err(error)) => panic!("Agent Turn failed: {error:?}"),
            }
        }
        output
    }
}

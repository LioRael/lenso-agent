use lenso::CtxExt;
use lenso::host::{Host as FrameworkHost, HostBuilder as FrameworkHostBuilder};
use lenso_agent_loop_plugin::{AgentBehaviorProvenance, TurnModelSelection};
use lenso_agent_native_support::WorkspaceScope;
use lenso_app_authoring::PluginConfigurationAuthority;
use lenso_app_plan::{
    RequestAdmissionPlan, ResolvedAppPlan,
    authoring::{
        HostBinding, HostCatalog, HostDefaultPlugin, HostPluginConfiguration, HostPluginRelease,
        HostSlot, PluginInstanceId, PluginRootSnapshot, resolve_plugin_root,
    },
};
use lenso_bun_adapter::{BunAdapter, BunCapabilityCodec};
use lenso_capability_agent::{Agent, AgentJsonCodec, CAPABILITY_ID as AGENT_CAPABILITY_ID};
use lenso_capability_agent_artifact::ArtifactJsonCodec;
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
use lenso_capability_agent_model::{
    CATALOG_OPERATION as MODEL_CATALOG_OPERATION, CatalogRequest as ModelCatalogRequest,
    ModelCatalog, ModelJsonCodec,
};
use lenso_capability_agent_model_selection::{
    CAPABILITY_ID as MODEL_SELECTION_CAPABILITY_ID, ModelSelectionJsonCodec,
};
use lenso_capability_agent_oauth_access::OauthAccessJsonCodec;
use lenso_capability_agent_prompt::PromptJsonCodec;
use lenso_capability_agent_session::{
    APPEND_OPERATION, AppendSessionRequest, AppendSessionRequestEventsItem,
    AppendSessionRequestEventsItemKind, LIST_OPERATION, ListSessionsRequest, ListSessionsResponse,
    OPEN_OPERATION, OpenSessionRequest, READ_OPERATION, RENAME_OPERATION, ReadSessionRequest,
    ReadSessionResponse, ReadSessionResponseEventsItemKind, RenameError, RenameSessionRequest,
    RenameSessionResponse, SessionAppend, SessionJsonCodec, SessionList, SessionOpen, SessionRead,
    SessionRename,
};
use lenso_capability_agent_session_control::{
    COMPACT_SESSION_OPERATION, CompactSessionRequest, CompactSessionResponse, SessionControl,
};
use lenso_capability_agent_session_presentation::SessionPresentationJsonCodec;
use lenso_capability_agent_task_supervisor::{
    SNAPSHOT_OPERATION as TASK_SNAPSHOT_OPERATION, SnapshotError as TaskSnapshotError,
    SnapshotRequest as TaskSnapshotRequest, SnapshotResponse as TaskSnapshotResponse,
    TaskSupervisor,
};
use lenso_capability_agent_tool_hook::ToolHookJsonCodec;
use lenso_capability_agent_tool_provider::ToolProviderJsonCodec;
use lenso_capability_agent_tools::{
    CATALOG_OPERATION, CatalogRequest, CatalogResponseToolsItem, ToolsCatalog, ToolsJsonCodec,
};
use lenso_capability_agent_turn_input::TurnInputJsonCodec;
use lenso_capability_agent_user_interaction::{
    ANSWER_OPERATION, AnswerRequest, CAPABILITY_ID as USER_INTERACTION_CAPABILITY_ID,
    InteractionAnswer, InteractiveSurface, PENDING_OPERATION, PendingInteraction, PendingRequest,
    UserInteractionAnswer, UserInteractionJsonCodec, UserInteractionPending,
};
use lenso_capability_agent_workspace_read::WorkspaceReadJsonCodec;
use lenso_capability_terminal_command::{
    CATALOG_OPERATION as TERMINAL_CATALOG_OPERATION, CatalogRequest as TerminalCatalogRequest,
    CatalogResponse as TerminalCatalogResponse, CommandCatalog, CommandExecute,
    EXECUTE_OPERATION as TERMINAL_EXECUTE_OPERATION, ExecuteOpen as TerminalExecuteOpen,
};
use lenso_capability_tui_panel::{
    Panel, PanelItem, SNAPSHOT_OPERATION, SnapshotRequest, validate_snapshot_panels,
};
use lenso_capability_tui_suggestion::{
    SNAPSHOT_OPERATION as SUGGESTION_SNAPSHOT_OPERATION,
    SnapshotRequest as SuggestionSnapshotRequest, Suggestion as TuiSuggestion,
    SuggestionItem as Suggestion, validate_snapshot_suggestions,
};
use lenso_kernel::{
    CancellationToken, ExecutionAdapterCatalog, InvocationContext, NativeApp, NativeRequestHandle,
    NativeStream, NativeStreamHandle,
};
use lenso_native_adapter::NativePluginRegistry;
use lenso_plugin_control_plane::{
    AdapterProfile, AppGenerationSpec, AppGenerationTransitionSpec, CanonicalDocument,
    CatalogFactory, ControlLifecycle, ControlPlaneError, ControlStateStore, DurableControlState,
    DurableGenerationRoute, EmbeddedPlugin, HostBuildManifest, HostExecutionPolicy,
    KernelGenerationRuntime, MultiExecutionCatalogFactory, PlanGenerationInput, ReplacementMode,
    ResolvedGeneration, RolloutPolicy, resolve_plan_generation, sha256_digest,
};
use sha2::{Digest, Sha256};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub use crate::online_generation::{
    OnlineGenerationEvent, OnlineGenerationEventPage, OnlineGenerationEventRecord,
    OnlineGenerationRejectionObservation, OnlineGenerationSelection, OnlineGenerationSnapshot,
};
use crate::online_generation::{OnlineGenerationEventLog, OnlineGenerationTracker};
use crate::runtime_state::{LedgerControlStateStore, RuntimeAttachment, RuntimeState};
use crate::{
    AgentDirectories, AgentSurfaceKind, official_prompts,
    plugin_configuration_authority::{
        BRIDGE_PLUGIN_ID, BRIDGE_PLUGIN_VERSION, PluginConfigurationAuthorityBridgeFactory,
        bridge_descriptor,
    },
};

mod online_reconciler;
use online_reconciler::GenerationReconciler;
pub use online_reconciler::OnlineReconcileTelemetry;

const APP_ID: &str = "lenso.agent.harness";
const GENERATION_SPEC_DIGEST_EXTENSION: &str = "lenso.app.generation-spec-digest@1";
const DEFAULT_MODEL: &str = "gpt-5.6-luna";
const HOST_BUILD_HASH_BUFFER_BYTES: usize = 256 * 1024;
static HOST_BUILD_HASHES: AtomicU64 = AtomicU64::new(0);
static HOST_BUILD_HASHED_BYTES: AtomicU64 = AtomicU64::new(0);
static HOST_BUILD_LOCATE_MICROS: AtomicU64 = AtomicU64::new(0);
static HOST_BUILD_OPEN_MICROS: AtomicU64 = AtomicU64::new(0);
static HOST_BUILD_HASH_MICROS: AtomicU64 = AtomicU64::new(0);

/// Cumulative process-local evidence for exact Host executable identity work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HostBuildIdentityTelemetry {
    pub hashes: u64,
    pub hashed_bytes: u64,
    pub locate_micros: u64,
    pub open_micros: u64,
    pub hash_micros: u64,
}

/// Returns cumulative process-local Host executable identity telemetry.
pub fn host_build_identity_telemetry() -> HostBuildIdentityTelemetry {
    HostBuildIdentityTelemetry {
        hashes: HOST_BUILD_HASHES.load(Ordering::Relaxed),
        hashed_bytes: HOST_BUILD_HASHED_BYTES.load(Ordering::Relaxed),
        locate_micros: HOST_BUILD_LOCATE_MICROS.load(Ordering::Relaxed),
        open_micros: HOST_BUILD_OPEN_MICROS.load(Ordering::Relaxed),
        hash_micros: HOST_BUILD_HASH_MICROS.load(Ordering::Relaxed),
    }
}
const DEFAULT_AGENT_INSTRUCTION: &str = r"Work persistently toward the user's requested outcome. Answer simple requests directly. When correctness depends on workspace or runtime facts, inspect them with the supplied Tools, distinguish observation from inference, and never claim an action or validation that did not happen.

For longer work, state the immediate next action before Tool use and send brief progress updates when the direction or status changes. Continue until the outcome is complete or a concrete blocker requires the user. Finish with the outcome first, then the material evidence or validation, and any remaining blocker.";
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
const TUI_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(2);
const CONTEXT_SOURCE_TIMEOUT: Duration = Duration::from_secs(10);
const TERMINAL_CATALOG_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TUI_PANELS: usize = 64;
const MAX_TUI_PANEL_BYTES: usize = 262_144;
const MAX_TUI_SUGGESTIONS: usize = 2_112;
const MAX_TUI_SUGGESTION_BYTES: usize = 2_097_152;

static NEXT_ROOT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub fn online_reconcile_telemetry() -> OnlineReconcileTelemetry {
    online_reconciler::telemetry()
}

#[derive(Debug)]
struct AgentCatalogFactory {
    plugin_configuration_authority: Option<Arc<dyn PluginConfigurationAuthority>>,
}

impl CatalogFactory for AgentCatalogFactory {
    fn catalog(
        &self,
        generation: &ResolvedGeneration,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        let (mut registry, _) = native_host_build();
        if let Some(authority) = &self.plugin_configuration_authority {
            registry = registry.with_factory(PluginConfigurationAuthorityBridgeFactory::new(
                Arc::clone(authority),
            ));
        }
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

#[derive(Clone, Debug)]
struct DesiredGeneration {
    plugin_root_revision: String,
    resolution_authority_digest: String,
    desired_state_digest: String,
    plan_digest: String,
    generation: ResolvedGeneration,
}

impl DesiredGeneration {
    fn selection(&self) -> OnlineGenerationSelection {
        OnlineGenerationSelection::new(
            self.plugin_root_revision.clone(),
            self.desired_state_digest.clone(),
            self.generation.spec.digest().to_owned(),
            self.plan_digest.clone(),
            self.generation.plan.clone(),
        )
    }
}

impl HostBuildIdentity {
    pub(crate) fn current() -> Result<Self, String> {
        let locate_started = Instant::now();
        let executable = env::current_exe()
            .map_err(|error| format!("failed to locate Host executable: {error}"))?;
        HOST_BUILD_LOCATE_MICROS.fetch_add(elapsed_micros(locate_started), Ordering::Relaxed);
        Self::from_path(&executable)
    }

    fn from_path(executable: &Path) -> Result<Self, String> {
        let open_started = Instant::now();
        let mut file = fs::File::open(executable).map_err(|error| {
            format!(
                "failed to read Host executable {}: {error}",
                executable.display()
            )
        })?;
        HOST_BUILD_OPEN_MICROS.fetch_add(elapsed_micros(open_started), Ordering::Relaxed);
        let hash_started = Instant::now();
        let mut hasher = Sha256::new();
        let mut bytes = 0_u64;
        let mut buffer = vec![0_u8; HOST_BUILD_HASH_BUFFER_BYTES];
        loop {
            let read = file.read(&mut buffer).map_err(|error| {
                format!(
                    "failed to read Host executable {}: {error}",
                    executable.display()
                )
            })?;
            if read == 0 {
                break;
            }
            let read_u64 = u64::try_from(read)
                .map_err(|_| "Host executable byte count overflowed".to_owned())?;
            bytes = bytes
                .checked_add(read_u64)
                .ok_or_else(|| "Host executable byte count overflowed".to_owned())?;
            hasher.update(&buffer[..read]);
        }
        HOST_BUILD_HASHES.fetch_add(1, Ordering::Relaxed);
        HOST_BUILD_HASHED_BYTES.fetch_add(bytes, Ordering::Relaxed);
        HOST_BUILD_HASH_MICROS.fetch_add(elapsed_micros(hash_started), Ordering::Relaxed);
        Ok(Self {
            executable_digest: format!("sha256:{:x}", hasher.finalize()),
        })
    }
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
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
    profile_name: Rc<RefCell<Option<String>>>,
    authoring_managed: bool,
    reconciler: Option<GenerationReconciler>,
    reconcile_events: Rc<RefCell<OnlineGenerationEventLog>>,
    online_generation: Rc<RefCell<OnlineGenerationTracker>>,
    legacy_event_cursor: Cell<u64>,
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
        plugin_configuration_authority: Option<Arc<dyn PluginConfigurationAuthority>>,
    ) -> Result<Self, String> {
        let resolved_plan: ResolvedAppPlan = serde_json::from_slice(plan_bytes)
            .map_err(|error| format!("failed to decode the resolved App Plan: {error}"))?;
        let runtime_state = RuntimeState::open(store_root)?;
        let runtime_attachment = runtime_state.attach(surface)?;
        let _authority_fence = runtime_attachment.authority_snapshot()?;
        let mut initial =
            resolve_and_record_current_generation(plan_bytes, store_root, &host_build)?;
        let generation = initial.generation.clone();
        let store = runtime_attachment.control_store();
        let durable = store.load(APP_ID).map_err(control_error)?;
        let runtime =
            KernelGenerationRuntime::new(agent_catalog_factory(plugin_configuration_authority));
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
        // A Provider may publish its first validated external catalog while the
        // initial candidate crosses the Ready Gate. Resolve once more before
        // exposing the Host so every routable Turn is bound to those exact bytes.
        let bootstrapped =
            resolve_and_record_current_generation(plan_bytes, store_root, &host_build)?;
        if bootstrapped.generation.spec.digest() != initial.generation.spec.digest() {
            let transition = maintenance_transition(&initial.generation, &bootstrapped.generation)
                .map_err(control_error)?;
            if let Err(error) = host
                .transition(transition, bootstrapped.generation.clone(), BTreeMap::new())
                .await
            {
                let _ = host.shutdown().await;
                return Err(control_error(error));
            }
            initial = bootstrapped;
        }
        if let Err(error) = runtime_attachment.state().confirm_legacy_migration() {
            let _ = host.shutdown().await;
            return Err(error);
        }
        let client = host.controller();
        let reconcile_events = Rc::new(RefCell::new(OnlineGenerationEventLog::default()));
        let online_generation = Rc::new(RefCell::new(OnlineGenerationTracker::new(
            initial.selection(),
        )));
        let authoring_managed =
            plan_is_authoring_managed(plan_bytes, store_root, profile_name.as_deref());
        let profile_name = Rc::new(RefCell::new(profile_name));
        let reconciler = online_reconciler::start(
            client.clone(),
            store_root.to_path_buf(),
            host_build,
            profile_name.clone(),
            authoring_managed,
            reconcile_events.clone(),
            online_generation.clone(),
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
            online_generation,
            legacy_event_cursor: Cell::new(0),
        })
    }

    /// Returns the immutable App Plan selected when this Host started.
    pub const fn resolved_plan(&self) -> &ResolvedAppPlan {
        &self.resolved_plan
    }

    /// Resolves the current author-owned Plugin Root without changing runtime authority.
    pub fn desired_plan(&self) -> Result<ResolvedAppPlan, String> {
        self.desired_plan_and_contents().map(|(plan, _)| plan)
    }

    fn desired_plan_and_contents(
        &self,
    ) -> Result<(ResolvedAppPlan, crate::plugin_root::PluginRootContents), String> {
        if !self.authoring_managed {
            return Err(
                "this Host runs an exact diagnostic Plan, not an author-managed Plugin Root"
                    .to_owned(),
            );
        }
        let directories = directories_for_store_root(self.runtime.state().root())?;
        let root = crate::plugin_root::snapshot_with_resources(&directories.plugins())?;
        let selected_profile = self.profile_name.borrow().clone();
        let plan = if let Some(profile_name) = selected_profile.as_deref() {
            let profile =
                crate::profile::select(profile_name, root.root(), &directories.profiles())?;
            resolve_host_plan_for_agent_in(&directories, profile.root(), profile.agent())?
        } else {
            resolve_host_plan_in(&directories, root.root())?
        };
        Ok((plan, root))
    }

    fn retained_generation_plan(
        &self,
        generation_spec_digest: &str,
    ) -> Option<Rc<ResolvedAppPlan>> {
        self.online_generation
            .borrow()
            .retained_plan(generation_spec_digest)
    }

    /// Projects the linked Model Providers and the exact selection in this App.
    pub async fn provider_model_catalog(&self) -> Result<crate::ProviderModelCatalog, String> {
        let route = self.host.route().await.map_err(control_error)?;
        let plan = self
            .retained_generation_plan(route.generation_spec_digest())
            .ok_or_else(|| {
                format!(
                    "active Generation `{}` has no retained Provider/Model catalog authority",
                    route.generation_spec_digest()
                )
            })?;
        let directories = directories_for_store_root(self.runtime.state().root())?;
        let host = linked_host_catalog_in(&directories)?;
        let agent_provider = selected_surface_agent_provider(&plan)?;
        let catalog = selected_model_catalog(&route, &agent_provider).await?;
        crate::provider_catalog::project(
            &host,
            &plan,
            route.generation_spec_digest(),
            Some(&catalog),
        )
    }

    /// Snapshots explicit disabled Instance markers for management surfaces.
    pub fn disabled_plugin_instances(&self) -> Result<Vec<PluginInstanceId>, String> {
        let directories = directories_for_store_root(self.runtime.state().root())?;
        Ok(crate::plugin_root::snapshot(&directories.plugins())?
            .disabled()
            .to_vec())
    }

    /// Reopens reconciliation after one explicit, successfully committed authoring mutation.
    pub fn reopen_plugin_reconciliation(&self) -> Result<(), String> {
        self.reconciler
            .as_ref()
            .ok_or_else(|| "Generation Reconciler is not available".to_owned())?
            .reopen()
    }

    /// Selects an authoring Profile and activates it through the ordinary Ready Gate.
    /// `None` selects the default Plugin Root composition.
    pub async fn select_profile(&self, profile_name: Option<String>) -> Result<(), String> {
        self.reconciler
            .as_ref()
            .ok_or_else(|| "Generation Reconciler is not available".to_owned())?
            .select_profile(profile_name)
            .await
    }

    /// Returns the Profile currently selected by this Host surface.
    pub fn selected_profile(&self) -> Option<String> {
        self.profile_name.borrow().clone()
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

    /// Pins one editor-submitted ACP Turn to the active App Generation.
    pub async fn lease_acp_turn(&self) -> Result<TurnGeneration, String> {
        self.lease_turn_for("acp").await
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

    /// Pins CLI command discovery and execution to one immutable App Generation.
    pub async fn lease_cli_terminal(&self) -> Result<TerminalGeneration, String> {
        self.lease_terminal("lenso.terminal.cli/cli").await
    }

    /// Returns no lease when the selected App intentionally omits the CLI terminal consumer.
    pub async fn try_lease_cli_terminal(&self) -> Result<Option<TerminalGeneration>, String> {
        self.try_lease_terminal("lenso.terminal.cli/cli").await
    }

    /// Pins TUI command discovery and execution to one immutable App Generation.
    pub async fn lease_tui_terminal(&self) -> Result<TerminalGeneration, String> {
        self.lease_terminal("lenso.terminal.tui/tui").await
    }

    /// Pins Web command discovery and execution to one immutable App Generation.
    pub async fn lease_web_terminal(&self) -> Result<TerminalGeneration, String> {
        self.lease_terminal("lenso.terminal.web/web").await
    }

    /// Returns no lease when the selected App intentionally omits the Web terminal consumer.
    pub async fn try_lease_web_terminal(&self) -> Result<Option<TerminalGeneration>, String> {
        self.try_lease_terminal("lenso.terminal.web/web").await
    }

    /// Pins any explicitly named terminal consumer Instance to one Generation.
    ///
    /// Custom Hosts may compose multiple CLI or TUI consumer Instances and
    /// lease each one by its canonical `plugin-id/instance-name` identity.
    pub async fn lease_terminal(
        &self,
        consumer_instance: &str,
    ) -> Result<TerminalGeneration, String> {
        self.try_lease_terminal(consumer_instance)
            .await?
            .ok_or_else(|| {
                format!("leased Generation has no terminal consumer `{consumer_instance}`")
            })
    }

    /// Optionally leases an explicitly named terminal consumer Instance.
    pub async fn try_lease_terminal(
        &self,
        consumer_instance: &str,
    ) -> Result<Option<TerminalGeneration>, String> {
        let route = self.host.route().await.map_err(control_error)?;
        let catalog = route
            .target()
            .optional_handle::<CommandCatalog>(consumer_instance);
        let execute = route
            .target()
            .optional_stream_handle::<CommandExecute>(consumer_instance);
        match (catalog, execute) {
            (None, None) => Ok(None),
            (Some(catalog), Some(execute)) => Ok(Some(TerminalGeneration {
                route,
                catalog: Rc::new(catalog),
                execute: Rc::new(execute),
            })),
            _ => Err(format!(
                "leased Generation has an incomplete terminal route for `{consumer_instance}`"
            )),
        }
    }

    /// Snapshots Prompt and Resource metadata explicitly visible to the CLI surface.
    pub async fn cli_context_sources(&self) -> Result<ContextSnapshotResponse, String> {
        self.context_sources("lenso.agent.cli/cli").await
    }

    /// Snapshots Prompt and Resource metadata explicitly visible to the TUI surface.
    pub async fn tui_context_sources(&self) -> Result<ContextSnapshotResponse, String> {
        self.context_sources("lenso.agent.tui/tui").await
    }

    /// Snapshots Prompt and Resource metadata explicitly visible to the Web surface.
    pub async fn web_context_sources(&self) -> Result<ContextSnapshotResponse, String> {
        self.context_sources("lenso.agent.web/web").await
    }

    /// Reads the typed child-task projection visible to the TUI surface.
    pub async fn tui_task_snapshot(&self) -> Result<TaskSnapshotResponse, String> {
        self.task_snapshot("lenso.agent.tui/tui").await
    }

    /// Reads the typed child-task projection visible to the Web surface.
    pub async fn web_task_snapshot(&self) -> Result<TaskSnapshotResponse, String> {
        self.task_snapshot("lenso.agent.web/web").await
    }

    async fn task_snapshot(&self, consumer_instance: &str) -> Result<TaskSnapshotResponse, String> {
        let route = self.host.route().await.map_err(control_error)?;
        task_snapshot_on_route(&route, consumer_instance).await
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

    /// Renders one user-selected Context Prompt for the Web surface.
    pub async fn render_web_context_prompt(
        &self,
        request: RenderPromptRequest,
    ) -> Result<RenderPromptResponse, String> {
        self.render_context_prompt("lenso.agent.web/web", request)
            .await
    }

    /// Reads one application-selected Context Resource for the Web surface.
    pub async fn read_web_context_resource(
        &self,
        request: ReadResourceRequest,
    ) -> Result<ReadResourceResponse, String> {
        self.read_context_resource("lenso.agent.web/web", request)
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
        let consumer_instance = surface_consumer_instance(consumer_instance)?;
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
        let (behavior_digest, resolved_turn_profile, model_catalog) = {
            let plan = self
                .retained_generation_plan(route.generation_spec_digest())
                .ok_or_else(|| {
                    format!(
                        "active Generation `{}` has no retained Agent behavior authority",
                        route.generation_spec_digest()
                    )
                })?;
            let behavior_digest = agent_behavior_digest(&plan, &agent_provider)?;
            let directories = directories_for_store_root(self.runtime.state().root())?;
            let host = linked_host_catalog_in(&directories)?;
            let provider_catalog = selected_model_catalog(&route, &agent_provider).await?;
            let catalog = crate::provider_catalog::project(
                &host,
                &plan,
                route.generation_spec_digest(),
                Some(&provider_catalog),
            )?;
            let resolved_turn_profile = catalog.resolved_turn_profile.clone().ok_or_else(|| {
                "leased Generation Agent has no resolved Turn model profile".to_owned()
            })?;
            (behavior_digest, resolved_turn_profile, catalog)
        };
        let tools_catalog = route
            .target()
            .handle::<ToolsCatalog>(&agent_provider)
            .map_err(|error| {
                format!("leased Generation Agent has no Tool catalog route: {error:?}")
            })?;
        let has_model_selection = agent_has_model_selection(&route, &agent_provider)?;
        let has_session_control = surface_dependencies.bindings().iter().any(|binding| {
            binding.capability_id() == lenso_capability_agent_session_control::CAPABILITY_ID
        });
        let session_control =
            session_control_handle(&route, consumer_instance, has_session_control)?;
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
            session_control,
            has_model_selection,
            behavior_digest,
            resolved_turn_profile,
            model_catalog,
        })
    }

    /// Snapshots every TUI panel provider in deterministic resolved order.
    pub async fn tui_panels(&self) -> Result<Vec<PanelItem>, String> {
        let route = self.host.route().await.map_err(control_error)?;
        let handle = route
            .target()
            .many_handle::<Panel>("lenso.terminal.tui/tui")
            .map_err(|error| format!("leased Generation has no TUI panel route: {error:?}"))?;
        let cancellation = CancellationToken::new();
        let context = route
            .target()
            .invocation_context_after(TUI_SNAPSHOT_TIMEOUT, cancellation.clone());
        let invocation =
            handle.invoke_many_with_context(SNAPSHOT_OPERATION, context, SnapshotRequest {});
        let responses = match tokio::time::timeout(TUI_SNAPSHOT_TIMEOUT, invocation).await {
            Ok(result) => {
                result.map_err(|error| format!("TUI panel snapshot failed: {error:?}"))?
            }
            Err(_) => {
                cancellation.cancel();
                return Err("TUI panel snapshot timed out".to_owned());
            }
        };
        let mut panels = Vec::new();
        for response in responses {
            let response = response
                .map_err(|error| format!("TUI panel provider rejected its snapshot: {error:?}"))?;
            validate_snapshot_panels(&response.panels).map_err(|error| {
                format!("TUI panel provider returned an invalid snapshot: {error}")
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
            .many_handle::<TuiSuggestion>("lenso.terminal.tui/tui")
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
        if let Some(reconciler) = self.reconciler.take() {
            reconciler.shutdown().await?;
        }
        self.host.suspend().await.map_err(control_error)?;
        self.runtime.release();
        crate::provenance::try_apply_automatic_gc(
            self.runtime.state().root(),
            &self.session_database,
        )?;
        Ok(())
    }

    /// Returns the current Desired/Preparing/Active Generation projection.
    pub fn online_generation_snapshot(&self) -> OnlineGenerationSnapshot {
        self.online_generation.borrow().snapshot()
    }

    /// Reads bounded reconcile events after one caller-owned monotonic cursor.
    pub fn online_generation_events(&self, after: Option<u64>) -> OnlineGenerationEventPage {
        self.reconcile_events.borrow().after(after)
    }

    /// Records degradation observed by an external Plugin configuration watcher
    /// in the same cursor-ordered journal as local reconciliation failures.
    pub fn report_plugin_watch_degraded(&self, detail: impl Into<String>) -> u64 {
        self.reconcile_events
            .borrow_mut()
            .push(OnlineGenerationEvent::WatchDegraded {
                detail: detail.into(),
            })
    }

    /// Compatibility projection for terminal consumers that previously drained
    /// the shared queue. Each `AgentApp` now owns a private legacy cursor, so this
    /// method cannot destroy events needed by another presentation surface.
    pub fn take_online_generation_events(&self) -> Vec<OnlineGenerationEvent> {
        let page = self.online_generation_events(Some(self.legacy_event_cursor.get()));
        self.legacy_event_cursor.set(page.cursor());
        page.events()
            .iter()
            .cloned()
            .map(OnlineGenerationEventRecord::into_event)
            .collect()
    }
}

fn session_control_handle(
    route: &DurableGenerationRoute<NativeApp>,
    consumer_instance: &str,
    required: bool,
) -> Result<Option<Rc<NativeRequestHandle<SessionControl>>>, String> {
    required
        .then(|| {
            route
                .target()
                .handle::<SessionControl>(consumer_instance)
                .map(Rc::new)
                .map_err(|error| {
                    format!("leased Generation has no Session Control route: {error:?}")
                })
        })
        .transpose()
}

fn selected_surface_agent_provider(plan: &ResolvedAppPlan) -> Result<String, String> {
    let providers = plan
        .capability_bindings()
        .iter()
        .filter(|binding| {
            binding.capability_id() == AGENT_CAPABILITY_ID
                && is_agent_surface(binding.consumer_instance())
        })
        .map(lenso_app_plan::CapabilityBinding::provider_instance)
        .collect::<BTreeSet<_>>();
    match providers.len() {
        1 => providers
            .into_iter()
            .next()
            .map(ToOwned::to_owned)
            .ok_or_else(|| "active Generation has no surface Agent provider".to_owned()),
        0 => Err("active Generation has no surface Agent provider".to_owned()),
        _ => Err("active Generation surfaces select more than one Agent provider".to_owned()),
    }
}

async fn selected_model_catalog(
    route: &DurableGenerationRoute<NativeApp>,
    agent_provider: &str,
) -> Result<lenso_capability_agent_model::CatalogResponse, String> {
    route
        .target()
        .handle::<ModelCatalog>(agent_provider)
        .map_err(|error| format!("Generation Agent has no Model catalog route: {error:?}"))?
        .invoke(MODEL_CATALOG_OPERATION, ModelCatalogRequest {})
        .await
        .map_err(|error| format!("Model catalog snapshot failed: {error:?}"))?
        .map_err(|error| format!("Model catalog snapshot was rejected: {error:?}"))
}

fn is_agent_surface(instance: &str) -> bool {
    matches!(
        instance,
        "lenso.agent.acp/acp"
            | "lenso.agent.cli/cli"
            | "lenso.agent.tui/tui"
            | "lenso.agent.telegram/telegram"
            | "lenso.agent.discord/discord"
            | "lenso.agent.web/web"
    )
}

fn surface_consumer_instance(surface: &str) -> Result<&'static str, String> {
    match surface {
        "acp" => Ok("lenso.agent.acp/acp"),
        "cli" => Ok("lenso.agent.cli/cli"),
        "tui" => Ok("lenso.agent.tui/tui"),
        "telegram" => Ok("lenso.agent.telegram/telegram"),
        "discord" => Ok("lenso.agent.discord/discord"),
        "web" => Ok("lenso.agent.web/web"),
        other => Err(format!("unknown Agent surface `{other}")),
    }
}

fn agent_has_model_selection(
    route: &DurableGenerationRoute<NativeApp>,
    agent_provider: &str,
) -> Result<bool, String> {
    Ok(route
        .target()
        .dependencies(agent_provider)
        .map_err(|error| format!("leased Generation has no Agent dependencies: {error:?}"))?
        .bindings()
        .iter()
        .any(|binding| binding.capability_id() == MODEL_SELECTION_CAPABILITY_ID))
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
) -> Result<DesiredGeneration, String> {
    let directories = directories_for_store_root(store_root)?;
    let authority = crate::generation_authority::load_generation_authority_unfenced(store_root);
    let plugin_root = crate::plugin_root::snapshot(&directories.plugins())?;
    let plugin_root_revision = crate::plugin_root::revision(&plugin_root)?;
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
    desired_generation(
        plugin_root_revision,
        authority.resolution_authority_digest,
        generation,
    )
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

async fn task_snapshot_on_route(
    route: &DurableGenerationRoute<NativeApp>,
    consumer_instance: &str,
) -> Result<TaskSnapshotResponse, String> {
    let handle = route
        .target()
        .many_handle::<TaskSupervisor>(consumer_instance)
        .map_err(|error| format!("Task Supervisor snapshot route is unavailable: {error:?}"))?;
    let cancellation = CancellationToken::new();
    let context = route
        .target()
        .invocation_context_after(TUI_SNAPSHOT_TIMEOUT, cancellation.clone());
    let invocation =
        handle.invoke_many_with_context(TASK_SNAPSHOT_OPERATION, context, TaskSnapshotRequest {});
    let responses = match tokio::time::timeout(TUI_SNAPSHOT_TIMEOUT, invocation).await {
        Ok(result) => {
            result.map_err(|error| format!("Task Supervisor snapshot failed: {error:?}"))?
        }
        Err(_) => {
            cancellation.cancel();
            return Err("Task Supervisor snapshot timed out".to_owned());
        }
    };
    let mut tasks = Vec::new();
    let mut task_ids = BTreeSet::new();
    for response in responses {
        let response = match response {
            Ok(response) => response,
            Err(TaskSnapshotError::SnapshotInvalid) => {
                return Err("Task Supervisor rejected its snapshot".to_owned());
            }
            Err(TaskSnapshotError::Unknown(error)) => {
                return Err(format!("Task Supervisor rejected its snapshot: {error:?}"));
            }
        };
        for task in response.tasks {
            if !task_ids.insert(task.task_id.clone()) {
                return Err(format!("duplicate supervised task id `{}`", task.task_id));
            }
            tasks.push(task);
        }
    }
    if tasks.len() > 64 {
        return Err("Task Supervisor aggregate exceeded its 64-task limit".to_owned());
    }
    tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    Ok(TaskSnapshotResponse { tasks })
}
fn validate_tui_panels(panels: &[PanelItem]) -> Result<(), String> {
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

/// Immutable Generation lease shared by terminal discovery and execution.
#[derive(Debug)]
pub struct TerminalGeneration {
    route: DurableGenerationRoute<NativeApp>,
    catalog: Rc<NativeRequestHandle<CommandCatalog>>,
    execute: Rc<NativeStreamHandle<CommandExecute>>,
}

impl TerminalGeneration {
    /// Reads the exact catalog bound to this terminal surface Generation.
    pub async fn catalog(&self) -> Result<TerminalCatalogResponse, String> {
        let cancellation = CancellationToken::new();
        let context = self
            .route
            .target()
            .invocation_context_after(TERMINAL_CATALOG_TIMEOUT, cancellation.clone());
        let invocation = self.catalog.invoke_with_context(
            TERMINAL_CATALOG_OPERATION,
            context,
            TerminalCatalogRequest {},
        );
        match tokio::time::timeout(TERMINAL_CATALOG_TIMEOUT, invocation).await {
            Ok(result) => result
                .map_err(|error| format!("terminal command catalog failed: {error:?}"))?
                .map_err(|error| format!("terminal command catalog was rejected: {error:?}")),
            Err(_) => {
                cancellation.cancel();
                Err("terminal command catalog timed out".to_owned())
            }
        }
    }

    /// Opens one command stream against the same immutable Generation as the catalog.
    pub async fn execute(
        &self,
        request: TerminalExecuteOpen,
    ) -> Result<NativeStream<CommandExecute>, String> {
        self.execute_with_cancellation(request, CancellationToken::new())
            .await
    }

    /// Opens one cancellable command stream against this immutable Generation.
    pub async fn execute_with_cancellation(
        &self,
        request: TerminalExecuteOpen,
        cancellation: CancellationToken,
    ) -> Result<NativeStream<CommandExecute>, String> {
        let context = self.route.target().invocation_context(None, cancellation);
        self.execute
            .open_with_context(TERMINAL_EXECUTE_OPERATION, context, request)
            .await
            .map_err(|error| format!("terminal command stream failed to open: {error:?}"))?
            .map_err(|error| format!("terminal command was rejected: {error:?}"))
    }
}

#[derive(Debug)]
pub struct TurnGeneration {
    consumer_instance: String,
    route: DurableGenerationRoute<NativeApp>,
    handle: Rc<NativeStreamHandle<Agent>>,
    interaction: Option<UserInteractionSurfaceHandles>,
    interactive: bool,
    tools_catalog: Rc<NativeRequestHandle<ToolsCatalog>>,
    session_control: Option<Rc<NativeRequestHandle<SessionControl>>>,
    has_model_selection: bool,
    behavior_digest: String,
    resolved_turn_profile: crate::ResolvedTurnProfile,
    model_catalog: crate::ProviderModelCatalog,
}

#[derive(Debug)]
struct UserInteractionSurfaceHandles {
    pending: Rc<NativeRequestHandle<UserInteractionPending>>,
    answer: Rc<NativeRequestHandle<UserInteractionAnswer>>,
}

impl TurnGeneration {
    /// Reads child-task facts from this Turn's immutable Generation lease.
    pub async fn task_snapshot(&self) -> Result<TaskSnapshotResponse, String> {
        task_snapshot_on_route(&self.route, &self.consumer_instance).await
    }

    pub fn handle(&self) -> &NativeStreamHandle<Agent> {
        &self.handle
    }

    pub fn invocation_context(&self) -> Result<InvocationContext, String> {
        self.invocation_context_with_cancellation(CancellationToken::new())
    }

    /// Lists models admitted by the Provider Instance already bound to this Generation.
    pub fn available_models(&self) -> Vec<String> {
        self.model_catalog.selected_provider_models()
    }

    /// Returns the model selected by this immutable Generation lease.
    pub fn selected_model(&self) -> &str {
        &self.resolved_turn_profile.model
    }

    /// Returns the Generation's default reasoning selection, when configured.
    pub fn selected_reasoning_effort(&self) -> Option<&str> {
        self.resolved_turn_profile.reasoning_effort.as_deref()
    }

    /// Returns the Generation's default reasoning toggle, when configured.
    pub const fn selected_reasoning_enabled(&self) -> Option<bool> {
        self.resolved_turn_profile.reasoning_enabled
    }

    /// Returns the Generation's default reasoning token budget, when configured.
    pub const fn selected_reasoning_budget_tokens(&self) -> Option<u64> {
        self.resolved_turn_profile.reasoning_budget_tokens
    }

    /// Returns the Generation's default service tier, when configured.
    pub fn selected_service_tier(&self) -> Option<&str> {
        self.resolved_turn_profile.service_tier.as_deref()
    }

    /// Returns whether this Agent has one selected dynamic Model Selection provider.
    pub const fn supports_dynamic_model_selection(&self) -> bool {
        self.has_model_selection
    }

    /// Creates a root context selecting one admitted model for this Turn only.
    pub fn invocation_context_for_model(
        &self,
        model_id: &str,
    ) -> Result<InvocationContext, String> {
        self.invocation_context_for_model_options(Some(model_id), None, None)
    }

    /// Creates a root context with model-specific, catalog-validated inference controls.
    pub fn invocation_context_for_model_options(
        &self,
        model_id: Option<&str>,
        reasoning_effort: Option<&str>,
        service_tier: Option<&str>,
    ) -> Result<InvocationContext, String> {
        self.invocation_context_for_model_options_with_cancellation(
            model_id,
            reasoning_effort,
            service_tier,
            CancellationToken::new(),
        )
    }

    /// Creates a controllable root context with model-specific, catalog-validated controls.
    pub fn invocation_context_for_model_options_with_cancellation(
        &self,
        model_id: Option<&str>,
        reasoning_effort: Option<&str>,
        service_tier: Option<&str>,
        cancellation: CancellationToken,
    ) -> Result<InvocationContext, String> {
        self.invocation_context_for_model_controls_with_cancellation(
            model_id,
            reasoning_effort,
            None,
            None,
            service_tier,
            cancellation,
        )
    }

    /// Creates a controllable root context with one typed, catalog-validated reasoning control.
    pub fn invocation_context_for_model_controls_with_cancellation(
        &self,
        model_id: Option<&str>,
        reasoning_effort: Option<&str>,
        reasoning_enabled: Option<bool>,
        reasoning_budget_tokens: Option<u64>,
        service_tier: Option<&str>,
        cancellation: CancellationToken,
    ) -> Result<InvocationContext, String> {
        let model_id = model_id.unwrap_or(self.resolved_turn_profile.model.as_str());
        match self.model_catalog.resolve_model_controls(
            model_id,
            reasoning_effort,
            reasoning_enabled,
            reasoning_budget_tokens,
            service_tier,
        ) {
            Ok(profile) => self.invocation_context_with_profile(cancellation, &profile),
            Err(error)
                if model_id != self.resolved_turn_profile.model
                    && !self
                        .model_catalog
                        .selected_provider_models()
                        .iter()
                        .any(|candidate| candidate == model_id) =>
            {
                if !self.has_model_selection {
                    return Err(format!(
                        "{error}; no Model Selection Plugin is bound for dynamic policy `{model_id}`"
                    ));
                }
                let candidates = self.model_catalog.resolve_model_control_candidates(
                    reasoning_effort,
                    reasoning_enabled,
                    reasoning_budget_tokens,
                    service_tier,
                );
                if candidates.is_empty() {
                    return Err(
                        "no admitted model accepts the requested dynamic inference controls"
                            .to_owned(),
                    );
                }
                self.invocation_context_with_profile(cancellation, &self.resolved_turn_profile)?
                    .with_typed_extension(&TurnModelSelection {
                        policy: model_id.to_owned(),
                        candidates,
                    })
                    .map_err(|error| format!("failed to attach dynamic Model Selection: {error}"))
            }
            Err(error) => Err(error),
        }
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

    /// Compacts the current Session through the leased Agent's durable control boundary.
    pub async fn compact_session(
        &self,
        session_id: String,
    ) -> Result<CompactSessionResponse, String> {
        let control = self
            .session_control
            .as_ref()
            .ok_or_else(|| "this Agent surface has no Session Control route".to_owned())?;
        control
            .invoke_with_context(
                COMPACT_SESSION_OPERATION,
                self.invocation_context()?,
                CompactSessionRequest { session_id },
            )
            .await
            .map_err(|error| format!("Session Control invocation failed: {error:?}"))?
            .map_err(|error| format!("Session compaction was rejected: {error:?}"))
    }

    /// Creates a root invocation context whose lifetime can be controlled by
    /// the owning Surface.
    pub fn invocation_context_with_cancellation(
        &self,
        cancellation: CancellationToken,
    ) -> Result<InvocationContext, String> {
        self.invocation_context_with_profile(cancellation, &self.resolved_turn_profile)
    }

    fn invocation_context_with_profile(
        &self,
        cancellation: CancellationToken,
        profile: &crate::ResolvedTurnProfile,
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
        let workspace = env::current_dir()
            .map_err(|error| format!("failed to resolve the current Workspace: {error}"))?
            .into_os_string()
            .into_string()
            .map_err(|_| "the current Workspace path is not UTF-8".to_owned())?;
        if workspace.is_empty() || workspace.len() > 4_096 {
            return Err("the current Workspace path is outside the supported bound".to_owned());
        }
        let context = InvocationContext::new(request_id, None, cancellation)
            .with_extension(
                GENERATION_SPEC_DIGEST_EXTENSION,
                self.generation_spec_digest().as_bytes().to_vec(),
            )
            .map_err(|error| format!("failed to attach Generation provenance: {error}"))?
            .with_typed_extension(&WorkspaceScope {
                absolute_path: workspace,
            })
            .map_err(|error| format!("failed to attach Workspace scope: {error}"))?
            .with_typed_extension(&AgentBehaviorProvenance::new(self.behavior_digest.clone())?)
            .map_err(|error| format!("failed to attach Agent behavior provenance: {error}"))?
            .with_typed_extension(profile)
            .map_err(|error| format!("failed to attach resolved Turn profile: {error}"))?;
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

fn agent_behavior_digest(plan: &ResolvedAppPlan, agent: &str) -> Result<String, String> {
    let mut closure = BTreeSet::from([agent.to_owned()]);
    loop {
        let before = closure.len();
        for binding in plan.capability_bindings() {
            if closure.contains(binding.consumer_instance()) {
                closure.insert(binding.provider_instance().to_owned());
            }
        }
        if closure.len() == before {
            break;
        }
    }
    let instances = plan
        .plugin_instances()
        .iter()
        .filter(|instance| closure.contains(instance.instance_key()))
        .collect::<Vec<_>>();
    let bindings = plan
        .capability_bindings()
        .iter()
        .filter(|binding| {
            closure.contains(binding.consumer_instance())
                && closure.contains(binding.provider_instance())
        })
        .collect::<Vec<_>>();
    if !instances
        .iter()
        .any(|instance| instance.instance_key() == agent)
    {
        return Err(format!(
            "Agent behavior root `{agent}` is absent from the active Plan"
        ));
    }
    serde_json::to_vec(&(agent, instances, bindings))
        .map(|bytes| sha256_digest(&bytes))
        .map_err(|error| format!("failed to identify Agent behavior: {error}"))
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
    let resources =
        crate::model_catalog_resources::inject_selected_catalog_snapshot(&plan, resources)?;
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

fn desired_generation(
    plugin_root_revision: String,
    resolution_authority_digest: String,
    generation: ResolvedGeneration,
) -> Result<DesiredGeneration, String> {
    let (desired_state_digest, plan_digest) = desired_generation_identity(
        &resolution_authority_digest,
        &generation.plan,
        &generation.resources,
    )?;
    Ok(DesiredGeneration {
        plugin_root_revision,
        resolution_authority_digest,
        desired_state_digest,
        plan_digest,
        generation,
    })
}

pub(crate) fn desired_generation_identity(
    resolution_authority_digest: &str,
    plan: &ResolvedAppPlan,
    resources: &lenso_runtime_codec::InstanceResourceCatalog,
) -> Result<(String, String), String> {
    let plan_digest = app_plan_digest(plan)?;
    let resource_identity = resources
        .iter()
        .map(|(instance, snapshot)| (instance, snapshot.digest()))
        .collect::<Vec<_>>();
    let desired_state_digest = sha256_digest(
        &serde_json::to_vec(&(resolution_authority_digest, plan, resource_identity))
            .map_err(|error| format!("failed to identify desired Plugin state: {error}"))?,
    );
    Ok((desired_state_digest, plan_digest))
}

fn app_plan_digest(plan: &ResolvedAppPlan) -> Result<String, String> {
    serde_json::to_vec(plan)
        .map(|bytes| sha256_digest(&bytes))
        .map_err(|error| format!("failed to identify desired App Plan: {error}"))
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
    if built_in_plugins
        .iter()
        .any(|plugin| plugin.package_id == "lenso.agent.console-plugin-tools")
    {
        built_in_plugins.push(EmbeddedPlugin {
            package_id: BRIDGE_PLUGIN_ID.to_owned(),
            factory_identity: format!("{BRIDGE_PLUGIN_ID}@{BRIDGE_PLUGIN_VERSION}"),
            execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        });
    }
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
    let mut available = registry
        .factories()
        .map(|factory| factory.package_id().to_owned())
        .collect::<BTreeSet<_>>();
    let console_plugin_tools = available.contains("lenso.agent.console-plugin-tools");
    if console_plugin_tools {
        available.insert(BRIDGE_PLUGIN_ID.to_owned());
    }
    let defaults = host_catalog_defaults(directories, &available)
        .into_iter()
        .filter(|plugin| available.contains(plugin.id().plugin_id()))
        .collect::<Vec<_>>();
    let configurations = host_catalog_configurations(directories, &available)
        .into_iter()
        .filter(|configuration| available.contains(configuration.id().plugin_id()))
        .collect::<Vec<_>>();
    NativePluginRegistry::host_catalog([], [])
        .map(|native_catalog| {
            let mut releases = native_catalog.plugins().to_vec();
            if console_plugin_tools {
                releases.push(HostPluginRelease::new(bridge_descriptor()));
            }
            HostCatalog::new(host_catalog_slots(), releases, defaults)
                .with_configurations(configurations)
                .with_bindings(host_catalog_bindings(root_agent, &available))
        })
        .map_err(|error| format!("linked Host Catalog is invalid: {error:?}"))
}

fn host_catalog_slots() -> Vec<HostSlot> {
    vec![
        HostSlot::many("agents"),
        HostSlot::one("artifact").replaceable(),
        HostSlot::optional("auth"),
        HostSlot::optional("console"),
        HostSlot::optional("plugin-configuration-authority"),
        HostSlot::one("context-compactor").replaceable(),
        HostSlot::one("memory").replaceable(),
        HostSlot::many("surfaces"),
        HostSlot::many("tool-providers"),
        HostSlot::many("tool-hooks"),
        HostSlot::many("lifecycle-hooks"),
        HostSlot::one("http-fetch"),
        HostSlot::one("model").replaceable(),
        HostSlot::optional("model-selection").replaceable(),
        HostSlot::optional("process"),
        HostSlot::one("oauth-access").replaceable(),
        HostSlot::many("prompt-providers"),
        HostSlot::one("prompt-runtime"),
        HostSlot::many("tools-runtimes"),
        HostSlot::optional("secrets"),
        HostSlot::one("session").replaceable(),
        HostSlot::optional("session-presentation").replaceable(),
        HostSlot::optional("restricted-tools-runtime"),
        HostSlot::optional("terminal-command-runtime").replaceable(),
        HostSlot::many("terminal-command-providers"),
        HostSlot::many("tui-panels"),
        HostSlot::many("tui-suggestions"),
        HostSlot::one("user-interaction").replaceable(),
        HostSlot::optional("workspace-import-read"),
    ]
}

#[allow(
    clippy::too_many_lines,
    reason = "one catalog function keeps every Host default auditable together"
)]
fn host_catalog_defaults(
    directories: &AgentDirectories,
    available: &BTreeSet<String>,
) -> Vec<HostDefaultPlugin> {
    let mut defaults = agent_defaults(available);
    defaults.extend(default_interactive_plugins(directories));
    defaults.extend(default_boundary_plugins(directories));
    defaults.extend([
        HostDefaultPlugin::new("lenso.agent.acp", "acp"),
        HostDefaultPlugin::new("lenso.agent.cli", "cli"),
        HostDefaultPlugin::new("lenso.terminal.cli", "cli"),
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
        HostDefaultPlugin::new("lenso.agent.session-terminal", "sessions"),
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
        HostDefaultPlugin::new("lenso.terminal.command", "commands"),
        HostDefaultPlugin::new("lenso.terminal.tui", "tui"),
        HostDefaultPlugin::new("lenso.terminal.web", "web").disableable(),
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
                "root": ".",
                "delegated_root": directories.runtime().join("child-worktrees")
            }),
        ),
        HostDefaultPlugin::new("lenso.agent.workspace-read-tools", "restricted-read-tools"),
    ]);
    if available.contains("lenso.agent.console-plugin-tools") {
        defaults.extend([
            HostDefaultPlugin::new(BRIDGE_PLUGIN_ID, "selected"),
            default_plugin(
                "lenso.agent.console-plugin-tools",
                "default",
                serde_json::json!({
                    "max_output_bytes": 131_072
                }),
            )
            .disableable(),
            default_plugin(
                "lenso.agent.interactive-approval-hook",
                "default",
                console_interactive_approval_configuration(),
            )
            .disableable(),
        ]);
    }
    if available.contains("lenso.agent.console-instructions") {
        defaults.push(
            HostDefaultPlugin::new("lenso.agent.console-instructions", "default").disableable(),
        );
    }
    defaults
}

fn default_boundary_plugins(directories: &AgentDirectories) -> [HostDefaultPlugin; 2] {
    [
        default_plugin(
            "lenso.agent.artifact.file",
            "artifacts",
            serde_json::json!({
                "directory": directories.artifacts(),
                "max_artifact_bytes": 16_777_216,
                "max_total_bytes": 1_073_741_824_u64,
                "max_items": 4_096
            }),
        ),
        default_plugin(
            "lenso.agent.oauth.client-credentials",
            "oauth",
            serde_json::json!({
                "resources": [],
                "request_timeout_ms": 30_000,
                "refresh_margin_seconds": 60
            }),
        ),
    ]
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

fn default_interactive_plugins(directories: &AgentDirectories) -> [HostDefaultPlugin; 3] {
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
                "catalog_cache_path": directories.model_catalog_cache(),
                "catalog_max_stale_seconds": 86_400,
                "catalog_snapshot_path": directories.model_catalog_snapshot(),
                "catalog_refresh_seconds": 3_600,
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
                    "version": "1.1.0",
                    "kind": "instruction",
                    "content": DEFAULT_AGENT_INSTRUCTION
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

fn host_catalog_configurations(
    directories: &AgentDirectories,
    available: &BTreeSet<String>,
) -> Vec<HostPluginConfiguration> {
    let console_plugin_tools = available.contains("lenso.agent.console-plugin-tools");
    let mut configurations = local_tool_configurations(directories, console_plugin_tools);
    configurations.extend(model_and_auth_configurations(directories));
    configurations.extend(official_prompts::configurations());
    configurations.extend(["worker-a", "worker-b"].into_iter().map(|instance| {
        HostPluginConfiguration::new(
            "lenso.agent.loop",
            instance,
            serde_json::json!({
                "model": DEFAULT_MODEL,
                "max_parallel_tool_calls": 4,
                "max_output_tokens": 1024,
                "max_history_events": 200,
                "max_compaction_summary_characters": 8192,
                "max_memory_items": 8,
                "max_memory_characters": 16384
            }),
        )
    }));
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
                "catalog_cache_path": directories.model_catalog_cache(),
                "catalog_max_stale_seconds": 86_400,
                "catalog_snapshot_path": directories.model_catalog_snapshot(),
                "catalog_refresh_seconds": 3_600,
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

#[allow(
    clippy::too_many_lines,
    reason = "one catalog function keeps every local Tool default auditable together"
)]
fn local_tool_configurations(
    directories: &AgentDirectories,
    console_plugin_tools: bool,
) -> Vec<HostPluginConfiguration> {
    let mut configurations = vec![
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
                    "list_worktrees", "review_worktree",
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
                "root": ".",
                "delegated_root": directories.runtime().join("child-worktrees")
            }),
        ),
        sandbox_process_configuration(directories),
        host_plugin_configuration(
            "lenso.agent.process-tools",
            serde_json::json!({
                "default_timeout_ms": 120_000,
                "max_background_processes": 8,
                "max_background_log_bytes": 262_144
            }),
        ),
        host_plugin_configuration(
            "lenso.agent.subagent-tools",
            serde_json::json!({
                "max_output_bytes": 1_048_576,
                "max_task_bytes": 262_144,
                "max_tasks": 8,
                "require_worktree_provider": false
            }),
        ),
        HostPluginConfiguration::new(
            "lenso.agent.subagent-tools",
            "worktree",
            serde_json::json!({
                "max_output_bytes": 1_048_576,
                "max_task_bytes": 262_144,
                "max_tasks": 8,
                "require_worktree_provider": true
            }),
        ),
        host_plugin_configuration(
            "lenso.agent.worktree-provider",
            serde_json::json!({
                "repository_root": ".",
                "worktree_root": directories.runtime().join("child-worktrees"),
                "mutation_agents": [
                    "lenso.agent.loop/worker-a",
                    "lenso.agent.loop/worker-b"
                ],
                "max_worktrees": 8,
                "timeout_ms": 120_000,
                "max_review_bytes": 1_048_576
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
                "root": ".",
                "delegated_root": directories.runtime().join("child-worktrees")
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
    ];
    if console_plugin_tools {
        configurations.retain(|configuration| {
            configuration.id().plugin_id() != "lenso.agent.interactive-approval-hook"
        });
    }
    configurations
}

fn console_interactive_approval_configuration() -> serde_json::Value {
    serde_json::json!({
        "allow_tools": [
            "inspect_app", "list_plugins", "inspect_plugin", "check_plugin_change"
        ],
        "ask_tools": ["apply_plugin_change"],
        "default_decision": "ask",
        "deny_tools": [],
        "max_preview_bytes": 16_384
    })
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
            "delegated_root": directories.runtime().join("child-worktrees"),
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

#[allow(
    clippy::too_many_lines,
    reason = "one Host Catalog function keeps immutable Agent and Tool authority wiring auditable together"
)]
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
    let mutation_agents = [
        PluginInstanceId::new("lenso.agent.loop", "worker-a"),
        PluginInstanceId::new("lenso.agent.loop", "worker-b"),
    ];
    let tool_admission = RequestAdmissionPlan::new(0, 4);
    let mut bindings = vec![
        HostBinding::to_instance(
            root_agent.clone(),
            "lenso.agent.tools@2",
            root_tools.clone(),
        )
        .with_admission(tool_admission),
    ];
    if available.contains("lenso.agent.console-plugin-tools") {
        bindings.push(
            HostBinding::to_instance(
                PluginInstanceId::new("lenso.agent.console-plugin-tools", "default"),
                lenso_capability_agent_plugin_configuration_authority::CAPABILITY_ID,
                PluginInstanceId::new(BRIDGE_PLUGIN_ID, "selected"),
            )
            .with_admission(RequestAdmissionPlan::new(4, 1)),
        );
    }
    if available.contains("lenso.agent.subagent-tools")
        && available.contains("lenso.agent.workspace-read-tools")
    {
        bindings.extend(child_agents.iter().cloned().map(|child_agent| {
            HostBinding::to_instance(child_agent, "lenso.agent.tools@2", restricted_tools.clone())
                .with_admission(tool_admission)
        }));
    }
    if available.contains("lenso.agent.worktree-provider") {
        bindings.extend(mutation_agents.iter().cloned().map(|child_agent| {
            HostBinding::to_instance(
                child_agent,
                "lenso.agent.tools@2",
                PluginInstanceId::new("lenso.agent.tools", "worker-tools"),
            )
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
        PluginInstanceId::new("lenso.agent.acp", "acp"),
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
    if available.contains("lenso.agent.tui") {
        bindings.push(HostBinding::to_instance(
            PluginInstanceId::new("lenso.agent.tui", "tui"),
            "lenso.agent.session-control@1",
            selected_agent.clone(),
        ));
    }
    if available.contains("lenso.agent.subagent-tools") {
        for surface in [
            PluginInstanceId::new("lenso.agent.tui", "tui"),
            PluginInstanceId::new("lenso.agent.web", "web"),
        ]
        .into_iter()
        .filter(|surface| available.contains(surface.plugin_id()))
        {
            bindings.push(
                HostBinding::new(surface, "lenso.agent.task-supervisor@2", "tool-providers")
                    .with_admission(RequestAdmissionPlan::new(8, 4)),
            );
        }
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
            child_agents.iter().cloned(),
        ));
    }
    if available.contains("lenso.agent.subagent-tools")
        && available.contains("lenso.agent.worktree-provider")
    {
        let subagent_tools = PluginInstanceId::new("lenso.agent.subagent-tools", "worktree");
        let worktree_children = child_agents
            .iter()
            .chain(mutation_agents.iter())
            .cloned()
            .collect::<Vec<_>>();
        bindings.push(HostBinding::to_instances(
            subagent_tools.clone(),
            "lenso.agent@3",
            worktree_children.iter().cloned(),
        ));
        bindings.push(HostBinding::to_instances(
            subagent_tools.clone(),
            "lenso.agent.turn-input@1",
            worktree_children,
        ));
        bindings.push(HostBinding::to_instance(
            subagent_tools,
            "lenso.agent.worktree@1",
            PluginInstanceId::new("lenso.agent.worktree-provider", "default"),
        ));
        let worker_tools = PluginInstanceId::new("lenso.agent.tools", "worker-tools");
        bindings.push(
            HostBinding::to_instances(
                worker_tools.clone(),
                "lenso.agent.tool-provider@2",
                [
                    PluginInstanceId::new("lenso.agent.workspace-read", "workspace-read"),
                    PluginInstanceId::new("lenso.agent.skills.filesystem", "skills"),
                    PluginInstanceId::new("lenso.agent.ask-user-tools", "ask-user"),
                    PluginInstanceId::new("lenso.agent.workspace-edit", "default"),
                    PluginInstanceId::new("lenso.agent.process-tools", "default"),
                    PluginInstanceId::new("lenso.agent.git-tools", "default"),
                ],
            )
            .with_admission(RequestAdmissionPlan::new(8, 4)),
        );
        bindings.push(HostBinding::to_instance(
            worker_tools,
            "lenso.agent.tool-hook@1",
            PluginInstanceId::new("lenso.agent.interactive-approval-hook", "default"),
        ));
    }
    if available.contains("lenso.agent.process-tools") {
        bindings.push(
            HostBinding::new(
                PluginInstanceId::new("lenso.agent.process-tools", "default"),
                "lenso.agent.process@1",
                "process",
            )
            .with_admission(RequestAdmissionPlan::new(8, 4)),
        );
    }
    if available.contains("lenso.agent.git-tools") {
        bindings.push(
            HostBinding::new(
                PluginInstanceId::new("lenso.agent.git-tools", "default"),
                "lenso.agent.process@1",
                "process",
            )
            .with_admission(RequestAdmissionPlan::new(8, 4)),
        );
    }
    if available.contains("lenso.agent.github-workflows") {
        bindings.push(
            HostBinding::new(
                PluginInstanceId::new("lenso.agent.github-workflows", "default"),
                "lenso.agent.process@1",
                "process",
            )
            .with_admission(RequestAdmissionPlan::new(8, 4)),
        );
    }
    if available.contains("lenso.agent.browser.playwright") {
        bindings.push(
            HostBinding::new(
                PluginInstanceId::new("lenso.agent.browser.playwright", "default"),
                "lenso.agent.process@1",
                "process",
            )
            .with_admission(RequestAdmissionPlan::new(8, 1)),
        );
    }
    if available.contains("lenso.agent.multimodal-tools") {
        bindings.push(
            HostBinding::new(
                PluginInstanceId::new("lenso.agent.multimodal-tools", "default"),
                "lenso.secrets@1",
                "secrets",
            )
            .with_admission(RequestAdmissionPlan::new(8, 4)),
        );
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

fn agent_catalog_factory(
    plugin_configuration_authority: Option<Arc<dyn PluginConfigurationAuthority>>,
) -> MultiExecutionCatalogFactory<AgentCatalogFactory> {
    MultiExecutionCatalogFactory::new(AgentCatalogFactory {
        plugin_configuration_authority,
    })
    .with_wasm_codec(AgentJsonCodec)
    .with_wasm_codec(ArtifactJsonCodec)
    .with_wasm_codec(ContextCompactionJsonCodec)
    .with_wasm_codec(ContextSourceJsonCodec)
    .with_wasm_codec(MemoryJsonCodec)
    .with_wasm_codec(HttpFetchJsonCodec)
    .with_wasm_codec(LifecycleJsonCodec)
    .with_wasm_codec(ModelJsonCodec)
    .with_wasm_codec(ModelSelectionJsonCodec)
    .with_wasm_codec(OauthAccessJsonCodec)
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
    .with_quickjs_codec(ArtifactJsonCodec)
    .with_quickjs_codec(ContextCompactionJsonCodec)
    .with_quickjs_codec(ContextSourceJsonCodec)
    .with_quickjs_codec(MemoryJsonCodec)
    .with_quickjs_codec(LifecycleJsonCodec)
    .with_quickjs_codec(ModelJsonCodec)
    .with_quickjs_codec(OauthAccessJsonCodec)
    .with_quickjs_codec(PromptJsonCodec)
    .with_quickjs_codec(SessionJsonCodec)
    .with_quickjs_codec(ToolHookJsonCodec)
    .with_quickjs_codec(TurnInputJsonCodec)
    .with_quickjs_codec(ToolsJsonCodec)
    .with_quickjs_codec(UserInteractionJsonCodec)
    .with_quickjs_codec(WorkspaceReadJsonCodec)
    .with_process_codec(AgentJsonCodec)
    .with_process_codec(ArtifactJsonCodec)
    .with_process_codec(ContextCompactionJsonCodec)
    .with_process_codec(ContextSourceJsonCodec)
    .with_process_codec(MemoryJsonCodec)
    .with_process_codec(HttpFetchJsonCodec)
    .with_process_codec(LifecycleJsonCodec)
    .with_process_codec(ModelJsonCodec)
    .with_process_codec(OauthAccessJsonCodec)
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
    fn host_build_identity_streams_the_exact_executable_digest() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("fixture-host");
        let bytes = vec![0x5a; HOST_BUILD_HASH_BUFFER_BYTES * 3 + 17];
        fs::write(&executable, &bytes).unwrap();
        let before = host_build_identity_telemetry();

        let identity = HostBuildIdentity::from_path(&executable).unwrap();
        let after = host_build_identity_telemetry();

        assert_eq!(identity.executable_digest, sha256_digest(&bytes));
        assert!(after.hashes >= before.hashes.saturating_add(1));
        assert!(
            after.hashed_bytes
                >= before
                    .hashed_bytes
                    .saturating_add(u64::try_from(bytes.len()).unwrap())
        );
    }

    #[test]
    fn host_build_identity_fails_closed_when_the_executable_cannot_be_opened() {
        let directory = tempfile::tempdir().unwrap();
        let error = HostBuildIdentity::from_path(&directory.path().join("missing")).unwrap_err();
        assert!(error.contains("failed to read Host executable"));
    }

    #[test]
    #[ignore = "manual Host executable hashing measurement; run with --ignored --nocapture"]
    fn reports_host_build_identity_measurement() {
        let before = host_build_identity_telemetry();
        HostBuildIdentity::current().unwrap();
        let after = host_build_identity_telemetry();

        let hashes = after.hashes.saturating_sub(before.hashes);
        let hashed_bytes = after.hashed_bytes.saturating_sub(before.hashed_bytes);
        let locate_micros = after.locate_micros.saturating_sub(before.locate_micros);
        let open_micros = after.open_micros.saturating_sub(before.open_micros);
        let hash_micros = after.hash_micros.saturating_sub(before.hash_micros);
        eprintln!(
            "host_build_identity hashes={hashes} hashed_bytes={hashed_bytes} \
             locate_micros={locate_micros} open_micros={open_micros} \
             hash_micros={hash_micros} buffer_bytes={HOST_BUILD_HASH_BUFFER_BYTES}"
        );
        assert_eq!(hashes, 1);
        assert!(hashed_bytes > 0);
    }

    #[test]
    fn default_agent_instruction_requires_evidence_progress_and_handoff() {
        for required in [
            "distinguish observation from inference",
            "never claim an action or validation that did not happen",
            "brief progress updates",
            "Finish with the outcome first",
        ] {
            assert!(DEFAULT_AGENT_INSTRUCTION.contains(required));
        }
    }

    #[test]
    fn console_plugin_publication_requires_interactive_approval() {
        let configuration = console_interactive_approval_configuration();
        assert_eq!(
            configuration["allow_tools"],
            serde_json::json!([
                "inspect_app",
                "list_plugins",
                "inspect_plugin",
                "check_plugin_change"
            ])
        );
        assert_eq!(
            configuration["ask_tools"],
            serde_json::json!(["apply_plugin_change"])
        );
        assert_eq!(configuration["default_decision"], "ask");
    }

    #[test]
    fn console_instruction_is_a_disableable_default_only_when_linked() {
        let directories = AgentDirectories::resolve().unwrap();
        let available = BTreeSet::from(["lenso.agent.console-instructions".to_owned()]);
        let defaults = host_catalog_defaults(&directories, &available);
        let instruction = defaults
            .iter()
            .find(|plugin| plugin.id().plugin_id() == "lenso.agent.console-instructions")
            .expect("linked Console instruction should be a Host default");

        assert_eq!(instruction.id().instance_key(), "default");
        assert!(instruction.is_disableable());
        assert!(
            host_catalog_defaults(&directories, &BTreeSet::new())
                .iter()
                .all(|plugin| plugin.id().plugin_id() != "lenso.agent.console-instructions")
        );
    }

    #[test]
    fn official_prompt_instance_resolves_the_host_shipped_instruction() {
        let root = PluginRootSnapshot::new(
            [],
            [lenso_app_plan::authoring::PluginRootInstance::new(
                "lenso.agent.prompt.static",
                "coding",
            )],
            [],
        );

        let plan = resolve_host_plan(&root).unwrap();
        let prompt = plan
            .plugin_instances()
            .iter()
            .find(|plugin| plugin.instance_key() == "lenso.agent.prompt.static/coding")
            .unwrap();
        let configuration: serde_json::Value =
            serde_json::from_str(prompt.configuration()).unwrap();

        assert_eq!(
            configuration["contributions"][0]["content"],
            official_prompts::CODING_INSTRUCTION
        );
        assert_eq!(
            configuration["contributions"][1]["id"],
            "harness.execution.native"
        );
    }

    #[test]
    fn explicit_prompt_content_opts_out_of_the_host_shipped_instruction() {
        let root = PluginRootSnapshot::new(
            [],
            [lenso_app_plan::authoring::PluginRootInstance::new(
                "lenso.agent.prompt.static",
                "coding",
            )
            .with_configuration(serde_json::json!({
                "contributions": [{
                    "id": "company.coding",
                    "version": "1.0.0",
                    "kind": "instruction",
                    "content": "Use the company coding workflow."
                }]
            }))],
            [],
        );

        let plan = resolve_host_plan(&root).unwrap();
        let prompt = plan
            .plugin_instances()
            .iter()
            .find(|plugin| plugin.instance_key() == "lenso.agent.prompt.static/coding")
            .unwrap();
        let configuration: serde_json::Value =
            serde_json::from_str(prompt.configuration()).unwrap();

        assert_eq!(configuration["contributions"].as_array().unwrap().len(), 1);
        assert_eq!(configuration["contributions"][0]["id"], "company.coding");
    }

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
            .map(|index| PanelItem {
                id: format!("agent.panel-{index}"),
                title: format!("Panel {index}"),
                body: "Content".to_owned(),
            })
            .collect::<Vec<_>>();
        assert!(validate_tui_panels(&panels).is_err());
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
        assert!(instances.contains("lenso.agent.artifact.file/artifacts"));
        assert!(instances.contains("lenso.agent.oauth.client-credentials/oauth"));
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
                        && binding["provider_instance"] == "lenso.agent.artifact.file/artifacts"
                        && binding["capability_id"] == "lenso.agent.artifact@1"
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
    fn agent_behavior_digest_ignores_surface_only_plan_changes() {
        let plan = resolve_host_plan(&PluginRootSnapshot::default()).unwrap();
        let baseline = agent_behavior_digest(&plan, "lenso.agent.loop/agent").unwrap();
        let mut surface_json = serde_json::to_value(&plan).unwrap();
        let surface = surface_json["plugin_instances"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|plugin| plugin["instance_key"] == "lenso.agent.cli/cli")
            .unwrap();
        surface["configuration"] =
            serde_json::Value::String(serde_json::json!({"surface_only": true}).to_string());
        let surface_plan: ResolvedAppPlan = serde_json::from_value(surface_json).unwrap();
        assert_eq!(
            agent_behavior_digest(&surface_plan, "lenso.agent.loop/agent").unwrap(),
            baseline
        );

        let mut behavior_json = serde_json::to_value(&plan).unwrap();
        let agent = behavior_json["plugin_instances"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|plugin| plugin["instance_key"] == "lenso.agent.loop/agent")
            .unwrap();
        agent["configuration"] = serde_json::Value::String(
            serde_json::json!({"agent_behavior_changed": true}).to_string(),
        );
        let behavior_plan: ResolvedAppPlan = serde_json::from_value(behavior_json).unwrap();
        assert_ne!(
            agent_behavior_digest(&behavior_plan, "lenso.agent.loop/agent").unwrap(),
            baseline
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
        let configurations = host_catalog_configurations(&directories, &available)
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
                && binding["capability_id"] == "lenso.agent.model@4"
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
                        "max_user_resumes": 2,
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
                "max_tasks": 8,
                "require_worktree_provider": false
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
    fn interactive_surfaces_bind_task_snapshots_through_the_tool_provider_slot() {
        let available = [
            "lenso.agent.subagent-tools",
            "lenso.agent.tui",
            "lenso.agent.web",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let bindings = host_catalog_bindings(
            &PluginInstanceId::new("lenso.agent.loop", "agent"),
            &available,
        );

        for surface in ["lenso.agent.tui/tui", "lenso.agent.web/web"] {
            assert!(bindings.iter().any(|binding| {
                binding.consumer().to_string() == surface
                    && binding.capability_id() == "lenso.agent.task-supervisor@2"
                    && binding.provider_slot() == Some("tool-providers")
            }));
        }
    }

    #[test]
    fn acp_surface_binds_only_to_the_selected_agent() {
        let available = ["lenso.agent.acp"].into_iter().map(str::to_owned).collect();
        let selected_agent = PluginInstanceId::new("lenso.agent.loop", "reviewer");
        let bindings = host_catalog_bindings(&selected_agent, &available);

        let acp_bindings = bindings
            .iter()
            .filter(|binding| binding.consumer().to_string() == "lenso.agent.acp/acp")
            .collect::<Vec<_>>();
        assert_eq!(acp_bindings.len(), 1);
        assert_eq!(acp_bindings[0].capability_id(), "lenso.agent@3");
        let binding = serde_json::to_value(acp_bindings[0]).unwrap();
        assert_eq!(
            binding["provider_instance"]["plugin_id"],
            "lenso.agent.loop"
        );
        assert_eq!(binding["provider_instance"]["instance_key"], "reviewer");
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
    fn github_workflows_are_opt_in_and_bind_to_process_and_tools() {
        let root = PluginRootSnapshot::new(
            [],
            [
                lenso_app_plan::authoring::PluginRootInstance::new(
                    "lenso.agent.process.native",
                    "default",
                )
                .with_configuration(serde_json::json!({
                    "allowed_programs": ["gh"],
                    "program_presets": [],
                    "environment_allowlist": ["PATH", "HOME", "GH_TOKEN"],
                    "max_argument_bytes": 131_072,
                    "max_output_bytes": 262_144,
                    "max_timeout_ms": 300_000,
                    "root": "."
                })),
                lenso_app_plan::authoring::PluginRootInstance::new(
                    "lenso.agent.github-workflows",
                    "default",
                )
                .with_configuration(serde_json::json!({
                    "allowed_repositories": ["LioRael/lenso-agent"],
                    "default_timeout_ms": 30_000,
                    "enable_mutations": false,
                    "max_body_bytes": 16_384
                })),
            ],
            [],
        );
        let plan = resolve_host_plan(&root).unwrap();
        let plan_json = serde_json::to_value(&plan).unwrap();
        let bindings = plan_json["capability_bindings"].as_array().unwrap();
        assert!(bindings.iter().any(|binding| {
            binding["consumer_instance"] == "lenso.agent.github-workflows/default"
                && binding["provider_instance"] == "lenso.agent.process.native/default"
                && binding["capability_id"] == "lenso.agent.process@1"
        }));
        assert!(bindings.iter().any(|binding| {
            binding["provider_instance"] == "lenso.agent.github-workflows/default"
                && binding["capability_id"] == "lenso.agent.tool-provider@2"
        }));
    }

    #[test]
    fn browser_and_multimodal_tools_are_opt_in_with_explicit_grants() {
        let root = PluginRootSnapshot::new(
            [],
            [
                lenso_app_plan::authoring::PluginRootInstance::new(
                    "lenso.agent.process.native",
                    "default",
                )
                .with_configuration(serde_json::json!({
                    "allowed_programs": ["node"],
                    "program_presets": [],
                    "environment_allowlist": ["PATH", "HOME"],
                    "max_argument_bytes": 131_072,
                    "max_output_bytes": 1_048_576,
                    "max_timeout_ms": 120_000,
                    "root": "."
                })),
                lenso_app_plan::authoring::PluginRootInstance::new(
                    "lenso.agent.browser.playwright",
                    "default",
                )
                .with_configuration(serde_json::json!({
                    "allowed_origins": ["https://example.com"],
                    "cdp_endpoint": "http://127.0.0.1:9222",
                    "max_snapshot_bytes": 65_536,
                    "screenshot_directory": ".lenso/browser",
                    "timeout_ms": 30_000
                })),
                lenso_app_plan::authoring::PluginRootInstance::new("lenso.secrets.env", "media")
                    .with_configuration(serde_json::json!({
                        "references": {"media/openai-api-key": "OPENAI_API_KEY"}
                    })),
                lenso_app_plan::authoring::PluginRootInstance::new(
                    "lenso.agent.multimodal-tools",
                    "default",
                )
                .with_configuration(serde_json::json!({
                    "api_key_ref": "media/openai-api-key",
                    "audio_model": "gpt-audio",
                    "base_url": "https://api.openai.com/v1",
                    "image_model": "gpt-vision",
                    "max_file_bytes": 10_485_760,
                    "root": ".",
                    "timeout_ms": 60_000
                })),
            ],
            [],
        );
        let plan = resolve_host_plan(&root).unwrap();
        let plan_json = serde_json::to_value(&plan).unwrap();
        let bindings = plan_json["capability_bindings"].as_array().unwrap();
        assert!(bindings.iter().any(|binding| {
            binding["consumer_instance"] == "lenso.agent.browser.playwright/default"
                && binding["provider_instance"] == "lenso.agent.process.native/default"
                && binding["capability_id"] == "lenso.agent.process@1"
        }));
        assert!(bindings.iter().any(|binding| {
            binding["consumer_instance"] == "lenso.agent.multimodal-tools/default"
                && binding["provider_instance"] == "lenso.secrets.env/media"
                && binding["capability_id"] == "lenso.secrets@1"
        }));
        for provider in [
            "lenso.agent.browser.playwright/default",
            "lenso.agent.multimodal-tools/default",
        ] {
            assert!(bindings.iter().any(|binding| {
                binding["provider_instance"] == provider
                    && binding["capability_id"] == "lenso.agent.tool-provider@2"
            }));
        }
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
        assert!(
            plan_json["capability_bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|binding| {
                    binding["consumer_instance"] == "lenso.agent.mcp-client/filesystem"
                        && binding["provider_instance"]
                            == "lenso.agent.oauth.client-credentials/oauth"
                        && binding["capability_id"] == "lenso.agent.oauth-access@1"
                })
        );
    }
}

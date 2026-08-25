use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::Path,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use lenso_agent_auth_openai_codex_module as _;
use lenso_agent_cli_module as _;
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
use lenso_agent_tools_module as _;
use lenso_agent_tui_module as _;
use lenso_agent_tui_static_module as _;
use lenso_agent_workspace_edit_module as _;
use lenso_agent_workspace_read_module as _;
use lenso_app_plan::ResolvedAppPlan;
use lenso_capability_agent::{Agent, AgentJsonCodec};
use lenso_capability_agent_model::ModelJsonCodec;
use lenso_capability_agent_prompt::PromptJsonCodec;
use lenso_capability_agent_session::SessionJsonCodec;
use lenso_capability_agent_tool_provider::ToolProviderJsonCodec;
use lenso_capability_agent_tools::ToolsJsonCodec;
use lenso_capability_agent_tui_contribution::{
    SNAPSHOT_OPERATION, SnapshotRequest, SnapshotResponsePanelsItem, TuiContribution,
    validate_snapshot_panels,
};
use lenso_kernel::{
    CancellationToken, ExecutionAdapterCatalog, InvocationContext, NativeApp, NativeStreamHandle,
};
use lenso_native_adapter::NativeModuleRegistry;
use lenso_plugin_control_plane::{
    AdapterProfile, AppGenerationSpec, AppGenerationTransitionSpec, BuiltInModule,
    CanonicalDocument, CatalogFactory, ClassPolicy, ControlLifecycle, ControlPlaneError,
    ControlStateStore, DurableControlState, DurableGenerationRoute, DurableGenerationSupervisor,
    FileControlStateStore, GenerationController, GenerationControllerClient, HostBuildManifest,
    HostExecutionPolicy, KernelGenerationRuntime, MemoryControlStateStore,
    MultiExecutionCatalogFactory, ReplacementMode, ResolutionInput, ResolvedGeneration,
    RolloutPolicy, resolve_generation, sha256_digest,
};
use lenso_secrets_env_module as _;

use crate::plugin_profiles::{
    NATIVE_EXECUTION_CLASS, QUICKJS_EXECUTION_CLASS, WASM_EXECUTION_CLASS, harness_plugin_profiles,
};

const APP_ID: &str = "lenso.agent.harness";
const READY_TIMEOUT_NANOS: u64 = 10_000_000_000;
const DRAIN_TIMEOUT_NANOS: u64 = 2_000_000_000;
const GENERATION_DIRECTORY: &str = "generations";
const CONTROL_DIRECTORY: &str = "generation-control";
const TUI_CONTROL_DIRECTORY: &str = "tui-generation-control";
const MAINTENANCE_INTERVAL: Duration = Duration::from_millis(10);
const TUI_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_TUI_PANELS: usize = 64;
const MAX_TUI_PANEL_BYTES: usize = 262_144;

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

#[derive(Debug)]
struct HostBuildIdentity {
    executable_digest: String,
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

#[derive(Debug)]
pub struct AgentApp {
    client: GenerationControllerClient<NativeApp>,
    controller: Option<tokio::task::JoinHandle<Result<DurableControlState, ControlPlaneError>>>,
}

impl AgentApp {
    pub async fn start(plan_bytes: &[u8]) -> Result<Self, String> {
        Self::start_with_store(plan_bytes, Path::new(".lenso/plugins")).await
    }

    pub async fn start_tui(plan_bytes: &[u8]) -> Result<Self, String> {
        Self::start_tui_with_store(plan_bytes, Path::new(".lenso/plugins")).await
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
        let authority = crate::authority::AuthorityCoordinator::prepare(store_root)?;
        let _authority_fence = authority.snapshot()?;
        let host_build = HostBuildIdentity::current()?;
        let generation = resolve_initial_generation_for_host(plan_bytes, store_root, &host_build)?;
        record_generation_spec(store_root, &generation.spec)?;
        crate::plugins::record_current_generation_authority(store_root)?;
        let store = FileControlStateStore::open(store_root.join(control_directory))
            .map_err(control_error)?;
        let durable = store.load(APP_ID).map_err(control_error)?;
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
        let runtime = KernelGenerationRuntime::new(harness_catalog_factory());
        let supervisor = if has_live_state {
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
            let recoverable = resolve_retained_generations(plan_bytes, store_root, &host_build)?;
            let missing = live_digests
                .iter()
                .filter(|digest| !recoverable.contains_key(**digest))
                .copied()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(format!(
                    "durable Generation recovery lacks retained exact Plugin authority for {}; recoverable Generation Specs: {}",
                    missing.join(", "),
                    recoverable.keys().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
            DurableGenerationSupervisor::recover(
                APP_ID,
                runtime,
                store,
                &recoverable,
                now_unix_nanos()?,
            )
            .await
            .map_err(control_error)?
        } else {
            DurableGenerationSupervisor::open(APP_ID, runtime, store).map_err(control_error)?
        };
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
        Ok(Self {
            client,
            controller: Some(task),
        })
    }

    pub async fn lease_turn(&self) -> Result<TurnGeneration, String> {
        self.lease_turn_for("cli").await
    }

    /// Pins one TUI-submitted Agent Turn to the active App Generation.
    pub async fn lease_tui_turn(&self) -> Result<TurnGeneration, String> {
        self.lease_turn_for("tui").await
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

    pub async fn shutdown(&mut self) -> Result<(), String> {
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
        Ok(())
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
        store: &authority.store,
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
    record_generation_spec(store_root, &candidate.spec)?;
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

fn harness_catalog_factory() -> MultiExecutionCatalogFactory<HarnessCatalogFactory> {
    MultiExecutionCatalogFactory::new(HarnessCatalogFactory)
        .with_wasm_codec(AgentJsonCodec)
        .with_wasm_codec(ModelJsonCodec)
        .with_wasm_codec(PromptJsonCodec)
        .with_wasm_codec(SessionJsonCodec)
        .with_wasm_codec(ToolProviderJsonCodec)
        .with_wasm_codec(ToolsJsonCodec)
        .with_quickjs_codec(AgentJsonCodec)
        .with_quickjs_codec(ModelJsonCodec)
        .with_quickjs_codec(PromptJsonCodec)
        .with_quickjs_codec(SessionJsonCodec)
        .with_quickjs_codec(ToolsJsonCodec)
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

    const PLAN: &[u8] = include_bytes!("../../../composition/headless-readonly/resolved-plan.json");
    const TUI_PLAN: &[u8] = include_bytes!("../../../composition/tui-readonly/resolved-plan.json");

    #[test]
    fn initial_generation_preserves_the_approved_plan() {
        let directory = tempfile::tempdir().unwrap();
        let generation = resolve_initial_generation(PLAN, directory.path()).unwrap();
        let approved: ResolvedAppPlan = serde_json::from_slice(PLAN).unwrap();
        assert_eq!(generation.plan, approved);
        assert!(generation.artifact_set.value().releases.is_empty());
        assert!(generation.artifact_set.value().instances.is_empty());
    }

    #[test]
    fn generation_spec_is_content_addressed_and_tampering_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let generation = resolve_initial_generation(PLAN, directory.path()).unwrap();
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
                let mut app = AgentApp::start_with_store(TUI_PLAN, directory.path())
                    .await
                    .unwrap();
                let panels = app.tui_panels().await.unwrap();
                assert_eq!(panels.len(), 1);
                assert_eq!(panels[0].id, "agent.help");

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
                        StreamEvent::Message(message) => output.push_str(&message.text),
                        StreamEvent::PeerHalfClosed => {}
                        StreamEvent::Terminal(Ok(())) => break,
                        StreamEvent::Terminal(Err(error)) => {
                            panic!("TUI Agent Turn failed: {error:?}")
                        }
                    }
                }
                assert_eq!(output, "Plugin: Direct answer.");
                drop(stream);
                drop(turn);
                app.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_turn_is_pinned_to_the_active_generation() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let mut app = AgentApp::start_with_store(PLAN, directory.path())
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

                let mut recovered = AgentApp::start_with_store(PLAN, directory.path())
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
                let mut headless = AgentApp::start_with_store(PLAN, directory.path())
                    .await
                    .unwrap();
                headless.shutdown().await.unwrap();

                let mut tui = AgentApp::start_tui_with_store(TUI_PLAN, directory.path())
                    .await
                    .unwrap();
                tui.shutdown().await.unwrap();
            })
            .await;
    }
}

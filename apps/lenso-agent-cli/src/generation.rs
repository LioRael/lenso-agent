use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::Path,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use futures::future::LocalBoxFuture;
use lenso_agent_auth_openai_codex_module::OpenAiCodexAuthFactory;
use lenso_agent_cli_module::CliModuleFactory;
use lenso_agent_loop_module::{AgentLoopFactory, GENERATION_SPEC_DIGEST_EXTENSION};
use lenso_agent_model_fixture_module::FixtureModelFactory;
use lenso_agent_model_openai_codex_direct_module::OpenAiCodexDirectModelFactory;
use lenso_agent_model_openai_compatible_module::OpenAiCompatibleModelFactory;
use lenso_agent_process_native_module::NativeProcessFactory;
use lenso_agent_process_tools_module::ProcessToolsFactory;
use lenso_agent_prompt_filesystem_module::FilesystemPromptFactory;
use lenso_agent_prompt_module::PromptFactory;
use lenso_agent_prompt_static_module::StaticPromptFactory;
use lenso_agent_session_file_module::FileSessionFactory;
use lenso_agent_skills_filesystem_module::FilesystemSkillsFactory;
use lenso_agent_text_tools_module::TextToolsFactory;
use lenso_agent_tools_module::ToolsFactory;
use lenso_agent_workspace_edit_module::WorkspaceEditFactory;
use lenso_agent_workspace_read_module::WorkspaceReadFactory;
use lenso_app_plan::ResolvedAppPlan;
use lenso_capability_agent::Agent;
use lenso_kernel::{
    CancellationToken, ExecutionAdapterCatalog, InvocationContext, Kernel, NativeApp,
    NativeStreamHandle, ShutdownOutcome,
};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleRegistry};
use lenso_plugin_control_plane::{
    AdapterProfile, AppGenerationSpec, AppGenerationTransitionSpec, BuiltInModule,
    CanonicalDocument, ClassPolicy, ControlPlaneError, GenerationLease, GenerationRuntime,
    GenerationSupervisor, HostBuildManifest, HostExecutionPolicy, ReplacementMode, ResolutionInput,
    ResolvedGeneration, RolloutPolicy, resolve_generation, sha256_digest,
};
use lenso_runner::TokioDriver;
use lenso_secrets_env_module::EnvSecretsFactory;

use crate::plugin_profiles::{NATIVE_EXECUTION_CLASS, harness_plugin_profiles};

const APP_ID: &str = "lenso.agent.harness";
const READY_TIMEOUT_NANOS: u64 = 10_000_000_000;
const DRAIN_TIMEOUT_NANOS: u64 = 2_000_000_000;
const GENERATION_DIRECTORY: &str = "generations";

static NEXT_ROOT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct LiveGeneration {
    app: NativeApp,
    driver: TokioDriver,
    agent: Rc<NativeStreamHandle<Agent>>,
}

#[derive(Clone, Debug, Default)]
struct GenerationRoutes {
    slots: Rc<RefCell<BTreeMap<String, LiveGeneration>>>,
}

impl GenerationRoutes {
    fn agent(&self, digest: &str) -> Result<Rc<NativeStreamHandle<Agent>>, String> {
        self.slots
            .borrow()
            .get(digest)
            .map(|slot| Rc::clone(&slot.agent))
            .ok_or_else(|| format!("leased Generation `{digest}` has no live Agent route"))
    }

    async fn shutdown_all(&self) -> Result<(), String> {
        let slots = std::mem::take(&mut *self.slots.borrow_mut());
        let mut failures = Vec::new();
        for (digest, slot) in slots {
            slot.driver.request_shutdown();
            match slot
                .app
                .shutdown(Duration::from_nanos(DRAIN_TIMEOUT_NANOS))
                .await
            {
                ShutdownOutcome::Clean => {}
                outcome => {
                    failures.push(format!(
                        "Generation `{digest}` shutdown was not clean: {outcome:?}"
                    ));
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

#[derive(Debug)]
struct HarnessGenerationRuntime {
    routes: GenerationRoutes,
}

impl GenerationRuntime for HarnessGenerationRuntime {
    type Handle = String;

    fn stage<'a>(
        &'a mut self,
        generation: &'a ResolvedGeneration,
        ready_timeout_nanos: u64,
    ) -> LocalBoxFuture<'a, Result<Self::Handle, ControlPlaneError>> {
        Box::pin(async move {
            let digest = generation.spec.digest().to_owned();
            if self.routes.slots.borrow().contains_key(&digest) {
                return Err(host_failure("Generation route was already live"));
            }
            let (registry, _) = native_host_build();
            let driver = TokioDriver::new();
            let app = tokio::time::timeout(
                Duration::from_nanos(ready_timeout_nanos),
                Kernel::start(
                    generation.plan.clone(),
                    driver.clone(),
                    ExecutionAdapterCatalog::single(registry),
                ),
            )
            .await
            .map_err(|_| host_failure("Kernel Generation Ready Gate timed out"))?
            .map_err(|error| {
                host_failure(format!("Kernel Generation failed before Ready: {error:?}"))
            })?;
            let agent = match app.stream_handle::<Agent>("cli") {
                Ok(agent) => Rc::new(agent),
                Err(error) => {
                    driver.request_shutdown();
                    let cleanup = app
                        .shutdown(Duration::from_nanos(DRAIN_TIMEOUT_NANOS))
                        .await;
                    return Err(host_failure(format!(
                        "Agent binding is unavailable: {error:?}; cleanup: {cleanup:?}"
                    )));
                }
            };
            let previous = self
                .routes
                .slots
                .borrow_mut()
                .insert(digest.clone(), LiveGeneration { app, driver, agent });
            debug_assert!(previous.is_none(), "live route was checked before staging");
            Ok(digest)
        })
    }

    fn shutdown(
        &mut self,
        handle: Self::Handle,
        drain_timeout_nanos: u64,
    ) -> LocalBoxFuture<'_, Result<(), ControlPlaneError>> {
        Box::pin(async move {
            let slot = self
                .routes
                .slots
                .borrow_mut()
                .remove(&handle)
                .ok_or_else(|| host_failure("Generation route was not live"))?;
            slot.driver.request_shutdown();
            match slot
                .app
                .shutdown(Duration::from_nanos(drain_timeout_nanos))
                .await
            {
                ShutdownOutcome::Clean => Ok(()),
                outcome => Err(host_failure(format!(
                    "Generation shutdown was not clean: {outcome:?}"
                ))),
            }
        })
    }
}

#[derive(Debug)]
pub struct AgentApp {
    supervisor: GenerationSupervisor<HarnessGenerationRuntime>,
    routes: GenerationRoutes,
}

impl AgentApp {
    pub async fn start(plan_bytes: &[u8]) -> Result<Self, String> {
        Self::start_with_store(plan_bytes, Path::new(".lenso/plugins")).await
    }

    async fn start_with_store(plan_bytes: &[u8], store_root: &Path) -> Result<Self, String> {
        let generation = resolve_initial_generation(plan_bytes, store_root)?;
        record_generation_spec(store_root, &generation.spec)?;
        let transition = initial_transition(&generation).map_err(control_error)?;
        let routes = GenerationRoutes::default();
        let runtime = HarnessGenerationRuntime {
            routes: routes.clone(),
        };
        let mut supervisor = GenerationSupervisor::new(APP_ID, runtime);
        supervisor
            .transition(&transition, &generation)
            .await
            .map_err(control_error)?;
        Ok(Self { supervisor, routes })
    }

    pub fn lease_turn(&self) -> Result<TurnGeneration, String> {
        let lease = self.supervisor.lease().map_err(control_error)?;
        let handle = self.routes.agent(lease.generation_spec_digest())?;
        Ok(TurnGeneration { lease, handle })
    }

    pub async fn shutdown(&mut self) -> Result<(), String> {
        if self
            .supervisor
            .generations()
            .iter()
            .any(|(_, _, leases)| *leases != 0)
        {
            return Err("cannot shut down while an Agent Turn holds a Generation lease".to_owned());
        }
        self.routes.shutdown_all().await
    }
}

#[derive(Debug)]
pub struct TurnGeneration {
    #[allow(dead_code, reason = "the lease is an RAII routing guard")]
    lease: GenerationLease,
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
        self.lease.generation_spec_digest()
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

pub(crate) fn resolve_initial_generation(
    plan_bytes: &[u8],
    store_root: &Path,
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
    let executable =
        env::current_exe().map_err(|error| format!("failed to locate Host executable: {error}"))?;
    let executable_bytes = fs::read(&executable).map_err(|error| {
        format!(
            "failed to read Host executable {}: {error}",
            executable.display()
        )
    })?;
    let (_, built_in_modules) = native_host_build();
    let plugin_profiles = harness_plugin_profiles()?;
    let native_profiles = plugin_profiles.profiles_for_execution_class(NATIVE_EXECUTION_CLASS);
    let native_support_channels =
        plugin_profiles.support_channels_for_execution_class(NATIVE_EXECUTION_CLASS);
    let native_trust_levels =
        plugin_profiles.trust_levels_for_execution_class(NATIVE_EXECUTION_CLASS);
    let host_build = CanonicalDocument::from_value(
        "lenso-host-build.json",
        HostBuildManifest {
            schema_version: 1,
            app_id: APP_ID.to_owned(),
            host_executable_digest: sha256_digest(&executable_bytes),
            target: target.clone(),
            built_in_modules,
            adapter_profiles: vec![AdapterProfile {
                execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
                adapter_build_identity: "lenso-native-adapter@runtime-a42c7f7".to_owned(),
                targets: vec![target.clone()],
                profiles: native_profiles.clone(),
            }],
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
            classes: vec![ClassPolicy {
                execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
                support_channels: native_support_channels,
                trust_levels: native_trust_levels,
                profiles: native_profiles,
            }],
            preference: vec![NATIVE_EXECUTION_CLASS.to_owned()],
            instance_overrides: Vec::new(),
        },
    )
    .map_err(control_error)?;
    let authority = crate::plugins::load_generation_authority(store_root)?;
    let composition = crate::plugins::generation_composition(&authority, &plan)?;
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

fn native_host_build() -> (NativeModuleRegistry, Vec<BuiltInModule>) {
    let mut registry = NativeModuleRegistry::new();
    let mut built_in_modules = Vec::new();
    macro_rules! register {
        ($factory:expr) => {{
            let factory = $factory;
            built_in_modules.push(BuiltInModule {
                package_id: factory.package_id().to_owned(),
                factory_identity: factory.factory_identity(),
                execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
            });
            registry = registry.with_factory(factory);
        }};
    }
    register!(CliModuleFactory);
    register!(AgentLoopFactory);
    register!(OpenAiCodexAuthFactory);
    register!(FixtureModelFactory);
    register!(OpenAiCompatibleModelFactory);
    register!(OpenAiCodexDirectModelFactory);
    register!(FilesystemPromptFactory);
    register!(PromptFactory);
    register!(StaticPromptFactory);
    register!(NativeProcessFactory);
    register!(ProcessToolsFactory);
    register!(FilesystemSkillsFactory);
    register!(TextToolsFactory);
    register!(ToolsFactory);
    register!(WorkspaceEditFactory);
    register!(WorkspaceReadFactory);
    register!(FileSessionFactory);
    register!(EnvSecretsFactory::new());
    built_in_modules.sort_by(|left, right| left.factory_identity.cmp(&right.factory_identity));
    (registry, built_in_modules)
}

fn host_failure(detail: impl Into<String>) -> ControlPlaneError {
    ControlPlaneError::HostFailure {
        detail: detail.into(),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn control_error(error: ControlPlaneError) -> String {
    format!("Plugin control plane failed: {error:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &[u8] = include_bytes!("../../../composition/headless-readonly/resolved-plan.json");

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

    #[tokio::test(flavor = "current_thread")]
    async fn one_turn_is_pinned_to_the_active_generation() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let mut app = AgentApp::start_with_store(PLAN, directory.path())
                    .await
                    .unwrap();
                let turn = app.lease_turn().unwrap();
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
            })
            .await;
    }
}

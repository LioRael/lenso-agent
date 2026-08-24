use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::Path,
    rc::Rc,
    time::Duration,
};

use futures::future::LocalBoxFuture;
use lenso_agent_auth_openai_codex_module::OpenAiCodexAuthFactory;
use lenso_agent_cli_module::CliModuleFactory;
use lenso_agent_loop_module::AgentLoopFactory;
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
    ExecutionAdapterCatalog, Kernel, NativeApp, NativeStreamHandle, ShutdownOutcome,
};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleRegistry};
use lenso_plugin_control_plane::{
    AdapterProfile, AppGenerationSpec, AppGenerationTransitionSpec, BuiltInModule,
    CanonicalDocument, ClassPolicy, ControlPlaneError, GenerationLease, GenerationRuntime,
    GenerationSupervisor, HostBuildManifest, HostExecutionPolicy, ReplacementMode, ResolutionInput,
    ResolvedGeneration, RolloutPolicy, SupportChannel, TrustLevel, resolve_generation,
    sha256_digest,
};
use lenso_runner::TokioDriver;
use lenso_secrets_env_module::EnvSecretsFactory;

const APP_ID: &str = "lenso.agent.harness";
const NATIVE_EXECUTION_CLASS: &str = "lenso.native-rust@1";
const NATIVE_TOOL_PROFILE: &str = "agent-tool-provider-v1";
const READY_TIMEOUT_NANOS: u64 = 10_000_000_000;
const DRAIN_TIMEOUT_NANOS: u64 = 2_000_000_000;

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

    #[cfg(test)]
    fn generation_spec_digest(&self) -> &str {
        self.lease.generation_spec_digest()
    }
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
                adapter_build_identity: "lenso-native-adapter@runtime-25812bc".to_owned(),
                targets: vec![target.clone()],
                profiles: vec![NATIVE_TOOL_PROFILE.to_owned()],
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
                support_channels: vec![SupportChannel::Stable],
                trust_levels: vec![TrustLevel::Trusted],
                profiles: vec![NATIVE_TOOL_PROFILE.to_owned()],
            }],
            preference: vec![NATIVE_EXECUTION_CLASS.to_owned()],
            instance_overrides: Vec::new(),
        },
    )
    .map_err(control_error)?;
    let authority = crate::plugins::load_generation_authority(store_root)?;
    let bindings = crate::plugins::generation_bindings(&authority, &plan)?;
    let generation = resolve_generation(&ResolutionInput {
        lock: &authority.lock,
        manifests: &authority.manifests,
        admission_receipts: &authority.admission_receipts,
        host_build: &host_build,
        policy: &policy,
        store: &authority.store,
        base_instances: plan.module_instances().to_vec(),
        bindings,
    })
    .map_err(control_error)?;
    close_over_base_binding_order(generation, &plan)
}

fn close_over_base_binding_order(
    mut generation: ResolvedGeneration,
    base_plan: &ResolvedAppPlan,
) -> Result<ResolvedGeneration, String> {
    let base_keys = base_plan
        .capability_bindings()
        .iter()
        .map(binding_key)
        .collect::<BTreeSet<_>>();
    let mut bindings = base_plan
        .capability_bindings()
        .iter()
        .map(|binding| serde_json::to_value(binding).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut next_orders = BTreeMap::<(String, String), usize>::new();
    for binding in base_plan.capability_bindings() {
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
                assert!(app.shutdown().await.is_err());
                drop(turn);
                app.shutdown().await.unwrap();
            })
            .await;
    }
}

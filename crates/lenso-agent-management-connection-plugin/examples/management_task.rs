//! A deterministic, independently runnable Host for one delegated Management task.

use lenso_agent_artifact_file_plugin as _;
use lenso_agent_context_compaction_plugin as _;
use lenso_agent_loop_plugin as _;
use lenso_agent_management_connection_plugin as _;
use lenso_agent_management_log_fixture as _;
use lenso_agent_management_tools_plugin as _;
use lenso_agent_memory_sqlite_plugin as _;
use lenso_agent_model_fixture_plugin as _;
use lenso_agent_prompt_plugin as _;
use lenso_agent_prompt_static_plugin as _;
use lenso_agent_session_file_plugin as _;
use lenso_agent_tools_plugin as _;

use lenso::CtxExt;
use lenso_app_plan::{
    CapabilityRequirementPlan,
    authoring::{HostCatalog, HostDefaultPlugin, HostPluginRelease, HostSlot, PluginDescriptor},
};
use lenso_capability_agent::{self as agent, AgentTaskBinding};
use lenso_capability_agent_model as model;
use lenso_capability_agent_session as session;
use lenso_kernel::{CancellationToken, InvocationContext, Kernel, RuntimeFailure, StreamEvent};
use lenso_native_adapter::{
    NativePluginFactory, NativePluginFactoryContext, NativePluginInstance, NativePluginRegistry,
};
use lenso_runner::TokioDriver;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{env, fs, path::PathBuf, time::Duration};

const SURFACE: &str = "fixture.management-task-surface";
const SURFACE_INSTANCE: &str = "fixture.management-task-surface/default";
const PLUGINS: &[(&str, &str)] = &[
    ("lenso.agent.loop", "agents"),
    ("lenso.agent.model.fixture", "model"),
    ("lenso.agent.prompt", "prompt-runtime"),
    ("lenso.agent.prompt.static", "prompt-providers"),
    ("lenso.agent.tools", "tools-runtimes"),
    ("lenso.agent.management-tools", "tool-providers"),
    ("fixture.management-task-logs", "third-party-logs"),
    (
        "lenso.agent.management-connection",
        "management-connections",
    ),
    ("lenso.agent.session.file", "session"),
    ("lenso.agent.memory.sqlite", "memory"),
    ("lenso.agent.context-compaction", "context-compactor"),
    ("lenso.agent.artifact.file", "artifact"),
];

#[derive(Debug)]
struct TaskSurface;

impl NativePluginFactory for TaskSurface {
    fn package_id(&self) -> &'static str {
        SURFACE
    }
    fn package_version(&self) -> &'static str {
        "0.1.0"
    }
    fn instantiate(
        &self,
        _: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(NativePluginInstance::default())
    }
}

fn host_catalog() -> Result<HostCatalog, String> {
    lenso_agent_tools_plugin::link();
    let generated = NativePluginRegistry::host_catalog(
        PLUGINS.iter().map(|(_, slot)| HostSlot::one(*slot)),
        [],
    )
    .map_err(|_| "fixture Host catalog unavailable".to_owned())?;
    let mut releases = generated
        .plugins()
        .iter()
        .filter(|release| {
            PLUGINS
                .iter()
                .any(|(id, _)| *id == release.descriptor().plugin_id())
        })
        .cloned()
        .collect::<Vec<_>>();
    if releases.len() != PLUGINS.len() {
        return Err("required fixture Plugin is not linked".into());
    }
    releases.push(HostPluginRelease::new(
        PluginDescriptor::new(SURFACE, "0.1.0", "task-surface")
            .with_requirement(CapabilityRequirementPlan::one(
                agent::CAPABILITY_ID,
                agent::DESCRIPTOR_VERSION,
            ))
            .with_requirement(CapabilityRequirementPlan::one(
                model::CAPABILITY_ID,
                model::DESCRIPTOR_VERSION,
            ))
            .with_requirement(CapabilityRequirementPlan::one(
                session::CAPABILITY_ID,
                session::DESCRIPTOR_VERSION,
            )),
    ));
    Ok(HostCatalog::new(
        PLUGINS
            .iter()
            .map(|(_, slot)| HostSlot::one(*slot))
            .chain([HostSlot::one("task-surface")]),
        releases,
        [HostDefaultPlugin::new(SURFACE, "default")],
    ))
}

fn arguments() -> Result<(PathBuf, String, PathBuf), String> {
    let mut args = env::args().skip(1);
    let mut root = None;
    let mut prompt = None;
    let mut receipt = None;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or("missing option value")?;
        match flag.as_str() {
            "--root" => root = Some(PathBuf::from(value)),
            "--prompt" => prompt = Some(value),
            "--receipt" => receipt = Some(PathBuf::from(value)),
            _ => return Err("unsupported task Host option".into()),
        }
    }
    Ok((
        root.ok_or("--root is required")?,
        prompt.ok_or("--prompt is required")?,
        receipt.ok_or("--receipt is required")?,
    ))
}

async fn run(root: PathBuf, prompt: String) -> Result<Value, String> {
    let host = host_catalog()?;
    let metadata = root.join(".lenso");
    fs::create_dir_all(&metadata).map_err(|_| "cannot prepare fixture Host metadata")?;
    fs::write(
        metadata.join("host-catalog.json"),
        serde_json::to_vec(&host).map_err(|_| "cannot encode Host")?,
    )
    .map_err(|_| "cannot save immutable fixture Host catalog")?;
    let resolved =
        lenso_app_authoring::load_resolved_app(&root).map_err(|error| error.to_string())?;
    let plan = resolved.plan().clone();
    let selected = plan
        .plugin_instances()
        .iter()
        .find(|instance| instance.package_id() == "lenso.agent.management-connection")
        .ok_or("delegated Management connection must be explicitly installed")?;
    let config: Value = serde_json::from_str(selected.configuration())
        .map_err(|_| "invalid selected connection configuration")?;
    let label = |name: &str| {
        config[name]
            .as_str()
            .map(str::to_owned)
            .ok_or("selected task binding is incomplete")
    };
    let binding = AgentTaskBinding {
        task_id: label("task_id")?,
        agent_session_id: label("agent_session_id")?,
        delegate_caller: label("delegate_caller")?,
    };
    binding.validate()?;
    let deployment = label("deployment")?;
    let plan_bytes = serde_json::to_vec(&plan).map_err(|_| "cannot encode resolved Plan")?;
    let generation = format!("sha256:{:x}", Sha256::digest(&plan_bytes));
    let selected_instances = plan
        .plugin_instances()
        .iter()
        .map(|instance| instance.instance_key().to_owned())
        .collect::<Vec<_>>();
    let app = Kernel::start_native(
        plan.clone(),
        TokioDriver::new(),
        NativePluginRegistry::new()
            .with_factory(TaskSurface)
            .with_linked_factories(),
    )
    .await
    .map_err(activation_failure)?;
    let input = TurnInput {
        host: &host,
        plan: &plan,
        binding: &binding,
        generation: &generation,
        selected_instances: &selected_instances,
        deployment: &deployment,
    };
    let result = run_turn(&app, &input, prompt).await;
    let shutdown = app.shutdown(Duration::from_secs(2)).await;
    if shutdown != lenso_kernel::ShutdownOutcome::Clean {
        return Err("task graph shutdown did not complete".into());
    }
    result.map(|mut receipt| {
        receipt["shutdown"] = json!("clean");
        receipt
    })
}

fn activation_failure(error: RuntimeFailure) -> String {
    match error {
        RuntimeFailure::MissingPluginFactory {
            instance,
            package_id,
        } => format!("missing selected factory {package_id}/{instance}"),
        RuntimeFailure::InvalidResolvedPlan { detail }
            if detail.starts_with("invalid Tool name") =>
        {
            detail
        }
        RuntimeFailure::InvalidResolvedPlan { .. } => "selected task plan admission failed".into(),
        RuntimeFailure::PluginFailure { .. } => "selected task Plugin activation failed".into(),
        _ => "selected task graph activation failed".into(),
    }
}

struct TurnInput<'a> {
    host: &'a HostCatalog,
    plan: &'a lenso_app_plan::ResolvedAppPlan,
    binding: &'a AgentTaskBinding,
    generation: &'a str,
    selected_instances: &'a [String],
    deployment: &'a str,
}

async fn run_turn(
    app: &lenso_kernel::NativeApp,
    input: &TurnInput<'_>,
    prompt: String,
) -> Result<Value, String> {
    let TurnInput {
        host,
        plan,
        binding,
        generation,
        selected_instances,
        deployment,
    } = input;
    let dependencies = app
        .dependencies(SURFACE_INSTANCE)
        .map_err(|_| "task surface dependencies unavailable")?;
    let models = model::ModelClient::from_dependencies(&dependencies)
        .map_err(|_| "selected model unavailable")?;
    let catalog = models
        .catalog(model::CatalogRequest {})
        .await
        .map_err(|_| "selected model catalog unavailable")?;
    let profile = lenso_agent_host::project_provider_model_catalog(host, plan, Some(&catalog))?
        .resolve_model(lenso_agent_model_fixture_plugin::MODEL_ID)?;
    let sessions = session::SessionClient::from_dependencies(&dependencies)
        .map_err(|_| "selected session unavailable")?;
    sessions
        .open(session::OpenSessionRequest {
            create_session_id: Some(binding.agent_session_id.clone()),
            session_id: None,
        })
        .await
        .map_err(|_| "bound session is unavailable")?;
    let context = InvocationContext::new(71, None, CancellationToken::new())
        .with_extension(
            lenso_agent_loop_plugin::GENERATION_SPEC_DIGEST_EXTENSION,
            generation.as_bytes().to_vec(),
        )
        .map_err(|_| "generation binding unavailable")?
        .with_typed_extension(&profile)
        .map_err(|_| "selected model profile unavailable")?;
    let context = binding.attach(context)?;
    let agent = agent::AgentClient::new(
        app.stream_handle::<agent::Agent>(SURFACE_INSTANCE)
            .map_err(|_| "selected Loop unavailable")?,
    );
    let stream = agent
        .run_turn_with_context(
            context,
            agent::RunTurnRequest {
                input: prompt,
                session_id: Some(binding.agent_session_id.clone()),
                attachments: None,
            },
        )
        .await
        .map_err(|_| "bound task turn could not start")?;
    let mut events = Vec::new();
    let (terminal, terminal_failure) = loop {
        match stream.receive().await.map_err(|_| "task stream failed")? {
            StreamEvent::Message(event) => {
                if events.len() >= 256 {
                    return Err("task event limit exceeded".into());
                }
                events.push(event);
            }
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => break ("succeeded", None),
            StreamEvent::Terminal(Err(error)) => {
                break ("failed", Some(terminal_failure_code(&error)));
            }
        }
    };
    let selected_refs = plan
        .plugin_instances()
        .iter()
        .map(|instance| {
            json!({
                "instance": instance.instance_key(),
                "package_id": instance.package_id(),
                "package_revision": instance.package_revision(),
                "execution_class": instance.execution_class().as_str(),
            })
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"schema":"lenso.agent.management-task-proof@1","terminal":terminal,"terminal_failure":terminal_failure,"task_id":binding.task_id,"agent_session_id":binding.agent_session_id,"delegate_caller":binding.delegate_caller,"generation":generation,"target_deployment":deployment,"selected_instances":selected_instances,"selected_agent_instances":selected_refs,"events":events,"model":"fixture","fixture_model_loop":true,"real_model":"not_run","real_remote_child":"unassessed_by_task_host"}),
    )
}

fn terminal_failure_code(error: &agent::RunTurnError) -> &'static str {
    match error {
        agent::RunTurnError::ConcurrentTurn => "concurrent_turn",
        agent::RunTurnError::ContextLimitExceeded => "context_limit_exceeded",
        agent::RunTurnError::InvalidSession => "invalid_session",
        agent::RunTurnError::StepLimitExceeded => "step_limit_exceeded",
        agent::RunTurnError::ToolCallLimitExceeded => "tool_call_limit_exceeded",
        agent::RunTurnError::ModelFailure { payload }
            if payload.reason_code == "tool_not_allowed" =>
        {
            "tool_not_allowed"
        }
        agent::RunTurnError::ModelFailure { .. } => "model_failure",
        agent::RunTurnError::Unknown(_) => "unknown_domain_error",
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let result = async {
        let (root, prompt, receipt) = arguments()?;
        let proof = tokio::task::LocalSet::new()
            .run_until(run(root, prompt))
            .await?;
        fs::write(
            receipt,
            serde_json::to_vec_pretty(&proof).map_err(|_| "cannot encode receipt")?,
        )
        .map_err(|_| "cannot save receipt")?;
        Ok::<(), String>(())
    }
    .await;
    if let Err(error) = result {
        eprintln!("management task unavailable: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_failure_records_scope_rejection_without_provider_data() {
        let error = agent::RunTurnError::ModelFailure {
            payload: agent::ModelFailurePayload {
                reason_code: "tool_not_allowed".into(),
                message: "private provider payload".into(),
            },
        };
        assert_eq!(terminal_failure_code(&error), "tool_not_allowed");
        let proof = json!({"terminal_failure": terminal_failure_code(&error)});
        assert!(!proof.to_string().contains("private"));
    }

    #[test]
    fn terminal_failure_does_not_promote_unknown_data_to_denial() {
        let error = agent::RunTurnError::ModelFailure {
            payload: agent::ModelFailurePayload {
                reason_code: "future-secret-code".into(),
                message: "private provider payload".into(),
            },
        };
        assert_eq!(terminal_failure_code(&error), "model_failure");
        let future: agent::RunTurnError = serde_json::from_value(json!({
            "code": "future-denial", "payload": {"detail": "private"}
        }))
        .unwrap();
        assert_eq!(terminal_failure_code(&future), "unknown_domain_error");
    }
}

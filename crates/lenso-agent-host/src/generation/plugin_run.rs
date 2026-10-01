//! Isolated task composition over the existing Loop and retained Host bindings.
use super::{AgentApp, NativeApp, TurnGeneration, selected_surface_agent_provider};
use lenso::CtxExt;
use lenso_agent_loop_plugin as _;
use lenso_agent_plugin_run_plugin::{Bindings, Usage};
use lenso_app_plan::{
    CapabilityRequirementPlan,
    authoring::{
        HostCatalog, HostDefaultPlugin, HostPluginRelease, HostSlot, PluginDescriptor,
        PluginRootSnapshot, resolve_plugin_root,
    },
};
use lenso_capability_agent::{self as agent, AgentBehaviorProvenance, AgentTaskBinding};
use lenso_kernel::{
    CancellationToken, InvocationContext, Kernel, RuntimeFailure, ShutdownOutcome, StreamEvent,
};
use lenso_native_adapter::{
    NativePluginFactory, NativePluginFactoryContext, NativePluginInstance, NativePluginRegistry,
};
use sha2::{Digest as _, Sha256};
use std::{cell::RefCell, collections::BTreeSet, rc::Rc, time::Duration};
const SURFACE: &str = "lenso.agent.plugin-run-surface";
#[derive(Debug)]
struct Surface;
impl NativePluginFactory for Surface {
    fn package_id(&self) -> &'static str {
        SURFACE
    }
    fn package_version(&self) -> &'static str {
        "1.0.0"
    }
    fn instantiate(
        &self,
        _: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(NativePluginInstance::default())
    }
}
#[derive(Clone, Debug)]
pub struct RunPolicy {
    pub model: String,
    pub max_calls: u32,
    pub max_output: u64,
    pub input_ceiling: u64,
    pub allowed_tools: BTreeSet<String>,
    pub workspace: String,
    pub session_id: Option<String>,
    pub session_namespace: Option<lenso_capability_agent_session::SessionNamespace>,
    pub expected_generation: Option<String>,
    pub expected_provider: Option<String>,
}
#[derive(Debug, serde::Serialize)]
pub struct RunResult {
    pub text: String,
    pub usage: Usage,
    pub session_id: Option<String>,
}
#[derive(Debug)]
pub struct PluginRun {
    _parent: TurnGeneration,
    app: NativeApp,
    context: InvocationContext,
    usage: Rc<RefCell<Usage>>,
    session_id: String,
    persistent: bool,
}
impl AgentApp {
    fn require_authenticated_session(
        &self,
        generation: &str,
        provider: &str,
    ) -> Result<(), String> {
        let plan = self
            .retained_generation_plan(generation)
            .ok_or("missing Session Generation")?;
        let plugin = plan
            .plugin_instances()
            .iter()
            .find(|p| p.instance_key() == provider)
            .ok_or("missing Session provider")?;
        let config: serde_json::Value =
            serde_json::from_str(plugin.configuration()).map_err(|e| e.to_string())?;
        if !provider.starts_with("lenso.agent.session.sqlite/")
            || !config["authentication"].is_object()
        {
            return Err("plugin history requires authenticated SQLite Session ownership".into());
        }
        Ok(())
    }
    #[expect(
        clippy::too_many_lines,
        reason = "atomic capture of retained bindings and isolated task composition"
    )]
    pub async fn lease_plugin_run(
        &self,
        actor: lenso_auth_sdk::ActorAssertion,
        policy: RunPolicy,
        cancellation: CancellationToken,
    ) -> Result<PluginRun, String> {
        if policy.max_calls == 0
            || policy.max_calls > 16
            || policy.max_output == 0
            || policy.max_output > 4096
            || policy.input_ceiling == 0
            || policy.allowed_tools.iter().any(|name| {
                matches!(
                    name.as_str(),
                    "delegate"
                        | "spawn_subagent"
                        | "send_subagent"
                        | "wait_subagent"
                        | "cancel_subagent"
                        | "list_subagents"
                        | "run_code"
                )
            })
            || !std::path::Path::new(&policy.workspace).is_absolute()
            || policy.workspace.len() > 4096
        {
            return Err("bounded explicit task policy required".into());
        }
        let parent = self.lease_web_turn_for_actor(actor).await?;
        let mut parent_context = parent.invocation_context_for_model_options_with_cancellation(
            Some(&policy.model),
            None,
            None,
            cancellation.clone(),
        )?;
        if let Some(namespace) = &policy.session_namespace {
            namespace.validate()?;
            parent_context = parent_context
                .with_typed_extension(namespace)
                .map_err(|e| e.to_string())?;
        }
        let plan = self
            .retained_generation_plan(parent.generation_spec_digest())
            .ok_or("missing parent Generation plan")?;
        let consumer = selected_surface_agent_provider(&plan)?;
        if policy
            .expected_generation
            .as_deref()
            .is_some_and(|expected| expected != parent.generation_spec_digest())
        {
            return Err("task Generation changed before admission".into());
        }
        let dependencies = parent
            .route
            .target()
            .dependencies(&consumer)
            .map_err(|e| format!("task dependencies: {e:?}"))?;
        if policy.expected_provider.as_ref().is_some_and(|expected| {
            !dependencies.bindings().iter().any(|b| {
                b.capability_id() == "lenso.agent.model@4" && b.provider_instance() == expected
            })
        }) {
            return Err("task Model provider changed before admission".into());
        }
        let model = lenso_capability_agent_model::ModelClient::from_dependencies(&dependencies)
            .map_err(|e| format!("task Model binding: {e:?}"))?;
        let tools = lenso_capability_agent_tools::ToolsClient::from_dependencies(&dependencies)
            .map_err(|e| format!("task Tools binding: {e:?}"))?;
        let prompt = lenso_capability_agent_prompt::PromptClient::from_dependencies(&dependencies)
            .map_err(|e| format!("task Prompt binding: {e:?}"))?;
        // An explicit Session is never created by a run request. Existing owner
        // verification occurs through the original provider before task startup.
        let persistent = policy.session_id.is_some();
        let session_id = policy
            .session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let session = if persistent {
            let provider = dependencies
                .bindings()
                .iter()
                .find(|b| b.capability_id() == "lenso.agent.session@1")
                .ok_or("missing Session binding")?;
            self.require_authenticated_session(
                parent.generation_spec_digest(),
                provider.provider_instance(),
            )?;
            let client =
                lenso_capability_agent_session::SessionClient::from_dependencies(&dependencies)
                    .map_err(|e| format!("task Session binding: {e:?}"))?;
            client
                .read_with_context(
                    parent_context.clone(),
                    lenso_capability_agent_session::ReadSessionRequest {
                        session_id: session_id.clone(),
                        after_revision: "0".into(),
                        limit: 1,
                    },
                )
                .await
                .map_err(|e| format!("existing Session owner denied: {e:?}"))?;
            Some(client)
        } else {
            None
        };
        let usage = Rc::new(RefCell::new(Usage::default()));
        let bindings = Bindings {
            model,
            tools,
            prompt,
            session,
            session_id: session_id.clone(),
            model_id: policy.model.clone(),
            max_calls: policy.max_calls,
            max_output: policy.max_output,
            input_ceiling: policy.input_ceiling,
            allowed_tools: policy.allowed_tools.clone(),
            usage: usage.clone(),
        };
        lenso_agent_plugin_run_plugin::link();
        let available = NativePluginRegistry::host_catalog([], [])
            .map_err(|e| format!("task catalog: {e:?}"))?;
        let releases = available
            .plugins()
            .iter()
            .filter(|p| {
                matches!(
                    p.descriptor().plugin_id(),
                    "lenso.agent.loop" | "lenso.agent.plugin-run"
                )
            })
            .cloned()
            .chain([HostPluginRelease::new(
                PluginDescriptor::new(SURFACE, "1.0.0", "task-surfaces").with_requirement(
                    CapabilityRequirementPlan::one(agent::CAPABILITY_ID, agent::DESCRIPTOR_VERSION),
                ),
            )])
            .collect::<Vec<_>>();
        let catalog=HostCatalog::new([HostSlot::one("agents"),HostSlot::one("run-boundaries"),HostSlot::one("task-surfaces")],releases,
            [HostDefaultPlugin::new("lenso.agent.loop","task").with_configuration(serde_json::json!({
                "model":policy.model,"tool_allowlist":policy.allowed_tools,"max_output_tokens":policy.max_output,"max_steps":policy.max_calls,
                "max_total_steps":policy.max_calls,"max_tool_calls":32,"max_total_tool_calls":32,"max_user_resumes":0,"max_turn_duration_ms":15000,
                "max_history_events":512,"max_compaction_summary_characters":4096,"max_memory_items":1,"max_memory_characters":256,
                "max_parallel_tool_calls":1,"artifact_spill_threshold_bytes":1_048_576
            })),HostDefaultPlugin::new("lenso.agent.plugin-run","boundary"),HostDefaultPlugin::new(SURFACE,"default")]);
        let resolved = resolve_plugin_root(&catalog, &PluginRootSnapshot::default())
            .map_err(|e| format!("task resolution: {e:?}"))?;
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(resolved.plan()).map_err(|e| e.to_string())?)
        );
        let mut context = InvocationContext::new(parent_context.request_id(), None, cancellation);
        for extension in parent_context.extensions() {
            if !matches!(
                extension.key(),
                agent::AGENT_BEHAVIOR_PROVENANCE_EXTENSION
                    | "lenso.agent.interactive-surface@1"
                    | "lenso.agent.approval-scope.v1"
                    | lenso_agent_native_support::WORKSPACE_SCOPE_EXTENSION
            ) {
                context = context
                    .with_extension(extension.key(), extension.value().to_vec())
                    .map_err(|e| e.to_string())?;
            }
        }
        // This task never inherits Full/Assisted approval or extra unmetered
        // approval-model calls. Original explicit allow/deny Hook rules remain.
        context = context
            .with_typed_extension(
                &lenso_agent_interactive_approval_hook_plugin::ApprovalScope {
                    mode: Some(lenso_agent_interactive_approval_hook_plugin::ApprovalMode::Request),
                    user_request: String::new(),
                },
            )
            .map_err(|e| e.to_string())?;
        for extension in parent_context.sealed_extensions() {
            context = context
                .with_sealed_extension(extension.clone())
                .map_err(|e| e.to_string())?;
        }
        context = context
            .with_typed_extension(&AgentBehaviorProvenance::new(digest)?)
            .map_err(|e| e.to_string())?;
        context = context
            .with_typed_extension(&lenso_agent_native_support::WorkspaceScope {
                absolute_path: policy.workspace,
            })
            .map_err(|e| e.to_string())?;
        context =
            lenso_capability_agent_tools::RunScope::new(policy.allowed_tools)?.attach(context)?;
        context = AgentTaskBinding {
            task_id: uuid::Uuid::new_v4().to_string(),
            agent_session_id: session_id.clone(),
            delegate_caller: "lenso.agent.web/web".into(),
        }
        .attach(context)?;
        let app = Kernel::start_native(
            resolved.plan().clone(),
            lenso_runner::TokioDriver::new(),
            NativePluginRegistry::new()
                .with_linked_factories()
                .with_factory(Surface)
                .with_factory_override(lenso_agent_plugin_run_plugin::factory(bindings))
                .map_err(|e| format!("task factory binding: {e:?}"))?,
        )
        .await
        .map_err(|e| format!("task startup: {e:?}"))?;
        Ok(PluginRun {
            _parent: parent,
            app,
            context,
            usage,
            session_id,
            persistent,
        })
    }
}
impl PluginRun {
    pub async fn run(self, prompt: String) -> Result<RunResult, String> {
        let result = self.execute(prompt).await;
        let shutdown = self.app.shutdown(Duration::from_secs(2)).await;
        if shutdown != ShutdownOutcome::Clean {
            return Err("task execution not settled at shutdown".into());
        }
        result
    }
    async fn execute(&self, prompt: String) -> Result<RunResult, String> {
        let handle = self
            .app
            .stream_handle::<agent::Agent>(&format!("{SURFACE}/default"))
            .map_err(|e| format!("task Agent binding: {e:?}"))?;
        let stream = handle
            .open_with_context(
                agent::RUN_TURN_OPERATION,
                self.context.clone(),
                agent::RunTurnRequest {
                    input: prompt,
                    session_id: Some(self.session_id.clone()),
                    attachments: None,
                },
            )
            .await
            .map_err(|e| format!("task open: {e:?}"))?
            .map_err(|e| format!("task rejected: {e:?}"))?;
        stream
            .close_send()
            .await
            .map_err(|e| format!("task send close: {e:?}"))?;
        let mut text = String::new();
        loop {
            match stream
                .receive()
                .await
                .map_err(|e| format!("task stream: {e:?}"))?
            {
                StreamEvent::Message(message) => {
                    if message.kind == Some(agent::RunTurnResponseKind::TextDelta) {
                        text.push_str(&message.text);
                        if text.len() > 1_048_576 {
                            stream.cancel();
                            return Err("task output limit".into());
                        }
                    }
                }
                StreamEvent::PeerHalfClosed => {}
                StreamEvent::Terminal(Ok(())) => break,
                StreamEvent::Terminal(Err(e)) => return Err(format!("task failed: {e:?}")),
            }
        }
        let usage = self.usage.borrow().clone();
        if usage.calls == 0 || usage.calls != usage.completed_calls {
            return Err("task usage incomplete".into());
        }
        Ok(RunResult {
            text,
            usage,
            session_id: self.persistent.then(|| self.session_id.clone()),
        })
    }
}

impl AgentApp {
    pub async fn plugin_session(
        &self,
        actor: lenso_auth_sdk::ActorAssertion,
        id: Option<String>,
        namespace: Option<lenso_capability_agent_session::SessionNamespace>,
    ) -> Result<String, String> {
        let lease = self.lease_web_turn_for_actor(actor).await?;
        let mut context = lease.invocation_context()?;
        if let Some(namespace) = namespace {
            namespace.validate()?;
            context = context
                .with_typed_extension(&namespace)
                .map_err(|e| e.to_string())?;
        }
        let deps = lease
            .route
            .target()
            .dependencies(&lease.consumer_instance)
            .map_err(|e| format!("Session scope route: {e:?}"))?;
        let provider = deps
            .bindings()
            .iter()
            .find(|b| b.capability_id() == "lenso.agent.session@1")
            .ok_or("missing Session binding")?;
        self.require_authenticated_session(
            lease.generation_spec_digest(),
            provider.provider_instance(),
        )?;
        let client = lenso_capability_agent_session::SessionClient::from_dependencies(&deps)
            .map_err(|e| format!("Session scope binding: {e:?}"))?;
        if let Some(id) = id {
            client
                .read_with_context(
                    context,
                    lenso_capability_agent_session::ReadSessionRequest {
                        session_id: id.clone(),
                        after_revision: "0".into(),
                        limit: 1,
                    },
                )
                .await
                .map_err(|e| format!("Session owner rejected: {e:?}"))?;
            Ok(id)
        } else {
            client
                .open_with_context(
                    context,
                    lenso_capability_agent_session::OpenSessionRequest {
                        create_session_id: None,
                        session_id: None,
                    },
                )
                .await
                .map(|r| r.session_id)
                .map_err(|e| format!("Session creation rejected: {e:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn real_loop_task_uses_two_metered_calls_without_history_and_scopes_interactive_sessions()
    {
        tokio::task::LocalSet::new().run_until(Box::pin(async {
            let home=tempfile::tempdir().unwrap();
            let issuer=lenso_auth_sdk::ActorAssertionIssuer::from_signing_key("synthetic.run",[7;32]);
            for (plugin,file,config) in [("lenso.agent.model.fixture","model.toml","model = \"fixture/readme-summary-v1\"\n".to_string()),
                ("lenso.agent.loop","agent.toml","model = \"fixture/readme-summary-v1\"\n".to_string()),
                ("lenso.agent.session.sqlite","sessions.toml",format!("database = {}\n[authentication]\nissuer = \"synthetic.run\"\nverification_key = {}\n",serde_json::to_string(&home.path().join("history.sqlite")).unwrap(),serde_json::to_string(&issuer.public_key_base64()).unwrap()))] {
                let dir=home.path().join("plugins").join(plugin);std::fs::create_dir_all(&dir).unwrap();std::fs::write(dir.join(file),config).unwrap();
            }
            let text_root=home.path().join("plugins/lenso.agent.text-tools");std::fs::create_dir_all(&text_root).unwrap();std::fs::write(text_root.join("default.toml"),"").unwrap();
            let host=crate::AgentHost::builder().plugins(lenso_agent_default_plugins::link).agent_home(home.path()).unwrap().surface(crate::WebSurface::browser()).build().unwrap();
            host.prepare_authoring().unwrap();let mut app=host.run(crate::Profile::Default).await.unwrap();
            let actor=|user:&str|{let now=time::OffsetDateTime::now_utc();issuer.issue(user,"user","synthetic",["open","read","append","list"].map(|op|lenso_auth_sdk::audience("lenso.agent.session@1",op)).into_iter().chain([lenso_auth_sdk::audience("lenso.agent@3","run_turn"),lenso_auth_sdk::audience("lenso.agent.model@4","complete")]),lenso_auth_sdk::Validity::new(now-time::Duration::seconds(1),now+time::Duration::minutes(5)).unwrap(),std::collections::BTreeMap::new())};
            let policy=RunPolicy{model:"fixture/readme-summary-v1".into(),max_calls:2,max_output:1024,input_ceiling:28672,allowed_tools:["uppercase".to_string()].into(),workspace:home.path().to_str().unwrap().into(),session_id:None,session_namespace:None,expected_generation:None,expected_provider:None};
            let mut nested=policy.clone();nested.allowed_tools=["delegate".to_owned(),"run_code".to_owned()].into();
            assert!(app.lease_plugin_run(actor("alice"),nested,CancellationToken::new()).await.is_err());
            let history=app.lease_web_history_for_actor(actor("alice")).await.unwrap();
            assert!(history.list_sessions(50).await.unwrap().sessions.is_empty());
            let result=app.lease_plugin_run(actor("alice"),policy.clone(),CancellationToken::new()).await.unwrap().run("Use the text Plugin to uppercase Lenso plugin.".into()).await.unwrap();
            assert_eq!(result.usage.calls,2);assert!(result.text.contains("LENSO PLUGIN"));assert!(result.session_id.is_none());
            assert!(history.list_sessions(50).await.unwrap().sessions.is_empty());
            let mut limited=policy.clone();limited.max_calls=1;
            assert!(app.lease_plugin_run(actor("alice"),limited,CancellationToken::new()).await.unwrap().run("Use the text Plugin to uppercase Lenso plugin.".into()).await.is_err());
            let mut denied=policy.clone();denied.allowed_tools.clear();
            assert!(app.lease_plugin_run(actor("alice"),denied,CancellationToken::new()).await.unwrap().run("Use the text Plugin to uppercase Lenso plugin.".into()).await.is_err());
            let namespace=lenso_capability_agent_session::SessionNamespace{consumer:"plugin-a/default".into(),user:"alice".into(),project:"one".into()};
            let id=app.plugin_session(actor("alice"),None,Some(namespace.clone())).await.unwrap();
            let mut interactive=policy;interactive.session_id=Some(id.clone());interactive.session_namespace=Some(namespace.clone());
            app.lease_plugin_run(actor("alice"),interactive,CancellationToken::new()).await.unwrap().run("What did you summarize?".into()).await.unwrap();
            assert!(history.list_sessions(50).await.unwrap().sessions.is_empty());
            assert!(app.plugin_session(actor("alice"),Some(id.clone()),Some(namespace.clone())).await.is_ok());
            for bad in [lenso_capability_agent_session::SessionNamespace{consumer:"plugin-b/default".into(),..namespace.clone()},lenso_capability_agent_session::SessionNamespace{project:"two".into(),..namespace.clone()}] {
                assert!(app.plugin_session(actor("alice"),Some(id.clone()),Some(bad)).await.is_err());
            }
            assert!(app.plugin_session(actor("bob"),Some(id),Some(namespace)).await.is_err());
            drop(history);app.shutdown().await.unwrap();
        })).await;
    }
}

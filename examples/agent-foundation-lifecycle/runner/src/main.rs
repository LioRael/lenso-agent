//! Independent Host with explicit Plugin composition and no legacy Host defaults.

mod generation;

use std::{fs, path::Path, time::Duration};

use lenso::CtxExt;
use lenso_agent_artifact_file_plugin as _;
use lenso_agent_context_compaction_plugin as _;
use lenso_agent_loop_plugin as _;
use lenso_agent_memory_sqlite_plugin as _;
use lenso_agent_model_fixture_plugin as _;
use lenso_agent_prompt_plugin as _;
use lenso_agent_session_file_plugin as _;
use lenso_agent_tools_plugin as _;
use lenso_app_plan::authoring::{
    HostCatalog, HostSlot, PluginRootInstance, PluginRootSnapshot, resolve_plugin_root,
};
use lenso_capability_agent as agent;
use lenso_capability_agent_model as model;
use lenso_capability_agent_session as session;
use lenso_capability_agent_tool_provider as tool;
use lenso_kernel::{CancellationToken, Kernel, NativeApp, ShutdownOutcome, StreamEvent};
use lenso_native_adapter::NativePluginRegistry;
use lenso_runner::TokioDriver;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CALLER: &str = "example.foundation-lifecycle-caller/surface";
const TIMEOUT: Duration = Duration::from_secs(5);

struct Fixture {
    app: NativeApp,
    generation_digest: String,
    profile: model::ResolvedTurnProfile,
    catalog: HostCatalog,
    root: PluginRootSnapshot,
}

impl Fixture {
    async fn start(home: &Path, pause_before_second_step: bool) -> Self {
        foundation_lifecycle_caller::link();
        foundation_lifecycle_task_tool::link();
        lenso_agent_tools_plugin::link();
        let catalog = NativePluginRegistry::host_catalog(
            [
                "fixture-surfaces",
                "agents",
                "model",
                "session",
                "memory",
                "artifact",
                "context-compactor",
                "prompt-runtime",
                "tools-runtimes",
                "tool-providers",
                "tool-hooks",
                "run-boundaries",
            ]
            .into_iter()
            .map(HostSlot::many),
            [],
        )
        .expect("public Host Catalog");
        let configured = |plugin, instance, configuration| {
            PluginRootInstance::new(plugin, instance).with_configuration(configuration)
        };
        let root = PluginRootSnapshot::new(
            [],
            [
                PluginRootInstance::new("example.foundation-lifecycle-caller", "surface"),
                configured(
                    "example.foundation-lifecycle-task-tool",
                    "task",
                    json!({
                        "storage_path": home.join("task.json"),
                        "pause_before_second_step": pause_before_second_step
                    }),
                ),
                configured(
                    "lenso.agent.loop",
                    "agent",
                    json!({
                        "model": "fixture/readme-summary-v1", "tool_allowlist": ["read"],
                        "max_steps": 8, "max_tool_calls": 4, "max_parallel_tool_calls": 1,
                        "max_output_tokens": 512, "max_history_events": 128,
                        "max_compaction_summary_characters": 2048,
                        "max_memory_items": 4, "max_memory_characters": 4096
                    }),
                ),
                configured(
                    "lenso.agent.model.fixture",
                    "model",
                    json!({
                        "model": "fixture/readme-summary-v1"
                    }),
                ),
                configured(
                    "lenso.agent.session.file",
                    "sessions",
                    json!({
                        "directory": home.join("sessions")
                    }),
                ),
                configured(
                    "lenso.agent.memory.sqlite",
                    "memory",
                    json!({
                        "database": home.join("memory.sqlite3"), "scope": "foundation-lifecycle",
                        "max_records": 32, "max_item_characters": 4096,
                        "max_recall_items": 4, "max_recall_characters": 4096
                    }),
                ),
                configured(
                    "lenso.agent.artifact.file",
                    "artifacts",
                    json!({
                        "directory": home.join("artifacts"), "max_artifact_bytes": 1048576,
                        "max_total_bytes": 16777216, "max_items": 128
                    }),
                ),
                configured(
                    "lenso.agent.context-compaction",
                    "compactor",
                    json!({
                        "max_input_characters": 262144, "max_summary_characters": 2048,
                        "retain_recent_turns": 4
                    }),
                ),
                PluginRootInstance::new("lenso.agent.prompt", "prompt"),
                PluginRootInstance::new("lenso.agent.tools", "tools"),
            ],
            [],
        );
        let resolved = resolve_plugin_root(&catalog, &root).expect("public Plugin Root resolution");
        let plan = resolved.plan().clone();
        let bytes = serde_json::to_vec_pretty(&plan).expect("resolved Plan evidence");
        fs::write(home.join("resolved-plan.json"), bytes).unwrap();
        let generation = generation::resolve(home, &catalog, &root, &plan);
        let generation_digest = generation.spec.digest().to_owned();
        fs::write(home.join("generation-spec-digest.txt"), &generation_digest).unwrap();
        let app = Kernel::start_native(
            generation.plan,
            TokioDriver::new(),
            NativePluginRegistry::new().with_linked_factories(),
        )
        .await
        .expect("explicit Agent composition starts");
        let model_catalog = app
            .handle::<model::ModelCatalog>(CALLER)
            .unwrap()
            .invoke(model::CATALOG_OPERATION, model::CatalogRequest {})
            .await
            .unwrap()
            .unwrap();
        let catalog_bytes = serde_json::to_vec(&model_catalog).unwrap();
        fs::write(home.join("model-catalog.json"), &catalog_bytes).unwrap();
        let selected = model_catalog
            .models
            .iter()
            .find(|entry| entry.id == "fixture/readme-summary-v1")
            .expect("the exact configured fixture Model is advertised");
        assert_eq!(
            selected.input_modalities,
            vec![model::CatalogInputModality::Text]
        );
        assert_eq!(
            selected.reasoning.status,
            model::CatalogControlStatus::Unsupported
        );
        assert_eq!(
            selected.service_tiers.status,
            model::CatalogControlStatus::Unsupported
        );
        let tokens = |value: &Option<Option<String>>| {
            value
                .as_ref()
                .and_then(|value| value.as_ref())
                .map(|value| value.parse::<u64>().expect("bounded fixture token count"))
        };
        let profile = model::ResolvedTurnProfile {
            catalog_revision: format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&model_catalog.models).unwrap())
            ),
            catalog_provenance: Some(model::ModelCatalogProvenance {
                source: model::ModelCatalogSource::Configured,
                freshness: model::ModelCatalogFreshness::Fresh,
                fetched_at_unix_seconds: None,
                validated_at_unix_seconds: None,
                revision: None,
                max_stale_seconds: None,
            }),
            provider_id: "lenso.agent.model.fixture".into(),
            provider_instance: "lenso.agent.model.fixture/model".into(),
            model: selected.id.clone(),
            reasoning_effort: None,
            reasoning_enabled: None,
            reasoning_budget_tokens: None,
            service_tier: None,
            limits: model::ModelLimits {
                context_window_tokens: tokens(&selected.limits.context_window_tokens),
                max_input_tokens: tokens(&selected.limits.max_input_tokens),
                max_output_tokens: tokens(&selected.limits.max_output_tokens),
            },
            capabilities: model::ModelCapabilities {
                input_modalities: vec![model::ModelInputModality::Text],
                text_output: selected.text_output,
                tool_calls: selected.tool_calls,
                parallel_tool_calls: selected.parallel_tool_calls,
                reasoning: model::ModelReasoningControl::Unsupported,
                service_tiers: model::ModelServiceTierControl::Unsupported,
            },
            wire_protocol: model::ModelWireProtocol::from_id(&selected.wire_protocol).unwrap(),
            compaction_compatibility: selected.compaction_compatibility.clone(),
        };
        assert_eq!(
            model_catalog.provenance.source,
            model::CatalogSource::Configured
        );
        assert_eq!(
            model_catalog.provenance.freshness,
            model::CatalogFreshness::Fresh
        );
        Self {
            app,
            generation_digest,
            profile,
            catalog,
            root,
        }
    }

    fn context(&self) -> lenso_kernel::InvocationContext {
        // This standalone Host retains one immutable native Generation for its
        // process lifetime and attaches its canonical public GenerationSpec.
        // This ordinary provenance is not a sealed authority grant.
        self.app
            .invocation_context_after(TIMEOUT, CancellationToken::new())
            .with_extension(
                lenso_agent_loop_plugin::GENERATION_SPEC_DIGEST_EXTENSION,
                self.generation_digest.as_bytes().to_vec(),
            )
            .expect("Host provenance extension")
            .with_typed_extension(&self.profile)
            .expect("exact selected Model profile")
    }

    async fn history(&self, session_id: &str) -> Vec<session::ReadSessionResponseEventsItem> {
        self.app
            .handle::<session::SessionRead>(CALLER)
            .unwrap()
            .invoke(
                session::READ_OPERATION,
                session::ReadSessionRequest {
                    session_id: session_id.into(),
                    after_revision: "0".into(),
                    limit: 1000,
                },
            )
            .await
            .unwrap()
            .unwrap()
            .events
    }

    async fn status(&self) -> Value {
        let response = self
            .app
            .handle::<tool::ToolProviderExecute>(CALLER)
            .unwrap()
            .invoke(
                tool::EXECUTE_OPERATION,
                tool::ExecuteRequest {
                    name: "fixture_task_status".into(),
                    arguments_json: "{}".try_into().unwrap(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&response.content).unwrap()
    }

    async fn turn(&self, session_id: Option<&str>, input: &str) -> Turn {
        let stream = self
            .app
            .stream_handle::<agent::Agent>(CALLER)
            .unwrap()
            .open_with_context(
                agent::RUN_TURN_OPERATION,
                self.context(),
                agent::RunTurnRequest {
                    input: input.into(),
                    attachments: None,
                    session_id: session_id.map(str::to_owned),
                },
            )
            .await
            .unwrap()
            .unwrap();
        stream.close_send().await.unwrap();
        let mut result = Turn::default();
        tokio::time::timeout(TIMEOUT, async {
            loop {
                match stream.receive().await.expect("Agent receive") {
                    StreamEvent::Message(message) => {
                        if message.is_text_delta() {
                            result.text.push_str(&message.text);
                        }
                        if let Some(id) = message.session_id {
                            result.session_id = id;
                        }
                        match message.kind {
                            Some(agent::RunTurnResponseKind::ToolStarted) => result.started += 1,
                            Some(agent::RunTurnResponseKind::ToolCompleted) => {
                                result.completed += 1
                            }
                            Some(agent::RunTurnResponseKind::ToolFailed) => result.failed += 1,
                            _ => {}
                        }
                    }
                    StreamEvent::PeerHalfClosed => {}
                    StreamEvent::Terminal(Ok(())) => break,
                    StreamEvent::Terminal(Err(error)) => panic!("Agent turn failed: {error:?}"),
                }
            }
        })
        .await
        .expect("bounded Agent turn completion");
        assert!(!result.session_id.is_empty());
        result
    }

    async fn pending(&self, session_id: &str, input: &str, cancel: bool) {
        let before = self.history(session_id).await;
        let revision = before.last().unwrap().revision.parse::<u64>().unwrap();
        let stream = self
            .app
            .stream_handle::<agent::Agent>(CALLER)
            .unwrap()
            .open_with_context(
                agent::RUN_TURN_OPERATION,
                self.context(),
                agent::RunTurnRequest {
                    input: input.into(),
                    attachments: None,
                    session_id: Some(session_id.into()),
                },
            )
            .await
            .unwrap()
            .unwrap();
        stream.close_send().await.unwrap();
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let events = self.history(session_id).await;
                let models = events.iter().filter(|event| {
                    event.revision.parse::<u64>().unwrap() > revision
                        && event.kind == session::ReadSessionResponseEventsItemKind::ModelRequested
                }).count();
                if models >= 1 { break; }
                tokio::select! {
                    event = stream.receive() => match event.expect("pending turn receive") {
                        StreamEvent::Message(_) | StreamEvent::PeerHalfClosed => {}
                        StreamEvent::Terminal(result) => panic!("pending Model terminated early: {result:?}"),
                    },
                    () = tokio::time::sleep(Duration::from_millis(5)) => {}
                }
            }
        }).await.expect("pending Model request is durably recorded");
        if !cancel {
            // Explicit fault injection: no destructors/shutdown. All preceding
            // state is persisted through public Plugin operations.
            println!("{{\"phase\":\"resume\",\"fault\":\"process_exit_with_open_turn\"}}");
            std::process::exit(0);
        }
        stream.cancel();
        tokio::time::timeout(TIMEOUT, async {
            loop {
                if self.history(session_id).await.iter().any(|event| {
                    event.revision.parse::<u64>().unwrap() > revision
                        && event.kind == session::ReadSessionResponseEventsItemKind::TurnCancelled
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("cancellation has a durable terminal fact");
    }

    async fn cancel_tool(&self, home: &Path, session_id: &str) {
        let stream = self
            .app
            .stream_handle::<agent::Agent>(CALLER)
            .unwrap()
            .open_with_context(
                agent::RUN_TURN_OPERATION,
                self.context(),
                agent::RunTurnRequest {
                    input: "Read README.md twice.".into(),
                    attachments: None,
                    session_id: Some(session_id.into()),
                },
            )
            .await
            .unwrap()
            .unwrap();
        stream.close_send().await.unwrap();
        tokio::time::timeout(TIMEOUT, async {
            loop {
                // The test owns this fixture Plugin's store. Reading its
                // evidence does not occupy the pending execute operation or
                // inspect any private framework/Session storage format.
                let state: Value =
                    serde_json::from_slice(&fs::read(home.join("task.json")).unwrap()).unwrap();
                if state["status"] == "waiting_before_step_2" {
                    break;
                }
                tokio::select! {
                    event = stream.receive() => match event.expect("pending Tool turn receive") {
                        StreamEvent::Message(_) | StreamEvent::PeerHalfClosed => {}
                        StreamEvent::Terminal(result) => panic!("pending Tool terminated early: {result:?}"),
                    },
                    () = tokio::time::sleep(Duration::from_millis(5)) => {}
                }
            }
        })
        .await
        .expect("stateful Tool reached its cancellation boundary");
        stream.cancel();
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let state: Value =
                    serde_json::from_slice(&fs::read(home.join("task.json")).unwrap()).unwrap();
                if state["status"] == "cancelled_before_step_2"
                    && self.history(session_id).await.iter().any(|event| {
                        event.kind == session::ReadSessionResponseEventsItemKind::TurnCancelled
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("cancellation reached the Tool and the Session");
    }

    async fn shutdown(self) {
        assert_eq!(self.app.shutdown(TIMEOUT).await, ShutdownOutcome::Clean);
    }

    async fn remove(self, home: &Path) {
        let before = fs::read(home.join("task.json")).unwrap();
        assert_eq!(self.app.shutdown(TIMEOUT).await, ShutdownOutcome::Clean);
        let removed = [
            "example.foundation-lifecycle-task-tool",
            "example.foundation-lifecycle-caller",
        ];
        let root = PluginRootSnapshot::new(
            [],
            self.root
                .instances()
                .iter()
                .filter(|instance| !removed.contains(&instance.id().plugin_id()))
                .cloned(),
            [],
        );
        let resolved =
            resolve_plugin_root(&self.catalog, &root).expect("removal resolves a new composition");
        assert!(
            resolved
                .plan()
                .plugin_instances()
                .iter()
                .all(|instance| !removed.contains(&instance.package_id()))
        );
        fs::write(
            home.join("removed-plan.json"),
            serde_json::to_vec_pretty(resolved.plan()).unwrap(),
        )
        .unwrap();
        let replacement = Kernel::start_native(
            resolved.plan().clone(),
            TokioDriver::new(),
            NativePluginRegistry::new().with_linked_factories(),
        )
        .await
        .expect("replacement is ready without fixture Plugins");
        assert!(replacement.stream_handle::<agent::Agent>(CALLER).is_err());
        assert_eq!(replacement.shutdown(TIMEOUT).await, ShutdownOutcome::Clean);
        assert_eq!(
            fs::read(home.join("task.json")).unwrap(),
            before,
            "removal retains Plugin-owned durable facts"
        );
        println!(
            "{}",
            json!({"removal":"ready_without_task_and_test_caller", "durable_bytes":"unchanged"})
        );
    }
}

#[derive(Default)]
struct Turn {
    text: String,
    session_id: String,
    started: u32,
    completed: u32,
    failed: u32,
}

async fn phase(name: &str, home: &Path) {
    fs::create_dir_all(home).unwrap();
    let fixture = Fixture::start(home, name == "start").await;
    match name {
        "start" => {
            let turn = fixture.turn(None, "Read README.md twice.").await;
            assert_eq!((turn.started, turn.completed, turn.failed), (2, 1, 1));
            assert!(turn.text.contains("step_1_committed"));
            fs::write(home.join("session-id.txt"), &turn.session_id).unwrap();
            let state = fixture.status().await;
            assert_eq!(state["attempts"], 2);
            assert_eq!(state["retryable_failures"], 1);
            assert_eq!(state["effects"], json!(["readme-step-1"]));
            fixture.cancel_tool(home, &turn.session_id).await;
            let cancelled = fixture.status().await;
            assert_eq!(cancelled["attempts"], 3);
            assert_eq!(cancelled["effects"], state["effects"]);
            assert_eq!(cancelled["status"], "cancelled_before_step_2");
            fixture
                .pending(&turn.session_id, "Remain pending until cancelled.", true)
                .await;
            assert_eq!(
                fixture.status().await,
                cancelled,
                "Model cancellation does not undo or advance task effects"
            );
            println!(
                "{}",
                json!({"phase":"start", "session_id":turn.session_id, "task":cancelled})
            );
        }
        "resume" => {
            let session_id = fs::read_to_string(home.join("session-id.txt")).unwrap();
            let before = fixture.status().await;
            assert_eq!(before["effects"], json!(["readme-step-1"]));
            assert!(fixture.history(&session_id).await.iter().any(|event| {
                event.kind == session::ReadSessionResponseEventsItemKind::TurnCancelled
            }));
            let turn = fixture
                .turn(Some(&session_id), "Read README.md twice.")
                .await;
            assert_eq!(turn.session_id, session_id);
            assert_eq!((turn.started, turn.completed, turn.failed), (2, 2, 0));
            let state = fixture.status().await;
            assert_eq!(state["status"], "completed");
            assert_eq!(state["attempts"], 5);
            assert_eq!(state["effects"], json!(["readme-step-1", "readme-step-2"]));
            fixture
                .pending(&session_id, "Remain pending until cancelled.", false)
                .await;
            unreachable!("fault injection exits the process");
        }
        "replay" => {
            let session_id = fs::read_to_string(home.join("session-id.txt")).unwrap();
            let before = fixture.status().await;
            assert_eq!(before["attempts"], 5);
            let turn = fixture
                .turn(Some(&session_id), "Read README.md twice.")
                .await;
            assert_eq!((turn.started, turn.completed, turn.failed), (2, 2, 0));
            assert_eq!(turn.session_id, session_id);
            let state = fixture.status().await;
            assert_eq!(state["attempts"], 7);
            assert_eq!(
                state["effects"], before["effects"],
                "completed local effects are never replayed"
            );
            let history = fixture.history(&session_id).await;
            let interrupted = history
                .iter()
                .filter(|event| {
                    event.kind == session::ReadSessionResponseEventsItemKind::TurnFailed
                        && event.payload_json.as_str().contains("host_interrupted")
                })
                .count();
            assert_eq!(
                interrupted, 1,
                "restart closes exactly one interrupted turn"
            );
            assert_eq!(
                history
                    .iter()
                    .filter(|event| {
                        event.kind == session::ReadSessionResponseEventsItemKind::TurnCancelled
                    })
                    .count(),
                2,
                "both cancelled turns stay terminal"
            );
            fs::write(
                home.join("session-evidence.json"),
                serde_json::to_vec_pretty(&history).unwrap(),
            )
            .unwrap();
            println!(
                "{}",
                json!({"phase":"replay", "session_id":session_id,
                "task":state, "interrupted_turns":interrupted, "events":history.len()})
            );
        }
        _ => panic!("unknown phase {name}"),
    }
    if name == "replay" {
        fixture.remove(home).await;
    } else {
        fixture.shutdown().await;
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut arguments = std::env::args().skip(1);
    let name = arguments.next().expect("phase: start, resume, replay");
    let home = std::path::PathBuf::from(arguments.next().expect("absolute fixture directory"));
    assert!(home.is_absolute());
    tokio::task::LocalSet::new()
        .run_until(phase(&name, &home))
        .await;
}

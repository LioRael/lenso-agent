use std::time::Duration;

use external_agent_caller as caller_plugin;
use external_alternate_agent_loop as loop_plugin;
use external_final_approval as approval_plugin;
use external_inspect_model as model_plugin;
use external_secret_tool as tool_plugin;
use external_turn_processor as processor_plugin;
use lenso_agent_tools_plugin as aggregate_tools_plugin;
use lenso_app_plan::authoring::{
    HostBinding, HostCatalog, HostSlot, PluginInstanceId, PluginRootInstance, PluginRootSnapshot,
    resolve_plugin_root,
};
use lenso_capability_agent as agent;
use lenso_capability_agent_model as model;
use lenso_capability_agent_tool_hook as hook;
use lenso_capability_agent_tool_provider as tool_provider;
use lenso_capability_agent_tools as tools;
use lenso_capability_agent_turn_processing as processing;
use lenso_kernel::{CancellationToken, Kernel, RuntimeFailure, ShutdownOutcome, StreamEvent};
use lenso_native_adapter::NativePluginRegistry;
use lenso_runner::TokioDriver;

const CALLER: &str = "example.agent-caller";
const LOOP: &str = "example.alternate-agent-loop";
const MODEL: &str = "example.inspect-model";
const PROCESSOR: &str = "example.turn-processor";
const TOOLS: &str = "lenso.agent.tools";
const TOOL_PROVIDER: &str = "example.secret-tool";
const APPROVAL: &str = "example.final-approval";

fn linked_host() -> HostCatalog {
    caller_plugin::link();
    loop_plugin::link();
    model_plugin::link();
    processor_plugin::link();
    aggregate_tools_plugin::link();
    tool_plugin::link();
    approval_plugin::link();
    NativePluginRegistry::host_catalog(
        [
            HostSlot::one("agent-callers"),
            HostSlot::one("agents"),
            HostSlot::one("models"),
            HostSlot::one("tools-runtimes"),
            HostSlot::many("tool-providers"),
            HostSlot::many("tool-hooks"),
            HostSlot::many("turn-processors"),
        ],
        [],
    )
    .expect("linked third-party processing fixture descriptors")
    .with_bindings([
        HostBinding::to_instance(
            PluginInstanceId::new(CALLER, "surface"),
            agent::CAPABILITY_ID,
            PluginInstanceId::new(LOOP, "alternate"),
        ),
        HostBinding::to_instance(
            PluginInstanceId::new(LOOP, "alternate"),
            model::CAPABILITY_ID,
            PluginInstanceId::new(MODEL, "inspect"),
        ),
        HostBinding::to_instance(
            PluginInstanceId::new(LOOP, "alternate"),
            tools::CAPABILITY_ID,
            PluginInstanceId::new(TOOLS, "aggregate"),
        ),
        HostBinding::to_instances(
            PluginInstanceId::new(LOOP, "alternate"),
            processing::CAPABILITY_ID,
            [
                PluginInstanceId::new(PROCESSOR, "first"),
                PluginInstanceId::new(PROCESSOR, "second"),
            ],
        ),
        HostBinding::to_instance(
            PluginInstanceId::new(TOOLS, "aggregate"),
            tool_provider::CAPABILITY_ID,
            PluginInstanceId::new(TOOL_PROVIDER, "secret"),
        ),
        HostBinding::to_instance(
            PluginInstanceId::new(TOOLS, "aggregate"),
            hook::CAPABILITY_ID,
            PluginInstanceId::new(APPROVAL, "final"),
        ),
        HostBinding::to_instances(
            PluginInstanceId::new(TOOLS, "aggregate"),
            processing::CAPABILITY_ID,
            [
                PluginInstanceId::new(PROCESSOR, "first"),
                PluginInstanceId::new(PROCESSOR, "second"),
            ],
        ),
    ])
}

fn root() -> PluginRootSnapshot {
    PluginRootSnapshot::new(
        [],
        [
            PluginRootInstance::new(CALLER, "surface"),
            PluginRootInstance::new(LOOP, "alternate"),
            PluginRootInstance::new(MODEL, "inspect").with_configuration(serde_json::json!({})),
            PluginRootInstance::new(TOOLS, "aggregate"),
            PluginRootInstance::new(TOOL_PROVIDER, "secret")
                .with_configuration(serde_json::json!({})),
            PluginRootInstance::new(APPROVAL, "final").with_configuration(serde_json::json!({})),
            PluginRootInstance::new(PROCESSOR, "first").with_configuration(serde_json::json!({
                "label": "first",
                "rewrite_tool_arguments": true,
                "redact_tool_result": true
            })),
            PluginRootInstance::new(PROCESSOR, "second").with_configuration(serde_json::json!({
                "label": "second",
                "rewrite_tool_arguments": false,
                "redact_tool_result": false
            })),
        ],
        [],
    )
}

#[tokio::test(flavor = "current_thread")]
async fn an_external_loop_projects_model_input_and_tool_results_without_default_loop() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let resolved = resolve_plugin_root(&linked_host(), &root())
                .expect("the external Host Plan resolves through public APIs");
            let default_loop = resolved
                .plan()
                .plugin_instances()
                .iter()
                .any(|instance| instance.package_id() == "lenso.agent.loop");
            assert!(!default_loop, "V04 must not link the default Loop");

            let app = Kernel::start_native(
                resolved.plan().clone(),
                TokioDriver::new(),
                NativePluginRegistry::new().with_linked_factories(),
            )
            .await
            .expect("third-party Agent composition starts");
            let handle = app
                .stream_handle::<agent::Agent>("example.agent-caller/surface")
                .expect("external Surface receives the external Agent binding");
            let stream = handle
                .open(
                    agent::RUN_TURN_OPERATION,
                    agent::RunTurnRequest {
                        attachments: None,
                        input: "inspect the transformed request".to_owned(),
                        session_id: None,
                    },
                )
                .await
                .expect("Agent stream infrastructure")
                .expect("external Agent accepts the turn");
            stream
                .close_send()
                .await
                .expect("close Agent input direction");

            let mut text = String::new();
            let mut metadata = None;
            loop {
                match stream.receive().await.expect("receive Agent output") {
                    StreamEvent::Message(message) => {
                        text.push_str(&message.text);
                        metadata = message.metadata_json.or(metadata);
                    }
                    StreamEvent::PeerHalfClosed => {}
                    StreamEvent::Terminal(Ok(())) => break,
                    StreamEvent::Terminal(Err(error)) => panic!("external Agent failed: {error:?}"),
                }
            }

            assert!(
                text.contains("[second][first]inspect the transformed request"),
                "external Model observed: {text}"
            );
            assert!(
                text.contains("[second]Tool returned a REDACTED_RESULT."),
                "external Model observed: {text}"
            );
            assert!(!text.contains("SECRET_RESULT"));
            let metadata: serde_json::Value = serde_json::from_str(
                metadata
                    .expect("external Agent emitted processing evidence")
                    .as_str(),
            )
            .expect("Agent evidence is JSON");
            for stage in ["model_request", "tool_result"] {
                let providers = metadata[stage]
                    .as_array()
                    .expect("each processing stage is an ordered array")
                    .iter()
                    .map(|entry| entry["provider_instance"].as_str().unwrap_or_default())
                    .collect::<Vec<_>>();
                assert_eq!(
                    providers,
                    [
                        "example.turn-processor/first",
                        "example.turn-processor/second"
                    ],
                    "{stage} follows the resolved Plan order"
                );
            }

            let cancelled = CancellationToken::new();
            cancelled.cancel();
            let result = handle
                .open_with_context(
                    agent::RUN_TURN_OPERATION,
                    app.invocation_context_after(Duration::from_secs(1), cancelled),
                    agent::RunTurnRequest {
                        attachments: None,
                        input: "cancel before strategy work".to_owned(),
                        session_id: None,
                    },
                )
                .await;
            assert!(matches!(result, Err(RuntimeFailure::Cancelled { .. })));

            assert_eq!(
                app.shutdown(Duration::from_secs(1)).await,
                ShutdownOutcome::Clean
            );
        })
        .await;
}

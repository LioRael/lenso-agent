use std::{fs, time::Duration};

use external_authority_caller as caller_plugin;
use external_policy_tool::{self as tool_plugin, INPUT_SCHEMA};
use external_tenant_policy as policy_plugin;
use lenso_agent_tools_plugin as aggregate_tools_plugin;
use lenso_app_plan::authoring::{
    HostBinding, HostCatalog, HostSlot, PluginInstanceId, PluginRootInstance, PluginRootSnapshot,
    resolve_plugin_root,
};
use lenso_capability_agent_dynamic_authority::{
    self as authority, ResourceIdentity, SelectRequest,
};
use lenso_capability_agent_tool_hook as hook;
use lenso_capability_agent_tool_provider as tool_provider;
use lenso_capability_agent_tools as tools;
use lenso_kernel::{CancellationToken, Kernel, RuntimeFailure, ShutdownOutcome};
use lenso_native_adapter::NativePluginRegistry;
use lenso_runner::TokioDriver;
use sha2::{Digest as _, Sha256};

const CALLER: &str = "example.authority-caller";
const CALLER_INSTANCE: &str = "example.authority-caller/surface";
const AUTHORITY: &str = "example.tenant-policy";
const TOOLS: &str = "lenso.agent.tools";
const TOOL_PROVIDER: &str = "example.policy-tool";

fn host() -> HostCatalog {
    caller_plugin::link();
    policy_plugin::link();
    aggregate_tools_plugin::link();
    tool_plugin::link();
    NativePluginRegistry::host_catalog(
        [
            HostSlot::one("tool-callers"),
            HostSlot::one("dynamic-authorities"),
            HostSlot::one("tools-runtimes"),
            HostSlot::many("tool-providers"),
        ],
        [],
    )
    .unwrap()
    .with_bindings([
        HostBinding::to_instance(
            PluginInstanceId::new(TOOLS, "aggregate"),
            hook::CAPABILITY_ID,
            PluginInstanceId::new(AUTHORITY, "default"),
        ),
        HostBinding::to_instance(
            PluginInstanceId::new(CALLER, "surface"),
            authority::CAPABILITY_ID,
            PluginInstanceId::new(AUTHORITY, "default"),
        ),
        HostBinding::to_instance(
            PluginInstanceId::new(CALLER, "surface"),
            tools::CAPABILITY_ID,
            PluginInstanceId::new(TOOLS, "aggregate"),
        ),
        HostBinding::to_instance(
            PluginInstanceId::new(TOOLS, "aggregate"),
            tool_provider::CAPABILITY_ID,
            PluginInstanceId::new(TOOL_PROVIDER, "default"),
        ),
        HostBinding::to_instances(
            PluginInstanceId::new(TOOLS, "aggregate"),
            authority::CAPABILITY_ID,
            [PluginInstanceId::new(AUTHORITY, "default")],
        ),
    ])
}

fn root(effect_path: &str, policy_path: &str) -> PluginRootSnapshot {
    PluginRootSnapshot::new(
        [],
        [
            PluginRootInstance::new(CALLER, "surface"),
            PluginRootInstance::new(AUTHORITY, "default").with_configuration(serde_json::json!({
                "policy_path": policy_path,
            })),
            PluginRootInstance::new(TOOLS, "aggregate"),
            PluginRootInstance::new(TOOL_PROVIDER, "default").with_configuration(
                serde_json::json!({
                    "effect_path": effect_path,
                }),
            ),
        ],
        [],
    )
}

fn selected_resource() -> ResourceIdentity {
    ResourceIdentity {
        kind: "tool".to_owned(),
        name: "read_policy_value".to_owned(),
        provider_instance: "example.policy-tool/default".to_owned(),
        revision: format!("sha256:{:x}", Sha256::digest(INPUT_SCHEMA)),
    }
}

fn execute_request() -> tools::ExecuteRequest {
    tools::ExecuteRequest {
        name: "read_policy_value".to_owned(),
        arguments_json: r#"{"key":"alpha"}"#.try_into().unwrap(),
    }
}

fn host_context(app: &lenso_kernel::NativeApp) -> lenso_kernel::InvocationContext {
    app.invocation_context_after(Duration::from_secs(1), CancellationToken::new())
}

#[tokio::test(flavor = "current_thread")]
async fn copied_external_plugin_revalidates_a_sealed_tool_grant_at_execution() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let temporary = tempfile::tempdir().unwrap();
            let effect_path = temporary.path().join("tool-effects.log");
            let policy_path = temporary.path().join("tenant-policy.txt");
            fs::write(&policy_path, "active").unwrap();
            let effect = effect_path.to_str().unwrap();
            let policy = policy_path.to_str().unwrap();
            let resolved = resolve_plugin_root(&host(), &root(effect, policy)).unwrap();
            let app = Kernel::start_native(
                resolved.plan().clone(),
                TokioDriver::new(),
                NativePluginRegistry::new().with_linked_factories(),
            )
            .await
            .unwrap();

            let select = app
                .handle::<authority::DynamicAuthoritySelect>(CALLER_INSTANCE)
                .unwrap();
            let invalid = select
                .invoke(
                    authority::SELECT_OPERATION,
                    SelectRequest {
                        selection_id: "wrong-resource".to_owned(),
                        requested_resources: vec![ResourceIdentity {
                            name: "other_tool".to_owned(),
                            ..selected_resource()
                        }],
                        context_json: "{}".try_into().unwrap(),
                    },
                )
                .await
                .unwrap();
            assert!(matches!(
                invalid,
                Err(authority::SelectError::InvalidSelection)
            ));
            let snapshot = select
                .invoke(
                    authority::SELECT_OPERATION,
                    SelectRequest {
                        selection_id: "tenant-a-read-alpha".to_owned(),
                        requested_resources: vec![selected_resource()],
                        context_json: "{}".try_into().unwrap(),
                    },
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(snapshot.authority_instance, "example.tenant-policy/default");

            let execute = app.handle::<tools::ToolsExecute>(CALLER_INSTANCE).unwrap();
            let approved_context = authority::attach_tool_authority(
                host_context(&app),
                snapshot.clone(),
                "fixture-host-issued-selection-proof",
            )
            .unwrap();
            let approved = execute
                .invoke_with_context(
                    tools::EXECUTE_OPERATION,
                    approved_context,
                    execute_request(),
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(approved.content, "policy value alpha");
            assert_eq!(
                fs::read_to_string(&effect_path).unwrap(),
                "executed-alpha\n"
            );

            fs::write(&policy_path, "revoked").unwrap();
            let revoked_context = authority::attach_tool_authority(
                host_context(&app),
                snapshot.clone(),
                "fixture-host-issued-selection-proof",
            )
            .unwrap();
            let denied = execute
                .invoke_with_context(tools::EXECUTE_OPERATION, revoked_context, execute_request())
                .await
                .unwrap();
            assert!(matches!(
                denied,
                Err(tools::ExecuteError::ToolError { payload })
                    if payload.provider_code == "dynamic_authorization_denied"
                        && payload.details_json.as_str().contains("tenant_revoked")
            ));
            assert_eq!(
                fs::read_to_string(&effect_path).unwrap(),
                "executed-alpha\n"
            );

            let settlement = policy_path.with_extension("settled");
            fs::remove_file(&settlement).unwrap();
            fs::write(&policy_path, "revoke-on-approval").unwrap();
            let revoked_during_approval = execute
                .invoke_with_context(
                    tools::EXECUTE_OPERATION,
                    authority::attach_tool_authority(host_context(&app), snapshot.clone(), "proof")
                        .unwrap(),
                    execute_request(),
                )
                .await
                .unwrap();
            assert!(
                matches!(revoked_during_approval, Err(tools::ExecuteError::ToolError { payload })
                if payload.provider_code == "dynamic_authorization_denied")
            );
            assert!(
                settlement.is_file(),
                "started hooks are settled on final denial"
            );
            assert_eq!(
                fs::read_to_string(&effect_path).unwrap(),
                "executed-alpha\n"
            );

            fs::remove_file(&settlement).unwrap();
            fs::write(&policy_path, "revoke-on-approval").unwrap();
            let stream = app
                .stream_handle::<tools::ToolsExecuteStream>(CALLER_INSTANCE)
                .unwrap();
            let rejected = stream
                .open_with_context(
                    tools::EXECUTE_STREAM_OPERATION,
                    authority::attach_tool_authority(host_context(&app), snapshot, "proof")
                        .unwrap(),
                    tools::ExecuteStreamRequest {
                        name: "read_policy_value".to_owned(),
                        arguments_json: r#"{"key":"alpha"}"#.try_into().unwrap(),
                    },
                )
                .await
                .unwrap();
            assert!(
                matches!(rejected, Err(tools::ExecuteStreamError::ToolError { payload })
                if payload.provider_code == "dynamic_authorization_denied")
            );
            assert!(settlement.is_file());
            assert_eq!(
                fs::read_to_string(&effect_path).unwrap(),
                "executed-alpha\n"
            );

            // Without a sealed selection, the existing static Plan route stays
            // usable even while this optional policy Provider is revoked.
            let static_result = execute
                .invoke(tools::EXECUTE_OPERATION, execute_request())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(static_result.content, "policy value alpha");
            assert_eq!(
                fs::read_to_string(&effect_path).unwrap(),
                "executed-alpha\nexecuted-alpha\n"
            );

            let forged_context = host_context(&app)
                .with_extension(
                    authority::TOOL_AUTHORITY_EXTENSION,
                    br#"{"snapshot":"ordinary-user-input"}"#.to_vec(),
                )
                .unwrap();
            let forged = execute
                .invoke_with_context(tools::EXECUTE_OPERATION, forged_context, execute_request())
                .await;
            assert!(matches!(
                forged,
                Err(RuntimeFailure::PluginFailure { detail })
                    if detail.contains("ordinary invocation input cannot impersonate")
            ));
            assert_eq!(
                app.shutdown(Duration::from_secs(1)).await,
                ShutdownOutcome::Clean
            );
        })
        .await;
}

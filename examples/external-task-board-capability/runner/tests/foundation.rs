use std::time::Duration;

use external_task_board as task_board;
use external_task_board_audit as audit;
use external_task_board_caller as caller_plugin;
use external_task_board_provider as provider_plugin;
use external_task_board_report as report;
use external_task_board_report_plugin as report_plugin;
use lenso_app_plan::authoring::{
    HostBinding, HostCatalog, HostSlot, PluginInstanceId, PluginRootInstance, PluginRootSnapshot,
    resolve_plugin_root,
};
use lenso_kernel::{CancellationToken, Kernel, RuntimeFailure, ShutdownOutcome};
use lenso_native_adapter::NativePluginRegistry;
use lenso_runner::TokioDriver;

const PROVIDER: &str = "example.external-task-board-provider";
const REPORT: &str = "example.external-task-board-report";
const CALLER: &str = "example.external-task-board-caller";

fn linked_host() -> HostCatalog {
    provider_plugin::link();
    report_plugin::link();
    caller_plugin::link();
    NativePluginRegistry::host_catalog(
        [
            HostSlot::one("task-boards"),
            HostSlot::one("task-board-reports"),
            HostSlot::one("task-board-callers"),
        ],
        [],
    )
    .expect("linked third-party Plugin descriptors")
    .with_bindings([
        HostBinding::to_instance(
            PluginInstanceId::new(REPORT, "report"),
            task_board::CAPABILITY_ID,
            PluginInstanceId::new(PROVIDER, "board"),
        ),
        HostBinding::to_instance(
            PluginInstanceId::new(CALLER, "caller"),
            report::CAPABILITY_ID,
            PluginInstanceId::new(REPORT, "report"),
        ),
        HostBinding::to_instance(
            PluginInstanceId::new(CALLER, "caller"),
            audit::CAPABILITY_ID,
            PluginInstanceId::new(REPORT, "report"),
        ),
    ])
}

fn root(provider_prefix: &str) -> PluginRootSnapshot {
    PluginRootSnapshot::new(
        [],
        [
            PluginRootInstance::new(PROVIDER, "board")
                .with_configuration(serde_json::json!({"prefix": provider_prefix})),
            PluginRootInstance::new(REPORT, "report")
                .with_configuration(serde_json::json!({"label": "summary"})),
            PluginRootInstance::new(CALLER, "caller"),
        ],
        [],
    )
}

#[test]
fn invalid_explicit_provider_binding_is_rejected_before_plugin_activation() {
    provider_plugin::link();
    report_plugin::link();
    caller_plugin::link();
    let host = NativePluginRegistry::host_catalog(
        [
            HostSlot::one("task-boards"),
            HostSlot::one("task-board-reports"),
            HostSlot::one("task-board-callers"),
        ],
        [],
    )
    .expect("linked third-party Plugin descriptors")
    .with_bindings([HostBinding::to_instance(
        PluginInstanceId::new(CALLER, "caller"),
        report::CAPABILITY_ID,
        PluginInstanceId::new(PROVIDER, "missing"),
    )]);

    assert!(
        resolve_plugin_root(&host, &root("board")).is_err(),
        "an explicit Host binding must name a selected provider instance"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn external_provider_consumer_resolve_invoke_cancel_and_close() {
    tokio::task::LocalSet::new()
        .run_until(async {
            provider_plugin::reset_lifecycle_observation();
            report_plugin::reset_lifecycle_observation();
            let resolved = resolve_plugin_root(&linked_host(), &root("board"))
                .expect("external Plugin Root resolves through public Host APIs");
            assert_eq!(resolved.plan().capability_bindings().len(), 3);

            let app = Kernel::start_native(
                resolved.plan().clone(),
                TokioDriver::new(),
                NativePluginRegistry::new().with_linked_factories(),
            )
            .await
            .expect("external Plugin graph starts");

            let response = app
                .invoke::<report::TaskBoardReport>(
                    "example.external-task-board-caller/caller",
                    report::EXECUTE_OPERATION,
                    report::ExecuteRequest {
                        text: "review foundation".to_owned(),
                    },
                )
                .await
                .expect("report invocation infrastructure")
                .expect("report domain success");
            assert_eq!(response.text, "summary: board: review foundation");

            let audit = app
                .invoke::<audit::TaskBoardAudit>(
                    "example.external-task-board-caller/caller",
                    audit::AUDIT_OPERATION,
                    audit::AuditRequest {},
                )
                .await
                .expect("audit invocation infrastructure")
                .expect("audit domain success");
            assert_eq!(audit.status, "summary: ready");

            let rejected = app
                .invoke::<report::TaskBoardReport>(
                    "example.external-task-board-caller/caller",
                    report::EXECUTE_OPERATION,
                    report::ExecuteRequest {
                        text: "reject".to_owned(),
                    },
                )
                .await
                .expect("domain rejection remains a completed invocation");
            assert!(matches!(rejected, Err(report::ExecuteError::Unavailable)));

            let cancellation = CancellationToken::new();
            cancellation.cancel();
            let cancelled = app
                .invoke_with_context::<report::TaskBoardReport>(
                    "example.external-task-board-caller/caller",
                    report::EXECUTE_OPERATION,
                    app.invocation_context_after(Duration::from_secs(1), cancellation),
                    report::ExecuteRequest {
                        text: "cancelled".to_owned(),
                    },
                )
                .await;
            assert!(matches!(cancelled, Err(RuntimeFailure::Cancelled { .. })));

            assert_eq!(
                app.shutdown(Duration::from_secs(1)).await,
                ShutdownOutcome::Clean
            );
            assert!(provider_plugin::was_stopped());
            assert!(report_plugin::was_stopped());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_plugin_configuration_is_rejected_before_ready() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let resolved = resolve_plugin_root(&linked_host(), &root(""))
                .expect("schema permits the string and Plugin validation owns its semantics");
            let result = Kernel::start_native(
                resolved.plan().clone(),
                TokioDriver::new(),
                NativePluginRegistry::new().with_linked_factories(),
            )
            .await;
            assert!(matches!(
                result,
                Err(RuntimeFailure::InvalidResolvedPlan { .. })
            ));
        })
        .await;
}

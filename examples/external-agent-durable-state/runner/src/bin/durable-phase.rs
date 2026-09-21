use std::{env, path::PathBuf, time::Duration};

use external_state_task_caller as caller_plugin;
use external_state_task_provider as provider_plugin;
use lenso_app_plan::authoring::{
    HostBinding, HostCatalog, HostSlot, PluginInstanceId, PluginRootInstance, PluginRootSnapshot,
    resolve_plugin_root,
};
use lenso_capability_agent_durable_task as durable;
use lenso_capability_agent_extension_state as extension;
use lenso_kernel::{Kernel, RuntimeFailure, ShutdownOutcome, StreamEvent};
use lenso_native_adapter::NativePluginRegistry;
use lenso_runner::TokioDriver;

const CALLER: &str = "example.state-task-caller";
const PROVIDER: &str = "example.state-task-provider";
const CALLER_INSTANCE: &str = "example.state-task-caller/default";

fn host() -> HostCatalog {
    caller_plugin::link();
    provider_plugin::link();
    NativePluginRegistry::host_catalog(
        [
            HostSlot::one("state-task-callers"),
            HostSlot::one("state-task-providers"),
        ],
        [],
    )
    .unwrap()
    .with_bindings([
        HostBinding::to_instance(
            PluginInstanceId::new(CALLER, "default"),
            extension::CAPABILITY_ID,
            PluginInstanceId::new(PROVIDER, "file"),
        ),
        HostBinding::to_instance(
            PluginInstanceId::new(CALLER, "default"),
            durable::CAPABILITY_ID,
            PluginInstanceId::new(PROVIDER, "file"),
        ),
    ])
}

fn root(storage_path: &str) -> PluginRootSnapshot {
    PluginRootSnapshot::new(
        [],
        [
            PluginRootInstance::new(CALLER, "default"),
            PluginRootInstance::new(PROVIDER, "file").with_configuration(serde_json::json!({
                "storage_path": storage_path,
            })),
        ],
        [],
    )
}

async fn start_app(path: &str) -> lenso_kernel::NativeApp {
    let resolved = resolve_plugin_root(&host(), &root(path)).expect("external App resolves");
    Kernel::start_native(
        resolved.plan().clone(),
        TokioDriver::new(),
        NativePluginRegistry::new().with_linked_factories(),
    )
    .await
    .expect("external App starts")
}

async fn shutdown(app: lenso_kernel::NativeApp) {
    assert_eq!(
        app.shutdown(Duration::from_secs(1)).await,
        ShutdownOutcome::Clean
    );
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut arguments = env::args().skip(1);
            let phase = arguments.next().expect("phase");
            let store = arguments.next().map(PathBuf::from).expect("store path");
            let storage_path = store.to_str().expect("UTF-8 store path");
            match phase.as_str() {
                "artifact" => println!("{}", provider_plugin::artifact_version()),
                "binary-recovery" => binary_recovery(storage_path).await,
                "start" => start(storage_path).await,
                "recover-wait" => recover_wait(storage_path).await,
                "signal" => signal(storage_path).await,
                "recover-resume" => recover_resume(storage_path).await,
                "wait" => wait(storage_path).await,
                "uncertain" => uncertain(storage_path).await,
                "remove" => remove(storage_path).await,
                "upgrade" => upgrade(storage_path).await,
                "child-policy" => child_policy(storage_path).await,
                other => panic!("unknown durable fixture phase: {other}"),
            }
        })
        .await;
}

async fn child_policy(storage_path: &str) {
    let app = start_app(storage_path).await;
    let start = app
        .handle::<durable::DurableTaskStart>(CALLER_INSTANCE)
        .unwrap();
    for (id, parent, deadline) in [
        ("parent", None, None),
        ("child", Some("parent"), None),
        ("grandchild", Some("child"), None),
        ("expired", None, Some("1")),
    ] {
        start
            .invoke(
                durable::START_OPERATION,
                durable::StartRequest {
                    task_id: id.to_owned(),
                    idempotency_key: format!("start-{id}"),
                    workflow_kind: "example.approval@1".to_owned(),
                    state_version: "1".to_owned(),
                    state_json: "{}".try_into().unwrap(),
                    parent_task_id: parent.map(|value| Some(value.to_owned())),
                    deadline_unix_ms: deadline.map(|value| Some(value.to_owned())),
                },
            )
            .await
            .unwrap()
            .unwrap();
    }
    app.handle::<durable::DurableTaskCancel>(CALLER_INSTANCE)
        .unwrap()
        .invoke(
            durable::CANCEL_OPERATION,
            durable::CancelRequest {
                task_id: "parent".to_owned(),
                reason_code: "user_cancelled".to_owned(),
            },
        )
        .await
        .unwrap()
        .unwrap();
    shutdown(app).await;
    let restarted = start_app(storage_path).await;
    for id in ["parent", "child", "grandchild", "expired"] {
        let observed = restarted
            .handle::<durable::DurableTaskObserve>(CALLER_INSTANCE)
            .unwrap()
            .invoke(
                durable::OBSERVE_OPERATION,
                durable::ObserveRequest {
                    task_id: id.to_owned(),
                },
            )
            .await
            .unwrap()
            .unwrap()
            .task;
        assert_eq!(
            observed.status,
            if id == "expired" {
                durable::TaskStatus::TimedOut
            } else {
                durable::TaskStatus::Cancelled
            }
        );
        let stale = restarted
            .handle::<durable::DurableTaskSignal>(CALLER_INSTANCE)
            .unwrap()
            .invoke(
                durable::SIGNAL_OPERATION,
                durable::SignalRequest {
                    task_id: id.to_owned(),
                    signal_id: format!("late-{id}"),
                    expected_revision: "1".to_owned(),
                    kind: "approval_granted".to_owned(),
                    payload_json: "{}".try_into().unwrap(),
                },
            )
            .await
            .unwrap();
        assert!(stale.is_err(), "stopped task accepted a stale approval");
    }
    shutdown(restarted).await;
}

async fn start(storage_path: &str) {
    let app = start_app(storage_path).await;
    let extensions = app
        .handle::<extension::ExtensionStateAppend>(CALLER_INSTANCE)
        .unwrap();
    let append = extension::AppendRequest {
        namespace: "example.approval-event@1".to_owned(),
        schema_version: "1.0.0".to_owned(),
        event_id: "approval-requested".to_owned(),
        sequence: "1".to_owned(),
        idempotency_key: "event-approval-1".to_owned(),
        association: extension::ExtensionAssociation {
            session_id: Some(Some("session-1".to_owned())),
            run_id: None,
            task_id: Some(Some("approval-1".to_owned())),
        },
        payload_json: r#"{"private_token":"never-rendered","step":"pending"}"#
            .try_into()
            .unwrap(),
        presentation: extension::SafePresentation {
            kind: "example.approval-card@1".to_owned(),
            title: "Approval requested".to_owned(),
            summary: "A durable task is waiting for approval.".to_owned(),
            attributes_json: r#"{"category":"approval"}"#.try_into().unwrap(),
        },
        recovery_required: true,
    };
    let first = extensions
        .invoke(extension::APPEND_OPERATION, append.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(!first.duplicate);
    assert_eq!(first.record.owner_instance, CALLER_INSTANCE);
    assert!(
        extensions
            .invoke(extension::APPEND_OPERATION, append)
            .await
            .unwrap()
            .unwrap()
            .duplicate
    );
    let tasks = app
        .handle::<durable::DurableTaskStart>(CALLER_INSTANCE)
        .unwrap();
    let started = tasks
        .invoke(
            durable::START_OPERATION,
            durable::StartRequest {
                task_id: "approval-1".to_owned(),
                idempotency_key: "start-approval-1".to_owned(),
                workflow_kind: "example.approval-workflow@1".to_owned(),
                state_version: "1".to_owned(),
                state_json: r#"{"step":"request_approval"}"#.try_into().unwrap(),
                parent_task_id: None,
                deadline_unix_ms: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(started.task.status, durable::TaskStatus::WaitingForSignal);
    shutdown(app).await;
}

async fn recover_wait(storage_path: &str) {
    let app = start_app(storage_path).await;
    let extensions = app
        .handle::<extension::ExtensionStateRead>(CALLER_INSTANCE)
        .unwrap();
    let records = extensions
        .invoke(
            extension::READ_OPERATION,
            extension::ReadRequest {
                namespace: "example.approval-event@1".to_owned(),
                association: extension::ExtensionAssociation {
                    session_id: Some(Some("session-1".to_owned())),
                    run_id: None,
                    task_id: Some(Some("approval-1".to_owned())),
                },
                after_sequence: None,
                limit: 8,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(records.records.len(), 1);
    let inspection = extension::generic_inspection(&records.records[0]);
    assert!(
        !serde_json::to_string(&inspection)
            .unwrap()
            .contains("private_token")
    );
    assert_eq!(
        extension::assess_recovery(&records.records[0], false),
        extension::RecoveryDisposition::BlockedByUnknownRequiredRecord
    );
    let tasks = app
        .handle::<durable::DurableTaskRecover>(CALLER_INSTANCE)
        .unwrap();
    let recovered = tasks
        .invoke(
            durable::RECOVER_OPERATION,
            durable::RecoverRequest {
                task_id: "approval-1".to_owned(),
                supported_state_versions: vec!["1".to_owned()],
                recoverer_artifact: "example.approval-workflow@1.0.0".to_owned(),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.action, durable::RecoveryAction::Wait);
    shutdown(app).await;
}

async fn signal(storage_path: &str) {
    let app = start_app(storage_path).await;
    let tasks = app
        .handle::<durable::DurableTaskSignal>(CALLER_INSTANCE)
        .unwrap();
    let signal = durable::SignalRequest {
        task_id: "approval-1".to_owned(),
        signal_id: "approval-webhook-1".to_owned(),
        expected_revision: "1".to_owned(),
        kind: "approval_granted".to_owned(),
        payload_json: r#"{"approved":true}"#.try_into().unwrap(),
    };
    let accepted = tasks
        .invoke(durable::SIGNAL_OPERATION, signal.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(accepted.task.status, durable::TaskStatus::Ready);
    if cfg!(feature = "upgraded") {
        let state: serde_json::Value =
            serde_json::from_str(accepted.task.state_json.as_str()).unwrap();
        assert_eq!(state["handled_by"], "2.0.0");
    }
    assert!(
        tasks
            .invoke(durable::SIGNAL_OPERATION, signal)
            .await
            .unwrap()
            .unwrap()
            .duplicate
    );
    let stale = tasks
        .invoke(
            durable::SIGNAL_OPERATION,
            durable::SignalRequest {
                task_id: "approval-1".to_owned(),
                signal_id: "approval-webhook-stale".to_owned(),
                expected_revision: "1".to_owned(),
                kind: "approval_granted".to_owned(),
                payload_json: r#"{"approved":true}"#.try_into().unwrap(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(stale, Err(durable::SignalError::StaleSignal)));
    shutdown(app).await;
}

async fn recover_resume(storage_path: &str) {
    let app = start_app(storage_path).await;
    let tasks = app
        .handle::<durable::DurableTaskRecover>(CALLER_INSTANCE)
        .unwrap();
    let recovered = tasks
        .invoke(
            durable::RECOVER_OPERATION,
            durable::RecoverRequest {
                task_id: "approval-1".to_owned(),
                supported_state_versions: vec!["1".to_owned()],
                recoverer_artifact: "example.approval-workflow@1.0.0".to_owned(),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.action, durable::RecoveryAction::Resume);
    shutdown(app).await;
}

async fn wait(storage_path: &str) {
    let app = start_app(storage_path).await;
    let handle = app
        .stream_handle::<durable::DurableTaskWait>(CALLER_INSTANCE)
        .unwrap();
    let stream = handle
        .open(
            durable::WAIT_OPERATION,
            durable::WaitOpen {
                task_id: "approval-1".to_owned(),
                after_revision: "1".to_owned(),
            },
        )
        .await
        .unwrap()
        .unwrap();
    stream.close_send().await.unwrap();
    let mut update = None;
    loop {
        match stream.receive().await.unwrap() {
            StreamEvent::Message(value) => update = Some(value),
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => break,
            StreamEvent::Terminal(Err(error)) => panic!("wait failed: {error:?}"),
        }
    }
    assert_eq!(update.unwrap().task.revision, "2");
    shutdown(app).await;
}

// A structural removal stops the old generation, cancels live transport work,
// and selects a new immutable composition. Durable facts remain Plugin-owned.
async fn remove(storage_path: &str) {
    let before = std::fs::read(storage_path).unwrap();
    let app = start_app(storage_path).await;
    let observe = app
        .handle::<durable::DurableTaskObserve>(CALLER_INSTANCE)
        .unwrap();
    let handle = app
        .stream_handle::<durable::DurableTaskWait>(CALLER_INSTANCE)
        .unwrap();
    let stream = handle
        .open(
            durable::WAIT_OPERATION,
            durable::WaitOpen {
                task_id: "approval-1".to_owned(),
                after_revision: "2".to_owned(),
            },
        )
        .await
        .unwrap()
        .unwrap();
    // Deliberately leave the input open: the Provider owns an active managed
    // task blocked on input when shutdown starts.
    app.request_shutdown();
    assert!(matches!(
        observe
            .invoke(
                durable::OBSERVE_OPERATION,
                durable::ObserveRequest {
                    task_id: "approval-1".to_owned(),
                }
            )
            .await,
        Err(RuntimeFailure::AdmissionClosed)
    ));
    // Shutdown must settle the stream without waiting for another client frame.
    loop {
        match stream.receive().await {
            Err(RuntimeFailure::Unavailable { capability }) => {
                assert_eq!(capability, durable::CAPABILITY_ID);
                break;
            }
            Ok(StreamEvent::PeerHalfClosed) => {}
            other => panic!("removed generation did not cancel active work: {other:?}"),
        }
    }
    drop(stream);
    assert_eq!(
        app.shutdown(Duration::from_secs(1)).await,
        ShutdownOutcome::Clean
    );
    assert_eq!(std::fs::read(storage_path).unwrap(), before);

    let removed = resolve_plugin_root(
        &HostCatalog::new([], [], []),
        &PluginRootSnapshot::new([], [], []),
    )
    .unwrap();
    let next = Kernel::start_native(
        removed.plan().clone(),
        TokioDriver::new(),
        NativePluginRegistry::new().with_linked_factories(),
    )
    .await
    .unwrap();
    assert!(
        next.handle::<durable::DurableTaskObserve>(CALLER_INSTANCE)
            .is_err()
    );
    assert!(matches!(
        observe
            .invoke(
                durable::OBSERVE_OPERATION,
                durable::ObserveRequest {
                    task_id: "approval-1".to_owned(),
                }
            )
            .await,
        Err(RuntimeFailure::AdmissionClosed)
    ));
    shutdown(next).await;
    assert_eq!(std::fs::read(storage_path).unwrap(), before);
}

async fn upgrade(storage_path: &str) {
    let app = start_app(storage_path).await;
    let observe = app
        .handle::<durable::DurableTaskObserve>(CALLER_INSTANCE)
        .unwrap();
    let original = observe
        .invoke(
            durable::OBSERVE_OPERATION,
            durable::ObserveRequest {
                task_id: "approval-1".to_owned(),
            },
        )
        .await
        .unwrap()
        .unwrap()
        .task;
    let recover = app
        .handle::<durable::DurableTaskRecover>(CALLER_INSTANCE)
        .unwrap();
    let request = durable::RecoverRequest {
        task_id: "approval-1".to_owned(),
        supported_state_versions: if cfg!(feature = "incompatible") {
            // Even a caller claiming v1 support cannot override the new binary.
            vec!["1".to_owned(), "2".to_owned()]
        } else {
            vec!["2".to_owned()]
        },
        recoverer_artifact: "example.approval-workflow@2.0.0".to_owned(),
    };
    let blocked = recover
        .invoke(durable::RECOVER_OPERATION, request.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(blocked.action, durable::RecoveryAction::BlockedUpgrade);
    assert_eq!(blocked.task.status, durable::TaskStatus::UpgradeRequired);
    assert_eq!(blocked.task.state_json, original.state_json);
    assert_eq!(blocked.task.state_version, "1");
    assert_eq!(
        blocked.task.revision.parse::<u64>().unwrap(),
        original.revision.parse::<u64>().unwrap() + 1
    );
    let repeated = recover
        .invoke(durable::RECOVER_OPERATION, request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(repeated.task, blocked.task);
    shutdown(app).await;

    let restarted = start_app(storage_path).await;
    let inspected = restarted
        .handle::<durable::DurableTaskObserve>(CALLER_INSTANCE)
        .unwrap()
        .invoke(
            durable::OBSERVE_OPERATION,
            durable::ObserveRequest {
                task_id: "approval-1".to_owned(),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(inspected.task, blocked.task);
    let stale = restarted
        .handle::<durable::DurableTaskSignal>(CALLER_INSTANCE)
        .unwrap()
        .invoke(
            durable::SIGNAL_OPERATION,
            durable::SignalRequest {
                task_id: "approval-1".to_owned(),
                signal_id: "post-upgrade-approval".to_owned(),
                expected_revision: original.revision,
                kind: "approval_granted".to_owned(),
                payload_json: r#"{"approved":true}"#.try_into().unwrap(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(stale, Err(durable::SignalError::StaleSignal)));
    shutdown(restarted).await;
}

async fn uncertain(storage_path: &str) {
    let app = start_app(storage_path).await;
    let tasks = app
        .handle::<durable::DurableTaskStart>(CALLER_INSTANCE)
        .unwrap();
    tasks
        .invoke(
            durable::START_OPERATION,
            durable::StartRequest {
                task_id: "uncertain-1".to_owned(),
                idempotency_key: "start-uncertain-1".to_owned(),
                workflow_kind: "example.approval-uncertain@1".to_owned(),
                state_version: "1".to_owned(),
                state_json: r#"{"effect":"submitted_without_receipt"}"#.try_into().unwrap(),
                parent_task_id: None,
                deadline_unix_ms: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    let recovered = app
        .handle::<durable::DurableTaskRecover>(CALLER_INSTANCE)
        .unwrap()
        .invoke(
            durable::RECOVER_OPERATION,
            durable::RecoverRequest {
                task_id: "uncertain-1".to_owned(),
                supported_state_versions: vec!["1".to_owned()],
                recoverer_artifact: "example.approval-workflow@1.0.0".to_owned(),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.action,
        durable::RecoveryAction::BlockedUncertainExternalEffect
    );
    shutdown(app).await;
}

async fn binary_recovery(storage_path: &str) {
    assert_eq!(provider_plugin::artifact_version(), "2.0.0");
    let bytes_before = std::fs::read(storage_path).unwrap();
    recover_wait(storage_path).await;
    let app = start_app(storage_path).await;
    let recovered = app
        .handle::<durable::DurableTaskRecover>(CALLER_INSTANCE)
        .unwrap()
        .invoke(
            durable::RECOVER_OPERATION,
            durable::RecoverRequest {
                task_id: "uncertain-1".to_owned(),
                supported_state_versions: vec!["1".to_owned()],
                recoverer_artifact: "example.approval-workflow@2.0.0".to_owned(),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.action,
        durable::RecoveryAction::BlockedUncertainExternalEffect
    );
    assert_eq!(recovered.task.revision, "1");
    shutdown(app).await;
    assert_eq!(std::fs::read(storage_path).unwrap(), bytes_before);
}

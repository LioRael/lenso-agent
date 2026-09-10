use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    PluginInstancePlan,
};
use lenso_capability_agent_auth_connection as auth;
use lenso_capability_agent_tool_provider as tools;
use lenso_capability_agent_turn_binding as binding;
use lenso_kernel::{CancellationToken, Kernel, RuntimeFailure, ShutdownOutcome};
use lenso_native_adapter::{
    NativePluginFactory, NativePluginFactoryContext, NativePluginInstance, NativePluginRegistry,
};
use lenso_runner::TokioDriver;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Debug)]
struct Caller;
impl NativePluginFactory for Caller {
    fn package_id(&self) -> &'static str {
        "test.caller"
    }
    fn instantiate(
        &self,
        _: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(NativePluginInstance::default())
    }
}
#[derive(Clone)]
struct Remote {
    origin: String,
    account: Arc<AtomicUsize>,
    revoked: Arc<AtomicBool>,
    approved: Arc<AtomicBool>,
}
async fn begin(State(s): State<Remote>) -> Json<Value> {
    s.account.fetch_add(1, Ordering::SeqCst);
    Json(
        json!({"attempt_id":"remote-attempt","polling_secret":"private-poll","authorization_url":format!("{}/auth/agent/authorize?attempt=remote-attempt",s.origin),"expires_at_millis":((time::OffsetDateTime::now_utc().unix_timestamp()+300)*1000).to_string()}),
    )
}
async fn poll(State(s): State<Remote>, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(body["polling_secret"], "private-poll");
    if !s.approved.load(Ordering::SeqCst) {
        return Json(json!({"state":"pending","grant":null}));
    }
    Json(
        json!({"state":"connected","grant":{"subject":format!("user-{}",s.account.load(Ordering::SeqCst)),"credential":format!("private-account-{}",s.account.load(Ordering::SeqCst)),"expires_at":(time::OffsetDateTime::now_utc()+time::Duration::minutes(10)).format(&time::format_description::well_known::Rfc3339).unwrap()}}),
    )
}
async fn execute(
    State(s): State<Remote>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    assert_eq!(
        body,
        json!({"name":"projects_read_issue","arguments_json":"{}"})
    );
    if s.revoked.load(Ordering::SeqCst) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let identity = match headers.get("authorization").and_then(|v| v.to_str().ok()) {
        Some("Bearer private-account-1") => "first",
        Some("Bearer private-account-2") => "second",
        _ => return Err(StatusCode::FORBIDDEN),
    };
    Ok(Json(
        json!({"content_type":"text","content":identity,"content_blocks":null,"metadata_json":"{}"}),
    ))
}
#[allow(
    clippy::too_many_lines,
    reason = "one native lifecycle scenario verifies account switches, revocation and shutdown together"
)]
#[tokio::test(flavor = "current_thread")]
async fn native_connection_keeps_grants_private_and_turns_pinned() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let _ = lenso_agent_business_connection_plugin::BusinessConnectionConfig {
                origin: "https://example.com".into(),
                label: "test".into(),
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let remote = Remote {
                origin: format!("http://{}", listener.local_addr().unwrap()),
                account: Arc::new(AtomicUsize::new(0)),
                revoked: Arc::new(AtomicBool::new(false)),
                approved: Arc::new(AtomicBool::new(false)),
            };
            let server = tokio::spawn(
                axum::serve(
                    listener,
                    Router::new()
                        .route("/auth/agent/connection/begin", post(begin))
                        .route("/auth/agent/connection/poll", post(poll))
                        .route("/projects/agent/tools/execute", post(execute))
                        .route(
                            "/projects/agent/manifest",
                            get(|headers: HeaderMap| async move {
                                assert!(!headers.contains_key("authorization"));
                                Json(json!({"tools":[]}))
                            }),
                        )
                        .with_state(remote.clone()),
                )
                .into_future(),
            );
            let mut plugin = PluginInstancePlan::new("business", "lenso.agent.business-connection")
                .with_configuration(json!({"origin":remote.origin,"label":"Projects"}).to_string());
            let mut caller = PluginInstancePlan::new("caller", "test.caller");
            let mut edges = Vec::new();
            for (id, version, operations) in [
                (
                    auth::CAPABILITY_ID,
                    auth::DESCRIPTOR_VERSION,
                    vec![
                        auth::BEGIN_OPERATION,
                        auth::CANCEL_OPERATION,
                        auth::DISCONNECT_OPERATION,
                        auth::POLL_OPERATION,
                        auth::STATUS_OPERATION,
                    ],
                ),
                (
                    binding::CAPABILITY_ID,
                    binding::DESCRIPTOR_VERSION,
                    vec![binding::CAPTURE_OPERATION],
                ),
                (
                    tools::CAPABILITY_ID,
                    tools::DESCRIPTOR_VERSION,
                    vec![tools::CATALOG_OPERATION, tools::EXECUTE_OPERATION],
                ),
            ] {
                plugin =
                    plugin.with_capability(CapabilityEndpointPlan::new(id, version, operations));
                caller = caller.with_requirement(CapabilityRequirementPlan::one(id, version));
                edges.push(CapabilityBinding::new("caller", id, version, "business"));
            }
            let app = Kernel::start_native(
                AppComposition::new(vec![plugin, caller], edges)
                    .resolve()
                    .unwrap(),
                TokioDriver::new(),
                NativePluginRegistry::new()
                    .with_linked_factories()
                    .with_factory(Caller),
            )
            .await
            .unwrap();
            let catalog = app
                .invoke::<tools::ToolProviderCatalog>(
                    "caller",
                    tools::CATALOG_OPERATION,
                    tools::CatalogRequest {},
                )
                .await
                .unwrap()
                .unwrap();
            assert!(catalog.tools.is_empty());
            let mut turns = Vec::new();
            for _ in 0..2 {
                remote.approved.store(false, Ordering::SeqCst);
                // Completed login can be replaced, without mutating admitted scopes.
                app.invoke::<auth::AuthConnectionDisconnect>(
                    "caller",
                    auth::DISCONNECT_OPERATION,
                    auth::DisconnectRequest {},
                )
                .await
                .unwrap()
                .unwrap();
                let attempt = app
                    .invoke::<auth::AuthConnectionBegin>(
                        "caller",
                        auth::BEGIN_OPERATION,
                        auth::BeginRequest {
                            method: auth::LoginMethod::BrowserConsent,
                        },
                    )
                    .await
                    .unwrap()
                    .unwrap();
                assert!(!serde_json::to_string(&attempt).unwrap().contains("private"));
                let resumed = app
                    .invoke::<auth::AuthConnectionBegin>(
                        "caller",
                        auth::BEGIN_OPERATION,
                        auth::BeginRequest {
                            method: auth::LoginMethod::BrowserConsent,
                        },
                    )
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(resumed.attempt_id, attempt.attempt_id);
                assert_eq!(resumed.authorization_url, attempt.authorization_url);
                remote.approved.store(true, Ordering::SeqCst);
                // Simulate navigating away: no UI poll request drives completion.
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let status = app
                            .invoke::<auth::AuthConnectionStatus>(
                                "caller",
                                auth::STATUS_OPERATION,
                                auth::StatusRequest {},
                            )
                            .await
                            .unwrap()
                            .unwrap();
                        if status.connected {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                let result = app
                    .invoke::<auth::AuthConnectionPoll>(
                        "caller",
                        auth::POLL_OPERATION,
                        auth::AttemptRequest {
                            attempt_id: attempt.attempt_id,
                        },
                    )
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    serde_json::to_string(&result).unwrap(),
                    "{\"state\":\"connected\"}"
                );
                let id = uuid::Uuid::new_v4().to_string();
                let token = CancellationToken::new();
                app.handle::<binding::TurnBinding>("caller")
                    .unwrap()
                    .invoke_with_context(
                        binding::CAPTURE_OPERATION,
                        app.invocation_context_after(Duration::from_secs(30), token.clone()),
                        binding::CaptureRequest {
                            scope_id: id.clone(),
                        },
                    )
                    .await
                    .unwrap()
                    .unwrap();
                turns.push((id, token));
            }
            for ((id, _), identity) in turns.iter().zip(["first", "second"]) {
                let context = app
                    .invocation_context_after(Duration::from_secs(30), CancellationToken::new())
                    .with_extension(binding::SCOPE_EXTENSION, id.as_bytes().to_vec())
                    .unwrap();
                let output = app
                    .handle::<tools::ToolProviderExecute>("caller")
                    .unwrap()
                    .invoke_with_context(
                        tools::EXECUTE_OPERATION,
                        context,
                        tools::ExecuteRequest {
                            name: "projects_read_issue".into(),
                            arguments_json: "{}".parse().unwrap(),
                        },
                    )
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(output.content, identity);
                assert!(!serde_json::to_string(&output).unwrap().contains("private"));
            }
            remote.revoked.store(true, Ordering::SeqCst);
            let invoke = |id: &str| {
                app.invocation_context_after(Duration::from_secs(30), CancellationToken::new())
                    .with_extension(binding::SCOPE_EXTENSION, id.as_bytes().to_vec())
                    .unwrap()
            };
            let output = app
                .handle::<tools::ToolProviderExecute>("caller")
                .unwrap()
                .invoke_with_context(
                    tools::EXECUTE_OPERATION,
                    invoke(&turns[0].0),
                    tools::ExecuteRequest {
                        name: "projects_read_issue".into(),
                        arguments_json: "{}".parse().unwrap(),
                    },
                )
                .await
                .unwrap();
            assert!(matches!(output, Err(tools::ExecuteError::ExecutionFailed { payload }) if payload.reason_code == "connection_required"));
            // An old Turn's 401 cannot disconnect the newer account.
            let status = app.invoke::<auth::AuthConnectionStatus>("caller", auth::STATUS_OPERATION, auth::StatusRequest {}).await.unwrap().unwrap();
            assert!(status.connected);
            let account = status.account.unwrap().unwrap();
            assert_eq!(account.subject.as_ref().unwrap().as_deref(), Some("user-2"));
            assert!(account.expires_at_millis.as_ref().unwrap().is_some());
            assert!(!account.reconnect_required);
            assert!(!serde_json::to_string(&account).unwrap().contains("private"));
            let _ = app.handle::<tools::ToolProviderExecute>("caller").unwrap().invoke_with_context(tools::EXECUTE_OPERATION, invoke(&turns[1].0), tools::ExecuteRequest { name: "projects_read_issue".into(), arguments_json: "{}".parse().unwrap() }).await.unwrap();
            let status = app.invoke::<auth::AuthConnectionStatus>("caller", auth::STATUS_OPERATION, auth::StatusRequest {}).await.unwrap().unwrap();
            assert!(!status.connected);
            assert!(status.account.unwrap().unwrap().reconnect_required);
            app.invoke::<auth::AuthConnectionDisconnect>(
                "caller",
                auth::DISCONNECT_OPERATION,
                auth::DisconnectRequest {},
            )
            .await
            .unwrap()
            .unwrap();
            remote.approved.store(false, Ordering::SeqCst);
            let cancelled = app
                .invoke::<auth::AuthConnectionBegin>(
                    "caller",
                    auth::BEGIN_OPERATION,
                    auth::BeginRequest {
                        method: auth::LoginMethod::BrowserConsent,
                    },
                )
                .await
                .unwrap()
                .unwrap();
            let cancelled = app
                .invoke::<auth::AuthConnectionCancel>(
                    "caller",
                    auth::CANCEL_OPERATION,
                    auth::AttemptRequest {
                        attempt_id: cancelled.attempt_id,
                    },
                )
                .await
                .unwrap()
                .unwrap();
            assert!(cancelled.cancelled);
            remote.approved.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(2100)).await;
            assert!(
                !app.invoke::<auth::AuthConnectionStatus>(
                    "caller",
                    auth::STATUS_OPERATION,
                    auth::StatusRequest {},
                )
                .await
                .unwrap()
                .unwrap()
                .connected
            );
            remote.approved.store(false, Ordering::SeqCst);
            app.invoke::<auth::AuthConnectionBegin>(
                "caller",
                auth::BEGIN_OPERATION,
                auth::BeginRequest {
                    method: auth::LoginMethod::BrowserConsent,
                },
            )
            .await
            .unwrap()
            .unwrap();
            // Shutdown owns and terminates the pending polling task.

            turns[0].1.cancel();
            tokio::task::yield_now().await;
            assert!(
                app.handle::<tools::ToolProviderExecute>("caller")
                    .unwrap()
                    .invoke_with_context(
                        tools::EXECUTE_OPERATION,
                        invoke(&turns[0].0),
                        tools::ExecuteRequest {
                            name: "projects_read_issue".into(),
                            arguments_json: "{}".parse().unwrap()
                        }
                    )
                    .await
                    .is_err()
            );
            turns[1].1.cancel();
            assert_eq!(
                app.shutdown(Duration::from_secs(1)).await,
                ShutdownOutcome::Clean
            );
            server.abort();
        })
        .await;
}

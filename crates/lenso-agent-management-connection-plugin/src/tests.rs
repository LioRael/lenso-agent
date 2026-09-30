use super::*;
use axum::{
    Router,
    extract::{Path as RoutePath, State},
    http::HeaderMap as HttpHeaders,
    response::IntoResponse,
    routing::{get, post},
};
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    PluginInstancePlan,
};
use lenso_kernel::{CancellationToken, InvocationContext, Kernel};
use lenso_native_adapter::{
    NativePluginFactory, NativePluginFactoryContext, NativePluginInstance, NativePluginRegistry,
};
use lenso_runner::TokioDriver;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

const CHILD: &str = "new_auth_issued_child_only";

#[derive(Clone, Default)]
struct RemoteState {
    revoked: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    echo_secret: Arc<AtomicBool>,
}

fn authorized(state: &RemoteState, headers: &HttpHeaders) -> bool {
    !state.revoked.load(Ordering::SeqCst)
        && [
            ("authorization", format!("Bearer {CHILD}")),
            ("x-lenso-task-id", "task-1".into()),
            ("x-lenso-agent-session-id", "session-1".into()),
            (
                "x-lenso-delegate-caller",
                "lenso.agent.management-connection/default".into(),
            ),
        ]
        .into_iter()
        .all(|(key, value)| {
            headers
                .get(key)
                .is_some_and(|header| header == value.as_str())
        })
}

async fn catalog(State(state): State<RemoteState>, headers: HttpHeaders) -> impl IntoResponse {
    state.calls.fetch_add(1, Ordering::SeqCst);
    if !authorized(&state, &headers) {
        return (axum::http::StatusCode::FORBIDDEN, String::new());
    }
    (
        axum::http::StatusCode::OK,
        management::encode_catalog_response(&management::CatalogResponse {
            deployment: "alpha".into(),
            revision: "1".into(),
            entries: vec![],
        })
        .unwrap(),
    )
}

async fn invoke(State(state): State<RemoteState>, headers: HttpHeaders) -> impl IntoResponse {
    state.calls.fetch_add(1, Ordering::SeqCst);
    if !authorized(&state, &headers) {
        return (axum::http::StatusCode::FORBIDDEN, String::new());
    }
    if state.echo_secret.load(Ordering::SeqCst) {
        return (axum::http::StatusCode::OK, CHILD.into());
    }
    (
        axum::http::StatusCode::OK,
        management::encode_invoke_response(&management::InvokeResponse {
            operation_id: Some("operation-1".into()),
            state: management::InvocationState::PendingApproval,
            audit_pending: false,
            result_json: None,
            receipt: None,
        })
        .unwrap(),
    )
}

async fn status(
    State(state): State<RemoteState>,
    headers: HttpHeaders,
    RoutePath(id): RoutePath<String>,
) -> impl IntoResponse {
    state.calls.fetch_add(1, Ordering::SeqCst);
    if !authorized(&state, &headers) {
        return (axum::http::StatusCode::FORBIDDEN, String::new());
    }
    if id != "operation-1" {
        return (axum::http::StatusCode::NOT_FOUND, String::new());
    }
    (
        axum::http::StatusCode::OK,
        management::encode_invoke_response(&management::InvokeResponse {
            operation_id: Some(id),
            state: management::InvocationState::Unknown,
            audit_pending: false,
            result_json: None,
            receipt: None,
        })
        .unwrap(),
    )
}

#[derive(Debug)]
struct Caller;
impl NativePluginFactory for Caller {
    fn package_id(&self) -> &'static str {
        "test.management.connection-caller"
    }
    fn instantiate(
        &self,
        _: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, lenso::RuntimeFailure> {
        Ok(NativePluginInstance::default())
    }
}

fn plan(origin: &str, file: &std::path::Path, prefix: &str) -> lenso_app_plan::ResolvedAppPlan {
    AppComposition::new(vec![
        PluginInstancePlan::new("caller", "test.management.connection-caller")
            .with_requirement(CapabilityRequirementPlan::one(management::CAPABILITY_ID, management::DESCRIPTOR_VERSION)),
        PluginInstancePlan::new("connection", PACKAGE_ID)
            .with_configuration(serde_json::json!({
                "origin":origin, "deployment":"alpha", "credential_file":file,
                "delegated_route_prefix":prefix,
                "task_id":"task-1", "agent_session_id":"session-1",
                "delegate_caller":"lenso.agent.management-connection/default", "request_timeout_millis":1000,
            }).to_string())
            .with_capability(CapabilityEndpointPlan::new(management::CAPABILITY_ID, management::DESCRIPTOR_VERSION, ["catalog","invoke","status"])),
    ], vec![CapabilityBinding::new("caller", management::CAPABILITY_ID, management::DESCRIPTOR_VERSION, "connection")])
        .resolve().unwrap()
}

fn context(session: &str) -> InvocationContext {
    AgentTaskBinding {
        task_id: "task-1".into(),
        agent_session_id: session.into(),
        delegate_caller: "lenso.agent.management-connection/default".into(),
    }
    .attach(InvocationContext::new(71, None, CancellationToken::new()))
    .unwrap()
}

fn command() -> management::InvokeRequest {
    management::InvokeRequest {
        entry_id: "state.update".into(),
        version: "1.0.0".into(),
        input_json: "{}".parse().unwrap(),
        idempotency_key: Some("request-1".into()),
        expected_revision: Some("0".into()),
    }
}

fn child_file(root: &std::path::Path) -> std::path::PathBuf {
    let file = root.join("delegation");
    fs::write(&file, CHILD).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    }
    file
}

#[tokio::test(flavor = "current_thread")]
async fn a_remote_task_is_private_bound_fresh_and_never_replays_uncertain_invocations() {
    selected_remote_task("").await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_explicit_agent_prefix_preserves_the_same_private_bound_protocol() {
    selected_remote_task("/agent").await;
}

async fn selected_remote_task(prefix: &str) {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = RemoteState::default();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let router = Router::new()
                .route("/management/catalog", get(catalog))
                .route("/management/invoke", post(invoke))
                .route("/management/operations/{operation_id}", get(status))
                .with_state(state.clone());
            let router = if prefix.is_empty() {
                router
            } else {
                Router::new().nest(prefix, router)
            };
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let root = tempfile::tempdir().unwrap();
            let file = child_file(root.path());
            let app = Kernel::start_native(
                plan(&origin, &file, prefix),
                TokioDriver::new(),
                NativePluginRegistry::new()
                    .with_factory(Caller)
                    .with_linked_factories(),
            )
            .await
            .unwrap();
            let client = management::ManagementClient::from_dependencies(
                &app.dependencies("caller").unwrap(),
            )
            .unwrap();
            let no_binding = InvocationContext::new(72, None, CancellationToken::new());
            assert!(
                client
                    .catalog_with_context(no_binding, management::CatalogRequest {})
                    .await
                    .is_err()
            );
            assert!(
                client
                    .catalog_with_context(context("another-session"), management::CatalogRequest {})
                    .await
                    .is_err()
            );
            assert_eq!(state.calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                client
                    .catalog_with_context(context("session-1"), management::CatalogRequest {})
                    .await
                    .unwrap()
                    .deployment,
                "alpha"
            );
            let pending = client
                .invoke_with_context(context("session-1"), command())
                .await
                .unwrap();
            assert_eq!(pending.state, management::InvocationState::PendingApproval);
            let queried = client
                .status_with_context(
                    context("session-1"),
                    management::StatusRequest {
                        operation_id: pending.operation_id.unwrap(),
                    },
                )
                .await
                .unwrap();
            assert_eq!(queried.state, management::InvocationState::Unknown);
            assert_eq!(queried.operation_id.as_deref(), Some("operation-1"));
            state.echo_secret.store(true, Ordering::SeqCst);
            let unknown = client
                .invoke_with_context(context("session-1"), command())
                .await
                .unwrap();
            assert_eq!(unknown.state, management::InvocationState::Unknown);
            assert!(
                !management::encode_invoke_response(&unknown)
                    .unwrap()
                    .contains(CHILD)
            );
            assert_eq!(state.calls.load(Ordering::SeqCst), 4);
            state.revoked.store(true, Ordering::SeqCst);
            assert!(
                client
                    .catalog_with_context(context("session-1"), management::CatalogRequest {})
                    .await
                    .is_err()
            );
            assert_eq!(
                app.shutdown(Duration::from_secs(1)).await,
                lenso_kernel::ShutdownOutcome::Clean
            );
            server.abort();
        })
        .await;
}

#[test]
fn host_configuration_cannot_supply_an_arbitrary_transport_or_expand_a_task() {
    let mut config = ConnectionConfig {
        origin: "https://operators.example".into(),
        delegated_route_prefix: String::new(),
        deployment: "alpha".into(),
        credential_file: "/private/delegation".into(),
        task_id: "task-1".into(),
        agent_session_id: "session-1".into(),
        delegate_caller: "lenso.agent.management-connection/default".into(),
        request_timeout_millis: 1000,
    };
    assert!(validate(&config).is_ok());
    for origin in [
        "https://user:password@operators.example",
        "https://operators.example/other",
        "http://192.0.2.1",
        "https://operators.example?target=other",
    ] {
        config.origin = origin.into();
        assert!(validate(&config).is_err());
    }
    config.origin = "https://operators.example".into();
    config.delegated_route_prefix = "/agent".into();
    assert!(validate(&config).is_ok());
    for prefix in ["/other", "/agent/../other", "https://other.example"] {
        config.delegated_route_prefix = prefix.into();
        assert!(validate(&config).is_err());
    }
}

#[test]
fn status_route_cannot_change_the_host_origin_or_management_path() {
    let origin = "https://operators.example";
    assert_eq!(
        Route::Status("operation-1")
            .endpoint(origin, "")
            .unwrap()
            .as_str(),
        "https://operators.example/management/operations/operation-1"
    );
    for id in [
        "",
        "..",
        "../catalog",
        "operation/other",
        "https://other.example",
        "operation?target=other",
    ] {
        assert!(Route::Status(id).endpoint(origin, "").is_err());
        assert!(Route::Status(id).endpoint(origin, "/agent").is_err());
    }
    assert_eq!(
        Route::Catalog.endpoint(origin, "/agent").unwrap().as_str(),
        "https://operators.example/agent/management/catalog"
    );
    assert_eq!(
        Route::Invoke.endpoint(origin, "/agent").unwrap().as_str(),
        "https://operators.example/agent/management/invoke"
    );
    assert_eq!(
        Route::Status("operation-1")
            .endpoint(origin, "/agent")
            .unwrap()
            .as_str(),
        "https://operators.example/agent/management/operations/operation-1"
    );
    assert!(Route::Catalog.endpoint(origin, "/other").is_err());
}

use super::*;
use tower::ServiceExt as _;

async fn request(
    surface: &AgentWebSurface,
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = surface
        .router()
        .oneshot(
            axum::http::Request::builder()
                .method(method)
                .uri(format!("/api/console/v1/agent/{path}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn config(root: &FsPath) -> AgentWebConfig {
    let mut config = AgentWebConfig::new(lenso_agent_default_plugins::link);
    config.agent_home = Some(root.to_path_buf());
    config.control = AgentWebControl::HostAuthorized;
    config.plugin_control = true;
    config.plugin_configuration_store = Some(PluginConfigurationStoreConfig::new(
        root.join("config.sqlite3"),
        "test/app",
    ));
    config
}

#[tokio::test(flavor = "current_thread")]
async fn sqlite_profiles_import_and_switch_online_then_restart() {
    let root = tempfile::tempdir().unwrap();
    configure_test_fixture_model(root.path());
    Box::pin(tokio::task::LocalSet::new().run_until(async {
        let surface = AgentWebSurface::start(config(root.path())).await.unwrap();
        let (status, _) = request(&surface, "POST", "control/profile", serde_json::json!({"profile":"plan"})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (_, inventory) = request(&surface, "GET", "plugins", serde_json::Value::Null).await;
        let (_, management) = request(&surface, "GET", "control/plugins", serde_json::Value::Null).await;
        let import = serde_json::json!({"expectedRevision": management["revision"], "expectedStreamId": inventory["streamId"]});
        let mut stale = import.clone(); stale["expectedStreamId"] = serde_json::json!("stale");
        assert_eq!(request(&surface, "POST", "control/profiles/import", stale).await.0, StatusCode::CONFLICT);
        let (status, imported) = request(&surface, "POST", "control/profiles/import", import.clone()).await;
        assert_eq!(status, StatusCode::OK, "{imported}");
        assert_eq!(request(&surface, "POST", "control/profiles/import", import).await.0, StatusCode::CONFLICT);
        let (status, selected) = request(&surface, "POST", "control/profile", serde_json::json!({"profile":"plan"})).await;
        assert_eq!(status, StatusCode::OK, "{selected}");
        assert_eq!(selected["profile"], "plan");
        let repeated = serde_json::json!({"expectedRevision": imported["revision"], "expectedStreamId": inventory["streamId"]});
        let (imported_again, selected_again) = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            tokio::join!(
                request(&surface, "POST", "control/profiles/import", repeated),
                request(&surface, "POST", "control/profile", serde_json::json!({"profile":"plan"}))
            )
        }).await.expect("concurrent import and selection must not block the runtime");
        assert_eq!(imported_again.0, StatusCode::OK, "{}", imported_again.1);
        if selected_again.0 == StatusCode::CONFLICT {
            assert!(selected_again.1.to_string().contains("Generation authority is busy"), "{}", selected_again.1);
            let (status, selected) = request(&surface, "POST", "control/profile", serde_json::json!({"profile":"plan"})).await;
            assert_eq!(status, StatusCode::OK, "{selected}");
        } else {
            assert_eq!(selected_again.0, StatusCode::OK, "{}", selected_again.1);
        }

        let (status, _) = request(&surface, "POST", "control/profile", serde_json::json!({"profile":"unknown"})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let authority = surface.runtime.sqlite_profiles.as_ref().unwrap();
        let revision = authority.inspect().unwrap().revision().clone();
        let edited = "working_directory = \".\"\nmax_file_bytes = 131072\n";
        let proposal = authority.propose(&revision, "lenso.agent.workspace-instructions", "default", edited.as_bytes()).unwrap();
        let (status, published) = request(&surface, "PUT", "control/plugins/lenso.agent.workspace-instructions/default/configuration",
            serde_json::json!({"expectedRevision":revision.as_str(), "expectedSourceDigest":proposal.base_source_digest().as_str(), "expectedStreamId": inventory["streamId"], "proposalDigest": proposal.digest(), "toml":edited})).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{published}");
        let expected = lenso_agent_host::snapshot_desired_plugin_root_for_home(root.path(), root.path(), Some("plan")).unwrap();
        assert_eq!(published["desired"]["planDigest"], expected.plan_digest());
        let revision = authority.inspect().unwrap().revision().clone();
        let invalid = authority.propose(&revision, "lenso.agent.process.native", "default",
            b"root = \".\"\nallowed_programs = [\"lenso-nonexistent-profile-test-program\"]\n").unwrap();
        authority.publish(&invalid).unwrap();
        let (status, selected) = request(&surface, "POST", "control/profile", serde_json::json!({"profile":"plan"})).await;
        assert_eq!(status, StatusCode::OK, "{selected}");
        let (_, before) = request(&surface, "GET", "plugins", serde_json::Value::Null).await;
        let (status, error) = request(&surface, "POST", "control/profile", serde_json::json!({"profile":"code"})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert!(error.to_string().contains("lenso-nonexistent-profile-test-program"), "{error}");
        let (_, after) = request(&surface, "GET", "plugins", serde_json::Value::Null).await;
        assert_eq!(before["active"]["planDigest"], after["active"]["planDigest"]);
        surface.shutdown().await.unwrap();
        let mut restarted = config(root.path());
        restarted.profile = Some("plan".to_owned());
        let surface = AgentWebSurface::start(restarted).await.unwrap();
        let (status, selected) = request(&surface, "POST", "control/profile", serde_json::json!({"profile":null})).await;
        assert_eq!(status, StatusCode::OK, "{selected}");
        assert_eq!(selected["profile"], serde_json::Value::Null);
        surface.shutdown().await.unwrap();
    })).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sqlite_profile_import_requires_host_authorization() {
    let root = tempfile::tempdir().unwrap();
    configure_test_fixture_model(root.path());
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut config = config(root.path());
            config.control = AgentWebControl::Bearer("test-secret".to_owned());
            let surface = AgentWebSurface::start(config).await.unwrap();
            let (status, _) = request(
                &surface,
                "POST",
                "control/profiles/import",
                serde_json::json!({"expectedRevision":"unused", "expectedStreamId":"unused"}),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert!(!root.path().join("profiles/plan.toml").exists());
            surface.shutdown().await.unwrap();
        })
        .await;
}

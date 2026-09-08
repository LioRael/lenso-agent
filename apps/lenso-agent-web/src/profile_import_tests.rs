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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{path}: {status}: {}: {error}",
                String::from_utf8_lossy(&bytes)
            )
        }),
    )
}

async fn turn_events(surface: &AgentWebSurface, id: &str, allowed: Option<Vec<&str>>) -> String {
    let mut body = serde_json::json!({"request_id": id, "input": "What did you summarize?"});
    if let Some(allowed) = allowed {
        body["allowed_tools"] = serde_json::json!(allowed);
    }
    let response = surface
        .router()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/console/v1/agent/turns")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        axum::body::to_bytes(response.into_body(), 1024 * 1024),
    )
    .await
    .unwrap()
    .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn config(root: &FsPath) -> AgentWebConfig {
    let mut config = AgentWebConfig::new(lenso_agent_default_plugins::link);
    config.agent_home = Some(root.to_path_buf());
    config.access = AgentWebAccess::HostAuthorized;
    config.control = AgentWebControl::HostAuthorized;
    config.plugin_control = true;
    config.tool_policy = Some(root.join("tool-policy.json"));
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
        assert_eq!(request(&surface, "GET", "bootstrap", serde_json::Value::Null).await.1["profile"], "plan");
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
        let (status, selected) = request(&surface, "POST", "control/profile", serde_json::json!({"profile":"code"})).await;
        assert_eq!(status, StatusCode::OK, "{selected}");
        let (_, bootstrap) = request(&surface, "GET", "bootstrap", serde_json::Value::Null).await;
        assert!(bootstrap["tools"]["available"].as_array().unwrap().iter().any(|tool| tool["name"] == "edit"));
        assert_eq!(bootstrap["tools"]["allowed"], serde_json::json!([]));
        let (status, policy) = request(&surface, "PUT", "control/tool-policy", serde_json::json!({"expectedRevision":0,"allowed":["edit","read"]})).await;
        assert_eq!(status, StatusCode::OK, "{policy}");
        let (status, selected) = request(&surface, "POST", "control/profile", serde_json::json!({"profile":"plan"})).await;
        assert_eq!(status, StatusCode::OK, "{selected}");
        let (_, bootstrap) = request(&surface, "GET", "bootstrap", serde_json::Value::Null).await;
        assert!(!bootstrap["tools"]["available"].as_array().unwrap().iter().any(|tool| tool["name"] == "edit"));
        assert_eq!(bootstrap["tools"]["allowed"], serde_json::json!(["edit", "read"]));
        let events = turn_events(&surface, "plan-default-tools", None).await;
        assert!(events.contains("turn.completed"), "{events}");
        let events = turn_events(&surface, "plan-stale-edit", Some(vec!["edit"])).await;
        assert!(events.contains("Turn Tool selection exceeds"), "{events}");
        let events = turn_events(&surface, "plan-after-rejected-edit", Some(vec!["read"])).await;
        assert!(events.contains("turn.completed"), "{events}");
        assert_eq!(request(&surface, "GET", "models", serde_json::Value::Null).await.0, StatusCode::OK);
        surface.shutdown().await.unwrap();
        let mut restarted = config(root.path());
        restarted.profile = Some("plan".to_owned());
        let surface = AgentWebSurface::start(restarted).await.unwrap();
        let (_, bootstrap) = request(&surface, "GET", "bootstrap", serde_json::Value::Null).await;
        assert_eq!(bootstrap["tools"]["allowed"], serde_json::json!(["edit", "read"]));
        let events = turn_events(&surface, "plan-after-restart", None).await;
        assert!(events.contains("turn.completed"), "{events}");
        let (_, inventory) = request(&surface, "GET", "plugins", serde_json::Value::Null).await;


        assert_eq!(request(&surface, "PUT", "control/tool-policy", serde_json::json!({"expectedRevision":1,"allowed":[]})).await.0, StatusCode::OK);
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
        assert_eq!(request(&surface, "GET", "bootstrap", serde_json::Value::Null).await.1["profile"], "plan");
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

#[tokio::test(flavor = "current_thread")]
async fn sqlite_profile_switch_refreshes_existing_session_prompt() {
    let root = tempfile::tempdir().unwrap();
    configure_test_fixture_model(root.path());
    tokio::task::LocalSet::new().run_until(async {
        let mut surface = AgentWebSurface::start(config(root.path())).await.unwrap();
        let (_, inventory) = request(&surface, "GET", "plugins", serde_json::Value::Null).await;
        let (_, management) = request(&surface, "GET", "control/plugins", serde_json::Value::Null).await;
        let (status, imported) = request(&surface, "POST", "control/profiles/import", serde_json::json!({
            "expectedRevision": management["revision"], "expectedStreamId": inventory["streamId"]
        })).await;
        assert_eq!(status, StatusCode::OK, "{imported}");
        let mut session_id: Option<String> = None;
        for (index, (profile, expected, excluded)) in [("plan", "harness.plan", "harness.coding"), ("code", "harness.coding", "harness.plan"), ("code", "harness.coding", "harness.plan"), ("plan", "harness.plan", "harness.coding")].into_iter().enumerate() {
            if index == 2 {
                surface.shutdown().await.unwrap();
                let mut restarted = config(root.path());
                restarted.profile = Some("code".to_owned());
                surface = AgentWebSurface::start(restarted).await.unwrap();
            }
            let (status, selected) = request(&surface, "POST", "control/profile", serde_json::json!({"profile": profile})).await;
            assert_eq!(status, StatusCode::OK, "{selected}");
            let response = surface.router().oneshot(axum::http::Request::builder()
                .method("POST").uri("/api/console/v1/agent/turns")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::json!({
                    "request_id": format!("prompt-{profile}-{index}"), "session_id": session_id,
                    "input": "What did you summarize?", "allowed_tools": []
                }).to_string())).unwrap()).await.unwrap();
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let events = String::from_utf8(body.to_vec()).unwrap();
            assert!(events.contains("turn.completed"), "{events}");
            let (_, sessions) = request(&surface, "GET", "sessions", serde_json::Value::Null).await;
            session_id = Some(sessions["sessions"][0]["sessionId"].as_str().unwrap().to_owned());
            let (_, session) = request(&surface, "GET", &format!("sessions/{}", session_id.as_ref().unwrap()), serde_json::Value::Null).await;
            let records = session["events"].as_array().unwrap();
            assert_eq!(records.iter().filter(|event| event["kind"] == "system_instruction_installed").count(), 1);
            assert_eq!(records.iter().filter(|event| event["kind"] == "system_instruction_revised").count(), [0, 1, 1, 2][index]);
            let event = session["events"].as_array().unwrap().iter().rev()
                .find(|event| event["kind"] == "model_requested").unwrap();
            let payload: serde_json::Value = serde_json::from_str(event["payload_json"].as_str().unwrap()).unwrap();
            let contributions = payload["prompt_contributions"].as_array().unwrap();
            assert!(contributions.iter().any(|item| item["id"] == expected), "{profile}: {payload}");
            assert!(!contributions.iter().any(|item| item["id"] == excluded), "{profile}: {payload}");
        }
        surface.shutdown().await.unwrap();
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn text_attachment_survives_restart_and_is_session_scoped() {
    let root = tempfile::tempdir().unwrap();
    configure_test_fixture_model(root.path());
    Box::pin(tokio::task::LocalSet::new().run_until(async {
        let surface = AgentWebSurface::start(config(root.path())).await.unwrap();
        let response = surface.router().oneshot(axum::http::Request::builder().method("POST")
            .uri("/api/console/v1/agent/turns").header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::json!({"request_id":"attachment-test","input":"Echo attached text:","attachments":[{"name":"notes.md","media_type":"text/plain","data_base64":"aGVsbG8="}]}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let events = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(events.contains("turn.completed"), "{events}");
        assert!(events.contains("hello"), "{events}");
        let (_, listed) = request(&surface,"GET","sessions",serde_json::Value::Null).await;
        let session_id = listed["sessions"][0]["sessionId"].as_str().unwrap().to_owned();
        let (_, session) = request(&surface,"GET",&format!("sessions/{session_id}"),serde_json::Value::Null).await;
        let reference = session["events"].as_array().unwrap().iter().find_map(|event| {
            let payload: serde_json::Value = serde_json::from_str(event["payload_json"].as_str()?).ok()?;
            payload.get("attachments")?.as_array()?.first().cloned()
        }).unwrap();
        assert!(reference.get("data_base64").is_none());
        let digest = reference["digest"].as_str().unwrap().strip_prefix("sha256:").unwrap();
        let path = format!("sessions/{session_id}/attachments/{digest}");
        let (status, content) = request(&surface,"GET",&path,serde_json::Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{content}");
        assert_eq!(content["data_base64"], "aGVsbG8=");
        surface.shutdown().await.unwrap();
        let surface = AgentWebSurface::start(config(root.path())).await.unwrap();
        assert_eq!(request(&surface,"GET",&path,serde_json::Value::Null).await.1["data_base64"], "aGVsbG8=");
        let response = surface.router().oneshot(axum::http::Request::builder().method("POST")
            .uri("/api/console/v1/agent/turns").header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::json!({"request_id":"attachment-resume","session_id":session_id,"input":"Echo previous attached text:"}).to_string())).unwrap()).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let events = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(events.contains("turn.completed") && events.contains("hello"), "{events}");
        assert_ne!(request(&surface,"GET",&format!("sessions/other/attachments/{digest}"),serde_json::Value::Null).await.0, StatusCode::OK);
        surface.shutdown().await.unwrap();
    })).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sqlite_profile_editor_separates_drafts_from_active_generations() {
    let root = tempfile::tempdir().unwrap();
    configure_test_fixture_model(root.path());
    Box::pin(tokio::task::LocalSet::new().run_until(async {
        let surface = AgentWebSurface::start(config(root.path())).await.unwrap();
        let (status, catalog) = request(&surface, "GET", "control/profiles", serde_json::Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{catalog}");
        let mut document = catalog["profiles"].as_array().unwrap().iter().find(|p| p["name"] == "default").unwrap()["document"].clone();
        document["allowed_tools"] = serde_json::json!([]);
        document["instructions"] = serde_json::json!("Always explain your assumptions.");
        let payload = serde_json::json!({"name":"review", "expectedRevision":null,"document":document});
        let (status, saved) = request(&surface, "POST", "control/profiles", payload.clone()).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert!(!root.path().join("profiles/review.toml").exists(), "Saving must not materialize a live Profile");
        assert_eq!(request(&surface, "POST", "control/profiles", payload).await.0, StatusCode::CONFLICT);
        let (status, applied) = request(&surface,"POST","control/profile",serde_json::json!({"profile":"review", "expectedRevision":saved["revision"]})).await;
        assert_eq!(status, StatusCode::OK, "{applied}");
        let active_file = std::fs::read_to_string(root.path().join("profiles/review.toml")).unwrap();
        assert!(active_file.contains("Always explain"));
        let events = turn_events(&surface, "profile-editor-turn", Some(vec![])).await;
        assert!(!events.contains("tool_call_started"), "{events}");
        let (_, policy) = request(&surface, "GET", "control/tool-policy", serde_json::Value::Null).await;
        assert_eq!(request(&surface, "PUT", "control/tool-policy", serde_json::json!({"expectedRevision": policy["revision"], "allowed": ["read"]})).await.0, StatusCode::OK);
        let response = surface.router().oneshot(axum::http::Request::builder().method("POST")
            .uri("/api/console/v1/agent/turns").header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::json!({"request_id":"profile-ceiling", "input":"Read README.md twice with parallel approval.", "allowed_tools":["read"]}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1_048_576).await.unwrap();
        let denied = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(denied.contains("outside the immutable Run Scope"), "A Profile must narrow even an explicit caller grant: {denied}");
        assert!(denied.contains("tool_not_allowed"), "{denied}");
        let resumed = turn_events(&surface, "profile-after-denied-tool", Some(vec![])).await;
        assert!(resumed.contains("turn.completed"), "A rejected Tool must not retire the Agent: {resumed}");

        document["instructions"] = serde_json::json!("Keep replies concise.");
        let (status, next) = request(&surface,"POST","control/profiles",serde_json::json!({"name":"review","expectedRevision":saved["revision"],"document":document})).await;
        assert_eq!(status, StatusCode::OK,"{next}");
        assert_eq!(std::fs::read_to_string(root.path().join("profiles/review.toml")).unwrap(), active_file);
        let (_, catalog) = request(&surface,"GET","control/profiles",serde_json::Value::Null).await;
        assert_eq!(catalog["activeRevision"], saved["revision"]);
        assert_eq!(request(&surface,"POST","control/profile",serde_json::json!({"profile":"review","expectedRevision":saved["revision"]})).await.0, StatusCode::CONFLICT);
        assert_eq!(request(&surface,"POST","control/profile",serde_json::json!({"profile":"review","expectedRevision":next["revision"]})).await.0, StatusCode::OK);
        document["instances"] = serde_json::json!(["missing.plugin/unknown"]);
        let invalid = request(&surface,"POST","control/profiles",serde_json::json!({"name":"review","expectedRevision":next["revision"],"document":document})).await;
        assert_eq!(invalid.0, StatusCode::CONFLICT,"{}",invalid.1);
    })).await;
    Box::pin(tokio::task::LocalSet::new().run_until(async {
        let mut restart = config(root.path());
        restart.profile = Some("review".to_owned());
        let restarted = AgentWebSurface::start(restart).await.unwrap();
        let (_, catalog) = request(
            &restarted,
            "GET",
            "control/profiles",
            serde_json::Value::Null,
        )
        .await;
        assert_eq!(catalog["activeProfile"], "review");
        assert!(
            catalog["activeRevision"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
    }))
    .await;
}

use std::{
    fs,
    net::{SocketAddr, TcpListener},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[tokio::test(flavor = "current_thread")]
async fn streams_lists_and_branches_a_durable_session() {
    let root = tempfile::tempdir().unwrap();
    write_web_fixture(root.path());

    let address = available_address();
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_lenso-agent-web"))
            .args(["--listen", &address.to_string(), "--profile", "web"])
            .current_dir(root.path())
            .env("LENSO_AGENT_HOME", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::new();
    wait_until_ready(&client, address, &mut server.0).await;
    let bootstrap = client
        .get(format!("http://{address}/api/console/v1/agent/bootstrap"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(bootstrap["tools"]["allowed"], serde_json::json!([]));
    assert!(
        bootstrap["tools"]["available"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "read")
    );
    assert_eq!(bootstrap["trajectory"], "lenso.agent.trajectory@1");
    assert_eq!(bootstrap["capabilities"]["taskSnapshot"], true);
    let tasks = client
        .get(format!("http://{address}/api/console/v1/agent/tasks"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(tasks, serde_json::json!({"tasks": []}));

    let response = client
        .post(format!("http://{address}/api/console/v1/agent/turns"))
        .header("accept", "text/event-stream")
        .json(&serde_json::json!({
            "input": "Answer directly: hello",
            "request_id": "request-initial",
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let body = response.text().await.unwrap();
    assert!(
        body.contains("I’ll answer directly from the current context."),
        "unexpected Agent stream: {body}"
    );
    assert!(body.contains("Direct "), "unexpected Agent stream: {body}");
    assert!(body.contains("answer."), "unexpected Agent stream: {body}");
    let session_id = stream_session_id(&body);

    let session = client
        .get(format!(
            "http://{address}/api/console/v1/agent/sessions/{session_id}"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(session["session_id"], session_id);
    assert!(
        session["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "turn_completed")
    );
    verify_trajectory_and_presentation(&client, address, &session_id, &session).await;
    verify_history_and_branch(&client, address, &session_id, &session).await;
}

async fn verify_trajectory_and_presentation(
    client: &reqwest::Client,
    address: SocketAddr,
    session_id: &str,
    session: &serde_json::Value,
) {
    let response = client
        .get(format!(
            "http://{address}/api/console/v1/agent/sessions/{session_id}/trajectory"
        ))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert!(status.is_success(), "trajectory request failed: {body}");
    let trajectory: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(trajectory["schema"], "lenso.agent.trajectory@1");
    assert_eq!(trajectory["summary"]["turns"], 1);
    assert_eq!(trajectory["summary"]["modelCalls"], 1);
    let model = trajectory["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["kind"] == "model")
        .unwrap();
    assert_eq!(model["status"], "completed");
    assert!(model["durationMs"].is_number());
    assert_eq!(model["sourceEventIds"].as_array().unwrap().len(), 2);
    assert!(
        session["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "turn_completed")
            .filter_map(|event| event["payload_json"].as_str())
            .filter_map(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
            .any(|payload| payload["presentation"]["title"] == "Answer directly: hello")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn answers_a_pending_web_interaction_and_resumes_the_same_turn() {
    let root = tempfile::tempdir().unwrap();
    write_web_fixture(root.path());

    let address = available_address();
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_lenso-agent-web"))
            .args([
                "--listen",
                &address.to_string(),
                "--profile",
                "web",
                "--allow-tool",
                "ask_user",
            ])
            .current_dir(root.path())
            .env("LENSO_AGENT_HOME", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::new();
    wait_until_ready(&client, address, &mut server.0).await;

    let response = client
        .post(format!("http://{address}/api/console/v1/agent/turns"))
        .header("accept", "text/event-stream")
        .json(&serde_json::json!({
            "input": "Ask me which mode to use.",
            "request_id": "request-interaction",
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let stream = tokio::spawn(async move { response.text().await.unwrap() });

    let interaction = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let response = client
                .get(format!(
                    "http://{address}/api/console/v1/agent/turns/request-interaction/interactions"
                ))
                .send()
                .await
                .unwrap();
            if response.status().is_success() {
                let body = response.json::<serde_json::Value>().await.unwrap();
                if let Some(interaction) = body["interactions"].as_array().unwrap().first() {
                    break interaction.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("ask_user should become visible to the Web surface");
    assert_eq!(interaction["questions"][0]["header"], "Mode");
    assert_eq!(
        interaction["questions"][0]["options"][0]["preview"],
        "mode = \"safe\""
    );
    assert_tasks_readable_while_turn_runs(&client, address).await;

    client
        .post(format!(
            "http://{address}/api/console/v1/agent/turns/request-interaction/interactions/{}/answer",
            interaction["interactionId"].as_str().unwrap()
        ))
        .json(&serde_json::json!({
            "answers": [{
                "questionId": "mode",
                "selectedOptionIds": ["safe"],
                "other": null,
            }]
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let body = tokio::time::timeout(Duration::from_secs(2), stream)
        .await
        .expect("the answered Turn should resume")
        .unwrap();
    assert!(
        body.contains("Selected mode: safe"),
        "unexpected stream: {body}"
    );
    assert!(body.contains("turn_completed"), "unexpected stream: {body}");
}

async fn assert_tasks_readable_while_turn_runs(client: &reqwest::Client, address: SocketAddr) {
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        client
            .get(format!("http://{address}/api/console/v1/agent/tasks"))
            .send(),
    )
    .await
    .expect("task snapshots must remain readable while a Turn is blocked")
    .unwrap()
    .error_for_status()
    .unwrap();
    let tasks = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(tasks, serde_json::json!({"tasks": []}));
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end proof retains reconnect, isolation, review, and integration evidence"
)]
async fn supervises_reconnects_reviews_and_integrates_two_isolated_workers() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let home = temporary.path().join("agent-home");
    initialize_git_workspace(&workspace);
    write_supervised_coding_fixture(&home);

    let address = available_address();
    let mut command = Command::new(env!("CARGO_BIN_EXE_lenso-agent-web"));
    command.args(["--listen", &address.to_string(), "--profile", "code"]);
    for tool in [
        "spawn_subagent",
        "wait_subagent",
        "ask_user",
        "review_worktree",
        "integrate_worktree",
        "create_file",
        "git_stage",
        "git_commit",
    ] {
        command.args(["--allow-tool", tool]);
    }
    let mut server = ChildGuard(
        command
            .current_dir(&workspace)
            .env("LENSO_AGENT_HOME", &home)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::new();
    wait_until_ready(&client, address, &mut server.0).await;

    let response = client
        .post(format!("http://{address}/api/console/v1/agent/turns"))
        .header("accept", "text/event-stream")
        .json(&serde_json::json!({
            "input": "Supervise and integrate two isolated mutation workers.",
            "request_id": "request-supervised-integration",
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let stream = tokio::spawn(async move { response.text().await.unwrap() });

    let interaction = wait_for_interaction(
        &client,
        address,
        "request-supervised-integration",
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(interaction["questions"][0]["header"], "Review");

    let before_reconnect = wait_for_completed_task_progress(&client, address).await;
    assert_eq!(before_reconnect["tasks"].as_array().unwrap().len(), 2);
    assert!(!workspace.join("worker-a.txt").exists());
    assert!(!workspace.join("worker-b.txt").exists());
    let workspaces = before_reconnect["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["workspace"].as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(workspaces.len(), 2);
    assert!(workspaces.iter().all(|path| Path::new(path).is_dir()));

    let reconnected = reqwest::Client::new()
        .get(format!("http://{address}/api/console/v1/agent/tasks"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(reconnected, before_reconnect);

    client
        .post(format!(
            "http://{address}/api/console/v1/agent/turns/request-supervised-integration/interactions/{}/answer",
            interaction["interactionId"].as_str().unwrap()
        ))
        .json(&serde_json::json!({
            "answers": [{
                "questionId": "integration",
                "selectedOptionIds": ["integrate"],
                "other": null,
            }]
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let body = tokio::time::timeout(Duration::from_secs(20), stream)
        .await
        .expect("reviewed integration Turn should complete")
        .unwrap();
    assert!(
        body.contains("Both reviewed worker commits were integrated."),
        "unexpected stream: {body}"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("worker-a.txt")).unwrap(),
        "worker-a\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("worker-b.txt")).unwrap(),
        "worker-b\n"
    );

    let session = client
        .get(format!(
            "http://{address}/api/console/v1/agent/sessions/{}",
            stream_session_id(&body)
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    let review = session.find("call-review-supervised-worker-a").unwrap();
    let integrate = session.find("call-integrate-supervised-worker-a").unwrap();
    assert!(
        review < integrate,
        "worktrees must be reviewed before integration"
    );
    assert!(session.contains("reviewed_commit="));
    assert!(session.contains("diff_sha256="));
}

async fn wait_for_interaction(
    client: &reqwest::Client,
    address: SocketAddr,
    request_id: &str,
    timeout: Duration,
) -> serde_json::Value {
    tokio::time::timeout(timeout, async {
        loop {
            let response = client
                .get(format!(
                    "http://{address}/api/console/v1/agent/turns/{request_id}/interactions"
                ))
                .send()
                .await
                .unwrap();
            if response.status().is_success() {
                let body = response.json::<serde_json::Value>().await.unwrap();
                if let Some(interaction) = body["interactions"].as_array().unwrap().first() {
                    break interaction.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("interaction should become visible")
}

async fn wait_for_completed_task_progress(
    client: &reqwest::Client,
    address: SocketAddr,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let tasks = client
            .get(format!("http://{address}/api/console/v1/agent/tasks"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        let complete = tasks["tasks"].as_array().is_some_and(|items| {
            items.len() == 2
                && items.iter().all(|task| {
                    task["status"] == "completed"
                        && task["progress"]["revision"]
                            .as_i64()
                            .is_some_and(|value| value > 0)
                        && task["progress"]["message_count"]
                            .as_i64()
                            .is_some_and(|value| value > 0)
                        && task["progress"]["tool_call_count"]
                            .as_i64()
                            .is_some_and(|value| value > 0)
                        && task["progress"]["content"]
                            .as_str()
                            .is_some_and(|value| value.len() <= 4096)
                })
        });
        if complete {
            return tasks;
        }
        assert!(
            Instant::now() < deadline,
            "both isolated workers should expose bounded terminal progress: {tasks}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_an_allowed_tool_outside_the_active_catalog_before_readiness() {
    let root = tempfile::tempdir().unwrap();
    write_web_fixture(root.path());
    let address = available_address();
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_lenso-agent-web"))
            .args([
                "--listen",
                &address.to_string(),
                "--profile",
                "web",
                "--allow-tool",
                "missing.tool",
            ])
            .current_dir(root.path())
            .env("LENSO_AGENT_HOME", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    for _ in 0..100 {
        if let Some(status) = server.0.try_wait().unwrap() {
            let stderr = server
                .0
                .stderr
                .take()
                .map(|stderr| std::io::read_to_string(stderr).unwrap_or_default())
                .unwrap_or_default();
            assert!(!status.success());
            assert!(stderr.contains("is not in the active Plan-bound catalog"));
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("Agent Web accepted an unknown Tool and reached readiness");
}

#[tokio::test(flavor = "current_thread")]
async fn updates_and_recovers_the_durable_tool_policy_with_revision_fencing() {
    let root = tempfile::tempdir().unwrap();
    write_web_fixture(root.path());
    let policy_path = root.path().join("state/tool-policy.json");
    let control_token = ["fixture", "agent", "control"].join("-");
    let address = available_address();
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_lenso-agent-web"))
            .args([
                "--listen",
                &address.to_string(),
                "--profile",
                "web",
                "--tool-policy",
                policy_path.to_str().unwrap(),
            ])
            .env("LENSO_AGENT_CONTROL_TOKEN", &control_token)
            .current_dir(root.path())
            .env("LENSO_AGENT_HOME", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::new();
    wait_until_ready(&client, address, &mut server.0).await;
    let endpoint = format!("http://{address}/api/console/v1/agent/control/tool-policy");

    assert_eq!(
        client.get(&endpoint).send().await.unwrap().status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let updated = client
        .put(&endpoint)
        .bearer_auth(&control_token)
        .json(&serde_json::json!({
            "allowed": ["read"],
            "expectedRevision": 0,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(updated["revision"], 1);
    assert_eq!(updated["allowed"], serde_json::json!(["read"]));
    assert_eq!(
        client
            .put(&endpoint)
            .bearer_auth(&control_token)
            .json(&serde_json::json!({
                "allowed": [],
                "expectedRevision": 0,
            }))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::CONFLICT
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(&policy_path).unwrap()).unwrap();
    assert_eq!(persisted["revision"], 1);
    assert_eq!(persisted["allowed"], serde_json::json!(["read"]));

    drop(server);
    let mut recovered = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_lenso-agent-web"))
            .args([
                "--listen",
                &address.to_string(),
                "--profile",
                "web",
                "--tool-policy",
                policy_path.to_str().unwrap(),
            ])
            .env("LENSO_AGENT_CONTROL_TOKEN", &control_token)
            .current_dir(root.path())
            .env("LENSO_AGENT_HOME", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    wait_until_ready(&client, address, &mut recovered.0).await;
    let bootstrap = client
        .get(format!("http://{address}/api/console/v1/agent/bootstrap"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(bootstrap["tools"]["allowed"], serde_json::json!(["read"]));
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::too_many_lines)]
async fn authorizes_plugin_root_changes_and_switches_only_a_valid_generation() {
    let root = tempfile::tempdir().unwrap();
    let control_token = ["fixture", "plugin", "control"].join("-");
    let address = available_address();
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_lenso-agent-web"))
            .args(["--listen", &address.to_string(), "--plugin-control"])
            .env("LENSO_AGENT_CONTROL_TOKEN", &control_token)
            .current_dir(root.path())
            .env("LENSO_AGENT_HOME", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::new();
    wait_until_ready(&client, address, &mut server.0).await;
    let endpoint = format!(
        "http://{address}/api/console/v1/agent/control/plugins/lenso.agent.loop/agent/configuration"
    );
    let proposals_endpoint = format!("{endpoint}/proposals");
    let management_endpoint = format!("http://{address}/api/console/v1/agent/control/plugins");
    let inventory_endpoint = format!("http://{address}/api/console/v1/agent/plugins");
    let configuration = root.path().join("plugins/lenso.agent.loop/agent.toml");
    assert!(!configuration.exists());
    let initial_generation = active_generation_digest(root.path());

    assert_plugin_management_forbidden(&client, &management_endpoint).await;
    let initial_management =
        read_plugin_management(&client, &management_endpoint, &control_token).await;
    assert_initial_plugin_management(&initial_management);
    assert_eq!(
        initial_management["configurationAuthority"]["kind"],
        "local_plugin_root"
    );
    let initial_revision = initial_management["revision"].as_str().unwrap();
    let initial_inventory = read_plugin_inventory(&client, &inventory_endpoint).await;
    assert_eq!(initial_inventory["desiredRevision"], initial_revision);
    assert_eq!(initial_inventory["appliedRevision"], initial_revision);
    assert_eq!(initial_inventory["configurationStatus"], "applied");
    assert_eq!(
        initial_inventory["configurationAuthority"]["reference"],
        "app"
    );

    assert_eq!(
        client
            .post(&proposals_endpoint)
            .json(&serde_json::json!({
                "expectedRevision": initial_revision,
                "toml": "unexpected = true\n",
            }))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert!(!configuration.exists());

    assert_eq!(
        client
            .post(&proposals_endpoint)
            .bearer_auth(&control_token)
            .json(&serde_json::json!({
                "expectedRevision": initial_revision,
                "toml": "unexpected = true\n",
            }))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    let rejected = client
        .post(&proposals_endpoint)
        .bearer_auth(&control_token)
        .json(&serde_json::json!({
            "expectedRevision": initial_revision,
            "toml": "unexpected = true\n",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(rejected["status"], "rejected");
    assert_eq!(rejected["application"], "blocked");
    assert_eq!(rejected["diagnostics"][0]["code"], "invalid_configuration");
    assert!(!configuration.exists());
    assert_eq!(active_generation_digest(root.path()), initial_generation);

    let updated_configuration = concat!(
        "model = \"gpt-5.6-luna\"\n",
        "max_steps = 9\n",
        "max_tool_calls = 4\n",
        "max_user_resumes = 8\n",
        "max_parallel_tool_calls = 4\n",
        "max_output_tokens = 1024\n",
        "max_history_events = 200\n",
        "max_compaction_summary_characters = 8192\n",
        "max_memory_items = 8\n",
        "max_memory_characters = 16384\n",
    );
    let proposal = client
        .post(&proposals_endpoint)
        .bearer_auth(&control_token)
        .json(&serde_json::json!({
            "expectedRevision": initial_revision,
            "toml": updated_configuration,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(proposal["schema"], "lenso.plugin-configuration-proposal.v1");
    assert_eq!(proposal["status"], "ready");
    assert_eq!(proposal["application"], "app_generation");
    assert_eq!(proposal["baseRevision"], initial_revision);
    assert_eq!(
        proposal["configurationAuthority"]["kind"],
        "local_plugin_root"
    );
    assert_ne!(proposal["candidateRevision"], initial_revision);
    assert!(!configuration.exists());

    assert_eq!(
        client
            .put(&endpoint)
            .bearer_auth(&control_token)
            .json(&serde_json::json!({
                "expectedRevision": initial_revision,
                "proposalDigest": "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                "toml": updated_configuration,
            }))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::CONFLICT
    );
    assert!(!configuration.exists());

    let accepted = client
        .put(&endpoint)
        .bearer_auth(&control_token)
        .json(&serde_json::json!({
            "expectedRevision": initial_revision,
            "proposalDigest": proposal["proposalDigest"],
            "toml": updated_configuration,
        }))
        .send()
        .await
        .unwrap();
    let accepted_status = accepted.status();
    let accepted_body = accepted.text().await.unwrap();
    assert_eq!(
        accepted_status,
        reqwest::StatusCode::ACCEPTED,
        "unexpected Plugin mutation response: {accepted_body}"
    );
    let accepted: serde_json::Value = serde_json::from_str(&accepted_body).unwrap();
    assert_eq!(
        accepted["schema"],
        "lenso.plugin-configuration-publication.v1"
    );
    assert_eq!(accepted["status"], "published");
    assert_eq!(accepted["baseRevision"], initial_revision);
    assert_eq!(accepted["revision"], proposal["candidateRevision"]);
    assert_eq!(accepted["proposalDigest"], proposal["proposalDigest"]);
    assert_eq!(
        accepted["configurationAuthority"],
        proposal["configurationAuthority"]
    );
    assert!(accepted["desired"]["plugins"].is_array());
    assert_eq!(accepted["desired"]["desiredRevision"], accepted["revision"]);
    assert_eq!(accepted["desired"]["configurationStatus"], "pending");
    assert_eq!(
        fs::read_to_string(&configuration).unwrap(),
        updated_configuration
    );
    let updated_management =
        read_plugin_management(&client, &management_endpoint, &control_token).await;
    assert_eq!(updated_management["revision"], accepted["revision"]);
    let updated_loop = managed_plugin(&updated_management, "lenso.agent.loop");
    assert_eq!(
        updated_loop["instances"][0]["rootConfigurationToml"],
        updated_configuration
    );
    assert_eq!(updated_loop["instances"][0]["hasRootDifference"], true);

    assert_eq!(
        client
            .put(&endpoint)
            .bearer_auth(&control_token)
            .json(&serde_json::json!({
                "expectedRevision": initial_revision,
                "proposalDigest": proposal["proposalDigest"],
                "toml": updated_configuration,
            }))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::CONFLICT
    );

    let switched_generation = wait_for_generation_change(root.path(), &initial_generation).await;
    assert_ne!(switched_generation, initial_generation);
    let applied = wait_for_configuration_applied(
        &client,
        &inventory_endpoint,
        accepted["revision"].as_str().unwrap(),
    )
    .await;
    assert_eq!(applied["desiredRevision"], accepted["revision"]);
    assert_eq!(applied["appliedRevision"], accepted["revision"]);
    assert_eq!(applied["configurationStatus"], "applied");
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::too_many_lines)]
async fn persists_managed_plugin_configuration_across_host_restart() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("configuration.sqlite3");
    let control_token = ["fixture", "managed", "configuration"].join("-");
    let address = available_address();
    let start_server = || {
        ChildGuard(
            Command::new(env!("CARGO_BIN_EXE_lenso-agent-web"))
                .args([
                    "--listen",
                    &address.to_string(),
                    "--plugin-control",
                    "--plugin-configuration-store",
                    database.to_str().unwrap(),
                ])
                .env("LENSO_AGENT_CONTROL_TOKEN", &control_token)
                .current_dir(root.path())
                .env("LENSO_AGENT_HOME", root.path())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    };
    let mut server = start_server();
    let client = reqwest::Client::new();
    wait_until_ready(&client, address, &mut server.0).await;
    let endpoint = format!(
        "http://{address}/api/console/v1/agent/control/plugins/lenso.agent.loop/agent/configuration"
    );
    let proposals_endpoint = format!("{endpoint}/proposals");
    let management_endpoint = format!("http://{address}/api/console/v1/agent/control/plugins");
    let initial = read_plugin_management(&client, &management_endpoint, &control_token).await;
    assert_eq!(
        initial["configurationAuthority"],
        serde_json::json!({
            "kind": "sqlite_configuration_store",
            "publicationHistory": true,
            "reference": "agent",
            "rollbackProposals": true,
        })
    );
    let initial_revision = initial["revision"].as_str().unwrap();
    assert_eq!(
        client
            .post(format!(
                "http://{address}/api/console/v1/agent/control/plugins/lenso.agent.loop/agent/disable"
            ))
            .bearer_auth(&control_token)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::CONFLICT
    );
    assert_eq!(
        read_plugin_management(&client, &management_endpoint, &control_token).await["revision"],
        initial_revision
    );
    let configuration = concat!(
        "model = \"gpt-5.6-luna\"\n",
        "max_steps = 7\n",
        "max_tool_calls = 4\n",
        "max_parallel_tool_calls = 4\n",
        "max_output_tokens = 1024\n",
        "max_history_events = 200\n",
        "max_compaction_summary_characters = 8192\n",
        "max_memory_items = 8\n",
        "max_memory_characters = 16384\n",
    );
    let proposal = client
        .post(&proposals_endpoint)
        .bearer_auth(&control_token)
        .json(&serde_json::json!({
            "expectedRevision": initial_revision,
            "toml": configuration,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let reformatted_configuration = configuration.replacen("model = ", "model=", 1);
    assert_eq!(
        client
            .put(&endpoint)
            .bearer_auth(&control_token)
            .json(&serde_json::json!({
                "expectedRevision": initial_revision,
                "proposalDigest": proposal["proposalDigest"],
                "toml": reformatted_configuration,
            }))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::CONFLICT
    );
    let publication = client
        .put(&endpoint)
        .bearer_auth(&control_token)
        .json(&serde_json::json!({
            "expectedRevision": initial_revision,
            "proposalDigest": proposal["proposalDigest"],
            "toml": configuration,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let published_revision = publication["revision"].as_str().unwrap().to_owned();
    drop(server);

    let mut recovered = start_server();
    wait_until_ready(&client, address, &mut recovered.0).await;
    let management = read_plugin_management(&client, &management_endpoint, &control_token).await;
    assert_eq!(management["revision"], published_revision);
    assert_eq!(
        management["configurationAuthority"],
        initial["configurationAuthority"]
    );
    let second_configuration = configuration.replacen("max_steps = 7", "max_steps = 8", 1);
    let second_proposal = client
        .post(&proposals_endpoint)
        .bearer_auth(&control_token)
        .json(&serde_json::json!({
            "expectedRevision": published_revision,
            "toml": second_configuration,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let second_publication = client
        .put(&endpoint)
        .bearer_auth(&control_token)
        .json(&serde_json::json!({
            "expectedRevision": published_revision,
            "proposalDigest": second_proposal["proposalDigest"],
            "toml": second_configuration,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let publications_endpoint = format!("{endpoint}/publications");
    let history = client
        .get(&publications_endpoint)
        .bearer_auth(&control_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(
        history["schema"],
        "lenso.agent.plugin-configuration-history.v1"
    );
    assert_eq!(history["publications"].as_array().unwrap().len(), 2);
    assert_eq!(
        history["publications"][0]["proposalDigest"],
        second_proposal["proposalDigest"]
    );
    assert_eq!(
        history["publications"][1]["configurationToml"],
        configuration
    );

    let rollback = client
        .post(format!("{endpoint}/rollback-proposals"))
        .bearer_auth(&control_token)
        .json(&serde_json::json!({
            "expectedRevision": second_publication["revision"],
            "publicationProposalDigest": proposal["proposalDigest"],
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(
        rollback["schema"],
        "lenso.agent.plugin-configuration-rollback-proposal.v1"
    );
    assert_eq!(rollback["configurationToml"], configuration);
    assert_eq!(rollback["proposal"]["status"], "ready");
    assert_eq!(
        fs::read_to_string(root.path().join("plugins/lenso.agent.loop/agent.toml")).unwrap(),
        second_configuration
    );
    client
        .put(&endpoint)
        .bearer_auth(&control_token)
        .json(&serde_json::json!({
            "expectedRevision": second_publication["revision"],
            "proposalDigest": rollback["proposal"]["proposalDigest"],
            "toml": rollback["configurationToml"],
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let history = client
        .get(&publications_endpoint)
        .bearer_auth(&control_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(history["publications"].as_array().unwrap().len(), 3);
    assert_eq!(
        history["publications"][0]["rollbackOfProposalDigest"],
        proposal["proposalDigest"]
    );
    let connection = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let publications = connection
        .query_row(
            "SELECT COUNT(*) FROM configuration_publications WHERE revision = ?1",
            [&published_revision],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(publications, 2);
}

async fn read_plugin_management(
    client: &reqwest::Client,
    endpoint: &str,
    control_token: &str,
) -> serde_json::Value {
    client
        .get(endpoint)
        .bearer_auth(control_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()
}

async fn read_plugin_inventory(client: &reqwest::Client, endpoint: &str) -> serde_json::Value {
    client
        .get(endpoint)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()
}

async fn wait_for_configuration_applied(
    client: &reqwest::Client,
    endpoint: &str,
    revision: &str,
) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let inventory = read_plugin_inventory(client, endpoint).await;
            if inventory["appliedRevision"] == revision {
                break inventory;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("published Plugin configuration should become the applied Generation")
}

async fn assert_plugin_management_forbidden(client: &reqwest::Client, endpoint: &str) {
    assert_eq!(
        client.get(endpoint).send().await.unwrap().status(),
        reqwest::StatusCode::FORBIDDEN
    );
}

fn managed_plugin<'a>(
    management: &'a serde_json::Value,
    package_id: &str,
) -> &'a serde_json::Value {
    management["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["packageId"] == package_id)
        .unwrap()
}

fn assert_initial_plugin_management(management: &serde_json::Value) {
    assert_eq!(management["schema"], "lenso.agent.plugin-management.v1");
    assert!(
        management["revision"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    let plugin = managed_plugin(management, "lenso.agent.loop");
    assert_eq!(plugin["instances"][0]["selection"], "enabled");
    assert_eq!(
        plugin["instances"][0]["rootConfigurationToml"],
        serde_json::Value::Null
    );
}

fn write_web_fixture(root: &std::path::Path) {
    let model_directory = root.join("plugins/lenso.agent.model.fixture");
    fs::create_dir_all(&model_directory).unwrap();
    fs::write(
        model_directory.join("web.toml"),
        "model = \"fixture/readme-summary-v1\"\n",
    )
    .unwrap();
    let agent_directory = root.join("plugins/lenso.agent.loop");
    fs::create_dir_all(&agent_directory).unwrap();
    fs::write(
        agent_directory.join("web.toml"),
        concat!(
            "model = \"fixture/readme-summary-v1\"\n",
            "max_steps = 2\n",
            "max_tool_calls = 2\n",
            "max_user_resumes = 1\n",
            "max_parallel_tool_calls = 1\n",
            "max_output_tokens = 128\n",
            "max_history_events = 100\n",
            "max_compaction_summary_characters = 8192\n",
            "max_memory_items = 8\n",
            "max_memory_characters = 16384\n",
        ),
    )
    .unwrap();
    fs::create_dir_all(root.join("profiles")).unwrap();
    fs::write(
        root.join("profiles/web.toml"),
        concat!(
            "description = \"Web fixture\"\n",
            "agent = \"lenso.agent.loop/web\"\n",
            "instances = [\"lenso.agent.loop/web\", \"lenso.agent.model.fixture/web\"]\n",
        ),
    )
    .unwrap();
}

fn initialize_git_workspace(workspace: &Path) {
    fs::create_dir_all(workspace).unwrap();
    fs::write(workspace.join("README.md"), "# Supervised Integration\n").unwrap();
    let git = |arguments: &[&str]| {
        let output = Command::new("git")
            .current_dir(workspace)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet"]);
    git(&["config", "user.name", "Lenso Test"]);
    git(&["config", "user.email", "test@example.invalid"]);
    git(&["add", "README.md"]);
    git(&["commit", "--quiet", "-m", "initial"]);
}

fn write_supervised_coding_fixture(root: &Path) {
    let files = [
        (
            "plugins/lenso.agent.workspace-instructions/default.toml",
            "working_directory = \".\"\nfile_name = \"AGENTS.md\"\nmax_ancestor_depth = 32\nmax_file_bytes = 262144\nmax_total_bytes = 1048576\n",
        ),
        (
            "plugins/lenso.agent.workspace-edit/default.toml",
            "root = \".\"\nmax_file_bytes = 1048576\nmax_edit_bytes = 131072\nrequire_checkpoint = false\n",
        ),
        (
            "plugins/lenso.agent.process.native/default.toml",
            "root = \".\"\nallowed_programs = [\"git\"]\nprogram_presets = []\nenvironment_allowlist = [\"PATH\", \"HOME\", \"TMPDIR\"]\nmax_timeout_ms = 120000\nmax_output_bytes = 262144\nmax_argument_bytes = 131072\n",
        ),
        (
            "plugins/lenso.agent.process-tools/default.toml",
            "default_timeout_ms = 120000\nmax_background_processes = 8\nmax_background_log_bytes = 262144\n",
        ),
        (
            "plugins/lenso.agent.git-tools/default.toml",
            "default_timeout_ms = 30000\nmax_log_entries = 50\nmax_commit_message_bytes = 4096\nenable_branch_management = false\nenable_history_integration = false\nallowed_network_remotes = []\n",
        ),
        (
            "plugins/lenso.agent.code-mode-tools/default.toml",
            "max_code_bytes = 32768\nmax_instructions = 1000000\nmax_memory_bytes = 8388608\nmax_output_bytes = 262144\nmax_parallel_subcalls = 4\nmax_subcalls = 16\n",
        ),
        (
            "plugins/lenso.agent.subagent-tools/worktree.toml",
            "max_output_bytes = 1048576\nmax_task_bytes = 262144\nmax_tasks = 8\nrequire_worktree_provider = true\n",
        ),
        (
            "plugins/lenso.agent.worktree-provider/default.toml",
            "mutation_agents = [\"lenso.agent.loop/worker-a\", \"lenso.agent.loop/worker-b\"]\nmax_worktrees = 8\ntimeout_ms = 120000\nmax_review_bytes = 1048576\n",
        ),
        (
            "plugins/lenso.agent.tools/worker-tools.toml",
            "# Private Tool runtime for mutation-capable child Agents.\n",
        ),
        (
            "plugins/lenso.agent.interactive-approval-hook/default.toml",
            "default_decision = \"ask\"\nallow_tools = [\"spawn_subagent\", \"wait_subagent\", \"ask_user\", \"review_worktree\", \"integrate_worktree\", \"create_file\", \"git_stage\", \"git_commit\"]\nask_tools = []\ndeny_tools = []\nmax_preview_bytes = 16384\n",
        ),
        (
            "plugins/lenso.agent.prompt.static/coding.toml",
            "[[contributions]]\nid = \"test.supervised-coding\"\nversion = \"1.0.0\"\nkind = \"instruction\"\ncontent = \"Use isolated workers and integrate only after explicit review approval.\"\n",
        ),
        (
            "plugins/lenso.agent.model.fixture/default.toml",
            "model = \"fixture/readme-summary-v1\"\n",
        ),
        (
            "plugins/lenso.agent.loop/agent.toml",
            "model = \"fixture/readme-summary-v1\"\nmax_steps = 8\nmax_tool_calls = 12\nmax_parallel_tool_calls = 4\nmax_output_tokens = 128\n",
        ),
        (
            "plugins/lenso.agent.loop/worker-a.toml",
            "model = \"fixture/readme-summary-v1\"\nmax_steps = 4\nmax_tool_calls = 4\nmax_parallel_tool_calls = 1\nmax_output_tokens = 128\n",
        ),
        (
            "plugins/lenso.agent.loop/worker-b.toml",
            "model = \"fixture/readme-summary-v1\"\nmax_steps = 4\nmax_tool_calls = 4\nmax_parallel_tool_calls = 1\nmax_output_tokens = 128\n",
        ),
        (
            "profiles/code.toml",
            concat!(
                "description = \"Supervised coding fixture\"\n",
                "instances = [\n",
                "  \"lenso.agent.workspace-instructions/default\",\n",
                "  \"lenso.agent.workspace-edit/default\",\n",
                "  \"lenso.agent.process.native/default\",\n",
                "  \"lenso.agent.process-tools/default\",\n",
                "  \"lenso.agent.git-tools/default\",\n",
                "  \"lenso.agent.code-mode-tools/default\",\n",
                "  \"lenso.agent.worktree-provider/default\",\n",
                "  \"lenso.agent.subagent-tools/worktree\",\n",
                "  \"lenso.agent.tools/worker-tools\",\n",
                "  \"lenso.agent.loop/worker-a\",\n",
                "  \"lenso.agent.loop/worker-b\",\n",
                "  \"lenso.agent.interactive-approval-hook/default\",\n",
                "  \"lenso.agent.prompt.static/coding\",\n",
                "  \"lenso.agent.model.fixture/default\",\n",
                "]\n",
            ),
        ),
    ];
    for (relative, content) in files {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

fn active_generation_digest(root: &std::path::Path) -> String {
    let connection = rusqlite::Connection::open_with_flags(
        root.join("runtime/.state/runtime.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let state_bytes = connection
        .query_row(
            "SELECT state_json FROM controller_states WHERE lineage = 'web'",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .unwrap();
    let state: serde_json::Value = serde_json::from_slice(&state_bytes).unwrap();
    state["active_generation_spec_digest"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn wait_for_generation_change(root: &std::path::Path, previous: &str) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = active_generation_digest(root);
            if current != previous {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the valid Plugin Root candidate should become the active Generation")
}

async fn verify_history_and_branch(
    client: &reqwest::Client,
    address: SocketAddr,
    session_id: &str,
    session: &serde_json::Value,
) {
    let original_revision = session["revision"].clone();
    let first_turn_id = session["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "turn_started")
        .and_then(|event| event["turn_id"].as_str())
        .unwrap()
        .to_owned();

    let listed = client
        .get(format!("http://{address}/api/console/v1/agent/sessions"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(listed["sessions"][0]["sessionId"], session_id);
    assert_eq!(listed["sessions"][0]["title"], "Answer directly: hello");
    assert_eq!(listed["sessions"][0]["latestPreview"], "Direct answer.");
    assert_eq!(listed["sessions"][0]["titleRevision"], "0");

    rename_session_and_assert(client, address, session_id).await;

    let edited = client
        .post(format!("http://{address}/api/console/v1/agent/turns"))
        .header("accept", "text/event-stream")
        .json(&serde_json::json!({
            "edit_turn_id": first_turn_id,
            "input": "Answer directly: edited",
            "request_id": "request-edit",
            "session_id": session_id,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    let branch_session_id = stream_session_id(&edited);
    assert_ne!(branch_session_id, session_id);

    let branch = client
        .get(format!(
            "http://{address}/api/console/v1/agent/sessions/{branch_session_id}"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let branch_inputs = branch["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "turn_started")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()["input"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(branch_inputs, ["Answer directly: edited"]);

    let original = client
        .get(format!(
            "http://{address}/api/console/v1/agent/sessions/{session_id}"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(original["revision"], original_revision);
}

async fn rename_session_and_assert(
    client: &reqwest::Client,
    address: SocketAddr,
    session_id: &str,
) {
    let renamed = client
        .patch(format!(
            "http://{address}/api/console/v1/agent/sessions/{session_id}"
        ))
        .json(&serde_json::json!({
            "title": "  My   renamed session  ",
            "expectedTitleRevision": "0",
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(renamed["title"], "My renamed session");
    assert_eq!(renamed["titleRevision"], "1");
    assert_eq!(
        client
            .patch(format!(
                "http://{address}/api/console/v1/agent/sessions/{session_id}"
            ))
            .json(&serde_json::json!({
                "title": "Stale title",
                "expectedTitleRevision": "0",
            }))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::CONFLICT
    );
    let renamed_list = client
        .get(format!("http://{address}/api/console/v1/agent/sessions"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(renamed_list["sessions"][0]["title"], "My renamed session");
    assert_eq!(renamed_list["sessions"][0]["titleRevision"], "1");
}

fn available_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    address
}

async fn wait_until_ready(client: &reqwest::Client, address: SocketAddr, child: &mut Child) {
    let url = format!("http://{address}/api/console/v1/agent/bootstrap");
    for _ in 0..100 {
        if let Some(status) = child.try_wait().unwrap() {
            let stderr = child
                .stderr
                .take()
                .map(|stderr| std::io::read_to_string(stderr).unwrap_or_default())
                .unwrap_or_default();
            panic!("Agent Web exited before readiness with {status}: {stderr}");
        }
        if client
            .get(&url)
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("Agent Web did not become ready at {url}");
}

fn stream_session_id(body: &str) -> String {
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|event| {
            (event["type"] == "turn_completed")
                .then(|| event["session_id"].as_str().map(ToOwned::to_owned))
                .flatten()
        })
        .expect("Turn stream should expose the durable Session ID")
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

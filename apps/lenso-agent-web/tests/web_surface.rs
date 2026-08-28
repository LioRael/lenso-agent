use std::{
    fs,
    net::{SocketAddr, TcpListener},
    process::{Child, Command, Stdio},
    time::Duration,
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
    let trajectory = client
        .get(format!(
            "http://{address}/api/console/v1/agent/sessions/{session_id}/trajectory"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
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
    verify_history_and_branch(&client, address, &session_id, &session).await;
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
            "max_tool_calls = 0\n",
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

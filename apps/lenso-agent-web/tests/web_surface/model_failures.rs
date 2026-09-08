use super::*;
use std::sync::atomic::AtomicUsize;
use tokio_tungstenite::tungstenite::{Message, accept};

#[tokio::test(flavor = "current_thread")]
async fn websocket_failures_preserve_generation_and_session_without_replaying() {
    let _server_test = WEB_SERVER_TEST.lock().await;
    let root = tempfile::tempdir().unwrap();
    let (provider, upgrades, creates) = failing_provider(root.path());
    let address = available_address();
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_lenso-agent-web"))
            .args(["--listen", &address.to_string()])
            .current_dir(root.path())
            .env("LENSO_AGENT_HOME", root.path())
            .env_remove("LENSO_AGENT_PROFILE")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    wait_until_ready(&client, address, &mut server.0).await;
    let base = format!("http://{address}/api/console/v1/agent");
    let mut session_id = None;
    for (index, reason) in [
        Some("websocket_connect_failed"),
        Some("websocket_stream_failed"),
        None,
    ]
    .iter()
    .enumerate()
    {
        let body = client.post(format!("{base}/turns"))
            .json(&serde_json::json!({"request_id":format!("attempt-{index}"),"session_id":session_id,"input":"Reply OK","allowed_tools":[]}))
            .send().await.unwrap().error_for_status().unwrap().text().await.unwrap();
        if let Some(reason) = reason {
            assert!(body.contains("turn.failed"), "{body}");
            assert!(body.contains(reason), "{body}");
            assert!(!body.contains("turn.cancelled"), "{body}");
        } else {
            assert!(body.contains("turn.completed"), "{body}");
        }
        assert!(
            client
                .get(format!("{base}/models"))
                .send()
                .await
                .unwrap()
                .status()
                .is_success(),
            "Model failure must not retire the active Generation"
        );
        let sessions: serde_json::Value = client
            .get(format!("{base}/sessions"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);
        session_id = Some(
            sessions["sessions"][0]["sessionId"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        assert_eq!(creates.load(Ordering::SeqCst), index);
        assert_eq!(upgrades.load(Ordering::SeqCst), index + 2);
    }
    let session: serde_json::Value = client
        .get(format!("{base}/sessions/{}", session_id.unwrap()))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let failures: Vec<serde_json::Value> = session["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "turn_failed")
        .map(|event| serde_json::from_str(event["payload_json"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(failures.len(), 2);
    assert_eq!(failures[0]["error"], "model_failure");
    assert_eq!(failures[0]["reason_code"], "websocket_connect_failed");
    assert_eq!(failures[1]["reason_code"], "websocket_stream_failed");
    assert!(!session.to_string().contains("catalog-fixture-access"));
    drop(server);
    drop(provider);
}

fn failing_provider(root: &Path) -> (CatalogServerGuard, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let provider_address = listener.local_addr().unwrap();
    write_direct_configuration(root, provider_address);
    let stopped = Arc::new(AtomicBool::new(false));
    let server_stopped = stopped.clone();
    let upgrades = Arc::new(AtomicUsize::new(0));
    let server_upgrades = upgrades.clone();
    let creates = Arc::new(AtomicUsize::new(0));
    let server_creates = creates.clone();
    let provider = CatalogServerGuard {
        address: provider_address,
        stopped,
        thread: Some(thread::spawn(move || {
            while !server_stopped.load(Ordering::Relaxed) {
                let (mut tcp, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                if server_stopped.load(Ordering::Relaxed) {
                    break;
                }
                tcp.set_nonblocking(false).unwrap();
                tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                // Peek only the request line; leave handshake bytes for Tungstenite.
                let mut prefix = [0; 32];
                let started = Instant::now();
                let count = loop {
                    let count = tcp.peek(&mut prefix).unwrap();
                    assert!(count > 0 && started.elapsed() < Duration::from_secs(5));
                    if count >= 18 {
                        break count;
                    }
                    thread::sleep(Duration::from_millis(1));
                };
                if prefix[..count].starts_with(b"GET /codex/models") {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        tcp.read_exact(&mut byte).unwrap();
                        request.push(byte[0]);
                    }
                    let body = direct_catalog_response();
                    write!(tcp, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
                    continue;
                }
                // Exhaust the Agent Loop's one safe pre-send retry on the first Turn.
                if server_upgrades.fetch_add(1, Ordering::SeqCst) < 2 {
                    continue;
                }
                let mut socket = accept(tcp).unwrap();
                socket.read().unwrap();
                let create = server_creates.fetch_add(1, Ordering::SeqCst);
                socket
                    .send(Message::Text(
                        r#"{"type":"response.output_text.delta","delta":"OK"}"#.into(),
                    ))
                    .unwrap();
                // The second Turn loses its socket after partial output. It must not replay.
                if create == 0 {
                    socket.close(None).unwrap();
                } else {
                    socket
                        .send(Message::Text(
                            r#"{"type":"response.completed","response":{"id":"recovered"}}"#.into(),
                        ))
                        .unwrap();
                }
            }
        })),
    };
    (provider, upgrades, creates)
}

#[tokio::test(flavor = "current_thread")]
async fn sse_failures_preserve_generation_and_session_without_replaying() {
    let _server_test = WEB_SERVER_TEST.lock().await;
    let root = tempfile::tempdir().unwrap();
    let (provider, creates) = failing_sse_provider(root.path());
    let address = available_address();
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_lenso-agent-web"))
            .args(["--listen", &address.to_string(), "--allow-tool", "read"])
            .current_dir(root.path())
            .env("LENSO_AGENT_HOME", root.path())
            .env_remove("LENSO_AGENT_PROFILE")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    wait_until_ready(&client, address, &mut server.0).await;
    let base = format!("http://{address}/api/console/v1/agent");
    let mut session_id = None;
    for (index, reason) in [
        Some("sse_stream_failed"),
        Some("sse_protocol_error"),
        Some("sse_incomplete_response"),
        None,
    ]
    .iter()
    .enumerate()
    {
        let body = client.post(format!("{base}/turns"))
            .json(&serde_json::json!({"request_id":format!("attempt-{index}"),"session_id":session_id,"input":if index == 0 { "Remember target Agent app and Plugin lenso.agent.artifact.file" } else { "Continue" },"allowed_tools":["read"]}))
            .send().await.unwrap().error_for_status().unwrap().text().await.unwrap();
        if let Some(reason) = reason {
            assert!(body.contains("turn.failed"), "{body}");
            assert!(body.contains(reason), "{body}");
            assert!(!body.contains("turn.cancelled"), "{body}");
        } else {
            assert!(body.contains("turn.completed"), "{body}");
        }
        assert!(
            client
                .get(format!("{base}/models"))
                .send()
                .await
                .unwrap()
                .status()
                .is_success(),
            "Model failure must not retire the active Generation"
        );
        let sessions: serde_json::Value = client
            .get(format!("{base}/sessions"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);
        session_id = Some(
            sessions["sessions"][0]["sessionId"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        assert_eq!(
            creates.load(Ordering::SeqCst),
            index + 1 + usize::from(reason.is_none())
        );
        if index == 0 {
            server.0.kill().unwrap();
            server.0.wait().unwrap();
            server = ChildGuard(
                Command::new(env!("CARGO_BIN_EXE_lenso-agent-web"))
                    .args(["--listen", &address.to_string(), "--allow-tool", "read"])
                    .current_dir(root.path())
                    .env("LENSO_AGENT_HOME", root.path())
                    .env_remove("LENSO_AGENT_PROFILE")
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            wait_until_ready(&client, address, &mut server.0).await;
        }
    }
    let session: serde_json::Value = client
        .get(format!("{base}/sessions/{}", session_id.unwrap()))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let failures: Vec<serde_json::Value> = session["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "turn_failed")
        .map(|event| serde_json::from_str(event["payload_json"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(failures.len(), 3);
    assert_eq!(failures[0]["error"], "model_failure");
    assert_eq!(failures[0]["reason_code"], "sse_stream_failed");
    assert_eq!(failures[1]["reason_code"], "sse_protocol_error");
    assert!(!session.to_string().contains("catalog-fixture-access"));
    drop(server);
    drop(provider);
}

fn failing_sse_provider(root: &Path) -> (CatalogServerGuard, Arc<AtomicUsize>) {
    scripted_sse_provider(root, false)
}

fn scripted_sse_provider(
    root: &Path,
    tool_domain_failure: bool,
) -> (CatalogServerGuard, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    write_direct_configuration(root, address);
    let config = root.join("plugins/lenso.agent.model.openai-codex-direct/model.toml");
    fs::write(
        &config,
        format!(
            "{}\ntransport = \"sse\"\n",
            fs::read_to_string(&config).unwrap()
        ),
    )
    .unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = stopped.clone();
    let creates = Arc::new(AtomicUsize::new(0));
    let requests = creates.clone();
    let provider = CatalogServerGuard {
        address,
        stopped,
        thread: Some(thread::spawn(move || {
            'requests: while !stop.load(Ordering::Relaxed) {
                let (mut tcp, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                tcp.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if let Err(error) = tcp.read_exact(&mut byte) {
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::UnexpectedEof
                        ) {
                            continue 'requests;
                        }
                        panic!("{error}");
                    }
                    header.push(byte[0]);
                }
                let header = String::from_utf8(header).unwrap();
                if header.starts_with("GET /codex/models") {
                    let body = direct_catalog_response();
                    write!(tcp, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
                    continue;
                }
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse().unwrap())
                    })
                    .unwrap();
                let mut request_body = vec![0; length];
                tcp.read_exact(&mut request_body).unwrap();
                let index = requests.fetch_add(1, Ordering::SeqCst);
                let body = scripted_sse_body(index, tool_domain_failure, request_body);
                // A short HTTP body reproduces a transport interruption after output.
                let length = body.len()
                    + if !tool_domain_failure && index == 0 {
                        100
                    } else {
                        0
                    };
                write!(tcp, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {length}\r\nconnection: close\r\n\r\n{body}").unwrap();
                if !tool_domain_failure && index == 0 {
                    tcp.flush().unwrap();
                    // Let the client consume headers and output before truncating the stream.
                    thread::sleep(Duration::from_millis(100));
                }
            }
        })),
    };
    (provider, creates)
}

fn scripted_sse_body(index: usize, tool_domain_failure: bool, request_body: Vec<u8>) -> String {
    if !tool_domain_failure && index > 0 {
        let request: serde_json::Value = serde_json::from_slice(&request_body).unwrap();
        let context = request["input"].to_string();
        assert!(
            context.contains("Remember target Agent app and Plugin lenso.agent.artifact.file"),
            "The actual model request lost the failed Turn's user input: {context}"
        );
        assert!(
            context.contains("did not complete"),
            "Incomplete work must be identified: {context}"
        );
    }

    use std::fmt::Write as _;
    let mut body =
        String::from("data: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\n");
    if tool_domain_failure {
        if index == 0 {
            for (id, name, arguments) in [
                ("listed", "list", r#"{"path":"."}"#),
                ("git-failed", "git_status", "{}"),
            ] {
                write!(
                    body,
                    "data: {}\n\n",
                    serde_json::json!({
                        "type":"response.output_item.done", "item": {
                            "type":"function_call", "call_id":id, "name":name, "arguments":arguments
                        }
                    })
                )
                .unwrap();
            }
        } else if index == 1 {
            let request = String::from_utf8(request_body).unwrap();
            assert!(
                request.contains("git_failed"),
                "Missing Tool error feedback"
            );
            assert!(
                request.contains("not a git repository"),
                "Missing Git failure detail"
            );
        }
        body.push_str(
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"recovered\"}}\n\n",
        );
    } else {
        if index == 1 {
            body.push_str("data: {invalid json}\n\n");
        }
        if index == 3 {
            body.push_str(r#"data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"invalid-read","name":"read","arguments":"{}"}}"#);
            body.push_str("\n\n");
        }
        if index == 4 {
            assert!(
                String::from_utf8(request_body)
                    .unwrap()
                    .contains("InvalidArguments")
            );
        }
        if index >= 3 {
            body.push_str(
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"recovered\"}}\n\n",
            );
        }
    }
    body
}

#[tokio::test(flavor = "current_thread")]
async fn git_domain_failure_preserves_generation_and_next_turn() {
    let _server_test = WEB_SERVER_TEST.lock().await;
    let root = tempfile::tempdir().unwrap();
    let (provider, creates) = scripted_sse_provider(root.path(), true);
    for (path, content) in [
        (
            "plugins/lenso.agent.process.native/default.toml",
            "root = \".\"\nallowed_programs = [\"git\"]\nprogram_presets = []\nenvironment_allowlist = [\"PATH\", \"HOME\", \"TMPDIR\"]\n",
        ),
        (
            "plugins/lenso.agent.git-tools/default.toml",
            "default_timeout_ms = 30000\n",
        ),
        (
            "profiles/git-test.toml",
            "description = \"Git domain failure regression\"\ninclude_enabled = true\ninstances = [\"lenso.agent.process.native/default\", \"lenso.agent.git-tools/default\"]\n",
        ),
    ] {
        let path = root.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    assert!(!root.path().join(".git").exists());
    let address = available_address();
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_lenso-agent-web"))
            .args([
                "--listen",
                &address.to_string(),
                "--profile",
                "git-test",
                "--allow-tool",
                "list",
                "--allow-tool",
                "git_status",
            ])
            .current_dir(root.path())
            .env("LENSO_AGENT_HOME", root.path())
            .env_remove("LENSO_AGENT_PROFILE")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    wait_until_ready(&client, address, &mut server.0).await;
    let base = format!("http://{address}/api/console/v1/agent");
    let generation = active_generation_digest(root.path());
    let mut session_id = None;
    for index in 0..2 {
        let body = client.post(format!("{base}/turns"))
            .json(&serde_json::json!({"request_id":format!("git-attempt-{index}"),"session_id":session_id,"input":"Inspect the workspace","allowed_tools":["list","git_status"]}))
            .send().await.unwrap().error_for_status().unwrap().text().await.unwrap();
        let sessions = client.get(format!("{base}/sessions")).send().await.unwrap();
        assert!(
            sessions.status().is_success(),
            "Tool domain failure must not retire the active Generation: {body}"
        );
        assert!(body.contains("turn.completed"), "{body}");
        assert_eq!(active_generation_digest(root.path()), generation);
        assert!(!body.contains("turn.cancelled"), "{body}");
        if index == 0 {
            assert!(body.contains("git_failed"), "{body}");
        }
        let sessions: serde_json::Value = sessions.json().await.unwrap();
        assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);
        session_id = Some(
            sessions["sessions"][0]["sessionId"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    assert_eq!(
        creates.load(Ordering::SeqCst),
        3,
        "Failed Tool is not automatically replayed"
    );
    drop(server);
    drop(provider);
}

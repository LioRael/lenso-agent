use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use lenso_agent_cli_plugin as _;

#[path = "../../../tests/support/mod.rs"]
mod support;

#[derive(Debug)]
struct CapturedRequest {
    headers: String,
    body: serde_json::Value,
}

fn canonical_plan_path(home: &Path) -> PathBuf {
    support::plan_for_home("openai-codex-direct", home)
}

fn test_plan(root: &Path, base_url: &str, credential_file: &Path) -> PathBuf {
    let mut plan = serde_json::from_slice::<serde_json::Value>(
        &fs::read(canonical_plan_path(root)).expect("read canonical direct Codex Plan"),
    )
    .expect("decode canonical direct Codex Plan");
    let plugins = plan["plugin_instances"]
        .as_array_mut()
        .expect("Plan plugin_instances");
    let model = plugins
        .iter_mut()
        .find(|plugin| plugin["instance_key"] == "lenso.agent.model.openai-codex-direct/model")
        .expect("direct Codex Model Instance");
    update_configuration(model, |configuration| {
        configuration["base_url"] = serde_json::Value::String(base_url.to_owned());
    });
    let auth = plugins
        .iter_mut()
        .find(|plugin| plugin["instance_key"] == "lenso.agent.auth.openai-codex/auth")
        .expect("direct Codex Auth Instance");
    update_configuration(auth, |configuration| {
        configuration["credential_file"] =
            serde_json::Value::String(credential_file.display().to_string());
    });
    let path = root.join("openai-codex-direct-test-plan.json");
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    path
}

fn update_configuration(
    plugin: &mut serde_json::Value,
    update: impl FnOnce(&mut serde_json::Value),
) {
    let mut configuration = serde_json::from_str::<serde_json::Value>(
        plugin["configuration"]
            .as_str()
            .expect("Plugin configuration bytes"),
    )
    .expect("decode Plugin configuration");
    update(&mut configuration);
    plugin["configuration"] = serde_json::Value::String(configuration.to_string());
}

fn write_credential(path: &Path) {
    fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "openai-codex": {
                "type": "oauth",
                "access": "direct-access-secret",
                "refresh": "direct-refresh-secret",
                "accountId": "account-test-1",
                "expires": u64::MAX
            }
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn direct_model_uses_private_auth_and_resumes_after_a_tool_call() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Direct Fixture\n").unwrap();
    let credential = temporary.path().join("credential.json");
    write_credential(&credential);
    let (base_url, server) = spawn_model_server();
    let plan = test_plan(temporary.path(), &base_url, &credential);
    let output = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .env("LENSO_AGENT_HOME", temporary.path())
        .args(["--plan", plan.to_str().unwrap()])
        .args(["--prompt", "Summarize the README."])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "README summary: # Direct Fixture\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("session: "));

    let requests = server.join().expect("fake direct provider should finish");
    assert_eq!(requests.len(), 2);
    for request in &requests {
        let headers = request.headers.to_ascii_lowercase();
        assert!(headers.contains("authorization: bearer direct-access-secret"));
        assert!(headers.contains("chatgpt-account-id: account-test-1"));
        assert!(headers.contains("originator: lenso"));
        assert!(!request.headers.contains("direct-refresh-secret"));
    }
    assert_eq!(requests[0].body["model"], "gpt-5.6-luna");
    assert_eq!(requests[0].body["reasoning"]["effort"], "medium");
    assert!(requests[0].body.get("temperature").is_none());
    assert!(requests[0].body.get("max_output_tokens").is_none());
    assert_eq!(requests[0].body["store"], false);
    assert!(
        requests[0].body["instructions"]
            .as_str()
            .unwrap()
            .contains("Be concise")
    );
    assert_eq!(
        requests[0].body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "ask_user",
            "list",
            "read",
            "search",
            "skill",
            "skill_list",
            "skill_resource",
            "skill_resources",
        ]
    );
    assert_eq!(requests[1].body["input"][0]["type"], "message");
    assert_eq!(
        requests[1].body["input"][0]["content"][0]["text"],
        "Summarize the README."
    );
    assert_eq!(requests[1].body["input"][1]["type"], "function_call");
    assert_eq!(requests[1].body["input"][2]["type"], "function_call_output");
}

#[test]
fn missing_direct_credential_rejects_generation_readiness_without_starting_http() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let missing = temporary.path().join("missing-credential.json");
    let plan = test_plan(temporary.path(), "http://127.0.0.1:1", &missing);
    let output = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .env("LENSO_AGENT_HOME", temporary.path())
        .args(["--plan", plan.to_str().unwrap()])
        .args(["--prompt", "Summarize the README."])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("direct Codex model catalog authentication failed"),
        "{stderr}"
    );
    assert!(!stderr.contains("direct-access-secret"));
    assert!(!stderr.contains("direct-refresh-secret"));
}

fn spawn_model_server() -> (String, thread::JoinHandle<Vec<CapturedRequest>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let (headers, body) = read_request_parts(&mut stream);
        assert!(headers.starts_with("GET /codex/models?client_version=99.99.99 HTTP/1.1"));
        assert!(body.is_empty());
        let lower = headers.to_ascii_lowercase();
        assert!(lower.contains("authorization: bearer direct-access-secret"));
        assert!(lower.contains("chatgpt-account-id: account-test-1"));
        write_json_response(&mut stream, model_catalog_response().as_bytes());
        for response_body in [tool_call_response(), text_response()] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let (headers, body) = read_request(&mut stream);
            requests.push(CapturedRequest {
                headers,
                body: serde_json::from_slice(&body).unwrap(),
            });
            write_response(&mut stream, response_body.as_bytes());
        }
        requests
    });
    (format!("http://{address}"), server)
}

fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let (headers, body) = read_request_parts(stream);
    assert!(headers.starts_with("POST /codex/responses HTTP/1.1"));
    (headers, body)
}

fn read_request_parts(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut received = Vec::new();
    let header_end = loop {
        let mut buffer = [0_u8; 4096];
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "provider request ended before headers");
        received.extend_from_slice(&buffer[..read]);
        if let Some(position) = received.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = String::from_utf8(received[..header_end].to_vec()).unwrap();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while received.len() - header_end < content_length {
        let mut buffer = [0_u8; 4096];
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "provider request ended before body");
        received.extend_from_slice(&buffer[..read]);
    }
    (
        headers,
        received[header_end..header_end + content_length].to_vec(),
    )
}

fn write_json_response(stream: &mut TcpStream, body: &[u8]) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\netag: \"catalog-v1\"\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
}

fn write_response(stream: &mut TcpStream, body: &[u8]) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
}

fn tool_call_response() -> String {
    concat!(
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call-readme-1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":20,\"output_tokens\":6}}}\n\n"
    )
    .to_owned()
}

fn model_catalog_response() -> String {
    serde_json::json!({
        "models": [{
            "slug": "gpt-5.6-luna",
            "display_name": "GPT-5.6 Luna",
            "description": "Fast coding model",
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [
                {"effort": "low", "description": "Light reasoning"},
                {"effort": "medium", "description": "Balanced reasoning"},
                {"effort": "high", "description": "Deep reasoning"}
            ],
            "visibility": "list",
            "additional_speed_tiers": ["fast"],
            "service_tiers": [],
            "default_service_tier": null,
            "supports_parallel_tool_calls": true,
            "context_window": 272_000,
            "effective_context_window_percent": 95,
            "input_modalities": ["text", "image"]
        }]
    })
    .to_string()
}

fn text_response() -> String {
    concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"README summary: \"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"# Direct Fixture\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":24,\"output_tokens\":8}}}\n\n"
    )
    .to_owned()
}

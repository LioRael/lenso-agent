use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

fn canonical_plan_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../composition/openai-readonly/resolved-plan.json")
}

fn test_plan(root: &Path, base_url: &str) -> PathBuf {
    let mut plan = serde_json::from_slice::<serde_json::Value>(
        &fs::read(canonical_plan_path()).expect("read canonical OpenAI Plan"),
    )
    .expect("decode canonical OpenAI Plan");
    let model = plan["module_instances"]
        .as_array_mut()
        .expect("Plan module_instances")
        .iter_mut()
        .find(|module| module["instance_key"] == "model")
        .expect("OpenAI Model Instance");
    let mut configuration = serde_json::from_str::<serde_json::Value>(
        model["configuration"]
            .as_str()
            .expect("Model configuration bytes"),
    )
    .expect("decode Model configuration");
    configuration["base_url"] = serde_json::Value::String(base_url.to_owned());
    model["configuration"] = serde_json::Value::String(configuration.to_string());
    let path = root.join("openai-test-plan.json");
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    path
}

#[test]
fn openai_model_streams_tool_call_and_resumes_through_real_http() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# OpenAI Fixture\n").unwrap();
    let (base_url, server) = spawn_model_server();
    let plan = test_plan(temporary.path(), &base_url);
    let output = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .env("OPENAI_API_KEY", "integration-secret")
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
        "README summary: # OpenAI Fixture\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("session: "));

    let requests = server.join().expect("fake provider should finish");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "gpt-4o-mini");
    assert_eq!(
        requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["workspace.list", "workspace.read_text", "workspace.search"]
    );
    assert_eq!(
        requests[1]["messages"][2]["tool_calls"][0]["function"]["name"],
        "workspace.read_text"
    );
    assert_eq!(requests[1]["messages"][0]["role"], "system");
    assert!(
        requests[1]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Be concise")
    );
    assert_eq!(requests[1]["messages"][1]["role"], "user");
    assert_eq!(requests[1]["messages"][3]["role"], "tool");
}

#[test]
fn missing_openai_credential_rejects_app_startup() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let plan = test_plan(temporary.path(), "http://127.0.0.1:1/v1");
    let output = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .env_remove("OPENAI_API_KEY")
        .args(["--plan", plan.to_str().unwrap()])
        .args(["--prompt", "Summarize the README."])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("App startup failed"));
    assert!(stderr.contains("model/openai-api-key"));
    assert!(!stderr.contains("integration-secret"));
}

fn spawn_model_server() -> (String, thread::JoinHandle<Vec<serde_json::Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for response_body in [tool_call_response(), text_response()] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let (headers, body) = read_request(&mut stream);
            assert!(headers.contains("authorization: Bearer integration-secret"));
            requests.push(serde_json::from_slice(&body).unwrap());
            write_response(&mut stream, response_body.as_bytes());
        }
        requests
    });
    (format!("http://{address}/v1"), server)
}

fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
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
        .expect("request Content-Length");
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
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-readme-1\",\"function\":{\"name\":\"workspace.read_text\",\"arguments\":\"{\\\"path\\\":\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"README.md\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":6}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_owned()
}

fn text_response() -> String {
    concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"README summary: \"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"# OpenAI Fixture\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":24,\"completion_tokens\":8}}\n\n",
        "data: [DONE]\n\n"
    )
    .to_owned()
}

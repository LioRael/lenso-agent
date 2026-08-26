use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    thread,
    time::Duration,
};

mod support;

#[test]
fn telegram_updates_run_real_agent_turns_and_resume_one_durable_session() {
    let temporary = tempfile::tempdir().unwrap();
    let state = temporary.path().join("telegram-state.json");
    let plan = support::plan("base");

    let (api_base, server) = spawn_telegram_server(11, 7, "Answer directly: hello");
    let first = Command::new(env!("CARGO_BIN_EXE_lenso-agent-telegram"))
        .current_dir(temporary.path())
        .env("TELEGRAM_BOT_TOKEN", "integration-secret-token")
        .env("LENSO_TELEGRAM_API_BASE", api_base)
        .args(["--plan", plan.to_str().unwrap()])
        .args(["--allow-chat", "100"])
        .args(["--state", state.to_str().unwrap()])
        .args(["--poll-timeout-seconds", "1"])
        .args(["--max-updates", "1"])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(!String::from_utf8_lossy(&first.stderr).contains("integration-secret-token"));
    let first_requests = server.join().expect("fake Telegram server should finish");
    assert_telegram_exchange(&first_requests, 7, &first.stderr);
    assert_eq!(first_requests[1].1["offset"], serde_json::Value::Null);

    let (api_base, server) = spawn_telegram_server(12, 8, "Answer directly: again");
    let second = Command::new(env!("CARGO_BIN_EXE_lenso-agent-telegram"))
        .current_dir(temporary.path())
        .env("TELEGRAM_BOT_TOKEN", "integration-secret-token")
        .env("LENSO_TELEGRAM_API_BASE", api_base)
        .args(["--plan", plan.to_str().unwrap()])
        .args(["--allow-chat", "100"])
        .args(["--state", state.to_str().unwrap()])
        .args(["--poll-timeout-seconds", "1"])
        .args(["--max-updates", "1"])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(!String::from_utf8_lossy(&second.stderr).contains("integration-secret-token"));
    let second_requests = server.join().expect("fake Telegram server should finish");
    assert_telegram_exchange(&second_requests, 8, &second.stderr);
    assert_eq!(second_requests[1].1["offset"], 12);

    let cursor: serde_json::Value =
        serde_json::from_slice(&fs::read(state).expect("Telegram cursor should persist")).unwrap();
    assert_eq!(cursor["schema_version"], 1);
    assert_eq!(cursor["next_update_id"], 13);
    assert!(
        cursor["sessions"]["telegram_42_100"]
            .as_str()
            .is_some_and(|session_id| !session_id.is_empty())
    );
    let session_path = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let session: serde_json::Value =
        serde_json::from_slice(&fs::read(session_path).unwrap()).unwrap();
    assert_eq!(
        session["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "turn_started")
            .count(),
        2
    );
}

fn assert_telegram_exchange(
    requests: &[(String, serde_json::Value)],
    message_id: i64,
    stderr: &[u8],
) {
    assert_eq!(requests.len(), 4);
    assert!(requests[0].0.ends_with("/getMe"));
    assert!(requests[1].0.ends_with("/getUpdates"));
    assert!(requests[2].0.ends_with("/sendChatAction"));
    assert!(requests[3].0.ends_with("/sendMessage"));
    assert_eq!(requests[3].1["chat_id"], 100);
    assert_eq!(requests[3].1["reply_parameters"]["message_id"], message_id);
    assert_eq!(
        requests[3].1["text"],
        "Plugin: Direct answer.",
        "{}",
        String::from_utf8_lossy(stderr)
    );
}

fn spawn_telegram_server(
    update_id: i64,
    message_id: i64,
    text: &'static str,
) -> (String, thread::JoinHandle<Vec<(String, serde_json::Value)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let responses = [
            serde_json::json!({
                "ok": true,
                "result": {"id": 42, "is_bot": true, "username": "lenso_bot"}
            }),
            serde_json::json!({
                "ok": true,
                "result": [{
                    "update_id": update_id,
                    "message": {
                        "message_id": message_id,
                        "from": {"id": 9, "is_bot": false},
                        "chat": {"id": 100, "type": "private"},
                        "text": text
                    }
                }]
            }),
            serde_json::json!({"ok": true, "result": true}),
            serde_json::json!({"ok": true, "result": {}}),
        ];
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            requests.push(read_request(&mut stream));
            write_response(&mut stream, &response);
        }
        requests
    });
    (format!("http://{address}"), server)
}

fn read_request(stream: &mut TcpStream) -> (String, serde_json::Value) {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).unwrap();
        assert_ne!(read, 0, "request ended before its headers");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break offset + 4;
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .map(str::parse::<usize>)
        })
        .transpose()
        .unwrap()
        .unwrap_or(0);
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut buffer).unwrap();
        assert_ne!(read, 0, "request ended before its body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    let path = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let body = serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap();
    (path, body)
}

fn write_response(stream: &mut TcpStream, value: &serde_json::Value) {
    let body = serde_json::to_vec(value).unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
    stream.flush().unwrap();
}

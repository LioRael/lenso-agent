use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    thread,
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use lenso_agent_discord_plugin as _;
use lenso_agent_session_inspection::SessionInspector;
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[path = "../../../tests/support/mod.rs"]
mod support;

#[test]
fn discord_messages_run_real_agent_turns_and_resume_gateway_and_session() {
    let temporary = tempfile::tempdir().unwrap();
    let state = temporary.path().join("discord-state.json");
    let plan = support::plan_for_home("base", temporary.path());
    let (gateway_url, gateway) = spawn_discord_gateway();
    let (api_base, api) = spawn_discord_api();

    let first = run_discord(temporary.path(), &plan, &state, &gateway_url, &api_base);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(!String::from_utf8_lossy(&first.stderr).contains("integration-secret-token"));

    let second = run_discord(temporary.path(), &plan, &state, &gateway_url, &api_base);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(!String::from_utf8_lossy(&second.stderr).contains("integration-secret-token"));

    let auth_payloads = gateway.join().expect("fake Discord Gateway should finish");
    assert_eq!(auth_payloads[0]["op"], 2);
    assert_eq!(auth_payloads[0]["d"]["intents"], 4_609);
    assert_eq!(auth_payloads[1]["op"], 6);
    assert_eq!(auth_payloads[1]["d"]["session_id"], "gateway-session");
    assert_eq!(auth_payloads[1]["d"]["seq"], 2);

    let requests = api.join().expect("fake Discord API should finish");
    assert_eq!(requests.len(), 4);
    for exchange in requests.as_chunks::<2>().0 {
        assert!(exchange[0].0.ends_with("/channels/100/typing"));
        assert!(exchange[1].0.ends_with("/channels/100/messages"));
        assert_eq!(exchange[1].1["content"], "Direct answer.");
        assert_eq!(
            exchange[1].1["allowed_mentions"]["parse"],
            serde_json::json!([])
        );
        assert_eq!(exchange[1].1["allowed_mentions"]["replied_user"], false);
    }

    let durable: serde_json::Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert_eq!(durable["schema_version"], 1);
    assert_eq!(durable["gateway"]["sequence"], 4);
    assert!(
        durable["sessions"]["discord_42_100"]
            .as_str()
            .is_some_and(|session_id| !session_id.is_empty())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(state.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    let sessions = lenso_agent_session_sqlite_plugin::SqliteSessionInspector::new(
        temporary.path().join("sessions.sqlite3"),
    )
    .inspect_all()
    .unwrap();
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];
    assert_eq!(
        session
            .events
            .iter()
            .filter(|event| event.kind == "turn_started")
            .count(),
        2
    );
    assert!(
        session
            .events
            .iter()
            .filter(|event| event.kind == "turn_started")
            .all(|event| {
                serde_json::from_str::<serde_json::Value>(&event.payload_json).unwrap()
            ["agent_behavior_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:"))
            })
    );
}

fn run_discord(
    directory: &std::path::Path,
    plan: &std::path::Path,
    state: &std::path::Path,
    gateway_url: &str,
    api_base: &str,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lenso-agent-discord"))
        .current_dir(directory)
        .env("LENSO_AGENT_HOME", directory)
        .env("DISCORD_BOT_TOKEN", "integration-secret-token")
        .env("LENSO_DISCORD_GATEWAY_URL", gateway_url)
        .env("LENSO_DISCORD_API_BASE", api_base)
        .args(["--plan", plan.to_str().unwrap()])
        .args(["--allow-channel", "100"])
        .args(["--state", state.to_str().unwrap()])
        .args(["--max-messages", "1"])
        .output()
        .unwrap()
}

fn spawn_discord_gateway() -> (String, thread::JoinHandle<Vec<serde_json::Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let gateway_url = format!("ws://{address}");
    let resume_url = gateway_url.clone();
    let server = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let mut auth_payloads = Vec::new();
                for attempt in 0..2_u64 {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut gateway = accept_async(stream).await.unwrap();
                    gateway
                        .send(json_message(&serde_json::json!({
                            "op": 10,
                            "d": {"heartbeat_interval": 60_000}
                        })))
                        .await
                        .unwrap();
                    let auth = gateway.next().await.unwrap().unwrap();
                    auth_payloads.push(parse_message(auth));
                    if attempt == 0 {
                        gateway
                            .send(json_message(&serde_json::json!({
                                "op": 0,
                                "s": 1,
                                "t": "READY",
                                "d": {
                                    "session_id": "gateway-session",
                                    "resume_gateway_url": resume_url,
                                    "user": {"id": "42", "bot": true}
                                }
                            })))
                            .await
                            .unwrap();
                    } else {
                        gateway
                            .send(json_message(&serde_json::json!({
                                "op": 0,
                                "s": 3,
                                "t": "RESUMED",
                                "d": {}
                            })))
                            .await
                            .unwrap();
                    }
                    gateway
                        .send(json_message(&serde_json::json!({
                            "op": 0,
                            "s": if attempt == 0 { 2 } else { 4 },
                            "t": "MESSAGE_CREATE",
                            "d": {
                                "id": if attempt == 0 { "500" } else { "501" },
                                "channel_id": "100",
                                "author": {"id": "9", "bot": false},
                                "content": if attempt == 0 {
                                    "Answer directly: hello"
                                } else {
                                    "Answer directly: again"
                                }
                            }
                        })))
                        .await
                        .unwrap();
                    while let Some(Ok(message)) = gateway.next().await {
                        if message.is_close() {
                            break;
                        }
                    }
                }
                auth_payloads
            })
    });
    (gateway_url, server)
}

fn json_message(value: &serde_json::Value) -> Message {
    Message::Text(value.to_string().into())
}

fn parse_message(message: Message) -> serde_json::Value {
    serde_json::from_str(message.into_text().unwrap().as_ref()).unwrap()
}

fn spawn_discord_api() -> (String, thread::JoinHandle<Vec<(String, serde_json::Value)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            requests.push(read_request(&mut stream));
            write_response(&mut stream);
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
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("authorization: bot integration-secret-token")
    );
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
    let body = if content_length == 0 {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap()
    };
    (path, body)
}

fn write_response(stream: &mut TcpStream) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    )
    .unwrap();
    stream.flush().unwrap();
}

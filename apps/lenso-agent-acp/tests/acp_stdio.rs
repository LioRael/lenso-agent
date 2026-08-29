use std::{fs, path::Path, process::Stdio, time::Duration};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
};

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end transcript keeps ACP lifecycle and persisted provenance proof together"
)]
async fn acp_stdio_preserves_generation_tool_and_approval_policy() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("README.md"), "# ACP Fixture\n").unwrap();
    write_fixture(home.path());

    let mut child = Command::new(env!("CARGO_BIN_EXE_lenso-agent-acp"))
        .args(["--profile", "acp-test"])
        .current_dir(workspace.path())
        .env("LENSO_AGENT_HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();

    send(
        &mut input,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": 1, "clientCapabilities": {}}
        }),
    )
    .await;
    let initialized = next_message(&mut output).await;
    assert_eq!(initialized["id"], 1);
    assert_eq!(initialized["result"]["protocolVersion"], 1);

    send(
        &mut input,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {
                "cwd": workspace.path(),
                "mcpServers": []
            }
        }),
    )
    .await;
    let opened = next_message(&mut output).await;
    let session_id = opened["result"]["sessionId"].as_str().unwrap().to_owned();

    send(
        &mut input,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{
                    "type": "text",
                    "text": "Create one approved workspace note."
                }]
            }
        }),
    )
    .await;

    let mut saw_tool = false;
    let mut saw_text = false;
    loop {
        let message = next_message(&mut output).await;
        if message["method"] == "session/request_permission" {
            let option = message["params"]["options"]
                .as_array()
                .unwrap()
                .iter()
                .find(|option| option["kind"] == "allow_once")
                .expect("approval must expose an allow-once choice");
            send(
                &mut input,
                json!({
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": {
                        "outcome": {
                            "outcome": "selected",
                            "optionId": option["optionId"]
                        }
                    }
                }),
            )
            .await;
        } else if message["method"] == "session/update" {
            let update = &message["params"]["update"];
            saw_tool |= matches!(
                update["sessionUpdate"].as_str(),
                Some("tool_call" | "tool_call_update")
            );
            saw_text |= update["sessionUpdate"] == "agent_message_chunk"
                && update["content"]["text"] == "Approved workspace note created";
        } else if message["id"] == 3 {
            assert_eq!(message["result"]["stopReason"], "end_turn");
            break;
        }
    }

    assert!(saw_tool, "ACP must project typed Tool progress");
    assert!(saw_text, "ACP must stream the Agent response");
    assert_eq!(
        fs::read_to_string(workspace.path().join("approved-note.txt")).unwrap(),
        "approved\n"
    );

    drop(input);
    let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("ACP process should stop after stdin closes")
        .unwrap();
    assert!(status.success());

    let database = rusqlite::Connection::open(home.path().join("sessions.sqlite3")).unwrap();
    let payload: String = database
        .query_row(
            "SELECT payload_json FROM events WHERE session_id = ?1 AND kind = 'turn_started'",
            [&session_id],
            |row| row.get(0),
        )
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert!(
        payload["generation_spec_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "ACP Turn must retain immutable Generation provenance"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn acp_cancel_stops_the_exact_active_turn() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write_fixture(home.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_lenso-agent-acp"))
        .args(["--profile", "acp-test"])
        .current_dir(workspace.path())
        .env("LENSO_AGENT_HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();

    send(
        &mut input,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": 1, "clientCapabilities": {}}
        }),
    )
    .await;
    assert_eq!(next_message(&mut output).await["id"], 1);
    send(
        &mut input,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": {"cwd": workspace.path(), "mcpServers": []}
        }),
    )
    .await;
    let opened = next_message(&mut output).await;
    let session_id = opened["result"]["sessionId"].as_str().unwrap();
    send(
        &mut input,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": "Remain pending until cancelled."}]
            }
        }),
    )
    .await;
    send(
        &mut input,
        json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {"sessionId": session_id}
        }),
    )
    .await;

    loop {
        let message = next_message(&mut output).await;
        if message["id"] == 3 {
            assert_eq!(message["result"]["stopReason"], "cancelled");
            break;
        }
    }

    drop(input);
    let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("ACP process should stop after stdin closes")
        .unwrap();
    assert!(status.success());
}

async fn send(input: &mut ChildStdin, message: Value) {
    input
        .write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
    input.flush().await.unwrap();
}

async fn next_message(
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) -> Value {
    let line = tokio::time::timeout(Duration::from_secs(20), output.next_line())
        .await
        .expect("ACP response timed out")
        .unwrap()
        .expect("ACP stdout closed unexpectedly");
    serde_json::from_str(&line).unwrap()
}

fn write_fixture(home: &Path) {
    let files = [
        (
            "plugins/lenso.agent.model.fixture/acp.toml",
            "model = \"fixture/readme-summary-v1\"\n",
        ),
        (
            "plugins/lenso.agent.loop/acp.toml",
            concat!(
                "model = \"fixture/readme-summary-v1\"\n",
                "max_steps = 3\n",
                "max_tool_calls = 3\n",
                "max_user_resumes = 1\n",
                "max_parallel_tool_calls = 1\n",
                "max_output_tokens = 128\n",
                "max_history_events = 100\n",
                "max_compaction_summary_characters = 8192\n",
                "max_memory_items = 8\n",
                "max_memory_characters = 16384\n",
            ),
        ),
        (
            "plugins/lenso.agent.workspace-edit/default.toml",
            concat!(
                "root = \".\"\n",
                "max_file_bytes = 1048576\n",
                "max_edit_bytes = 131072\n",
                "require_checkpoint = false\n",
            ),
        ),
        (
            "plugins/lenso.agent.interactive-approval-hook/default.toml",
            concat!(
                "default_decision = \"ask\"\n",
                "allow_tools = [\"read_text\"]\n",
                "ask_tools = []\n",
                "deny_tools = []\n",
                "max_preview_bytes = 16384\n",
            ),
        ),
        (
            "profiles/acp-test.toml",
            concat!(
                "description = \"ACP fixture\"\n",
                "agent = \"lenso.agent.loop/acp\"\n",
                "instances = [\n",
                "  \"lenso.agent.loop/acp\",\n",
                "  \"lenso.agent.model.fixture/acp\",\n",
                "  \"lenso.agent.workspace-edit/default\",\n",
                "  \"lenso.agent.interactive-approval-hook/default\",\n",
                "]\n",
            ),
        ),
    ];
    for (relative, content) in files {
        let path = home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

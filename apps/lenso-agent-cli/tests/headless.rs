use std::{fs, path::Path, process::Command};

fn plan_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../composition/headless-readonly/resolved-plan.json")
}

fn run(root: &Path, session: Option<&str>) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"));
    command
        .current_dir(root)
        .args(["--plan", plan_path().to_str().unwrap()])
        .args(["--prompt", "Summarize the README."]);
    if let Some(session) = session {
        command.args(["--session", session]);
    }
    command.output().unwrap()
}

#[test]
fn headless_turn_uses_tool_and_resumes_durable_session_after_restart() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Durable Fixture\n").unwrap();
    let first = run(temporary.path(), None);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        "README summary: # Durable Fixture\n"
    );
    let first_stderr = String::from_utf8(first.stderr).unwrap();
    let session = first_stderr.trim().strip_prefix("session: ").unwrap();

    let second = run(temporary.path(), Some(session));
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let stored = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(stored.len(), 1);
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(stored[0].path()).unwrap()).unwrap();
    assert_eq!(state["revision"], 13);
    assert_eq!(state["events"].as_array().unwrap().len(), 13);
}

#[test]
fn missing_session_is_a_domain_rejection() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let output = run(temporary.path(), Some("missing-session"));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("InvalidSession"));
}

#[test]
fn unavailable_durable_store_rejects_startup() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    fs::write(temporary.path().join(".lenso"), "not a directory").unwrap();
    let output = run(temporary.path(), None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("App startup failed"));
}

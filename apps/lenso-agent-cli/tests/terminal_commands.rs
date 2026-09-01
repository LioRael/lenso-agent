use std::{fs, path::Path, process::Command};

fn configure_fixture_app(home: &Path) {
    for (relative, configuration) in [
        (
            "lenso.agent.loop/agent.toml",
            "model = \"fixture/readme-summary-v1\"\n",
        ),
        (
            "lenso.agent.model.fixture/model.toml",
            "model = \"fixture/readme-summary-v1\"\n",
        ),
    ] {
        let path = home.join("plugins").join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, configuration).unwrap();
    }
}

fn agent(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"));
    command.env("LENSO_AGENT_HOME", home).current_dir(home);
    command
}

#[test]
fn session_commands_are_discovered_and_executed_through_plugins() {
    let home = tempfile::tempdir().unwrap();
    configure_fixture_app(home.path());

    let text = agent(home.path())
        .args(["sessions", "list"])
        .output()
        .unwrap();
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    assert_eq!(String::from_utf8(text.stdout).unwrap(), "No sessions.\n");

    let json = agent(home.path())
        .args(["sessions", "list", "--json"])
        .output()
        .unwrap();
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&json.stdout).unwrap(),
        serde_json::json!({"sessions": []})
    );
}

#[test]
fn dynamic_command_help_comes_from_the_catalog() {
    let home = tempfile::tempdir().unwrap();
    configure_fixture_app(home.path());
    let group = agent(home.path())
        .args(["sessions", "--help"])
        .output()
        .unwrap();
    assert!(
        group.status.success(),
        "{}",
        String::from_utf8_lossy(&group.stderr)
    );
    let group_stdout = String::from_utf8(group.stdout).unwrap();
    assert!(group_stdout.contains("list"));
    assert!(group_stdout.contains("show"));

    let output = agent(home.path())
        .args(["sessions", "show", "--help"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Read one session and its durable event log"));
    assert!(stdout.contains("--after <REVISION>"));
    assert!(stdout.contains("--json"));
}

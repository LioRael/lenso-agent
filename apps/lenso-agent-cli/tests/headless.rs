use std::{fs, path::Path, process::Command};

fn plan_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../composition/headless-readonly/resolved-plan.json")
}

fn run(root: &Path, plan: &Path, prompt: &str, session: Option<&str>) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"));
    command
        .current_dir(root)
        .args(["--plan", plan.to_str().unwrap()])
        .args(["--prompt", prompt]);
    if let Some(session) = session {
        command.args(["--session", session]);
    }
    command.output().unwrap()
}

fn plan_with_limits(root: &Path, max_steps: u64, max_tool_calls: u64) -> std::path::PathBuf {
    let mut plan = serde_json::from_slice::<serde_json::Value>(&fs::read(plan_path()).unwrap())
        .expect("decode canonical Plan");
    let agent = plan["module_instances"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|module| module["instance_key"] == "agent")
        .unwrap();
    let mut configuration =
        serde_json::from_str::<serde_json::Value>(agent["configuration"].as_str().unwrap())
            .unwrap();
    configuration["max_steps"] = max_steps.into();
    configuration["max_tool_calls"] = max_tool_calls.into();
    agent["configuration"] = configuration.to_string().into();
    let path = root.join(format!("plan-{max_steps}-{max_tool_calls}.json"));
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    path
}

fn plan_without_prompt_plugins(root: &Path) -> std::path::PathBuf {
    let mut plan = serde_json::from_slice::<serde_json::Value>(&fs::read(plan_path()).unwrap())
        .expect("decode canonical Plan");
    plan["module_instances"]
        .as_array_mut()
        .unwrap()
        .retain(|module| {
            module["instance_key"] != "fixture-instructions"
                && module["instance_key"] != "summary-skill"
        });
    plan["capability_bindings"]
        .as_array_mut()
        .unwrap()
        .retain(|binding| binding["consumer_instance"] != "prompt");
    let path = root.join("plan-without-prompt-plugins.json");
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    path
}

fn plan_with_filesystem_skill(root: &Path, skill_root: &Path) -> std::path::PathBuf {
    let mut plan = serde_json::from_slice::<serde_json::Value>(&fs::read(plan_path()).unwrap())
        .expect("decode canonical Plan");
    let provider = plan["module_instances"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|module| module["instance_key"] == "summary-skill")
        .unwrap();
    provider["instance_key"] = "filesystem-skills".into();
    provider["package_id"] = "lenso.agent.prompt.filesystem".into();
    provider["package_revision"] = "0.1.0".into();
    provider["configuration"] = serde_json::json!({
        "id_prefix": "agents.skills",
        "max_file_bytes": 4096,
        "max_total_bytes": 8192,
        "root": skill_root,
        "skills": ["test-skill"]
    })
    .to_string()
    .into();
    let binding = plan["capability_bindings"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|binding| binding["provider_instance"] == "summary-skill")
        .unwrap();
    binding["provider_instance"] = "filesystem-skills".into();
    let path = root.join("plan-with-filesystem-skill.json");
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    path
}

fn write_filesystem_skill(root: &Path) {
    let directory = root.join("test-skill");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("SKILL.md"),
        "---\nname: test-skill\ndescription: fixture\n---\n\nPrefix direct answers with `Filesystem: `.\n",
    )
    .unwrap();
}

#[test]
fn headless_turn_uses_tool_and_resumes_durable_session_after_restart() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Durable Fixture\n").unwrap();
    let plan = plan_path();
    let first = run(temporary.path(), &plan, "Summarize the README.", None);
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

    let second = run(
        temporary.path(),
        &plan,
        "What did you summarize?",
        Some(session),
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&second.stdout),
        "Previous answer: README summary: # Durable Fixture\n"
    );
    let stored = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(stored.len(), 1);
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(stored[0].path()).unwrap()).unwrap();
    assert_eq!(state["revision"], 12);
    assert_eq!(state["events"].as_array().unwrap().len(), 12);
}

#[test]
fn direct_answer_finishes_without_a_tool_call() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let output = run(
        temporary.path(),
        &plan_path(),
        "Answer directly: hello",
        None,
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Plugin: Direct answer.\n"
    );
    let session = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state: serde_json::Value = serde_json::from_slice(&fs::read(session).unwrap()).unwrap();
    assert_eq!(state["revision"], 5);
    assert!(
        !state["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "tool_requested")
    );
    let requested = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "model_requested")
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(requested["payload_json"].as_str().unwrap()).unwrap();
    let contributions = payload["prompt_contributions"].as_array().unwrap();
    assert_eq!(contributions[0]["id"], "fixture.behavior");
    assert_eq!(contributions[1]["id"], "workspace.summary");
    assert_eq!(contributions[0]["digest"].as_str().unwrap().len(), 64);
}

#[test]
fn removing_all_prompt_plugins_leaves_the_agent_composition_runnable() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let plan = plan_without_prompt_plugins(temporary.path());
    let output = run(temporary.path(), &plan, "Answer directly: hello", None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Direct answer.\n");
}

#[test]
fn explicitly_selected_filesystem_skill_reaches_the_model_and_session_manifest() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let skill_root = temporary.path().join("skills");
    write_filesystem_skill(&skill_root);
    let plan = plan_with_filesystem_skill(temporary.path(), &skill_root);
    let output = run(temporary.path(), &plan, "Answer directly: hello", None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Filesystem: Plugin: Direct answer.\n"
    );

    let session = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state: serde_json::Value = serde_json::from_slice(&fs::read(session).unwrap()).unwrap();
    let requested = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "model_requested")
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(requested["payload_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        payload["prompt_contributions"][1]["id"],
        "agents.skills/test-skill"
    );
    assert_eq!(
        payload["prompt_contributions"][1]["version"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
}

#[test]
fn missing_selected_filesystem_skill_rejects_startup() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let skill_root = temporary.path().join("skills");
    fs::create_dir(&skill_root).unwrap();
    let plan = plan_with_filesystem_skill(temporary.path(), &skill_root);
    let output = run(temporary.path(), &plan, "Answer directly: hello", None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("App startup failed"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("test-skill"));
}

#[test]
fn bounded_loop_executes_two_sequential_tool_calls() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Multi Tool\n").unwrap();
    let plan = plan_with_limits(temporary.path(), 3, 2);
    let output = run(temporary.path(), &plan, "Read README.md twice.", None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "README summary: # Multi Tool\n"
    );
    let session = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state: serde_json::Value = serde_json::from_slice(&fs::read(session).unwrap()).unwrap();
    assert_eq!(state["revision"], 11);
    assert_eq!(
        state["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "tool_requested")
            .count(),
        2
    );
}

#[test]
fn tool_call_limit_fails_before_the_excess_tool_executes() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Limit\n").unwrap();
    let plan = plan_with_limits(temporary.path(), 3, 1);
    let output = run(temporary.path(), &plan, "Read README.md twice.", None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ToolCallLimitExceeded"));
}

#[test]
fn step_limit_fails_before_a_tool_that_cannot_be_resumed() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Limit\n").unwrap();
    let plan = plan_with_limits(temporary.path(), 1, 1);
    let output = run(temporary.path(), &plan, "Summarize the README.", None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("StepLimitExceeded"));
    let session = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state: serde_json::Value = serde_json::from_slice(&fs::read(session).unwrap()).unwrap();
    assert!(
        !state["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "tool_requested")
    );
}

#[test]
fn missing_session_is_a_domain_rejection() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let output = run(
        temporary.path(),
        &plan_path(),
        "Summarize the README.",
        Some("missing-session"),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("InvalidSession"));
}

#[test]
fn unavailable_durable_store_rejects_startup() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    fs::write(temporary.path().join(".lenso"), "not a directory").unwrap();
    let output = run(
        temporary.path(),
        &plan_path(),
        "Summarize the README.",
        None,
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("App startup failed"));
}

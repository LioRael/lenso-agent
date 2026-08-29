use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::Command,
};

use lenso_agent_cli_plugin as _;
use lenso_agent_session_inspection::SessionInspector;
use lenso_plugin_control_plane::sha256_digest;

#[path = "../../../tests/support/mod.rs"]
mod support;

fn plan_path(root: &Path) -> std::path::PathBuf {
    support::plan_for_home("base", root)
}

fn command(root: &Path) -> Command {
    command_with_home(root, root)
}

fn command_with_home(workspace: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"));
    command.current_dir(workspace).env("LENSO_AGENT_HOME", home);
    command
}

fn runtime_generation_count(root: &Path) -> usize {
    let output = command(root).args(["runtime", "status"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("recoverable-generations: "))
        .unwrap()
        .parse()
        .unwrap()
}

fn run(root: &Path, plan: &Path, prompt: &str, session: Option<&str>) -> std::process::Output {
    let mut command = command(root);
    command
        .args(["--plan", plan.to_str().unwrap()])
        .args(["--prompt", prompt]);
    if let Some(session) = session {
        command.args(["--session", session]);
    }
    command.output().unwrap()
}

fn run_derived(root: &Path, prompt: &str, session: Option<&str>) -> std::process::Output {
    configure_fixture_app(root);
    let mut command = command(root);
    command.arg(prompt);
    if let Some(session) = session {
        command.args(["--session", session]);
    }
    command.output().unwrap()
}

fn run_configured_derived(
    root: &Path,
    prompt: &str,
    session: Option<&str>,
) -> std::process::Output {
    let mut command = command(root);
    command.arg(prompt);
    if let Some(session) = session {
        command.args(["--session", session]);
    }
    command.output().unwrap()
}

fn configure_fixture_app(root: &Path) {
    for (relative, configuration) in support::fixture_configurations() {
        let path = root.join("plugins").join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, configuration).unwrap();
    }
}

fn configure_plugin(root: &Path, plugin_id: &str) {
    configure_plugin_with(root, plugin_id, "");
}

fn configure_plugin_with(root: &Path, plugin_id: &str, configuration: &str) {
    let directory = root.join("plugins").join(plugin_id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("default.toml"), configuration).unwrap();
}

fn stored_session(root: &Path) -> serde_json::Value {
    let sessions = stored_sessions(root);
    assert_eq!(sessions.len(), 1);
    sessions.into_iter().next().unwrap()
}

fn stored_sessions(root: &Path) -> Vec<serde_json::Value> {
    let database = root.join("sessions.sqlite3");
    let sessions = if database.is_file() {
        lenso_agent_session_sqlite_plugin::SqliteSessionInspector::new(database)
            .inspect_all()
            .unwrap()
    } else {
        lenso_agent_session_file_plugin::FileSessionInspector::new(root.join("sessions"))
            .inspect_all()
            .unwrap()
    };
    sessions
        .into_iter()
        .map(|session| serde_json::to_value(session).unwrap())
        .collect()
}

fn stored_session_path(root: &Path) -> std::path::PathBuf {
    fs::read_dir(root.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}

fn stored_file_session(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(stored_session_path(root)).unwrap()).unwrap()
}

fn configure_file_sessions(root: &Path) {
    configure_plugin_with(
        root,
        "lenso.agent.session.file",
        "directory = \"sessions\"\n",
    );
}

#[test]
fn official_coding_sandbox_and_plan_profiles_install_and_resolve() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let home = temporary.path().join("agent-home");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(workspace.join("README.md"), "# Coding Profile\n").unwrap();
    for arguments in [
        vec!["init"],
        vec!["add", "README.md"],
        vec![
            "-c",
            "user.name=Lenso Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    ] {
        let status = Command::new("git")
            .current_dir(&workspace)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let install = command_with_home(&workspace, &home)
        .args(["profiles", "install", "coding"])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );

    for profile in ["code", "code-sandbox", "plan"] {
        let output = command_with_home(&workspace, &home)
            .args(["contexts", "--profile", profile])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{profile}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one headless proof retains profile installation, concurrent execution, and Git isolation evidence"
)]
fn coding_profile_runs_two_mutation_children_in_separate_worktrees() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let home = temporary.path().join("agent-home");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(workspace.join("README.md"), "# Isolated Workers\n").unwrap();
    let git = |cwd: &Path, arguments: &[&str]| {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    git(&workspace, &["init", "--quiet"]);
    git(&workspace, &["config", "user.name", "Lenso Test"]);
    git(
        &workspace,
        &["config", "user.email", "test@example.invalid"],
    );
    git(&workspace, &["add", "README.md"]);
    git(&workspace, &["commit", "--quiet", "-m", "initial"]);

    let install = command_with_home(&workspace, &home)
        .args(["profiles", "install", "coding"])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    for instance in ["agent", "worker-a", "worker-b"] {
        let path = home
            .join("plugins/lenso.agent.loop")
            .join(format!("{instance}.toml"));
        fs::write(path, "model = \"fixture/readme-summary-v1\"\n").unwrap();
    }
    configure_plugin_with(
        &home,
        "lenso.agent.model.fixture",
        "model = \"fixture/readme-summary-v1\"\nallowed_models = [\"gpt-5.6-luna\", \"gpt-4o-mini\", \"fixture/session-presentation-v1\"]\n",
    );
    let code_profile = home.join("profiles/code.toml");
    let profile = fs::read_to_string(&code_profile).unwrap();
    fs::write(
        &code_profile,
        profile.replace("]\n", "  \"lenso.agent.model.fixture/default\",\n]\n"),
    )
    .unwrap();
    fs::write(
        home.join("plugins/lenso.agent.workspace-edit/default.toml"),
        "root = \".\"\nmax_file_bytes = 1048576\nmax_edit_bytes = 131072\nrequire_checkpoint = false\n",
    )
    .unwrap();
    fs::write(
        home.join("plugins/lenso.agent.interactive-approval-hook/default.toml"),
        "default_decision = \"ask\"\nallow_tools = [\"spawn_subagent\", \"wait_subagent\", \"create_file\", \"git_stage\", \"git_commit\"]\nask_tools = []\ndeny_tools = []\nmax_preview_bytes = 16384\n",
    )
    .unwrap();

    let output = command_with_home(&workspace, &home)
        .args([
            "--profile",
            "code",
            "--prompt",
            "Spawn two isolated mutation workers.",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Both isolated workers committed their changes.\n"
    );
    assert!(!workspace.join("worker-a.txt").exists());
    assert!(!workspace.join("worker-b.txt").exists());

    let worktree_root = home.join("runtime/child-worktrees");
    let mut children = fs::read_dir(&worktree_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    children.sort();
    assert_eq!(children.len(), 2);
    let worker_a = children
        .iter()
        .find(|child| child.join("worker-a.txt").is_file())
        .expect("worker-a change must exist in one retained child worktree");
    let worker_b = children
        .iter()
        .find(|child| child.join("worker-b.txt").is_file())
        .expect("worker-b change must exist in one retained child worktree");
    assert_ne!(worker_a, worker_b);
    assert!(!worker_a.join("worker-b.txt").exists());
    assert!(!worker_b.join("worker-a.txt").exists());
    assert_eq!(
        String::from_utf8_lossy(&git(worker_a, &["log", "-1", "--pretty=%s"]).stdout).trim(),
        "test: worker-a isolated change"
    );
    assert_eq!(
        String::from_utf8_lossy(&git(worker_b, &["log", "-1", "--pretty=%s"]).stdout).trim(),
        "test: worker-b isolated change"
    );
}

fn turn_generation_digests(session: &serde_json::Value) -> Vec<String> {
    session["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "turn_started")
        .map(|event| {
            let payload: serde_json::Value =
                serde_json::from_str(event["payload_json"].as_str().unwrap()).unwrap();
            payload["generation_spec_digest"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn turn_behavior_digests(session: &serde_json::Value) -> Vec<String> {
    session["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "turn_started")
        .map(|event| {
            let payload: serde_json::Value =
                serde_json::from_str(event["payload_json"].as_str().unwrap()).unwrap();
            payload["agent_behavior_digest"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn plan_with_limits(root: &Path, max_steps: u64, max_tool_calls: u64) -> std::path::PathBuf {
    let mut plan = serde_json::from_slice::<serde_json::Value>(&fs::read(plan_path(root)).unwrap())
        .expect("decode canonical Plan");
    let agent = plan["plugin_instances"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|plugin| plugin["instance_key"] == "lenso.agent.loop/agent")
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
    let mut plan = serde_json::from_slice::<serde_json::Value>(&fs::read(plan_path(root)).unwrap())
        .expect("decode canonical Plan");
    plan["plugin_instances"]
        .as_array_mut()
        .unwrap()
        .retain(|plugin| plugin["instance_key"] != "lenso.agent.prompt.static/summary-skill");
    plan["capability_bindings"]
        .as_array_mut()
        .unwrap()
        .retain(|binding| binding["consumer_instance"] != "lenso.agent.prompt/prompt");
    let path = root.join("plan-without-prompt-plugins.json");
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    path
}

fn plan_without_session_presentation(root: &Path) -> std::path::PathBuf {
    let mut plan = serde_json::from_slice::<serde_json::Value>(&fs::read(plan_path(root)).unwrap())
        .expect("decode canonical Plan");
    plan["plugin_instances"]
        .as_array_mut()
        .unwrap()
        .retain(|plugin| plugin["instance_key"] != "lenso.agent.session-presentation/presentation");
    plan["capability_bindings"]
        .as_array_mut()
        .unwrap()
        .retain(|binding| binding["capability_id"] != "lenso.agent.session-presentation@1");
    let path = root.join("plan-without-session-presentation.json");
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    path
}

fn plan_with_filesystem_skill(root: &Path, skill_root: &Path) -> std::path::PathBuf {
    let mut plan = serde_json::from_slice::<serde_json::Value>(&fs::read(plan_path(root)).unwrap())
        .expect("decode canonical Plan");
    let provider = plan["plugin_instances"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|plugin| plugin["instance_key"] == "lenso.agent.prompt.static/summary-skill")
        .unwrap();
    provider["instance_key"] = "lenso.agent.prompt.filesystem/filesystem-skills".into();
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
        .find(|binding| binding["provider_instance"] == "lenso.agent.prompt.static/summary-skill")
        .unwrap();
    binding["provider_instance"] = "lenso.agent.prompt.filesystem/filesystem-skills".into();
    let path = root.join("plan-with-filesystem-skill.json");
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    path
}

fn plan_with_on_demand_skills(root: &Path, skill_root: &Path) -> std::path::PathBuf {
    let mut plan = serde_json::from_slice::<serde_json::Value>(&fs::read(plan_path(root)).unwrap())
        .expect("decode canonical Plan");
    let plugins = plan["plugin_instances"].as_array_mut().unwrap();
    let provider = plugins
        .iter_mut()
        .find(|plugin| plugin["instance_key"] == "lenso.agent.skills.filesystem/skills")
        .unwrap();
    provider["configuration"] = serde_json::json!({
        "max_catalog_bytes": 8192,
        "catalog_contribution_id": "agents.skills.catalog",
        "max_file_bytes": 8192,
        "max_prompt_catalog_bytes": 8192,
        "max_skills": 16,
        "max_total_bytes": 32768,
        "max_resource_entries": 64,
        "max_resource_file_bytes": 8192,
        "max_resource_total_bytes": 32768,
        "max_resource_manifest_bytes": 8192,
        "root": skill_root,
    })
    .to_string()
    .into();

    let path = root.join("plan-with-on-demand-skills.json");
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

fn write_on_demand_skills(root: &Path) {
    for (name, description, body) in [
        (
            "rust-review",
            "Review Rust changes with project conventions.",
            "RUST REVIEW INSTRUCTION: check ownership and error boundaries. Read references/checklist.md when a detailed checklist is needed.",
        ),
        (
            "unused-secret",
            "A Skill that must remain unopened.",
            "UNSELECTED SKILL CONTENT MUST NOT REACH THE MODEL",
        ),
    ] {
        let directory = root.join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
        )
        .unwrap();
    }
    let rust_review = root.join("rust-review");
    fs::create_dir_all(rust_review.join("references")).unwrap();
    fs::create_dir_all(rust_review.join("scripts")).unwrap();
    fs::write(
        rust_review.join("references/checklist.md"),
        "RESOURCE CHECKLIST CONTENT: verify ownership, errors, and tests.\n",
    )
    .unwrap();
    fs::write(
        rust_review.join("scripts/do-not-run.sh"),
        "touch resource-script-executed\n# UNREAD RESOURCE CONTENT MUST NOT REACH THE MODEL\n",
    )
    .unwrap();
}

#[test]
fn headless_turn_uses_tool_and_resumes_durable_session_after_restart() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Durable Fixture\n").unwrap();
    let plan = plan_path(temporary.path());
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
    let state = stored_session(temporary.path());
    assert_eq!(state["revision"], 18);
    assert_eq!(state["events"].as_array().unwrap().len(), 18);
    assert_eq!(
        state["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "system_instruction_installed")
            .count(),
        1
    );
    let presentations = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "turn_completed")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()["presentation"]
                .clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(presentations.len(), 2);
    assert_eq!(presentations[0]["title"], "Summarize the README.");
    assert_eq!(presentations[1]["title"], "Summarize the README.");
    assert_eq!(
        presentations[1]["latest_preview"],
        "Previous answer: README summary: # Durable Fixture"
    );
}

#[test]
fn global_agent_home_keeps_configuration_and_state_outside_the_workspace() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    configure_fixture_app(home.path());
    fs::write(workspace.path().join("README.md"), "# Separate Workspace\n").unwrap();

    let output = command_with_home(workspace.path(), home.path())
        .arg("Summarize the README.")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "README summary: # Separate Workspace\n"
    );
    assert!(home.path().join(".lenso/host-catalog.json").is_file());
    assert!(home.path().join("runtime").is_dir());
    assert!(home.path().join("sessions.sqlite3").is_file());
    assert!(!workspace.path().join("plugins").exists());
    assert!(!workspace.path().join(".lenso").exists());
}

#[test]
fn automatic_compaction_commits_a_durable_projection_without_rewriting_session_history() {
    let temporary = tempfile::tempdir().unwrap();
    configure_fixture_app(temporary.path());
    fs::write(
        temporary.path().join("plugins/lenso.agent.loop/agent.toml"),
        "model = \"fixture/readme-summary-v1\"\nmax_history_events = 1\n",
    )
    .unwrap();
    let compactor = temporary
        .path()
        .join("plugins/lenso.agent.context-compaction/context-compactor.toml");
    fs::create_dir_all(compactor.parent().unwrap()).unwrap();
    fs::write(
        compactor,
        "max_input_characters = 1048576\nmax_summary_characters = 8192\nretain_recent_turns = 1\n",
    )
    .unwrap();

    let first = run_configured_derived(temporary.path(), "Answer directly: durable context", None);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let session_id = String::from_utf8(first.stderr)
        .unwrap()
        .trim()
        .strip_prefix("session: ")
        .unwrap()
        .to_owned();
    let before = stored_session(temporary.path());
    let original_events = before["events"].as_array().unwrap().len();

    let second = run_configured_derived(
        temporary.path(),
        "What did you summarize?",
        Some(&session_id),
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let after = stored_session(temporary.path());
    let events = after["events"].as_array().unwrap();
    assert!(events.len() > original_events);
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "context_compaction_started")
    );
    let committed = events
        .iter()
        .find(|event| event["kind"] == "context_compaction_committed")
        .unwrap();
    let checkpoint: serde_json::Value =
        serde_json::from_str(committed["payload_json"].as_str().unwrap()).unwrap();
    assert!(
        checkpoint["summary"]
            .as_str()
            .unwrap()
            .contains("durable context")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["kind"] == "turn_completed")
            .count(),
        2
    );
}

#[test]
fn resumed_session_reuses_its_installed_system_instruction() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    configure_file_sessions(temporary.path());
    let first = run_derived(temporary.path(), "Answer directly: first", None);
    assert!(first.status.success());
    let first_stderr = String::from_utf8(first.stderr).unwrap();
    let session_id = first_stderr.trim().strip_prefix("session: ").unwrap();

    let path = stored_session_path(temporary.path());
    let mut state = stored_file_session(temporary.path());
    let installed = state["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["kind"] == "system_instruction_installed")
        .unwrap();
    let mut payload: serde_json::Value =
        serde_json::from_str(installed["payload_json"].as_str().unwrap()).unwrap();
    let pinned = "This instruction belongs to the original Session.";
    payload["content"] = pinned.into();
    payload["digest"] = sha256_digest(pinned.as_bytes()).into();
    installed["payload_json"] = payload.to_string().into();
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

    let second = run_derived(
        temporary.path(),
        "Answer directly: second",
        Some(session_id),
    );
    assert!(second.status.success());
    let resumed = stored_session(temporary.path());
    assert_eq!(
        resumed["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "system_instruction_installed")
            .count(),
        1
    );
    let latest_request = resumed["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|event| event["kind"] == "model_requested")
        .unwrap();
    let latest_payload: serde_json::Value =
        serde_json::from_str(latest_request["payload_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        latest_payload["system_instruction_digest"],
        sha256_digest(pinned.as_bytes())
    );
}

#[test]
fn legacy_session_installs_one_system_instruction_when_first_resumed() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    configure_file_sessions(temporary.path());
    let first = run_derived(temporary.path(), "Answer directly: first", None);
    assert!(first.status.success());
    let first_stderr = String::from_utf8(first.stderr).unwrap();
    let session_id = first_stderr.trim().strip_prefix("session: ").unwrap();

    let path = stored_session_path(temporary.path());
    let mut state = stored_file_session(temporary.path());
    let events = state["events"].as_array_mut().unwrap();
    events.retain(|event| event["kind"] != "system_instruction_installed");
    for (index, event) in events.iter_mut().enumerate() {
        event["revision"] = u64::try_from(index + 1).unwrap().into();
    }
    state["revision"] = u64::try_from(events.len()).unwrap().into();
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

    let second = run_derived(
        temporary.path(),
        "Answer directly: second",
        Some(session_id),
    );
    assert!(second.status.success());
    let resumed = stored_session(temporary.path());
    assert_eq!(
        resumed["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "system_instruction_installed")
            .count(),
        1
    );
}

#[test]
fn resumed_session_closes_a_host_interrupted_turn_before_new_work() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Durable Fixture\n").unwrap();
    configure_file_sessions(temporary.path());
    let first = run_derived(temporary.path(), "Answer directly: hello", None);
    assert!(first.status.success());
    let first_stderr = String::from_utf8(first.stderr).unwrap();
    let session_id = first_stderr.trim().strip_prefix("session: ").unwrap();

    let path = stored_session_path(temporary.path());
    let mut session = stored_file_session(temporary.path());
    let generation_spec_digest = turn_generation_digests(&session).pop().unwrap();
    let interrupted_revision = session["revision"].as_u64().unwrap() + 1;
    let interrupted_payload = serde_json::json!({
        "generation_spec_digest": generation_spec_digest,
        "input": "interrupted"
    })
    .to_string();
    session["revision"] = interrupted_revision.into();
    session["events"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "revision": interrupted_revision,
            "event_id": "simulated-host-crash",
            "kind": "turn_started",
            "turn_id": "interrupted-turn",
            "occurred_at": "2026-08-25T00:00:00Z",
            "payload_json": interrupted_payload
        }));
    fs::write(&path, serde_json::to_vec(&session).unwrap()).unwrap();

    let second = run_derived(
        temporary.path(),
        "Answer directly: recovered",
        Some(session_id),
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let resumed = stored_session(temporary.path());
    let events = resumed["events"].as_array().unwrap();
    let recovered = events
        .iter()
        .find(|event| event["kind"] == "turn_failed" && event["turn_id"] == "interrupted-turn")
        .unwrap();
    assert!(
        recovered["payload_json"]
            .as_str()
            .unwrap()
            .contains("host_interrupted")
    );
    let recovery_index = events.iter().position(|event| event == recovered).unwrap();
    let new_turn_index = events
        .iter()
        .rposition(|event| event["kind"] == "turn_started")
        .unwrap();
    assert!(recovery_index < new_turn_index);
}

#[test]
fn run_scope_cannot_add_a_tool_outside_the_plan_catalog() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Scope Fixture\n").unwrap();
    let output = command(temporary.path())
        .args(["--plan", plan_path(temporary.path()).to_str().unwrap()])
        .args(["--prompt", "Answer directly: hello"])
        .args(["--allow-tool", "ambient.superpower"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("outside the Plan-bound catalog"));
    let session = stored_session(temporary.path());
    let started: serde_json::Value = serde_json::from_str(
        session["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["kind"] == "turn_started")
            .unwrap()["payload_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        started["run_scope"]["allowed_tools"][0],
        "ambient.superpower"
    );
}

const MCP_CONTINUATION_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *\"method\":\"server/discover\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{},"prompts":{},"resources":{}}}}\n' "$id"
      ;;
    *\"method\":\"tools/list\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","tools":[{"name":"ping","description":"Return pong.","inputSchema":{"type":"object","additionalProperties":false}}]}}\n' "$id"
      ;;
    *\"method\":\"tools/call\"*\"inputResponses\"*)
      case "$line" in
        *\"text\":\"Paris.\"*\"requestState\":\"opaque-fixture-state\"*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","content":[{"type":"text","text":"pong"}],"isError":false}}\n' "$id"
          ;;
        *)
          printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32602,"message":"continuation response was incomplete"}}\n' "$id"
          ;;
      esac
      ;;
    *\"method\":\"tools/call\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"input_required","inputRequests":{"sample":{"method":"sampling/createMessage","params":{"messages":[{"role":"user","content":{"type":"text","text":"What is the capital of France?"}}],"maxTokens":32}}},"requestState":"opaque-fixture-state"}}\n' "$id"
      ;;
    *\"method\":\"prompts/get\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","description":"Fixture review","messages":[{"role":"user","content":{"type":"text","text":"Review carefully."}}]}}\n' "$id"
      ;;
    *\"method\":\"prompts/list\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","prompts":[{"name":"review","description":"Review carefully.","arguments":[]}]}}\n' "$id"
      ;;
    *\"method\":\"resources/read\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","contents":[{"uri":"fixture://guide","mimeType":"text/plain","text":"Fixture guide content."}]}}\n' "$id"
      ;;
    *\"method\":\"resources/list\"*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resultType":"complete","resources":[{"uri":"fixture://guide","name":"Fixture Guide","description":"Fixture guide.","mimeType":"text/plain"}]}}\n' "$id"
      ;;
  esac
done
"#;

#[test]
fn mcp_client_discovers_and_invokes_a_real_stdio_server() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# MCP Fixture\n").unwrap();
    let server = temporary.path().join("mcp-server.sh");
    fs::write(&server, MCP_CONTINUATION_SERVER).unwrap();
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.mcp-client",
        &format!(
            r#"transport = "stdio"
program = "/bin/sh"
arguments = ["{}"]
working_directory = "{}"
environment_allowlist = []
protocol = "auto"
tool_namespace = "fixture"
startup_timeout_ms = 1000
request_timeout_ms = 1000
allow_elicitation = false
allow_sampling = true
continuation_max_rounds = 2
sampling_model = "fixture/readme-summary-v1"
max_sampling_tokens = 64
"#,
            server.display(),
            temporary.path().display()
        ),
    );

    let output = run_derived(temporary.path(), "Use the MCP fixture to ping.", None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "MCP result: pong\n"
    );

    let session = stored_session(temporary.path());
    let events = session["events"].as_array().unwrap();
    assert!(events.iter().any(|event| {
        event["kind"] == "tool_requested"
            && event["payload_json"]
                .as_str()
                .is_some_and(|payload| payload.contains("mcp__fixture__ping"))
    }));
    assert!(events.iter().any(|event| event["kind"] == "turn_completed"));

    let catalog_output = command(temporary.path()).arg("contexts").output().unwrap();
    assert!(catalog_output.status.success());
    let catalog: serde_json::Value = serde_json::from_slice(&catalog_output.stdout).unwrap();
    assert_eq!(catalog["prompts"][0]["name"], "review");
    assert_eq!(catalog["resources"][0]["uri"], "fixture://guide");

    let context_output = command(temporary.path())
        .arg("Use the selected context.")
        .args(["--context-prompt", "fixture/review"])
        .args(["--context-resource", "fixture=fixture://guide"])
        .output()
        .unwrap();
    assert!(
        context_output.status.success(),
        "{}",
        String::from_utf8_lossy(&context_output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&context_output.stdout),
        "Context result: prompt and resource applied\n"
    );
}

#[test]
fn resumed_session_records_each_host_generation_and_keeps_its_specs() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Generation Fixture\n").unwrap();
    let first = run_derived(temporary.path(), "Answer directly: hello", None);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stderr = String::from_utf8(first.stderr).unwrap();
    let session_id = first_stderr.trim().strip_prefix("session: ").unwrap();

    configure_plugin(temporary.path(), "lenso.agent.text-tools");
    let second = run_derived(temporary.path(), "What did you answer?", Some(session_id));
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );

    let session = stored_session(temporary.path());
    let digests = turn_generation_digests(&session);
    let behavior_digests = turn_behavior_digests(&session);
    assert_eq!(digests.len(), 2);
    assert_eq!(behavior_digests.len(), 2);
    assert_ne!(digests[0], digests[1]);
    assert_ne!(behavior_digests[0], behavior_digests[1]);
    let provenance = command(temporary.path())
        .args(["sessions", "provenance", "--session", session_id])
        .output()
        .unwrap();
    assert!(provenance.status.success());
    let provenance_stdout = String::from_utf8(provenance.stdout).unwrap();
    for (digest, behavior_digest) in digests.iter().zip(&behavior_digests) {
        assert!(provenance_stdout.contains(&format!("generation={digest}")));
        assert!(provenance_stdout.contains(&format!("behavior={behavior_digest}")));
        assert!(provenance_stdout.contains("spec=available"));
        let inspect = command(temporary.path())
            .args(["generations", "inspect", "--digest", digest])
            .output()
            .unwrap();
        assert!(inspect.status.success());
        let inspect_stdout = String::from_utf8(inspect.stdout).unwrap();
        assert!(inspect_stdout.contains(&format!("generation: {digest}")));
        assert!(inspect_stdout.contains("app: lenso.agent.harness"));
        assert!(inspect_stdout.contains("resolution-authority: sha256:"));
    }
    assert_eq!(runtime_generation_count(temporary.path()), 2);
}

#[test]
fn session_facts_support_replay_evaluation_and_otlp_export() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Replay Fixture\n").unwrap();
    let run = run_derived(temporary.path(), "Answer directly: hello", None);
    assert!(run.status.success());
    let session_id = String::from_utf8(run.stderr)
        .unwrap()
        .trim()
        .strip_prefix("session: ")
        .unwrap()
        .to_owned();

    let replay = command(temporary.path())
        .args(["sessions", "replay", "--session", &session_id])
        .output()
        .unwrap();
    assert!(replay.status.success());
    let replay: serde_json::Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay["schema"], "lenso.agent.trajectory@1");
    assert_eq!(replay["sessionId"], session_id);
    assert_eq!(replay["summary"]["status"], "completed");

    let evaluation = command(temporary.path())
        .args(["sessions", "evaluate", "--session", &session_id])
        .output()
        .unwrap();
    assert!(evaluation.status.success());
    let evaluation: serde_json::Value = serde_json::from_slice(&evaluation.stdout).unwrap();
    assert_eq!(evaluation["schema"], "lenso.agent.evaluation@1");
    assert_eq!(evaluation["passed"], true);

    let otlp_path = temporary.path().join("session.otlp.json");
    let otlp = command(temporary.path())
        .args([
            "sessions",
            "otlp",
            "--session",
            &session_id,
            "--output",
            otlp_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(otlp.status.success());
    let otlp: serde_json::Value = serde_json::from_slice(&fs::read(otlp_path).unwrap()).unwrap();
    let spans = otlp["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .unwrap();
    assert!(spans.len() >= 2);
    assert_eq!(spans[0]["traceId"].as_str().unwrap().len(), 32);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let collector = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap();
            if bytes.len() >= header_end + 4 + content_length {
                break;
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        String::from_utf8(bytes).unwrap()
    });
    let endpoint = format!("http://{address}/v1/traces");
    let export = command(temporary.path())
        .args([
            "sessions",
            "otlp",
            "--session",
            &session_id,
            "--endpoint",
            &endpoint,
        ])
        .output()
        .unwrap();
    assert!(export.status.success());
    let request = collector.join().unwrap();
    assert!(request.starts_with("POST /v1/traces HTTP/1.1"));
    assert!(request.contains("resourceSpans"));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end fixture proves preview, Controller reconciliation, apply, and idempotency"
)]
fn generation_gc_preview_and_apply_preserve_every_reachability_root() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# GC Fixture\n").unwrap();
    let first = run_derived(temporary.path(), "Answer directly: hello", None);
    assert!(first.status.success());
    configure_plugin(temporary.path(), "lenso.agent.text-tools");
    let second = run_derived(temporary.path(), "Answer directly: hello", None);
    assert!(second.status.success());
    let digests = stored_sessions(temporary.path())
        .iter()
        .map(|session| turn_generation_digests(session).pop().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(digests.len(), 2);

    fs::remove_dir_all(temporary.path().join("plugins/lenso.agent.text-tools")).unwrap();
    let empty_sessions = temporary.path().join("empty-sessions");
    fs::create_dir(&empty_sessions).unwrap();
    let before_reconcile = command(temporary.path())
        .args([
            "generations",
            "gc-preview",
            "--sessions",
            empty_sessions.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(before_reconcile.status.success());
    let before_reconcile_stdout = String::from_utf8(before_reconcile.stdout).unwrap();
    assert!(before_reconcile_stdout.contains("reason=controller"));
    assert!(before_reconcile_stdout.contains("summary: protected=1 candidates=1"));
    let reconcile = run_derived(
        temporary.path(),
        "Answer directly: reconcile removed Plugin authority",
        None,
    );
    assert!(reconcile.status.success());
    let preview = command(temporary.path())
        .args([
            "generations",
            "gc-preview",
            "--sessions",
            empty_sessions.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let stdout = String::from_utf8(preview.stdout).unwrap();
    assert!(
        stdout.contains("summary: protected=1 candidates=1"),
        "{stdout}"
    );
    let records_before = runtime_generation_count(temporary.path());

    let preview_with_sessions = command(temporary.path())
        .args(["generations", "gc-preview"])
        .output()
        .unwrap();
    assert!(preview_with_sessions.status.success());
    assert!(
        String::from_utf8_lossy(&preview_with_sessions.stdout)
            .contains("summary: protected=2 candidates=0")
    );
    assert_eq!(runtime_generation_count(temporary.path()), records_before);

    let apply = command(temporary.path())
        .args([
            "generations",
            "gc",
            "--apply",
            "--sessions",
            empty_sessions.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    let apply_stdout = String::from_utf8(apply.stdout).unwrap();
    assert!(apply_stdout.contains("summary: removed-generations=1"));
    assert_eq!(
        runtime_generation_count(temporary.path()),
        records_before - 1
    );

    let second_apply = command(temporary.path())
        .args([
            "generations",
            "gc",
            "--apply",
            "--sessions",
            empty_sessions.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(second_apply.status.success());
    assert!(
        String::from_utf8_lossy(&second_apply.stdout)
            .contains("summary: removed-generations=0 removed-recovery-authorities=0")
    );
}

#[test]
fn corrupted_generation_provenance_rejects_startup_before_a_turn() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Generation Fixture\n").unwrap();
    let first = run_derived(temporary.path(), "Answer directly: hello", None);
    assert!(first.status.success());
    let before = stored_session(temporary.path());
    let digest = turn_generation_digests(&before).pop().unwrap();
    let database = temporary.path().join("runtime/.state/runtime.sqlite3");
    rusqlite::Connection::open(database)
        .unwrap()
        .execute(
            "UPDATE generation_specs SET spec_json = ?1 WHERE digest = ?2",
            rusqlite::params![b"{}".as_slice(), &digest],
        )
        .unwrap();

    let session_id = before["session_id"].as_str().unwrap_or_default();
    let inspect = command(temporary.path())
        .args(["sessions", "provenance", "--session", session_id])
        .output()
        .unwrap();
    assert!(inspect.status.success());
    assert!(String::from_utf8_lossy(&inspect.stdout).contains("spec=invalid"));

    let gc_preview = command(temporary.path())
        .args(["generations", "gc-preview"])
        .output()
        .unwrap();
    assert!(!gc_preview.status.success());
    assert!(
        String::from_utf8_lossy(&gc_preview.stderr).contains("Generation Spec validation failed")
    );

    let second = run_derived(temporary.path(), "Answer directly: this must not run", None);
    assert!(!second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("App startup failed"));
    assert!(
        stderr.contains("runtime state") && stderr.contains("digest"),
        "{stderr}"
    );
    assert_eq!(stored_session(temporary.path()), before);
}

#[test]
fn direct_answer_finishes_without_a_tool_call() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let output = run(
        temporary.path(),
        &plan_path(temporary.path()),
        "Answer directly: hello",
        None,
    );
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Direct answer.\n");
    let state = stored_session(temporary.path());
    assert_eq!(state["revision"], 8);
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
    assert_eq!(contributions.len(), 4);
    assert_eq!(contributions[0]["id"], "harness.base");
    assert!(
        contributions
            .iter()
            .any(|contribution| contribution["id"] == "agents.skills.catalog")
    );
    let summary = contributions
        .iter()
        .find(|contribution| contribution["id"] == "workspace.summary")
        .unwrap();
    assert_eq!(summary["digest"].as_str().unwrap().len(), 64);
    let installed = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "system_instruction_installed")
        .unwrap();
    let installed_payload: serde_json::Value =
        serde_json::from_str(installed["payload_json"].as_str().unwrap()).unwrap();
    let installed_content = installed_payload["content"].as_str().unwrap();
    assert!(!installed_content.trim().is_empty());
    assert_eq!(
        installed_payload["digest"],
        sha256_digest(installed_content.as_bytes())
    );
    assert_eq!(installed_payload["contributions"][0]["id"], "harness.base");
    assert_eq!(
        payload["system_instruction_digest"],
        installed_payload["digest"]
    );
}

#[test]
fn completed_turns_are_recalled_across_sessions_with_durable_provenance() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();

    let first = run_derived(
        temporary.path(),
        "Answer directly: durable storage uses SQLite WAL.",
        None,
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = run_derived(
        temporary.path(),
        "Answer directly: which durable storage did we discuss?",
        None,
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(temporary.path().join("memory.sqlite3").is_file());

    let sessions = stored_sessions(temporary.path());
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().any(|session| {
        session["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "memory_committed")
    }));
    let recalled = sessions
        .iter()
        .flat_map(|session| session["events"].as_array().unwrap())
        .find(|event| {
            if event["kind"] != "memory_recalled" {
                return false;
            }
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .ok()
                .and_then(|payload| payload["memory_ids"].as_array().map(Vec::len))
                .is_some_and(|count| count > 0)
        })
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(recalled["payload_json"].as_str().unwrap()).unwrap();
    assert!(!payload["memory_ids"].as_array().unwrap().is_empty());
}

#[test]
fn lifecycle_audit_observes_start_resume_and_terminal_turns() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let first = run_derived(
        temporary.path(),
        "Answer directly: first lifecycle turn",
        None,
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let session_id = String::from_utf8(first.stderr)
        .unwrap()
        .trim()
        .strip_prefix("session: ")
        .unwrap()
        .to_owned();
    let second = run_derived(
        temporary.path(),
        "Answer directly: resumed lifecycle turn",
        Some(&session_id),
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );

    let audit = fs::read_to_string(temporary.path().join("lifecycle/events.jsonl")).unwrap();
    let events = audit
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 6);
    assert_eq!(events[0]["kind"], "session_started");
    assert_eq!(events[1]["kind"], "turn_started");
    assert_eq!(events[2]["kind"], "turn_completed");
    assert_eq!(events[3]["kind"], "session_resumed");
    assert_eq!(events[4]["kind"], "turn_started");
    assert_eq!(events[5]["kind"], "turn_completed");
    assert!(events.iter().all(|event| event["session_id"] == session_id));
}

#[test]
fn sqlite_session_adapter_runs_a_real_turn_and_backend_neutral_provenance() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    configure_fixture_app(temporary.path());
    let sqlite_configuration = temporary
        .path()
        .join("plugins/lenso.agent.session.sqlite/local.toml");
    fs::create_dir_all(sqlite_configuration.parent().unwrap()).unwrap();
    fs::write(sqlite_configuration, "database = \"sessions.sqlite3\"\n").unwrap();
    fs::create_dir_all(temporary.path().join("profiles")).unwrap();
    fs::write(
        temporary.path().join("profiles/sqlite.toml"),
        r#"description = "SQLite sessions"
instances = [
  "lenso.agent.model.fixture/model",
  "lenso.agent.session.sqlite/local",
]
"#,
    )
    .unwrap();
    let output = command(temporary.path())
        .args(["--profile", "sqlite", "--prompt", "Answer directly: sqlite"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(temporary.path().join("sessions.sqlite3").is_file());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let session_id = stderr.trim().strip_prefix("session: ").unwrap();
    let provenance = command(temporary.path())
        .args([
            "sessions",
            "provenance",
            "--session",
            session_id,
            "--database",
            "sessions.sqlite3",
        ])
        .output()
        .unwrap();
    assert!(
        provenance.status.success(),
        "{}",
        String::from_utf8_lossy(&provenance.stderr)
    );
    assert!(String::from_utf8_lossy(&provenance.stdout).contains("generation=sha256:"));
    let gc = command(temporary.path())
        .args([
            "generations",
            "gc-preview",
            "--session-database",
            "sessions.sqlite3",
        ])
        .output()
        .unwrap();
    assert!(
        gc.status.success(),
        "{}",
        String::from_utf8_lossy(&gc.stderr)
    );
    assert!(
        String::from_utf8_lossy(&gc.stdout)
            .lines()
            .any(|line| line.starts_with("protected:") && line.contains("session"))
    );
}

#[test]
fn sessions_export_import_and_migrate_between_file_and_sqlite() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let turn = run_derived(temporary.path(), "Answer directly: portable session", None);
    assert!(
        turn.status.success(),
        "{}",
        String::from_utf8_lossy(&turn.stderr)
    );
    let session_id = String::from_utf8(turn.stderr)
        .unwrap()
        .trim()
        .strip_prefix("session: ")
        .unwrap()
        .to_owned();
    let archive = temporary.path().join("session-archive.json");

    let exported = command(temporary.path())
        .args([
            "sessions",
            "export",
            "--archive",
            archive.to_str().unwrap(),
            "--session",
            &session_id,
        ])
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let archive_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&archive).unwrap()).unwrap();
    assert_eq!(archive_json["format"], "lenso.agent.session-archive@1");

    let database = temporary.path().join("imported/sessions.sqlite3");
    let imported = command(temporary.path())
        .args([
            "sessions",
            "import",
            "--archive",
            archive.to_str().unwrap(),
            "--database",
            database.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );

    let migrated_directory = temporary.path().join("migrated-sessions");
    let migrated = command(temporary.path())
        .args([
            "sessions",
            "migrate",
            "--from-database",
            database.to_str().unwrap(),
            "--to-directory",
            migrated_directory.to_str().unwrap(),
            "--session",
            &session_id,
        ])
        .output()
        .unwrap();
    assert!(
        migrated.status.success(),
        "{}",
        String::from_utf8_lossy(&migrated.stderr)
    );

    let original = lenso_agent_session_sqlite_plugin::SqliteSessionInspector::new(
        temporary.path().join("sessions.sqlite3"),
    )
    .inspect_one(&session_id)
    .unwrap();
    let sqlite = lenso_agent_session_sqlite_plugin::SqliteSessionInspector::new(&database)
        .inspect_one(&session_id)
        .unwrap();
    let migrated = lenso_agent_session_file_plugin::FileSessionInspector::new(&migrated_directory)
        .inspect_one(&session_id)
        .unwrap();
    assert_eq!(sqlite, original);
    assert_eq!(migrated, original);

    let duplicate = command(temporary.path())
        .args([
            "sessions",
            "import",
            "--archive",
            archive.to_str().unwrap(),
            "--database",
            database.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already exists"));
}

#[test]
fn product_runner_accepts_a_positional_prompt_with_the_authoring_plan_environment() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let output = command(temporary.path())
        .env("LENSO_RESOLVED_PLAN", plan_path(temporary.path()))
        .arg("Answer directly: hello")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Direct answer.\n");
}

#[test]
fn product_runner_resolves_a_configured_app_without_cargo_or_a_plan_path() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    configure_fixture_app(temporary.path());

    let output = command(temporary.path())
        .env("CARGO", temporary.path().join("missing-cargo"))
        .arg("Answer directly: hello")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Direct answer.\n");
    assert!(!temporary.path().join("resolved-plan.json").exists());
    assert!(temporary.path().join("plugins").is_dir());
    assert!(temporary.path().join(".lenso/host-catalog.json").is_file());
}

#[test]
fn product_runner_help_leads_with_the_simple_interface_and_plugin_workflow() {
    let output = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("usage: lenso-agent-cli <prompt> [--profile <name>]"));
    assert!(stdout.contains("current directory remains the Workspace"));
    assert!(stdout.contains("Advanced: --prompt <text> and --plan <path> remain available"));
}

#[test]
fn product_runner_rejects_the_removed_named_app_interface() {
    let output = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .args(["--app", "unknown", "hello"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unknown argument `--app`"));
}

#[test]
fn removing_all_optional_prompt_providers_keeps_the_required_base_instruction() {
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
    let session = stored_session(temporary.path());
    let installed = session["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "system_instruction_installed")
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(installed["payload_json"].as_str().unwrap()).unwrap();
    assert_eq!(payload["contributions"].as_array().unwrap().len(), 1);
    assert_eq!(payload["contributions"][0]["id"], "harness.base");
}

#[test]
fn removing_session_presentation_keeps_turn_execution_valid() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let plan = plan_without_session_presentation(temporary.path());
    let output = run(temporary.path(), &plan, "Answer directly: hello", None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Direct answer.\n");
    let session = stored_session(temporary.path());
    let completed = session["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "turn_completed")
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(completed["payload_json"].as_str().unwrap()).unwrap();
    assert!(payload.get("presentation").is_none());
}

#[test]
fn profile_can_select_model_backed_session_title_and_preview() {
    let temporary = tempfile::tempdir().unwrap();
    configure_fixture_app(temporary.path());
    fs::write(
        temporary
            .path()
            .join("plugins/lenso.agent.model.fixture/model.toml"),
        "model = \"fixture/readme-summary-v1\"\nallowed_models = [\"fixture/session-presentation-v1\"]\n",
    )
    .unwrap();
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.session-presentation.model",
        "model = \"fixture/session-presentation-v1\"\n",
    );
    fs::create_dir_all(temporary.path().join("profiles")).unwrap();
    fs::write(
        temporary.path().join("profiles/semantic.toml"),
        r#"description = "Semantic Session presentation"
instances = [
  "lenso.agent.model.fixture/model",
  "lenso.agent.session-presentation.model/default",
]
"#,
    )
    .unwrap();

    let output = command(temporary.path())
        .args([
            "--profile",
            "semantic",
            "--prompt",
            "Answer directly: semantic presentation",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Direct answer.\n");

    let session = stored_session(temporary.path());
    let completed = session["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "turn_completed")
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(completed["payload_json"].as_str().unwrap()).unwrap();
    assert_eq!(payload["presentation"]["title"], "Model-generated title");
    assert_eq!(
        payload["presentation"]["latest_preview"],
        "Model preview: Direct answer."
    );
}

#[test]
fn rejected_presentation_model_does_not_fail_the_completed_turn() {
    let temporary = tempfile::tempdir().unwrap();
    configure_fixture_app(temporary.path());
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.session-presentation.model",
        r#"model = "fixture/unsupported"
instruction = "Create concise Session display metadata."
temperature = 0.0
max_output_tokens = 256
max_input_characters = 524288
max_title_characters = 80
max_preview_characters = 240
"#,
    );
    fs::create_dir_all(temporary.path().join("profiles")).unwrap();
    fs::write(
        temporary.path().join("profiles/rejected-presentation.toml"),
        r#"description = "Rejected presentation model"
instances = [
  "lenso.agent.model.fixture/model",
  "lenso.agent.session-presentation.model/default",
]
"#,
    )
    .unwrap();

    let output = command(temporary.path())
        .args([
            "--profile",
            "rejected-presentation",
            "--prompt",
            "Answer directly: presentation failure",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Direct answer.\n");

    let session = stored_session(temporary.path());
    let completed = session["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "turn_completed")
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(completed["payload_json"].as_str().unwrap()).unwrap();
    assert!(payload.get("presentation").is_none());
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
        "Filesystem: Direct answer.\n"
    );

    let state = stored_session(temporary.path());
    let requested = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "model_requested")
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(requested["payload_json"].as_str().unwrap()).unwrap();
    let contribution = payload["prompt_contributions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|contribution| contribution["id"] == "agents.skills/test-skill")
        .unwrap();
    assert_eq!(contribution["version"].as_str().unwrap().len(), 64);
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
    let state = stored_session(temporary.path());
    assert_eq!(state["revision"], 16);
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
fn delegated_child_records_versioned_result_metadata_and_durable_session() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Plugin Fixture\n").unwrap();
    configure_fixture_app(temporary.path());
    configure_plugin(temporary.path(), "lenso.agent.subagent-tools");

    let output = run_configured_derived(temporary.path(), "Delegate a README.md summary.", None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Delegated result: Child summary: # Plugin Fixture\n"
    );

    let sessions = stored_sessions(temporary.path());
    assert_eq!(sessions.len(), 2);
    let delegate_result = sessions
        .iter()
        .flat_map(|session| session["events"].as_array().unwrap())
        .filter(|event| event["kind"] == "tool_result")
        .find_map(|event| {
            let payload: serde_json::Value =
                serde_json::from_str(event["payload_json"].as_str().unwrap()).unwrap();
            (payload["name"] == "delegate").then_some(payload)
        })
        .expect("parent Session must retain the delegate Tool result");
    let metadata: serde_json::Value =
        serde_json::from_str(delegate_result["metadata_json"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["schema"], "lenso.agent.subagent-result@1");
    assert_eq!(metadata["agent"], "lenso.agent.loop/researcher");
    assert_eq!(metadata["status"], "completed");
    assert_eq!(metadata["context_mode"], "fresh");
    assert_eq!(metadata["task_bytes"], 41);
    assert_eq!(metadata["output_bytes"], 31);
    assert_eq!(metadata["text_delta_count"], 1);
    assert_eq!(metadata["tool_call_count"], 1);

    let child_session_id = metadata["child_session_id"].as_str().unwrap();
    let child = sessions
        .iter()
        .find(|session| session["session_id"] == child_session_id)
        .expect("metadata must locate the durable child Session");
    let child_turn = child["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "turn_started")
        .unwrap();
    let child_turn_payload: serde_json::Value =
        serde_json::from_str(child_turn["payload_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        child_turn_payload["input"],
        "Summarize README.md for the parent Agent."
    );
}

#[test]
fn spawned_child_can_be_joined_by_task_id_without_losing_session_provenance() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Plugin Fixture\n").unwrap();
    configure_fixture_app(temporary.path());
    configure_plugin(temporary.path(), "lenso.agent.subagent-tools");

    let output = run_configured_derived(
        temporary.path(),
        "Spawn and wait for a README.md subagent.",
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Asynchronous result: Child summary: # Plugin Fixture\n"
    );

    let sessions = stored_sessions(temporary.path());
    assert_eq!(sessions.len(), 2);
    let tool_results = sessions
        .iter()
        .flat_map(|session| session["events"].as_array().unwrap())
        .filter(|event| event["kind"] == "tool_result")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let spawned = tool_results
        .iter()
        .find(|result| result["name"] == "spawn_subagent")
        .expect("parent Session must retain the spawned task ID");
    let spawn_metadata: serde_json::Value =
        serde_json::from_str(spawned["metadata_json"].as_str().unwrap()).unwrap();
    assert_eq!(spawn_metadata["schema"], "lenso.agent.subagent-task@1");
    assert_eq!(spawn_metadata["agent"], "lenso.agent.loop/researcher");
    assert_eq!(spawn_metadata["status"], "running");
    let task_id = spawn_metadata["task_id"].as_str().unwrap();

    let waited = tool_results
        .iter()
        .find(|result| result["name"] == "wait_subagent")
        .expect("parent Session must retain the joined child result");
    let wait_metadata: serde_json::Value =
        serde_json::from_str(waited["metadata_json"].as_str().unwrap()).unwrap();
    assert_eq!(wait_metadata["schema"], "lenso.agent.subagent-result@1");
    assert_eq!(wait_metadata["agent"], "lenso.agent.loop/researcher");
    assert_eq!(wait_metadata["status"], "completed");
    assert_eq!(wait_metadata["task_id"], task_id);
    let child_session_id = wait_metadata["child_session_id"].as_str().unwrap();
    assert!(
        sessions
            .iter()
            .any(|session| session["session_id"] == child_session_id)
    );
}

#[test]
fn running_child_accepts_input_only_after_the_session_fact_is_durable() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Plugin Fixture\n").unwrap();
    configure_fixture_app(temporary.path());
    configure_plugin(temporary.path(), "lenso.agent.subagent-tools");

    let output = run_configured_derived(
        temporary.path(),
        "Spawn, steer, and wait for a README.md subagent.",
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Steered result: Steered child summary: # Plugin Fixture\n"
    );

    let sessions = stored_sessions(temporary.path());
    let tool_results = sessions
        .iter()
        .flat_map(|session| session["events"].as_array().unwrap())
        .filter(|event| event["kind"] == "tool_result")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let sent = tool_results
        .iter()
        .find(|result| result["name"] == "send_subagent")
        .expect("parent Session must retain the accepted send_subagent result");
    let acceptance: serde_json::Value =
        serde_json::from_str(sent["content"].as_str().unwrap()).unwrap();
    assert_eq!(acceptance["status"], "input_accepted");
    let child_session_id = acceptance["child_session_id"].as_str().unwrap();
    let accepted_revision = acceptance["accepted_revision"].as_str().unwrap();

    let child = sessions
        .iter()
        .find(|session| session["session_id"] == child_session_id)
        .expect("accepted input must identify its durable child Session");
    let accepted_event = child["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| {
            event["revision"]
                .as_u64()
                .map(|revision| revision.to_string())
                .as_deref()
                == Some(accepted_revision)
                && event["kind"] == "model_requested"
                && serde_json::from_str::<serde_json::Value>(
                    event["payload_json"].as_str().unwrap(),
                )
                .is_ok_and(|payload| {
                    payload["additional_inputs"] == serde_json::json!(["Emphasize the heading."])
                })
        })
        .expect("acceptance revision must point at the durable additional input fact");
    assert_eq!(accepted_event["turn_id"].as_str().unwrap().len(), 36);
}

#[test]
fn cancelling_a_spawned_child_does_not_cancel_the_parent_turn() {
    let temporary = tempfile::tempdir().unwrap();
    configure_fixture_app(temporary.path());
    configure_plugin(temporary.path(), "lenso.agent.subagent-tools");

    let output = run_configured_derived(
        temporary.path(),
        "Spawn and cancel a pending subagent.",
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Pending subagent cancelled without cancelling the parent Turn.\n"
    );

    let sessions = stored_sessions(temporary.path());
    assert_eq!(sessions.len(), 2);
    let tool_results = sessions
        .iter()
        .flat_map(|session| session["events"].as_array().unwrap())
        .filter(|event| event["kind"] == "tool_result")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let cancelled = tool_results
        .iter()
        .find(|result| result["name"] == "cancel_subagent")
        .unwrap();
    let cancel_metadata: serde_json::Value =
        serde_json::from_str(cancelled["metadata_json"].as_str().unwrap()).unwrap();
    assert_eq!(cancel_metadata["status"], "cancellation_requested");
    assert_eq!(cancel_metadata["agent"], "lenso.agent.loop/reviewer");
    let listed = tool_results
        .iter()
        .find(|result| result["name"] == "list_subagents")
        .unwrap();
    let listed_content: serde_json::Value =
        serde_json::from_str(listed["content"].as_str().unwrap()).unwrap();
    let listed_metadata: serde_json::Value =
        serde_json::from_str(listed["metadata_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        listed_metadata["schema"],
        "lenso.agent.task-supervisor-snapshot@1"
    );
    assert_eq!(listed_metadata["task_count"], 1);
    assert_eq!(listed_content["task_count"], 1);
    assert_eq!(listed_content["tasks"][0]["status"], "running");
    assert_eq!(
        listed_content["tasks"][0]["agent"],
        "lenso.agent.loop/reviewer"
    );
    assert_eq!(
        listed_content["tasks"][0]["task_id"],
        cancel_metadata["task_id"]
    );
    assert!(listed_content["tasks"][0]["owner"]["session_id"].is_string());
    assert!(listed_content["tasks"][0]["owner"]["turn_id"].is_string());
    assert!(listed_content["tasks"][0]["owner"]["tool_call_id"].is_string());
    assert!(listed_content["tasks"][0]["generation_spec_digest"].is_string());
    assert_eq!(
        listed_content["tasks"][0]["workspace"],
        temporary
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    let waited = tool_results
        .iter()
        .find(|result| result["name"] == "wait_subagent")
        .unwrap();
    let wait_metadata: serde_json::Value =
        serde_json::from_str(waited["metadata_json"].as_str().unwrap()).unwrap();
    assert_eq!(wait_metadata["status"], "cancelled");
    assert_eq!(wait_metadata["task_id"], cancel_metadata["task_id"]);
}

#[test]
fn delegated_output_limit_failure_retains_child_session_provenance() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Plugin Fixture\n").unwrap();
    configure_fixture_app(temporary.path());
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.subagent-tools",
        "max_output_bytes = 8\nmax_task_bytes = 262144\nmax_tasks = 8\n",
    );

    let output = run_configured_derived(temporary.path(), "Delegate a README.md summary.", None);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("child_output_limit_exceeded"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let sessions = stored_sessions(temporary.path());
    assert_eq!(sessions.len(), 2);
    let failed_result = sessions
        .iter()
        .flat_map(|session| session["events"].as_array().unwrap())
        .filter(|event| event["kind"] == "tool_result")
        .find_map(|event| {
            let payload: serde_json::Value =
                serde_json::from_str(event["payload_json"].as_str().unwrap()).unwrap();
            (payload["name"] == "delegate").then_some(payload)
        })
        .expect("parent Session must retain the failed delegate Tool result");
    let error = failed_result["error"].as_str().unwrap();
    assert!(error.contains("child_output_limit_exceeded"));
    let child_session_id = sessions
        .iter()
        .find(|session| {
            session["events"].as_array().unwrap().iter().any(|event| {
                event["kind"] == "turn_started"
                    && serde_json::from_str::<serde_json::Value>(
                        event["payload_json"].as_str().unwrap(),
                    )
                    .is_ok_and(|payload| {
                        payload["input"] == "Summarize README.md for the parent Agent."
                    })
            })
        })
        .unwrap()["session_id"]
        .as_str()
        .unwrap();
    assert!(error.contains(child_session_id));
}

#[test]
fn headless_ask_user_fails_immediately_instead_of_waiting_for_a_timeout() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let started = std::time::Instant::now();
    let output = run(
        temporary.path(),
        &plan_path(temporary.path()),
        "Ask me which mode to use.",
        None,
    );

    assert!(!output.status.success());
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("interaction_unavailable"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn readonly_navigation_lists_searches_then_reads_the_selected_file() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    fs::create_dir(temporary.path().join("docs")).unwrap();
    fs::write(
        temporary.path().join("docs/guide.md"),
        "NAVIGATION_TARGET: bounded workspace discovery.\n",
    )
    .unwrap();
    let output = run(
        temporary.path(),
        &plan_path(temporary.path()),
        "Navigate the workspace to find the navigation target.",
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Navigation result: NAVIGATION_TARGET: bounded workspace discovery.\n"
    );

    let state = stored_session(temporary.path());
    let requests = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "tool_requested")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requests
            .iter()
            .map(|request| request["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["list", "search", "read"]
    );
}

#[test]
fn workspace_edit_plugin_creates_edits_then_reads_back_one_file() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.workspace-edit",
        "root = \".\"\nmax_file_bytes = 1048576\nmax_edit_bytes = 131072\n",
    );
    let output = run_derived(temporary.path(), "Create and edit a workspace note.", None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Workspace mutation result: after\n"
    );
    assert_eq!(
        fs::read_to_string(temporary.path().join("note.txt")).unwrap(),
        "after\n"
    );

    let state = stored_session(temporary.path());
    let events = state["events"].as_array().unwrap();
    let requests = events
        .iter()
        .filter(|event| event["kind"] == "tool_requested")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requests
            .iter()
            .map(|request| request["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["create_file", "edit", "read",]
    );
    let mutation_results = events
        .iter()
        .filter(|event| event["kind"] == "tool_result")
        .take(2)
        .map(|event| {
            let payload =
                serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                    .unwrap();
            serde_json::from_str::<serde_json::Value>(payload["metadata_json"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(mutation_results[0]["operation"], "created");
    assert_eq!(mutation_results[1]["operation"], "edited");
    assert_eq!(mutation_results[1]["sha256"].as_str().unwrap().len(), 64);
}

#[test]
fn local_coding_profile_edits_checks_and_reads_back_a_rust_project() {
    let temporary = tempfile::tempdir().unwrap();
    fs::create_dir(temporary.path().join("src")).unwrap();
    fs::write(
        temporary.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        temporary.path().join("src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    )
    .unwrap();
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.process.native",
        "root = \".\"\nallowed_programs = [\"cargo\", \"git\", \"rg\"]\nenvironment_allowlist = [\"PATH\", \"HOME\", \"CARGO_HOME\", \"RUSTUP_HOME\", \"TMPDIR\", \"LANG\", \"LC_ALL\"]\nmax_timeout_ms = 600000\nmax_output_bytes = 262144\nmax_argument_bytes = 131072\n",
    );
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.process-tools",
        "default_timeout_ms = 120000\nmax_background_processes = 8\nmax_background_log_bytes = 262144\n",
    );
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.workspace-edit",
        "root = \".\"\nmax_file_bytes = 1048576\nmax_edit_bytes = 131072\n",
    );
    let output = run_derived(
        temporary.path(),
        "Edit and validate the workspace project.",
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Local coding result: cargo check passed.\n"
    );
    assert_eq!(
        fs::read_to_string(temporary.path().join("src/lib.rs")).unwrap(),
        "pub fn value() -> u32 { 2 }\n"
    );

    let state = stored_session(temporary.path());
    let events = state["events"].as_array().unwrap();
    let requests = events
        .iter()
        .filter(|event| event["kind"] == "tool_requested")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requests
            .iter()
            .map(|request| request["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["edit", "run_process", "read"]
    );
    let process_result = events
        .iter()
        .filter(|event| event["kind"] == "tool_result")
        .nth(1)
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(process_result["payload_json"].as_str().unwrap()).unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(payload["metadata_json"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["program"], "cargo");
    assert_eq!(metadata["exit_code"], "0");
}

#[test]
fn background_process_handles_retain_logs_and_append_a_durable_terminal_fact() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Background fixture\n").unwrap();
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.process.native",
        "root = \".\"\nallowed_programs = [\"sh\"]\nenvironment_allowlist = [\"PATH\"]\nmax_timeout_ms = 600000\nmax_output_bytes = 262144\nmax_argument_bytes = 131072\n",
    );
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.process-tools",
        "default_timeout_ms = 120000\nmax_background_processes = 2\nmax_background_log_bytes = 4096\n",
    );

    let output = run_derived(
        temporary.path(),
        "Run and observe one background process.",
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Background process completed with durable terminal facts.\n"
    );

    let state = stored_session(temporary.path());
    let terminal = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "tool_result")
        .filter_map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str()?).ok()
        })
        .find(|payload| payload["name"] == "background_process")
        .expect("background terminal fact must be durable");
    assert_eq!(terminal["status"], "completed");
    assert!(
        terminal["content"]
            .as_str()
            .unwrap()
            .contains("background-output")
    );
    let metadata: serde_json::Value =
        serde_json::from_str(terminal["metadata_json"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["schema"], "lenso.agent.background-process@1");
    assert_eq!(metadata["process"]["logs_truncated"], false);
}

#[test]
fn background_process_cancellation_is_detached_and_durable() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Cancel fixture\n").unwrap();
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.process.native",
        "root = \".\"\nallowed_programs = [\"sh\"]\nenvironment_allowlist = [\"PATH\"]\nmax_timeout_ms = 600000\nmax_output_bytes = 262144\nmax_argument_bytes = 131072\n",
    );
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.process-tools",
        "default_timeout_ms = 120000\nmax_background_processes = 2\nmax_background_log_bytes = 4096\n",
    );

    let output = run_derived(temporary.path(), "Cancel one background process.", None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Background process cancellation became durable.\n"
    );

    let state = stored_session(temporary.path());
    let terminal = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "tool_result")
        .filter_map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str()?).ok()
        })
        .find(|payload| payload["name"] == "background_process" && payload["status"] == "cancelled")
        .expect("cancelled process terminal fact must be durable");
    let metadata: serde_json::Value =
        serde_json::from_str(terminal["metadata_json"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["process"]["cancel_requested"], true);
    assert!(matches!(
        metadata["process"]["reason_code"].as_str(),
        Some("terminated" | "runtime_cancelled" | "runtime_admission_closed")
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn sandbox_coding_profile_runs_a_real_cargo_check_inside_seatbelt() {
    let temporary = tempfile::tempdir().unwrap();
    fs::create_dir(temporary.path().join("src")).unwrap();
    fs::write(
        temporary.path().join("Cargo.toml"),
        "[package]\nname = \"sandbox-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        temporary.path().join("src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    )
    .unwrap();
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.process.sandbox",
        "root = \".\"\nbackend = \"seatbelt\"\nallow_network = false\nallowed_programs = [\"cargo\"]\nenvironment_allowlist = [\"PATH\", \"HOME\", \"CARGO_HOME\", \"RUSTUP_HOME\", \"LANG\", \"LC_ALL\"]\nmax_timeout_ms = 600000\nmax_output_bytes = 262144\nmax_argument_bytes = 131072\n",
    );
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.process-tools",
        "default_timeout_ms = 120000\nmax_background_processes = 8\nmax_background_log_bytes = 262144\n",
    );
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.workspace-edit",
        "root = \".\"\nmax_file_bytes = 1048576\nmax_edit_bytes = 131072\n",
    );

    let output = run_derived(
        temporary.path(),
        "Edit and validate the workspace project.",
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Local coding result: cargo check passed.\n"
    );
    assert_eq!(
        fs::read_to_string(temporary.path().join("src/lib.rs")).unwrap(),
        "pub fn value() -> u32 { 2 }\n"
    );
    let state = stored_session(temporary.path());
    let process_result = state["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "tool_result")
        .nth(1)
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(process_result["payload_json"].as_str().unwrap()).unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(payload["metadata_json"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["program"], "cargo");
    assert_eq!(metadata["exit_code"], "0");
}

#[test]
fn git_plugin_inspects_stages_commits_and_reads_history_in_a_real_repository() {
    let temporary = tempfile::tempdir().unwrap();
    let git = |arguments: &[&str]| {
        let output = Command::new("git")
            .current_dir(temporary.path())
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    git(&["init", "--quiet"]);
    git(&["config", "user.name", "Fixture User"]);
    git(&["config", "user.email", "fixture@example.com"]);
    fs::write(temporary.path().join("note.txt"), "before\n").unwrap();
    git(&["add", "note.txt"]);
    git(&["commit", "--quiet", "-m", "test: initial"]);
    fs::write(temporary.path().join("note.txt"), "after\n").unwrap();

    let hook = temporary.path().join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 91\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }

    configure_plugin_with(
        temporary.path(),
        "lenso.agent.process.native",
        "root = \".\"\nallowed_programs = [\"git\"]\nenvironment_allowlist = [\"PATH\", \"HOME\", \"TMPDIR\", \"LANG\", \"LC_ALL\"]\nmax_timeout_ms = 600000\nmax_output_bytes = 262144\nmax_argument_bytes = 131072\n",
    );
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.git-tools",
        "default_timeout_ms = 30000\nmax_log_entries = 50\nmax_commit_message_bytes = 4096\n",
    );

    let output = run_derived(
        temporary.path(),
        "Inspect and commit the prepared Git change.",
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Git Plugin result: prepared change committed.\n"
    );
    let log = git(&["log", "-1", "--pretty=%s"]);
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).trim(),
        "test: bounded Git Plugin commit"
    );
    assert!(
        String::from_utf8_lossy(&git(&["status", "--porcelain", "--", "note.txt"]).stdout)
            .trim()
            .is_empty()
    );

    let session = stored_session(temporary.path());
    let requested = session["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "tool_requested")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()["name"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requested,
        ["git_status", "git_stage", "git_commit", "git_log"]
    );
}

#[test]
fn git_plugin_fails_readiness_when_process_does_not_authorize_git() {
    let temporary = tempfile::tempdir().unwrap();
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.process.native",
        "root = \".\"\nallowed_programs = [\"cargo\"]\nenvironment_allowlist = [\"PATH\", \"HOME\", \"TMPDIR\", \"LANG\", \"LC_ALL\"]\nmax_timeout_ms = 600000\nmax_output_bytes = 262144\nmax_argument_bytes = 131072\n",
    );
    configure_plugin_with(
        temporary.path(),
        "lenso.agent.git-tools",
        "default_timeout_ms = 30000\nmax_log_entries = 50\nmax_commit_message_bytes = 4096\n",
    );

    let output = run_derived(
        temporary.path(),
        "Inspect and commit the prepared Git change.",
        None,
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Git Tools requires its Process provider to authorize `git`"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn on_demand_skill_catalog_lists_then_reads_only_the_selected_skill() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let skill_root = temporary.path().join("skills");
    write_on_demand_skills(&skill_root);
    let plan = plan_with_on_demand_skills(temporary.path(), &skill_root);
    let output = run(temporary.path(), &plan, "Use a Skill to review Rust.", None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Skill applied: Rust review used the selected instructions.\n"
    );

    let state = stored_session(temporary.path());
    let events = state["events"].as_array().unwrap();
    let requests = events
        .iter()
        .filter(|event| event["kind"] == "tool_requested")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["name"], "skill");

    let results = events
        .iter()
        .filter(|event| event["kind"] == "tool_result")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 1);
    let read_metadata =
        serde_json::from_str::<serde_json::Value>(results[0]["metadata_json"].as_str().unwrap())
            .unwrap();
    assert_eq!(read_metadata["name"], "rust-review");
    assert!(results[0].to_string().contains("sha256:"));
    let model_requested = events
        .iter()
        .find(|event| event["kind"] == "model_requested")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()
        })
        .unwrap();
    assert!(
        model_requested["prompt_contributions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|contribution| contribution["id"] == "agents.skills.catalog")
    );
}

#[test]
fn skill_resources_are_listed_then_one_resource_is_read_without_executing_scripts() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let skill_root = temporary.path().join("skills");
    write_on_demand_skills(&skill_root);
    let plan = plan_with_on_demand_skills(temporary.path(), &skill_root);
    let output = run(
        temporary.path(),
        &plan,
        "Use a Skill resource to review Rust.",
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Resource applied: Rust review used references/checklist.md.\n"
    );
    assert!(!temporary.path().join("resource-script-executed").exists());

    let state = stored_session(temporary.path());
    let events = state["events"].as_array().unwrap();
    let requests = events
        .iter()
        .filter(|event| event["kind"] == "tool_requested")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requests
            .iter()
            .map(|request| request["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["skill", "skill_resources", "skill_resource",]
    );
    let read_result = events
        .iter()
        .rfind(|event| event["kind"] == "tool_result")
        .map(|event| {
            serde_json::from_str::<serde_json::Value>(event["payload_json"].as_str().unwrap())
                .unwrap()
        })
        .unwrap();
    let metadata =
        serde_json::from_str::<serde_json::Value>(read_result["metadata_json"].as_str().unwrap())
            .unwrap();
    assert_eq!(metadata["name"], "rust-review");
    assert_eq!(metadata["path"], "references/checklist.md");
    assert!(metadata["digest"].as_str().unwrap().starts_with("sha256:"));
}

#[test]
fn missing_on_demand_skill_root_starts_with_an_empty_catalog() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let missing = temporary.path().join("missing-skills");
    let plan = plan_with_on_demand_skills(temporary.path(), &missing);
    let output = run(temporary.path(), &plan, "Answer directly: hello", None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Direct answer.\n");
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
    let state = stored_session(temporary.path());
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
        &plan_path(temporary.path()),
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
    fs::create_dir(temporary.path().join("sessions.sqlite3")).unwrap();
    let output = run(
        temporary.path(),
        &plan_path(temporary.path()),
        "Summarize the README.",
        None,
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("App startup failed"), "{stderr}");
    assert!(
        stderr.contains("Session database is not a regular file"),
        "{stderr}"
    );
}

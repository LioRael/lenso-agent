use std::{fs, path::Path, process::Command};

use lenso_agent_cli_plugin as _;
use lenso_plugin_control_plane::sha256_digest;

#[path = "../../../tests/support/mod.rs"]
mod support;

fn plan_path() -> std::path::PathBuf {
    support::plan("base")
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

fn run_derived(root: &Path, prompt: &str, session: Option<&str>) -> std::process::Output {
    configure_fixture_app(root);
    let mut command = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"));
    command.current_dir(root).arg(prompt);
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
    let path = stored_session_path(root);
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn stored_session_path(root: &Path) -> std::path::PathBuf {
    fs::read_dir(root.join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
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

fn plan_with_limits(root: &Path, max_steps: u64, max_tool_calls: u64) -> std::path::PathBuf {
    let mut plan = serde_json::from_slice::<serde_json::Value>(&fs::read(plan_path()).unwrap())
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
    let mut plan = serde_json::from_slice::<serde_json::Value>(&fs::read(plan_path()).unwrap())
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

fn plan_with_filesystem_skill(root: &Path, skill_root: &Path) -> std::path::PathBuf {
    let mut plan = serde_json::from_slice::<serde_json::Value>(&fs::read(plan_path()).unwrap())
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
    let mut plan = serde_json::from_slice::<serde_json::Value>(&fs::read(plan_path()).unwrap())
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
    assert_eq!(state["revision"], 13);
    assert_eq!(state["events"].as_array().unwrap().len(), 13);
    assert_eq!(
        state["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "system_instruction_installed")
            .count(),
        1
    );
}

#[test]
fn resumed_session_reuses_its_installed_system_instruction() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let first = run(
        temporary.path(),
        &plan_path(),
        "Answer directly: first",
        None,
    );
    assert!(first.status.success());
    let first_stderr = String::from_utf8(first.stderr).unwrap();
    let session_id = first_stderr.trim().strip_prefix("session: ").unwrap();

    let path = stored_session_path(temporary.path());
    let mut state = stored_session(temporary.path());
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

    let second = run(
        temporary.path(),
        &plan_path(),
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
    let first = run(
        temporary.path(),
        &plan_path(),
        "Answer directly: first",
        None,
    );
    assert!(first.status.success());
    let first_stderr = String::from_utf8(first.stderr).unwrap();
    let session_id = first_stderr.trim().strip_prefix("session: ").unwrap();

    let path = stored_session_path(temporary.path());
    let mut state = stored_session(temporary.path());
    let events = state["events"].as_array_mut().unwrap();
    events.retain(|event| event["kind"] != "system_instruction_installed");
    for (index, event) in events.iter_mut().enumerate() {
        event["revision"] = u64::try_from(index + 1).unwrap().into();
    }
    state["revision"] = u64::try_from(events.len()).unwrap().into();
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

    let second = run(
        temporary.path(),
        &plan_path(),
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
    let first = run_derived(temporary.path(), "Answer directly: hello", None);
    assert!(first.status.success());
    let first_stderr = String::from_utf8(first.stderr).unwrap();
    let session_id = first_stderr.trim().strip_prefix("session: ").unwrap();

    let path = stored_session_path(temporary.path());
    let mut session = stored_session(temporary.path());
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
    let output = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .args(["--plan", plan_path().to_str().unwrap()])
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

#[test]
fn resumed_session_records_each_host_generation_and_keeps_its_specs() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Generation Fixture\n").unwrap();
    let plan = plan_path();
    let first = run(temporary.path(), &plan, "Answer directly: hello", None);
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
    assert_eq!(digests.len(), 2);
    assert_ne!(digests[0], digests[1]);
    let provenance = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .args(["sessions", "provenance", "--session", session_id])
        .output()
        .unwrap();
    assert!(provenance.status.success());
    let provenance_stdout = String::from_utf8(provenance.stdout).unwrap();
    for digest in &digests {
        assert!(provenance_stdout.contains(&format!("generation={digest} spec=available")));
        let inspect = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
            .current_dir(temporary.path())
            .args(["generations", "inspect", "--digest", digest])
            .output()
            .unwrap();
        assert!(inspect.status.success());
        let inspect_stdout = String::from_utf8(inspect.stdout).unwrap();
        assert!(inspect_stdout.contains(&format!("generation: {digest}")));
        assert!(inspect_stdout.contains("app: lenso.agent.harness"));
        assert!(inspect_stdout.contains("resolution-authority: sha256:"));
    }
    for digest in digests {
        let hash = digest.strip_prefix("sha256:").unwrap();
        let record = temporary
            .path()
            .join(".lenso/runtime/generations")
            .join(format!("{hash}.json"));
        let bytes = fs::read(record).unwrap();
        assert_eq!(sha256_digest(&bytes), digest);
        let spec: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(spec["app_id"], "lenso.agent.harness");
    }
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
    let digests = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .map(|entry| {
            let session: serde_json::Value =
                serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap();
            turn_generation_digests(&session).pop().unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(digests.len(), 2);

    fs::remove_dir_all(temporary.path().join("plugins/lenso.agent.text-tools")).unwrap();
    let empty_sessions = temporary.path().join("empty-sessions");
    fs::create_dir(&empty_sessions).unwrap();
    let before_reconcile = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
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
    let preview = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
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
    let records_before = fs::read_dir(temporary.path().join(".lenso/runtime/generations"))
        .unwrap()
        .count();

    let preview_with_sessions = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .args(["generations", "gc-preview"])
        .output()
        .unwrap();
    assert!(preview_with_sessions.status.success());
    assert!(
        String::from_utf8_lossy(&preview_with_sessions.stdout)
            .contains("summary: protected=2 candidates=0")
    );
    assert_eq!(
        fs::read_dir(temporary.path().join(".lenso/runtime/generations"))
            .unwrap()
            .count(),
        records_before
    );

    let apply = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
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
        fs::read_dir(temporary.path().join(".lenso/runtime/generations"))
            .unwrap()
            .count(),
        records_before - 1
    );

    let second_apply = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
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
    let first = run(
        temporary.path(),
        &plan_path(),
        "Answer directly: hello",
        None,
    );
    assert!(first.status.success());
    let before = stored_session(temporary.path());
    let digest = turn_generation_digests(&before).pop().unwrap();
    let record = temporary
        .path()
        .join(".lenso/runtime/generations")
        .join(format!("{}.json", digest.strip_prefix("sha256:").unwrap()));
    fs::write(record, "{}").unwrap();

    let session_id = before["session_id"].as_str().unwrap_or_default();
    let inspect = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .args(["sessions", "provenance", "--session", session_id])
        .output()
        .unwrap();
    assert!(inspect.status.success());
    assert!(String::from_utf8_lossy(&inspect.stdout).contains("spec=invalid"));

    let gc_preview = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .args(["generations", "gc-preview"])
        .output()
        .unwrap();
    assert!(!gc_preview.status.success());
    assert!(
        String::from_utf8_lossy(&gc_preview.stderr).contains("Generation Spec validation failed")
    );

    let second = run(
        temporary.path(),
        &plan_path(),
        "Answer directly: this must not run",
        None,
    );
    assert!(!second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("App startup failed"));
    assert!(stderr.contains("does not match its digest"));
    assert_eq!(stored_session(temporary.path()), before);
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
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Direct answer.\n");
    let session = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state: serde_json::Value = serde_json::from_slice(&fs::read(session).unwrap()).unwrap();
    assert_eq!(state["revision"], 6);
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
fn lifecycle_audit_observes_session_start_resume_and_turn_start() {
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

    let audit = fs::read_to_string(temporary.path().join(".lenso/lifecycle/events.jsonl")).unwrap();
    let events = audit
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0]["kind"], "session_started");
    assert_eq!(events[1]["kind"], "turn_started");
    assert_eq!(events[2]["kind"], "session_resumed");
    assert_eq!(events[3]["kind"], "turn_started");
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
    fs::write(
        sqlite_configuration,
        "database = \".lenso/sessions.sqlite3\"\n",
    )
    .unwrap();
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
    let output = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .args(["--profile", "sqlite", "--prompt", "Answer directly: sqlite"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(temporary.path().join(".lenso/sessions.sqlite3").is_file());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let session_id = stderr.trim().strip_prefix("session: ").unwrap();
    let provenance = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .args([
            "sessions",
            "provenance",
            "--session",
            session_id,
            "--database",
            ".lenso/sessions.sqlite3",
        ])
        .output()
        .unwrap();
    assert!(
        provenance.status.success(),
        "{}",
        String::from_utf8_lossy(&provenance.stderr)
    );
    assert!(String::from_utf8_lossy(&provenance.stdout).contains("generation=sha256:"));
    let gc = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .args([
            "generations",
            "gc-preview",
            "--session-database",
            ".lenso/sessions.sqlite3",
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
fn product_runner_accepts_a_positional_prompt_with_the_authoring_plan_environment() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
        .env("LENSO_RESOLVED_PLAN", plan_path())
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

    let output = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(temporary.path())
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
    assert!(!temporary.path().join(".lenso/resolved-plan.json").exists());
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
    assert!(stdout.contains("Host defaults boot with an empty `plugins/` directory"));
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
    let session = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state: serde_json::Value = serde_json::from_slice(&fs::read(session).unwrap()).unwrap();
    assert_eq!(state["revision"], 12);
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
fn headless_ask_user_fails_immediately_instead_of_waiting_for_a_timeout() {
    let temporary = tempfile::tempdir().unwrap();
    fs::write(temporary.path().join("README.md"), "# Fixture\n").unwrap();
    let started = std::time::Instant::now();
    let output = run(
        temporary.path(),
        &plan_path(),
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
        &plan_path(),
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

    let session = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state: serde_json::Value = serde_json::from_slice(&fs::read(session).unwrap()).unwrap();
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

    let session = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state: serde_json::Value = serde_json::from_slice(&fs::read(session).unwrap()).unwrap();
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
        "default_timeout_ms = 120000\n",
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

    let session = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state: serde_json::Value = serde_json::from_slice(&fs::read(session).unwrap()).unwrap();
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

    let session = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state: serde_json::Value = serde_json::from_slice(&fs::read(session).unwrap()).unwrap();
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

    let session = fs::read_dir(temporary.path().join(".lenso/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let state: serde_json::Value = serde_json::from_slice(&fs::read(session).unwrap()).unwrap();
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("App resolution failed"));
}

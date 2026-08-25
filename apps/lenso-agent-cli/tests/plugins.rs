use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::Command,
    thread,
};

use lenso_plugin_bundle::{ArtifactSource, BundleBuild, build_bundle};
use lenso_plugin_control_plane::sha256_digest;

mod support;

fn plan_path() -> std::path::PathBuf {
    support::plan("base")
}

#[test]
fn bundled_plugin_catalog_is_visible_without_product_app_variants() {
    let available = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .args(["plugins", "available"])
        .output()
        .unwrap();

    assert!(available.status.success());
    let stdout = String::from_utf8(available.stdout).unwrap();
    assert!(stdout.contains("text-tools       stable"));
    assert!(stdout.contains("workspace-edit   experimental"));
    assert!(stdout.contains("skills           experimental"));
    assert!(stdout.contains("local-process    experimental"));
    assert!(stdout.contains("openai-compatible experimental"));
}

#[test]
fn skills_plugin_closes_prompt_and_tool_bindings_from_one_selection() {
    let workspace = tempfile::tempdir().unwrap();
    let skill = workspace.path().join(".agents/skills/fixture");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: fixture\ndescription: Fixture skill\n---\n\nUse this fixture skill.\n",
    )
    .unwrap();

    let enable = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .env("HOME", workspace.path())
        .args(["plugins", "enable", "skills", "--evidence"])
        .arg("reviewed local Skills catalog")
        .arg("--plan")
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(
        enable.status.success(),
        "{}",
        String::from_utf8_lossy(&enable.stderr)
    );
    let active: serde_json::Value = serde_json::from_slice(
        &fs::read(workspace.path().join(".lenso/plugins/active-set.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(active["lock"]["instances"].as_array().unwrap().len(), 1);

    let disable = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .env("HOME", workspace.path())
        .args(["plugins", "disable", "skills", "--plan"])
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(
        disable.status.success(),
        "{}",
        String::from_utf8_lossy(&disable.stderr)
    );
}

#[test]
fn openai_compatible_plugin_replaces_model_and_closes_secrets() {
    let workspace = tempfile::tempdir().unwrap();
    let enable = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .env("OPENAI_API_KEY", "ready-gate-fixture")
        .args(["plugins", "enable", "openai-compatible", "--evidence"])
        .arg("reviewed OpenAI-compatible provider")
        .arg("--plan")
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(
        enable.status.success(),
        "{}",
        String::from_utf8_lossy(&enable.stderr)
    );
    let active: serde_json::Value = serde_json::from_slice(
        &fs::read(workspace.path().join(".lenso/plugins/active-set.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(active["lock"]["instances"].as_array().unwrap().len(), 2);

    let run = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["--plan"])
        .arg(plan_path())
        .arg("Answer directly: hello")
        .output()
        .unwrap();
    assert!(!run.status.success());
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("configured secret reference"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

fn fixture_model_bundle_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/model-fixture")
}

fn codex_direct_bundle_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/codex-direct")
}

#[test]
fn cli_installs_lists_and_runs_with_a_reviewed_passive_release() {
    let _plugin_test_guard = plugin_test_guard();
    let workspace = tempfile::tempdir().unwrap();
    let bundle = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("README.md"), "# Plugin Fixture\n").unwrap();
    write_passive_bundle(bundle.path());

    let install = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "install", "--bundle"])
        .arg(bundle.path())
        .args(["--feature", "extras"])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let install_stdout = String::from_utf8(install.stdout).unwrap();
    assert!(install_stdout.contains("installed: example.passive@1.0.0"));
    assert!(install_stdout.contains("receipt: sha256:"));
    assert!(install_stdout.contains("governance: automatic:local-passive-release"));
    assert!(
        workspace
            .path()
            .join(".lenso/plugins/active-set.json")
            .is_file()
    );

    let status = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "status"])
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("example.passive@1.0.0 sha256:"));

    let run = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["--plan"])
        .arg(plan_path())
        .args(["--prompt", "Answer directly: hello"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "Plugin: Direct answer.\n"
    );
}

#[test]
fn provenance_inspection_does_not_create_missing_authority() {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().join("missing-plugins");
    let history = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "history", "--root"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(!history.status.success());
    assert!(!root.exists());
}

#[test]
fn reviewed_native_tool_plugin_executes_and_remove_deletes_the_capability() {
    let _plugin_test_guard = plugin_test_guard();
    let workspace = tempfile::tempdir().unwrap();
    let bundle = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("README.md"), "# Plugin Fixture\n").unwrap();
    write_tool_bundle(bundle.path());

    let install = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "install", "--bundle"])
        .arg(bundle.path())
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(
        String::from_utf8_lossy(&install.stdout)
            .contains("governance: automatic:local-trusted-stateless-append-many")
    );

    let run = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["--plan"])
        .arg(plan_path())
        .args(["--prompt", "Use the text Plugin to uppercase Lenso plugin."])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "Text Plugin result: LENSO PLUGIN\n"
    );

    let remove = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "remove", "--plugin", "example.text-tools"])
        .output()
        .unwrap();
    assert!(remove.status.success());
    assert!(String::from_utf8_lossy(&remove.stdout).contains("removed: example.text-tools"));

    let after_remove = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["--plan"])
        .arg(plan_path())
        .args(["--prompt", "Use the text Plugin to uppercase Lenso plugin."])
        .output()
        .unwrap();
    assert!(!after_remove.status.success());
    assert!(
        String::from_utf8_lossy(&after_remove.stderr).contains("InvalidRequest"),
        "{}",
        String::from_utf8_lossy(&after_remove.stderr)
    );
}

#[test]
fn reviewed_workspace_edit_plugin_composes_with_another_enabled_tool_plugin() {
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("README.md"), "# Plugin Fixture\n").unwrap();

    let text_install = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "enable", "text-tools"])
        .args(["--plan"])
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(
        text_install.status.success(),
        "{}",
        String::from_utf8_lossy(&text_install.stderr)
    );

    let edit_install = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "enable", "workspace-edit"])
        .args(["--evidence", "reviewed-workspace-mutation"])
        .args(["--plan"])
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(
        edit_install.status.success(),
        "{}",
        String::from_utf8_lossy(&edit_install.stderr)
    );
    assert!(
        String::from_utf8_lossy(&edit_install.stdout)
            .contains("release: lenso.workspace-edit@1.0.0")
    );

    let status = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "status"])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status_stdout = String::from_utf8(status.stdout).unwrap();
    assert!(status_stdout.contains("example.text-tools@1.0.0"));
    assert!(status_stdout.contains("lenso.workspace-edit@1.0.0"));

    let run = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["--plan"])
        .arg(plan_path())
        .args(["--prompt", "Create and edit a workspace note."])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "Workspace mutation result: after\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join("note.txt")).unwrap(),
        "after\n"
    );

    let remove = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "disable", "workspace-edit"])
        .args(["--plan"])
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(remove.status.success());

    let status_after = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "status"])
        .output()
        .unwrap();
    let status_after_stdout = String::from_utf8(status_after.stdout).unwrap();
    assert!(status_after_stdout.contains("example.text-tools@1.0.0"));
    assert!(!status_after_stdout.contains("lenso.workspace-edit@1.0.0"));
}

#[test]
fn bundled_plugin_enable_keeps_active_state_unchanged_when_ready_check_fails() {
    let workspace = tempfile::tempdir().unwrap();
    let invalid_plan = workspace.path().join("invalid-plan.json");
    fs::write(&invalid_plan, b"{}\n").unwrap();

    let enable = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "enable", "workspace-edit"])
        .args(["--evidence", "reviewed-workspace-mutation", "--plan"])
        .arg(&invalid_plan)
        .output()
        .unwrap();

    assert!(!enable.status.success());
    assert!(String::from_utf8_lossy(&enable.stderr).contains("resolved Plan"));
    assert!(
        !workspace
            .path()
            .join(".lenso/plugins/active-set.json")
            .exists()
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one external Plugin scenario proves the complete release lifecycle"
)]
fn external_wasm_tool_plugin_builds_installs_upgrades_rolls_back_and_removes() {
    let _plugin_test_guard = plugin_test_guard();
    let workspace = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let bundles = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("README.md"), "# Plugin Fixture\n").unwrap();
    copy_external_wasm_tool_source(external.path());
    let artifact = build_external_wasm_tool(external.path());
    let release_one = build_external_wasm_tool_bundle(
        external.path(),
        bundles.path().join("v1"),
        &artifact,
        "1.0.0",
    );
    let release_two = build_external_wasm_tool_bundle(
        external.path(),
        bundles.path().join("v2"),
        &artifact,
        "2.0.0",
    );

    let install = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "install", "--bundle"])
        .arg(&release_one)
        .args(["--evidence", "external-plugin-review-v1"])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let install_stdout = String::from_utf8(install.stdout).unwrap();
    assert!(install_stdout.contains("installed: dev.example.wasm-text-tools@1.0.0"));
    assert!(install_stdout.contains("governance: reviewed"));
    assert_external_text_tool_runs(workspace.path(), "after install");

    let upgrade = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "upgrade", "--bundle"])
        .arg(&release_two)
        .args(["--evidence", "external-plugin-review-v2", "--plan"])
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(
        upgrade.status.success(),
        "{}",
        String::from_utf8_lossy(&upgrade.stderr)
    );
    let upgrade_stdout = String::from_utf8(upgrade.stdout).unwrap();
    assert!(upgrade_stdout.contains("upgraded: dev.example.wasm-text-tools@2.0.0"));
    let previous = output_value(&upgrade_stdout, "previous-active-set: ");
    assert_external_text_tool_runs(workspace.path(), "after upgrade");

    let rollback = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "rollback", "--to", previous, "--plan"])
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(
        rollback.status.success(),
        "{}",
        String::from_utf8_lossy(&rollback.stderr)
    );
    assert!(String::from_utf8_lossy(&rollback.stdout).contains("rolled-back-to:"));
    assert_external_text_tool_runs(workspace.path(), "after rollback");

    let remove = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args([
            "plugins",
            "remove",
            "--plugin",
            "dev.example.wasm-text-tools",
        ])
        .output()
        .unwrap();
    assert!(remove.status.success());

    let after_remove = run_external_text_tool(workspace.path());
    assert!(!after_remove.status.success());
    assert!(
        String::from_utf8_lossy(&after_remove.stderr).contains("InvalidRequest"),
        "{}",
        String::from_utf8_lossy(&after_remove.stderr)
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one external Plugin scenario proves imported workspace authority and its release lifecycle"
)]
fn external_wasm_tool_imports_only_the_host_selected_workspace_reader() {
    let _plugin_test_guard = plugin_test_guard();
    let workspace = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let bundles = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("README.md"), "# Plugin Fixture\n").unwrap();
    copy_external_wasm_workspace_reader_source(external.path());
    let artifact = build_external_wasm_workspace_reader(external.path());
    let release_one = build_external_wasm_workspace_reader_bundle(
        external.path(),
        bundles.path().join("v1"),
        &artifact,
        "1.0.0",
        false,
    );
    let release_two = build_external_wasm_workspace_reader_bundle(
        external.path(),
        bundles.path().join("v2"),
        &artifact,
        "2.0.0",
        false,
    );
    let expanded = build_external_wasm_workspace_reader_bundle(
        external.path(),
        bundles.path().join("expanded"),
        &artifact,
        "3.0.0",
        true,
    );

    let rejected = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "install", "--bundle"])
        .arg(&expanded)
        .args(["--evidence", "review-must-not-expand-authority"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("does not match a registered Plugin profile"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );

    let install = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "install", "--bundle"])
        .arg(&release_one)
        .args(["--evidence", "workspace-import-review-v1"])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(String::from_utf8_lossy(&install.stdout).contains("governance: reviewed"));
    assert_external_workspace_reader_runs(workspace.path(), "after install");

    let upgrade = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "upgrade", "--bundle"])
        .arg(&release_two)
        .args(["--evidence", "workspace-import-review-v2", "--plan"])
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(
        upgrade.status.success(),
        "{}",
        String::from_utf8_lossy(&upgrade.stderr)
    );
    let upgrade_stdout = String::from_utf8(upgrade.stdout).unwrap();
    let previous = output_value(&upgrade_stdout, "previous-active-set: ");
    assert_external_workspace_reader_runs(workspace.path(), "after upgrade");

    let rollback = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "rollback", "--to", previous, "--plan"])
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(
        rollback.status.success(),
        "{}",
        String::from_utf8_lossy(&rollback.stderr)
    );
    assert_external_workspace_reader_runs(workspace.path(), "after rollback");

    let remove = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args([
            "plugins",
            "remove",
            "--plugin",
            "dev.example.wasm-workspace-reader",
        ])
        .output()
        .unwrap();
    assert!(remove.status.success());
    let after_remove = run_external_workspace_reader(workspace.path());
    assert!(!after_remove.status.success());
    assert!(
        String::from_utf8_lossy(&after_remove.stderr).contains("InvalidRequest"),
        "{}",
        String::from_utf8_lossy(&after_remove.stderr)
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one external Plugin scenario proves reviewed network authority across the release lifecycle"
)]
fn external_wasm_tool_uses_only_the_reviewed_network_origin() {
    let _plugin_test_guard = plugin_test_guard();
    let workspace = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let bundles = tempfile::tempdir().unwrap();
    let (origin, server) = start_http_fixture(3);
    copy_external_wasm_http_fetch_source(external.path());
    let artifact = build_external_wasm_http_fetch(external.path());
    let release_one = build_external_wasm_http_fetch_bundle(
        external.path(),
        bundles.path().join("v1"),
        &artifact,
        "1.0.0",
        &[origin.as_str()],
    );
    let release_two = build_external_wasm_http_fetch_bundle(
        external.path(),
        bundles.path().join("v2"),
        &artifact,
        "2.0.0",
        &[origin.as_str()],
    );
    let expanded = build_external_wasm_http_fetch_bundle(
        external.path(),
        bundles.path().join("expanded"),
        &artifact,
        "3.0.0",
        &[origin.as_str(), "http://127.0.0.1:1"],
    );
    let plan = write_network_plan(workspace.path(), &origin);

    let install = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "install", "--bundle"])
        .arg(&release_one)
        .args(["--evidence", "network-origin-review-v1"])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let install_stdout = String::from_utf8(install.stdout).unwrap();
    assert!(install_stdout.contains("governance: reviewed"));
    let active_set =
        sha256_digest(&fs::read(workspace.path().join(".lenso/plugins/active-set.json")).unwrap());

    let inspect = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "inspect", "--active-set", &active_set])
        .output()
        .unwrap();
    let inspect_stdout = String::from_utf8_lossy(&inspect.stdout);
    assert!(
        inspect.status.success(),
        "{}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    assert!(inspect_stdout.contains("enforcer=lenso.agent.http-fetch"));
    assert!(inspect_stdout.contains(&origin));
    assert_external_http_fetch_runs(workspace.path(), &plan, &origin, "after install");

    let active_before = fs::read(workspace.path().join(".lenso/plugins/active-set.json")).unwrap();
    let rejected = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "upgrade", "--bundle"])
        .arg(&expanded)
        .args(["--evidence", "review-must-not-expand-origin", "--plan"])
        .arg(&plan)
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("exceeds the App HTTP enforcer allowlist"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert_eq!(
        fs::read(workspace.path().join(".lenso/plugins/active-set.json")).unwrap(),
        active_before
    );

    let upgrade = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "upgrade", "--bundle"])
        .arg(&release_two)
        .args(["--evidence", "network-origin-review-v2", "--plan"])
        .arg(&plan)
        .output()
        .unwrap();
    assert!(
        upgrade.status.success(),
        "{}",
        String::from_utf8_lossy(&upgrade.stderr)
    );
    let upgrade_stdout = String::from_utf8(upgrade.stdout).unwrap();
    let previous = output_value(&upgrade_stdout, "previous-active-set: ");
    assert_external_http_fetch_runs(workspace.path(), &plan, &origin, "after upgrade");

    let rollback = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "rollback", "--to", previous, "--plan"])
        .arg(&plan)
        .output()
        .unwrap();
    assert!(
        rollback.status.success(),
        "{}",
        String::from_utf8_lossy(&rollback.stderr)
    );
    assert_external_http_fetch_runs(workspace.path(), &plan, &origin, "after rollback");

    let remove = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args([
            "plugins",
            "remove",
            "--plugin",
            "dev.example.wasm-http-fetch",
        ])
        .output()
        .unwrap();
    assert!(remove.status.success());
    let after_remove = run_external_http_fetch(workspace.path(), &plan, &origin);
    assert!(!after_remove.status.success());
    assert!(String::from_utf8_lossy(&after_remove.stderr).contains("InvalidRequest"));
    server.join().unwrap();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one subprocess scenario preserves the authority digests across upgrade and rollback"
)]
fn upgrade_is_ready_gated_and_manual_rollback_restores_the_previous_authority() {
    let _plugin_test_guard = plugin_test_guard();
    let workspace = tempfile::tempdir().unwrap();
    let release_one = tempfile::tempdir().unwrap();
    let release_two = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("README.md"), "# Plugin Fixture\n").unwrap();
    let manifest_one = write_tool_bundle_release(release_one.path(), "1.0.0");
    let manifest_two = write_tool_bundle_release(release_two.path(), "2.0.0");

    let install = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "install", "--bundle"])
        .arg(release_one.path())
        .args(["--evidence", "review-ticket-upgrade-1"])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let active_before = fs::read(workspace.path().join(".lenso/plugins/active-set.json")).unwrap();

    let failed_cas = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "upgrade", "--bundle"])
        .arg(release_two.path())
        .args([
            "--evidence",
            "review-ticket-upgrade-2",
            "--expected-manifest",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "--plan",
        ])
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(!failed_cas.status.success());
    assert!(
        String::from_utf8_lossy(&failed_cas.stderr).contains("compare-and-swap failed"),
        "{}",
        String::from_utf8_lossy(&failed_cas.stderr)
    );
    assert_eq!(
        fs::read(workspace.path().join(".lenso/plugins/active-set.json")).unwrap(),
        active_before
    );

    let invalid_plan = workspace.path().join("invalid-plan.json");
    fs::write(&invalid_plan, b"{}\n").unwrap();
    let failed_ready = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "upgrade", "--bundle"])
        .arg(release_two.path())
        .args([
            "--evidence",
            "review-ticket-upgrade-2",
            "--expected-manifest",
            &manifest_one,
            "--plan",
        ])
        .arg(&invalid_plan)
        .output()
        .unwrap();
    assert!(!failed_ready.status.success());
    assert!(String::from_utf8_lossy(&failed_ready.stderr).contains("resolved Plan"));
    assert_eq!(
        fs::read(workspace.path().join(".lenso/plugins/active-set.json")).unwrap(),
        active_before
    );

    let upgrade = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "upgrade", "--bundle"])
        .arg(release_two.path())
        .arg("--plan")
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(
        upgrade.status.success(),
        "{}",
        String::from_utf8_lossy(&upgrade.stderr)
    );
    let upgrade_stdout = String::from_utf8(upgrade.stdout).unwrap();
    assert!(upgrade_stdout.contains("upgraded: example.text-tools@2.0.0"));
    assert!(upgrade_stdout.contains(&format!("manifest: {manifest_two}")));
    assert!(upgrade_stdout.contains("governance: automatic:local-trusted-stateless-append-many"));
    let previous = output_value(&upgrade_stdout, "previous-active-set: ");
    let upgraded = output_value(&upgrade_stdout, "active-set: ");
    assert_ne!(previous, upgraded);
    assert!(upgrade_stdout.contains("generation: sha256:"));

    let history = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "history"])
        .output()
        .unwrap();
    assert!(
        history.status.success(),
        "{}",
        String::from_utf8_lossy(&history.stderr)
    );
    let history_stdout = String::from_utf8(history.stdout).unwrap();
    assert!(history_stdout.contains(&format!("retained: {previous}")));
    assert!(history_stdout.contains(&format!("current: {upgraded}")));

    let inspect = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "inspect", "--active-set", previous])
        .output()
        .unwrap();
    assert!(inspect.status.success());
    let inspect_stdout = String::from_utf8(inspect.stdout).unwrap();
    assert!(inspect_stdout.contains("current: false"));
    assert!(inspect_stdout.contains("release: example.text-tools@1.0.0"));
    assert!(inspect_stdout.contains("instance: plugin:18:example.text-tools:text-tools"));

    let status = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "status"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&status.stdout).contains("example.text-tools@2.0.0"));

    let rollback = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "rollback", "--to", previous, "--plan"])
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(
        rollback.status.success(),
        "{}",
        String::from_utf8_lossy(&rollback.stderr)
    );
    let rollback_stdout = String::from_utf8(rollback.stdout).unwrap();
    assert!(rollback_stdout.contains(&format!("rolled-back-to: {previous}")));
    assert!(rollback_stdout.contains(&format!("previous-active-set: {upgraded}")));
    assert_eq!(
        fs::read(workspace.path().join(".lenso/plugins/active-set.json")).unwrap(),
        active_before
    );

    let upgraded_record = workspace
        .path()
        .join(".lenso/plugins/active-sets")
        .join(format!(
            "{}.json",
            upgraded.strip_prefix("sha256:").unwrap()
        ));
    fs::write(&upgraded_record, b"{}\n").unwrap();
    let tampered_rollback = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "rollback", "--to", upgraded, "--plan"])
        .arg(plan_path())
        .output()
        .unwrap();
    assert!(!tampered_rollback.status.success());
    assert!(
        String::from_utf8_lossy(&tampered_rollback.stderr).contains("Plugin control plane failed")
    );
    assert_eq!(
        fs::read(workspace.path().join(".lenso/plugins/active-set.json")).unwrap(),
        active_before
    );
    let tampered_history = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "history"])
        .output()
        .unwrap();
    assert!(!tampered_history.status.success());
}

#[test]
fn reviewed_fixture_model_plugin_replaces_the_base_provider_and_runs() {
    let _plugin_test_guard = plugin_test_guard();
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("README.md"), "# Plugin Fixture\n").unwrap();

    let install = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "install", "--bundle"])
        .arg(fixture_model_bundle_path())
        .args(["--evidence", "review-ticket-88"])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(
        String::from_utf8_lossy(&install.stdout).contains("installed: example.fixture-model@1.0.0")
    );

    let run = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["--plan"])
        .arg(plan_path())
        .args(["--prompt", "Answer directly: hello"])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "Plugin: Direct answer.\n"
    );

    let remove = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "remove", "--plugin", "example.fixture-model"])
        .output()
        .unwrap();
    assert!(remove.status.success());
}

#[test]
fn reviewed_codex_direct_plugin_installs_and_fails_closed_without_login() {
    let _plugin_test_guard = plugin_test_guard();
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("README.md"), "# Plugin Fixture\n").unwrap();

    let install = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "install", "--bundle"])
        .arg(codex_direct_bundle_path())
        .args(["--evidence", "review-ticket-92"])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(
        String::from_utf8_lossy(&install.stdout).contains("installed: example.codex-direct@1.0.0")
    );

    let isolated_home = workspace.path().join("home");
    fs::create_dir(&isolated_home).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .env("HOME", &isolated_home)
        .args(["--plan"])
        .arg(plan_path())
        .args(["--prompt", "Answer directly: hello"])
        .output()
        .unwrap();
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("direct Codex authentication failed"),
        "{stderr}"
    );
    assert!(!stderr.contains("access_token"));

    let remove = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "remove", "--plugin", "example.codex-direct"])
        .output()
        .unwrap();
    assert!(remove.status.success());
}

fn write_passive_bundle(root: &Path) {
    let artifact = b"passive artifact";
    let metadata = b"{\"kind\":\"fixture\"}";
    let target = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    let manifest = serde_json::json!({
        "schema_version": 1,
        "plugin_id": "example.passive",
        "release_version": "1.0.0",
        "artifacts": [{
            "id": "extra",
            "kind": "data",
            "digest": sha256_digest(artifact),
            "size": artifact.len(),
            "media_type": "application/octet-stream",
            "path": "extra.bin",
            "targets": [target]
        }],
        "module_contributions": [],
        "data_contributions": [],
        "permission_requests": [],
        "features": [{
            "id": "extras",
            "module_contribution_ids": [],
            "data_contribution_ids": [],
            "artifact_ids": ["extra"],
            "permission_request_ids": [],
            "product_metadata_ids": ["extra-meta"]
        }],
        "binding_templates": [],
        "product_metadata": [{
            "id": "extra-meta",
            "namespace": "example.passive",
            "schema_id": "example.passive.metadata@1",
            "path": "extra.json",
            "digest": sha256_digest(metadata)
        }]
    });
    fs::write(
        root.join("lenso-plugin.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(root.join("extra.bin"), artifact).unwrap();
    fs::write(root.join("extra.json"), metadata).unwrap();
}

fn write_tool_bundle(root: &Path) {
    write_tool_bundle_release(root, "1.0.0");
}

fn write_tool_bundle_release(root: &Path, release_version: &str) -> String {
    let target = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    let empty_configuration_schema = br#"{"additionalProperties":false,"type":"object"}"#;
    let manifest = serde_json::json!({
        "schema_version": 1,
        "plugin_id": "example.text-tools",
        "release_version": release_version,
        "artifacts": [],
        "module_contributions": [{
            "id": "text-tools",
            "package_id": "lenso.agent.text-tools",
            "configuration_schema_digest": sha256_digest(empty_configuration_schema),
            "provides": [{
                "capability_id": "lenso.agent.tool-provider@1",
                "descriptor_version": "1.0.0",
                "descriptor_digest": sha256_digest(include_bytes!("../../../crates/lenso-capability-agent-tool-provider/capability.json")),
                "request_operations": ["catalog", "execute"]
            }],
            "requires": [],
            "implementations": [{
                "id": "native",
                "artifact": null,
                "built_in_factory": "lenso.agent.text-tools@0.2.0",
                "entrypoint": "default",
                "execution_class": "lenso.native-rust@1",
                "targets": [target],
                "profiles": ["agent-tool-provider-v1"],
                "support_channel": "stable",
                "trust": "trusted"
            }],
            "permission_request_ids": [],
            "state": null
        }],
        "data_contributions": [],
        "permission_requests": [],
        "features": [],
        "binding_templates": [],
        "product_metadata": []
    });
    let bytes = serde_json::to_vec(&manifest).unwrap();
    fs::write(root.join("lenso-plugin.json"), &bytes).unwrap();
    sha256_digest(&bytes)
}

fn copy_external_wasm_tool_source(destination: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/external-plugins/wasm-text-tools");
    fs::create_dir_all(destination.join("guest/src")).unwrap();
    fs::create_dir_all(destination.join("guest/wit")).unwrap();
    for relative in [
        "guest/Cargo.toml",
        "guest/Cargo.lock",
        "guest/src/lib.rs",
        "guest/wit/world.wit",
        "lenso-plugin.template.json",
    ] {
        fs::copy(source.join(relative), destination.join(relative)).unwrap();
    }
}

fn plugin_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

fn plugin_test_guard() -> std::sync::MutexGuard<'static, ()> {
    plugin_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn copy_external_wasm_workspace_reader_source(destination: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/external-plugins/wasm-workspace-reader");
    fs::create_dir_all(destination.join("guest/src")).unwrap();
    fs::create_dir_all(destination.join("guest/wit")).unwrap();
    for relative in [
        "guest/Cargo.toml",
        "guest/Cargo.lock",
        "guest/src/lib.rs",
        "guest/wit/world.wit",
        "lenso-plugin.template.json",
    ] {
        fs::copy(source.join(relative), destination.join(relative)).unwrap();
    }
}

fn copy_external_wasm_http_fetch_source(destination: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/external-plugins/wasm-http-fetch");
    fs::create_dir_all(destination.join("guest/src")).unwrap();
    fs::create_dir_all(destination.join("guest/wit")).unwrap();
    for relative in [
        "guest/Cargo.toml",
        "guest/Cargo.lock",
        "guest/src/lib.rs",
        "guest/wit/world.wit",
        "lenso-plugin.template.json",
    ] {
        fs::copy(source.join(relative), destination.join(relative)).unwrap();
    }
}

fn build_external_wasm_tool(source: &Path) -> std::path::PathBuf {
    let target = source.join("target");
    let output = Command::new(env!("CARGO"))
        .args([
            "build",
            "--locked",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--manifest-path",
        ])
        .arg(source.join("guest/Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    target.join("wasm32-unknown-unknown/release/external_wasm_text_tools.wasm")
}

fn build_external_wasm_workspace_reader(source: &Path) -> std::path::PathBuf {
    let target = source.join("target");
    let output = Command::new(env!("CARGO"))
        .args([
            "build",
            "--locked",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--manifest-path",
        ])
        .arg(source.join("guest/Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    target.join("wasm32-unknown-unknown/release/external_wasm_workspace_reader.wasm")
}

fn build_external_wasm_http_fetch(source: &Path) -> std::path::PathBuf {
    let target = source.join("target");
    let output = Command::new(env!("CARGO"))
        .args([
            "build",
            "--locked",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--manifest-path",
        ])
        .arg(source.join("guest/Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    target.join("wasm32-unknown-unknown/release/external_wasm_http_fetch.wasm")
}

fn build_external_wasm_tool_bundle(
    source: &Path,
    output: std::path::PathBuf,
    artifact: &Path,
    release_version: &str,
) -> std::path::PathBuf {
    let mut template: serde_json::Value =
        serde_json::from_slice(&fs::read(source.join("lenso-plugin.template.json")).unwrap())
            .unwrap();
    template["release_version"] = release_version.into();
    let template_path = source.join(format!("lenso-plugin-{release_version}.template.json"));
    fs::write(&template_path, serde_json::to_vec(&template).unwrap()).unwrap();
    build_bundle(&BundleBuild {
        template: template_path,
        output: output.clone(),
        artifact_sources: vec![ArtifactSource {
            artifact_id: "tool-wasm".to_owned(),
            path: artifact.to_owned(),
        }],
    })
    .unwrap();
    output
}

fn build_external_wasm_workspace_reader_bundle(
    source: &Path,
    output: std::path::PathBuf,
    artifact: &Path,
    release_version: &str,
    expand_authority: bool,
) -> std::path::PathBuf {
    let mut template: serde_json::Value =
        serde_json::from_slice(&fs::read(source.join("lenso-plugin.template.json")).unwrap())
            .unwrap();
    template["release_version"] = release_version.into();
    if expand_authority {
        let requirements = template["module_contributions"][0]["requires"]
            .as_array_mut()
            .unwrap();
        requirements.push(serde_json::json!({
            "capability_id": "lenso.agent.process@1",
            "descriptor_version": "1.0.0",
            "cardinality": "one"
        }));
        requirements.sort_by(|left, right| {
            left["capability_id"]
                .as_str()
                .cmp(&right["capability_id"].as_str())
        });
    }
    let template_path = source.join(format!("lenso-plugin-{release_version}.template.json"));
    fs::write(&template_path, serde_json::to_vec(&template).unwrap()).unwrap();
    build_bundle(&BundleBuild {
        template: template_path,
        output: output.clone(),
        artifact_sources: vec![ArtifactSource {
            artifact_id: "tool-wasm".to_owned(),
            path: artifact.to_owned(),
        }],
    })
    .unwrap();
    output
}

fn build_external_wasm_http_fetch_bundle(
    source: &Path,
    output: std::path::PathBuf,
    artifact: &Path,
    release_version: &str,
    origins: &[&str],
) -> std::path::PathBuf {
    let mut template: serde_json::Value =
        serde_json::from_slice(&fs::read(source.join("lenso-plugin.template.json")).unwrap())
            .unwrap();
    template["release_version"] = release_version.into();
    let mut origins = origins.to_vec();
    origins.sort_unstable();
    template["permission_requests"][0]["scope"]["origins"] = serde_json::json!(origins);
    let template_path = source.join(format!("lenso-plugin-{release_version}.template.json"));
    fs::write(&template_path, serde_json::to_vec(&template).unwrap()).unwrap();
    build_bundle(&BundleBuild {
        template: template_path,
        output: output.clone(),
        artifact_sources: vec![ArtifactSource {
            artifact_id: "tool-wasm".to_owned(),
            path: artifact.to_owned(),
        }],
    })
    .unwrap();
    output
}

fn run_external_text_tool(workspace: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace)
        .args(["--plan"])
        .arg(plan_path())
        .args(["--prompt", "Use the text Plugin to uppercase Lenso plugin."])
        .output()
        .unwrap()
}

fn assert_external_text_tool_runs(workspace: &Path, stage: &str) {
    let run = run_external_text_tool(workspace);
    assert!(
        run.status.success(),
        "{stage}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "Text Plugin result: LENSO PLUGIN\n"
    );
}

fn run_external_workspace_reader(workspace: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace)
        .args(["--plan"])
        .arg(plan_path())
        .args(["--prompt", "Use the workspace Plugin to read README.md."])
        .output()
        .unwrap()
}

fn assert_external_workspace_reader_runs(workspace: &Path, stage: &str) {
    let run = run_external_workspace_reader(workspace);
    assert!(
        run.status.success(),
        "{stage}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "Workspace Plugin result: # Plugin Fixture\n"
    );
}

fn write_network_plan(workspace: &Path, origin: &str) -> std::path::PathBuf {
    let mut plan: serde_json::Value =
        serde_json::from_slice(&fs::read(plan_path()).unwrap()).unwrap();
    let modules = plan["module_instances"].as_array_mut().unwrap();
    let provider = modules
        .iter_mut()
        .find(|module| module["instance_key"] == "http-fetch")
        .unwrap();
    provider["configuration"] = serde_json::to_string(&serde_json::json!({
        "allowed_origins": [origin],
        "max_response_bytes": 262_144,
        "timeout_ms": 10_000
    }))
    .unwrap()
    .into();
    let path = workspace.join("network-plan.json");
    fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
    path
}

fn run_external_http_fetch(workspace: &Path, plan: &Path, origin: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace)
        .args(["--plan"])
        .arg(plan)
        .args([
            "--prompt",
            &format!("Use the network Plugin to fetch {origin}/fixture."),
        ])
        .output()
        .unwrap()
}

fn assert_external_http_fetch_runs(workspace: &Path, plan: &Path, origin: &str, stage: &str) {
    let run = run_external_http_fetch(workspace, plan, origin);
    assert!(
        run.status.success(),
        "{stage}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "Network Plugin result: network fixture\n"
    );
}

fn start_http_fixture(request_count: usize) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        for _ in 0..request_count {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 15\r\nConnection: close\r\n\r\nnetwork fixture",
                )
                .unwrap();
        }
    });
    (origin, server)
}

fn output_value<'a>(stdout: &'a str, prefix: &str) -> &'a str {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("missing `{prefix}` in output: {stdout}"))
}

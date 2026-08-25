use std::{fs, path::Path, process::Command};

use lenso_plugin_control_plane::sha256_digest;

fn plan_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../composition/headless-readonly/resolved-plan.json")
}

fn fixture_model_bundle_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/model-fixture")
}

fn codex_direct_bundle_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/codex-direct")
}

#[test]
fn cli_installs_lists_and_runs_with_a_reviewed_passive_release() {
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
    assert!(String::from_utf8_lossy(&after_remove.stderr).contains("InvalidRequest"));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one subprocess scenario preserves the authority digests across upgrade and rollback"
)]
fn upgrade_is_ready_gated_and_manual_rollback_restores_the_previous_authority() {
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
        .env("LENSO_RESOLVED_PLAN", plan_path())
        .args(["plugins", "upgrade", "--bundle"])
        .arg(release_two.path())
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
                "built_in_factory": "lenso.agent.text-tools@0.1.0",
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

fn output_value<'a>(stdout: &'a str, prefix: &str) -> &'a str {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("missing `{prefix}` in output: {stdout}"))
}

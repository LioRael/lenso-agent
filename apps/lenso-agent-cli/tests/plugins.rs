use std::{fs, path::Path, process::Command};

use lenso_plugin_control_plane::sha256_digest;

fn plan_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../composition/headless-readonly/resolved-plan.json")
}

#[test]
fn cli_installs_lists_and_runs_with_a_reviewed_passive_release() {
    let workspace = tempfile::tempdir().unwrap();
    let bundle = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("README.md"), "# Plugin Fixture\n").unwrap();
    write_bundle(bundle.path());

    let install = Command::new(env!("CARGO_BIN_EXE_lenso-agent-cli"))
        .current_dir(workspace.path())
        .args(["plugins", "install", "--bundle"])
        .arg(bundle.path())
        .args(["--feature", "extras", "--evidence", "review-ticket-42"])
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

fn write_bundle(root: &Path) {
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

//! Opt-in acceptance against the published macOS Echo archive. No artifact seeding.
use super::*;

const POLICY: &str = include_str!("../../../../tests/fixtures/marketplace-remote-policy.json");
const PLUGIN: &str = "lenso.marketplace.echo";
const VERSION: &str = "0.1.1";
const ARCHIVE_DIGEST: &str =
    "sha256:e7ab05a76127b94ac43fe4fc280eda2e48d49569d554e2fe9062ef7f08773a0e";
const PROCESS_TEST: &str =
    "plugin_control::marketplace::tests::remote_acceptance::remote_marketplace_process";

#[test]
#[ignore = "downloads and executes the reviewed public macOS arm64 Echo release"]
fn remote_marketplace_installation_survives_process_restart() {
    assert_eq!(
        (std::env::consts::OS, std::env::consts::ARCH),
        ("macos", "aarch64")
    );
    let root = tempfile::tempdir().unwrap();
    for phase in ["install", "restart"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", PROCESS_TEST, "--ignored", "--nocapture"])
            .env("LENSO_REMOTE_PROOF_HOME", root.path())
            .env("LENSO_REMOTE_PROOF_PHASE", phase)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{phase}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let read = |phase: &str| -> Value {
        serde_json::from_slice(&fs::read(root.path().join(format!("{phase}.json"))).unwrap())
            .unwrap()
    };
    let installed = read("install");
    let restarted = read("restart");
    assert_eq!(
        installed["installation"]["operation_id"],
        restarted["installation"]["operation_id"]
    );
    let receipt = serde_json::json!({
        "schema": "lenso.marketplace.remote-acceptance.v1",
        "plugin_id": PLUGIN, "version": VERSION, "artifact_digest": ARCHIVE_DIGEST,
        "empty_home": true, "artifact_cache_seeded": false,
        "install": installed, "restart": restarted
    });
    let encoded = serde_json::to_string_pretty(&receipt).unwrap();
    if let Ok(path) = std::env::var("LENSO_REMOTE_PROOF_RECEIPT") {
        fs::write(path, &encoded).unwrap();
    }
    println!("{encoded}");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "child process of the remote acceptance test"]
async fn remote_marketplace_process() {
    let root = PathBuf::from(std::env::var("LENSO_REMOTE_PROOF_HOME").unwrap());
    let phase = std::env::var("LENSO_REMOTE_PROOF_PHASE").unwrap();
    assert!(matches!(phase.as_str(), "install" | "restart"));
    if phase == "install" {
        assert!(fs::read_dir(&root).unwrap().next().is_none());
        crate::configure_test_fixture_model(&root);
        let approval = root.join("plugins/lenso.agent.interactive-approval-hook");
        fs::create_dir_all(&approval).unwrap();
        fs::write(approval.join("default.toml"), concat!(
            "default_decision = \"ask\"\nask_tools = []\nmax_preview_bytes = 4096\n",
            "allow_tools = [\"list_available_plugins\", \"check_plugin_install\", \"apply_plugin_install\", \"get_plugin_installation\", \"echo\"]\n"
        )).unwrap();
        fs::create_dir_all(root.join(".lenso")).unwrap();
        fs::write(root.join(".lenso/marketplace-policy.json"), POLICY).unwrap();
    }
    let mut config = crate::AgentWebConfig::new(lenso_agent_console_plugins::link);
    config.agent_home = Some(root.clone());
    config.control = crate::AgentWebControl::HostAuthorized;
    config.plugin_control = true;
    config.tool_policy = Some(root.join("tool-policy.json"));
    tokio::task::LocalSet::new()
        .run_until(async {
            tokio::time::timeout(Duration::from_secs(240), async {
                let surface = crate::AgentWebSurface::start(config).await.unwrap();
                let runtime = surface.runtime.clone();
                let receipt = if phase == "install" {
                    install(&runtime, &root).await
                } else {
                    let installed: Value =
                        serde_json::from_slice(&fs::read(root.join("install.json")).unwrap())
                            .unwrap();
                    let digest = installed["proposal"]["proposal_digest"].as_str().unwrap();
                    let installation = wait_tool(&runtime, digest).await;
                    assert_eq!(installation["status"], "succeeded", "{installation}");
                    let echo = remote_tool(
                        &runtime,
                        "echo",
                        serde_json::json!({"text":"Echo after process restart"}),
                    )
                    .await;
                    assert_eq!(echo, "Echo after process restart");
                    serde_json::json!({"installation":installation,"echo":echo})
                };
                surface.shutdown().await.unwrap();
                fs::write(
                    root.join(format!("{phase}.json")),
                    serde_json::to_vec_pretty(&receipt).unwrap(),
                )
                .unwrap();
            })
            .await
            .expect("remote acceptance exceeded its deadline");
        })
        .await;
}

async fn remote_tool(runtime: &WebRuntime, name: &str, arguments: Value) -> Value {
    // The transport's own timeout is 60s; the normal fixture helper allows only 20s.
    let response = tokio::time::timeout(
        Duration::from_secs(120),
        raw_tool_inner(runtime, name, arguments),
    )
    .await
    .unwrap();
    assert_eq!(response["status"], "succeeded", "{name}: {response}");
    let content = response["response"]["content"].as_str().unwrap();
    serde_json::from_str(content).unwrap_or_else(|_| content.into())
}

async fn install(runtime: &WebRuntime, root: &Path) -> Value {
    assert!(!root.join("plugins").join(PLUGIN).exists());
    assert!(!root.join(".lenso/marketplace/artifacts").exists());
    let catalog = remote_tool(
        runtime,
        "list_available_plugins",
        serde_json::json!({"agent_id":"console","query":"Echo"}),
    )
    .await;
    assert_eq!(
        fs::read_dir(root.join(".lenso/marketplace/artifacts"))
            .unwrap()
            .count(),
        0
    );
    let entry = catalog["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["package_id"] == PLUGIN && entry["package_revision"] == VERSION)
        .expect("reviewed Echo release must be listed");
    assert_eq!(entry["source_digest"], ARCHIVE_DIGEST);
    let proposal = remote_tool(runtime, "check_plugin_install", serde_json::json!({"agent_id":"console","catalog_entry_id":entry["catalog_entry_id"],"expected_revision":catalog["revision"]})).await;
    assert!(
        !root.join("plugins").join(PLUGIN).exists(),
        "precheck must not install"
    );
    assert_eq!(
        fs::read_dir(root.join(".lenso/marketplace/artifacts"))
            .unwrap()
            .count(),
        1
    );
    let apply = serde_json::json!({"agent_id":"console","catalog_entry_id":entry["catalog_entry_id"],"expected_revision":catalog["revision"],"proposal_digest":proposal["proposal_digest"]});
    remote_tool(runtime, "apply_plugin_install", apply.clone()).await;
    let installation = wait_tool(runtime, proposal["proposal_digest"].as_str().unwrap()).await;
    assert_eq!(installation["status"], "succeeded", "{installation}");
    remote_tool(runtime, "apply_plugin_install", apply).await;
    let repeated = wait_tool(runtime, proposal["proposal_digest"].as_str().unwrap()).await;
    assert_eq!(installation["operation_id"], repeated["operation_id"]);
    let echo = remote_tool(
        runtime,
        "echo",
        serde_json::json!({"text":"Echo over HTTPS"}),
    )
    .await;
    assert_eq!(echo, "Echo over HTTPS");
    serde_json::json!({"catalog":catalog,"proposal":proposal,"installation":installation,"echo":echo})
}

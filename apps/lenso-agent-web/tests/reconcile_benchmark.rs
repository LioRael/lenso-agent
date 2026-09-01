use std::{process::Command, time::Instant};

use lenso_agent_host::generation::{OnlineReconcileTelemetry, online_reconcile_telemetry};
use lenso_agent_web::{AgentWebConfig, AgentWebSurface};

#[tokio::test(flavor = "current_thread")]
#[ignore = "manual performance smoke; run with --ignored --nocapture"]
#[allow(
    clippy::too_many_lines,
    reason = "one performance scenario keeps cold start, idle, switch, and repeat measurements together"
)]
async fn reports_reconciler_performance_as_stable_json() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            configure_fixture_model(root.path());
            let before_start = online_reconcile_telemetry();
            let started_at = Instant::now();
            let mut config = AgentWebConfig::new(lenso_agent_default_plugins::link);
            config.agent_home = Some(root.path().to_path_buf());
            let surface = AgentWebSurface::start(config).await.unwrap();
            let cold_start_ms = started_at.elapsed().as_millis();
            let cold_start_io = online_reconcile_telemetry().delta(before_start);

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let router = surface.router();
            let server = tokio::task::spawn_local(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let client = reqwest::Client::new();
            let inventory_url = format!("http://{address}/api/console/v1/agent/plugins");
            let initial = read_inventory(&client, &inventory_url).await;
            let initial_cursor = initial["cursor"].as_str().unwrap().to_owned();
            let initial_plan = initial["active"]["planDigest"].as_str().unwrap().to_owned();

            let before_idle = online_reconcile_telemetry();
            tokio::time::timeout(std::time::Duration::from_secs(8), async {
                loop {
                    if online_reconcile_telemetry()
                        .delta(before_idle)
                        .metadata_probes
                        >= 3
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("three idle consistency probes should complete");
            let idle = online_reconcile_telemetry().delta(before_idle);
            assert_eq!(idle.canonical_snapshots, 0);
            assert_eq!(idle.full_reconcile_attempts, 0);
            assert_eq!(idle.resource_directory_reads, 0);

            let plugin_directory = root.path().join("plugins/lenso.agent.loop");
            std::fs::create_dir_all(&plugin_directory).unwrap();
            let changed_at = Instant::now();
            std::fs::write(
                plugin_directory.join("agent.toml"),
                concat!(
                    "model = \"fixture/readme-summary-v1\"\n",
                    "max_steps = 9\n",
                    "max_tool_calls = 4\n",
                    "max_parallel_tool_calls = 4\n",
                    "max_output_tokens = 1024\n",
                    "max_history_events = 200\n",
                    "max_compaction_summary_characters = 8192\n",
                    "max_memory_items = 8\n",
                    "max_memory_characters = 16384\n",
                ),
            )
            .unwrap();
            let switched = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let inventory =
                        read_inventory(&client, &format!("{inventory_url}?after={initial_cursor}"))
                            .await;
                    if inventory["active"]["planDigest"] != initial_plan
                        && inventory["events"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|event| event["status"] == "switched")
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            })
            .await;
            let change_to_switched_ms = changed_at.elapsed().as_millis();
            assert!(switched.is_ok(), "Plugin Root change did not switch");

            let _ = read_inventory(&client, &inventory_url).await;
            let before_repeated_inventory = online_reconcile_telemetry();
            let _ = read_inventory(&client, &inventory_url).await;
            let repeated_inventory_io =
                online_reconcile_telemetry().delta(before_repeated_inventory);
            assert_eq!(repeated_inventory_io.canonical_snapshots, 0);
            assert_eq!(repeated_inventory_io.resource_directory_reads, 0);

            let rss_bytes = process_rss_bytes();
            let report = performance_report(
                cold_start_ms,
                cold_start_io,
                idle,
                repeated_inventory_io,
                change_to_switched_ms,
                rss_bytes,
            );
            println!("{}", serde_json::to_string(&report).unwrap());

            surface.shutdown().await.unwrap();
            server.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_inventory_after_switch_does_not_resnapshot_plugin_root() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let root = tempfile::tempdir().unwrap();
            configure_fixture_model(root.path());
            let mut config = AgentWebConfig::new(lenso_agent_default_plugins::link);
            config.agent_home = Some(root.path().to_path_buf());
            let surface = AgentWebSurface::start(config).await.unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::task::spawn_local(async move {
                axum::serve(listener, surface.router()).await.unwrap();
            });
            let client = reqwest::Client::new();
            let inventory_url = format!("http://{address}/api/console/v1/agent/plugins");
            let initial = read_inventory(&client, &inventory_url).await;
            let initial_plan = initial["active"]["planDigest"].as_str().unwrap().to_owned();
            let plugin_directory = root.path().join("plugins/lenso.agent.loop");
            std::fs::create_dir_all(&plugin_directory).unwrap();
            std::fs::write(
                plugin_directory.join("agent.toml"),
                concat!(
                    "model = \"fixture/readme-summary-v1\"\n",
                    "max_steps = 9\n",
                    "max_tool_calls = 4\n",
                    "max_parallel_tool_calls = 4\n",
                    "max_output_tokens = 1024\n",
                    "max_history_events = 200\n",
                    "max_compaction_summary_characters = 8192\n",
                    "max_memory_items = 8\n",
                    "max_memory_characters = 16384\n",
                ),
            )
            .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let inventory = read_inventory(&client, &inventory_url).await;
                    if inventory["active"]["planDigest"] != initial_plan {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("Plugin Root change should switch");

            let _ = read_inventory(&client, &inventory_url).await;
            let before = online_reconcile_telemetry();
            let _ = read_inventory(&client, &inventory_url).await;
            let delta = online_reconcile_telemetry().delta(before);
            assert_eq!(delta.canonical_snapshots, 0);
            assert_eq!(delta.resource_directory_reads, 0);

            server.abort();
        })
        .await;
}

fn configure_fixture_model(root: &std::path::Path) {
    let model_directory = root.join("plugins/lenso.agent.model.fixture");
    std::fs::create_dir_all(&model_directory).unwrap();
    std::fs::write(
        model_directory.join("model.toml"),
        concat!(
            "model = \"fixture/readme-summary-v1\"\n",
            "allowed_models = [\"fixture/alternate-v1\"]\n",
        ),
    )
    .unwrap();

    let loop_directory = root.join("plugins/lenso.agent.loop");
    std::fs::create_dir_all(&loop_directory).unwrap();
    std::fs::write(
        loop_directory.join("agent.toml"),
        "model = \"fixture/readme-summary-v1\"\n",
    )
    .unwrap();
}

fn performance_report(
    cold_start_ms: u128,
    cold_start_io: OnlineReconcileTelemetry,
    idle: OnlineReconcileTelemetry,
    repeated_inventory_io: OnlineReconcileTelemetry,
    change_to_switched_ms: u128,
    rss_bytes: Option<u64>,
) -> serde_json::Value {
    serde_json::json!({
        "schema": "lenso.agent.reconcile-benchmark.v1",
        "coldStart": {
            "elapsedMs": cold_start_ms,
            "canonicalSnapshots": cold_start_io.canonical_snapshots,
            "resourceDirectoryReads": cold_start_io.resource_directory_reads,
        },
        "idleConsistency": {
            "cycles": idle.metadata_probes,
            "canonicalSnapshots": idle.canonical_snapshots,
            "fullReconcileAttempts": idle.full_reconcile_attempts,
            "metadataProbes": idle.metadata_probes,
            "resourceDirectoryReads": idle.resource_directory_reads,
        },
        "repeatedInventoryAfterSwitch": {
            "canonicalSnapshots": repeated_inventory_io.canonical_snapshots,
            "resourceDirectoryReads": repeated_inventory_io.resource_directory_reads,
        },
        "pluginRootChangeToSwitchedMs": change_to_switched_ms,
        "rssBytes": rss_bytes,
        "platformSupport": {
            "rss": rss_bytes.is_some(),
        },
    })
}

async fn read_inventory(client: &reqwest::Client, url: &str) -> serde_json::Value {
    client
        .get(url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn process_rss_bytes() -> Option<u64> {
    let process_id = std::process::id().to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &process_id])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let kibibytes = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    kibibytes.checked_mul(1024)
}

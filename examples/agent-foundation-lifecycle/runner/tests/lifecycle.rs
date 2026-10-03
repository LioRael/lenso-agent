use std::{
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn composed_agent_recovers_plugin_task_across_cancellation_and_process_restarts() {
    let temporary;
    let home = if let Some(path) = std::env::var_os("LENSO_FIXTURE_EVIDENCE_DIR") {
        let path = std::path::PathBuf::from(path);
        assert!(path.is_absolute(), "evidence directory must be absolute");
        std::fs::create_dir(&path).expect("evidence directory must not already exist");
        path
    } else {
        temporary = tempfile::tempdir().unwrap();
        temporary.path().to_owned()
    };
    for phase in ["start", "resume", "replay"] {
        let mut process = Command::new(env!("CARGO_BIN_EXE_foundation-lifecycle-phase"))
            .arg(phase)
            .arg(&home)
            .spawn()
            .expect("start isolated phase process");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = process.try_wait().unwrap() {
                assert!(status.success(), "phase {phase} failed: {status}");
                for file in [
                    "task.json",
                    "resolved-plan.json",
                    "generation-spec-digest.txt",
                    "model-catalog.json",
                ] {
                    std::fs::copy(home.join(file), home.join(format!("{phase}-{file}"))).unwrap();
                }
                break;
            }
            if Instant::now() >= deadline {
                process.kill().unwrap();
                process.wait().unwrap();
                panic!("phase {phase} exceeded the 20 second process bound");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("task.json")).unwrap()).unwrap();
    assert_eq!(state["status"], "completed");
    assert_eq!(state["retryable_failures"], 1);
    assert_eq!(
        state["effects"],
        serde_json::json!(["readme-step-1", "readme-step-2"])
    );
}

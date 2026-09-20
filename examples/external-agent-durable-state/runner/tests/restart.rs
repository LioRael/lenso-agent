use std::{path::Path, process::Command};

fn phase(binary: &Path, name: &str, store: &Path) {
    let output = Command::new(binary)
        .arg(name)
        .arg(store)
        .output()
        .expect("start a separate durable fixture process");
    assert!(
        output.status.success(),
        "phase {name} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn external_plugin_state_survives_real_process_restarts() {
    let temporary = tempfile::tempdir().unwrap();
    let store = temporary.path().join("external-durable-state.json");
    let binary = Path::new(env!("CARGO_BIN_EXE_durable-phase"));
    phase(binary, "start", &store);
    phase(binary, "recover-wait", &store);
    phase(binary, "signal", &store);
    phase(binary, "recover-resume", &store);
    phase(binary, "wait", &store);
    phase(binary, "uncertain", &store);
    phase(binary, "remove", &store);
    phase(binary, "upgrade", &store);
}

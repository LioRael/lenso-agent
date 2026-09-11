#![cfg(unix)]

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn fixture(script: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_lenso-agent"),
        root.path().join("lenso-agent"),
    )
    .unwrap();
    for name in ["lenso-agent-cli", "lenso-agent-acp"] {
        let path = root.path().join(name);
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    root
}

#[test]
fn dispatch_preserves_arguments_workspace_environment_and_stdio() {
    let root = fixture(
        "printf '%s\\n' \"$PWD\" \"$LENSO_DISPATCH_TEST\" \"$@\"; cat; printf 'diagnostic' >&2; exit 17",
    );
    for command in [
        "run",
        "auth",
        "profiles",
        "sessions",
        "models",
        "contexts",
        "approvals",
        "doctor",
        "generations",
        "runtime",
        "cli",
        "acp",
    ] {
        let mut child = Command::new(root.path().join("lenso-agent"))
            .args([command, "literal ; $(request)", "--profile", "code"])
            .current_dir(root.path())
            .env("LENSO_DISPATCH_TEST", "preserved")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"input\n").unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(17));
        let prefix = match command {
            "cli" | "acp" => String::new(),
            "run" => "--agent-headless\n".to_owned(),
            _ => format!("{command}\n"),
        };
        let expected = format!(
            "{}\npreserved\n{prefix}literal ; $(request)\n--profile\ncode\ninput\n",
            root.path().canonicalize().unwrap().display()
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        assert_eq!(output.stderr, b"diagnostic");
    }
}

#[test]
fn symlink_launch_resolves_companions_beside_the_real_executable() {
    let root = fixture("printf '%s\\n' \"$@\"; exit 17");
    let links = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(
        root.path().join("lenso-agent"),
        links.path().join("lenso-agent"),
    )
    .unwrap();
    let output = Command::new(links.path().join("lenso-agent"))
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(17));
    assert_eq!(output.stdout, b"doctor\n--json\n");
}

#[test]
fn missing_sibling_never_falls_back_to_path() {
    let root = fixture("exit 0");
    let decoy = tempfile::tempdir().unwrap();
    fs::rename(
        root.path().join("lenso-agent-cli"),
        decoy.path().join("lenso-agent-cli"),
    )
    .unwrap();
    let output = Command::new(root.path().join("lenso-agent"))
        .args(["doctor"])
        .env("PATH", decoy.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("matching Agent components"));
}

#[test]
fn help_and_unknown_commands_do_not_start_a_surface() {
    let root = fixture("echo unexpected-surface >&2; exit 17");
    for args in [vec!["--help"], vec!["tui", "--help"]] {
        let output = Command::new(root.path().join("lenso-agent"))
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("run <prompt>"));
        assert!(output.stderr.is_empty());
    }
    let output = Command::new(root.path().join("lenso-agent"))
        .arg("typo-command")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("unexpected-surface"));
}

#[test]
fn signal_reaches_the_replaced_process() {
    let root = fixture("echo ready; exec /bin/sleep 30");
    let mut child = Command::new(root.path().join("lenso-agent"))
        .arg("acp")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    assert!(
        Command::new("/bin/kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.signal(), Some(15));
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("the surface did not receive SIGTERM");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

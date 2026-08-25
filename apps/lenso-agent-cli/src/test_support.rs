use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

static HEADLESS_PLAN: OnceLock<Vec<u8>> = OnceLock::new();

pub(crate) fn headless_plan() -> &'static [u8] {
    HEADLESS_PLAN.get_or_init(resolve)
}

fn resolve() -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = generated_path();
    fs::create_dir_all(output.parent().expect("generated Plan parent")).unwrap();
    let lenso = env::var_os("LENSO_BIN").unwrap_or_else(|| "lenso".into());
    let result = Command::new(lenso)
        .current_dir(&root)
        .args(["app", "resolve", "--definition"])
        .arg(root.join("lenso.app.json"))
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "failed to resolve the test App: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    fs::read(output).unwrap()
}

fn generated_path() -> PathBuf {
    env::temp_dir()
        .join(format!("lenso-agent-unit-plans-{}", std::process::id()))
        .join("lenso-agent.json")
}

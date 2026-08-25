use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

static HEADLESS_PLAN: OnceLock<PathBuf> = OnceLock::new();
static CODEX_PLAN: OnceLock<PathBuf> = OnceLock::new();
static OPENAI_PLAN: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn plan(app: &str) -> PathBuf {
    match app {
        "base" => HEADLESS_PLAN
            .get_or_init(|| resolve("lenso-agent", "lenso.app.json"))
            .clone(),
        "openai-codex-direct" => CODEX_PLAN
            .get_or_init(|| {
                resolve(
                    app,
                    "apps/lenso-agent-cli/tests/fixtures/openai-codex-direct.app.json",
                )
            })
            .clone(),
        "openai-compatible" => OPENAI_PLAN
            .get_or_init(|| {
                resolve(
                    app,
                    "apps/lenso-agent-cli/tests/fixtures/openai-compatible.app.json",
                )
            })
            .clone(),
        _ => panic!("unsupported test App `{app}`"),
    }
}

fn resolve(app: &str, definition: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = env::temp_dir()
        .join(format!(
            "lenso-agent-integration-plans-{}",
            std::process::id()
        ))
        .join(format!("{app}.json"));
    fs::create_dir_all(output.parent().expect("generated Plan parent")).unwrap();
    let lenso = env::var_os("LENSO_BIN").unwrap_or_else(|| "lenso".into());
    let result = Command::new(lenso)
        .current_dir(&root)
        .args(["app", "resolve", "--definition"])
        .arg(root.join(definition))
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "failed to resolve test App `{app}`: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    output
}

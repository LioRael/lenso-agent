use std::{env, fs, path::PathBuf, sync::OnceLock};

static HEADLESS_PLAN: OnceLock<PathBuf> = OnceLock::new();
static CODEX_PLAN: OnceLock<PathBuf> = OnceLock::new();
static OPENAI_PLAN: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn plan(app: &str) -> PathBuf {
    match app {
        "base" => HEADLESS_PLAN
            .get_or_init(|| resolve("lenso-agent", &[]))
            .clone(),
        "openai-codex-direct" => CODEX_PLAN
            .get_or_init(|| {
                resolve(
                    app,
                    &[
                        (
                            "lenso.agent.loop/agent.toml",
                            r#"model = "gpt-5.6-luna"
max_steps = 8
max_tool_calls = 4
max_parallel_tool_calls = 4
max_output_tokens = 4096
max_history_events = 200
"#,
                        ),
                        (
                            "lenso.agent.loop/subagent-agent.toml",
                            r#"model = "gpt-5.6-luna"
"#,
                        ),
                        (
                            "lenso.agent.auth.openai-codex/auth.toml",
                            r#"issuer = "https://auth.openai.com"
profile = "default"
refresh_margin_seconds = 60
"#,
                        ),
                        (
                            "lenso.agent.model.openai-codex-direct/model.toml",
                            r#"base_url = "https://chatgpt.com/backend-api"
model = "gpt-5.6-luna"
reasoning_effort = "medium"
max_event_bytes = 1048576
"#,
                        ),
                        (
                            "lenso.agent.prompt.static/default-instructions.toml",
                            r#"[[contributions]]
id = "harness.default"
version = "1.0.0"
kind = "instruction"
content = "Be concise, follow explicit user instructions, and use only the Tools supplied by this App."
"#,
                        ),
                    ],
                )
            })
            .clone(),
        "openai-compatible" => OPENAI_PLAN
            .get_or_init(|| {
                resolve(
                    app,
                    &[
                        (
                            "lenso.agent.loop/agent.toml",
                            r#"model = "gpt-4o-mini"
"#,
                        ),
                        (
                            "lenso.agent.loop/subagent-agent.toml",
                            r#"model = "gpt-4o-mini"
"#,
                        ),
                        (
                            "lenso.agent.model.openai-compatible/model.toml",
                            r#"api_key_ref = "model/openai-api-key"
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"
"#,
                        ),
                        (
                            "lenso.secrets.env/secrets.toml",
                            r#"[references]
"model/openai-api-key" = "OPENAI_API_KEY"
"#,
                        ),
                        (
                            "lenso.agent.prompt.static/default-instructions.toml",
                            r#"[[contributions]]
id = "harness.default"
version = "1.0.0"
kind = "instruction"
content = "Be concise, follow explicit user instructions, and use only the Tools supplied by this App."
"#,
                        ),
                    ],
                )
            })
            .clone(),
        _ => panic!("unsupported test App `{app}`"),
    }
}

fn resolve(app: &str, configurations: &[(&str, &str)]) -> PathBuf {
    let root = env::temp_dir()
        .join(format!(
            "lenso-agent-integration-roots-{}",
            std::process::id()
        ))
        .join(app);
    let plugin_root = root.join("plugins");
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    for (relative, configuration) in configurations {
        let path = plugin_root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, configuration).unwrap();
    }
    let output = env::temp_dir()
        .join(format!(
            "lenso-agent-integration-plans-{}",
            std::process::id()
        ))
        .join(format!("{app}.json"));
    fs::create_dir_all(output.parent().expect("generated Plan parent")).unwrap();
    let plan = lenso_agent_cli::derived_plan_bytes(&plugin_root)
        .unwrap_or_else(|error| panic!("failed to derive test App `{app}`: {error}"));
    fs::write(&output, plan).unwrap();
    output
}

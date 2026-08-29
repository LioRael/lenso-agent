use std::{
    collections::hash_map::DefaultHasher,
    env, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

pub(crate) fn plan_for_home(app: &str, home: &Path) -> PathBuf {
    match app {
        "base" => resolve("lenso-agent", fixture_configurations(), home),
        "interaction-resume-budget" => resolve_interaction_resume_budget(home),
        "openai-codex-direct" => resolve(
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
                    "lenso.agent.loop/researcher.toml",
                    r#"model = "gpt-5.6-luna"
"#,
                ),
                (
                    "lenso.agent.loop/reviewer.toml",
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
            home,
        ),
        "openai-compatible" => resolve(
            app,
            &[
                (
                    "lenso.agent.loop/agent.toml",
                    r#"model = "gpt-4o-mini"
"#,
                ),
                (
                    "lenso.agent.loop/researcher.toml",
                    r#"model = "gpt-4o-mini"
"#,
                ),
                (
                    "lenso.agent.loop/reviewer.toml",
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
            home,
        ),
        _ => panic!("unsupported test App `{app}`"),
    }
}

fn resolve_interaction_resume_budget(home: &Path) -> PathBuf {
    resolve(
        "interaction-resume-budget",
        &[
            (
                "lenso.agent.loop/agent.toml",
                "model = \"fixture/readme-summary-v1\"\nmax_steps = 8\nmax_tool_calls = 4\n",
            ),
            (
                "lenso.agent.model.fixture/model.toml",
                "model = \"fixture/readme-summary-v1\"\n",
            ),
        ],
        home,
    )
}

pub(crate) fn fixture_configurations() -> &'static [(&'static str, &'static str)] {
    &[
        (
            "lenso.agent.loop/agent.toml",
            "model = \"fixture/readme-summary-v1\"\n",
        ),
        (
            "lenso.agent.loop/researcher.toml",
            "model = \"fixture/readme-summary-v1\"\n",
        ),
        (
            "lenso.agent.loop/reviewer.toml",
            "model = \"fixture/readme-summary-v1\"\n",
        ),
        (
            "lenso.agent.model.fixture/model.toml",
            "model = \"fixture/readme-summary-v1\"\n",
        ),
    ]
}

fn resolve(app: &str, configurations: &[(&str, &str)], home: &Path) -> PathBuf {
    lenso_agent_default_plugins::link();
    let mut hasher = DefaultHasher::new();
    home.hash(&mut hasher);
    let root = env::temp_dir()
        .join(format!(
            "lenso-agent-integration-roots-{}",
            std::process::id()
        ))
        .join(format!("{:016x}", hasher.finish()))
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
    let output = root.join(format!("{app}.json"));
    fs::create_dir_all(output.parent().expect("generated Plan parent")).unwrap();
    let plan = lenso_agent_host::derived_plan_bytes_for_home(home, &plugin_root)
        .unwrap_or_else(|error| panic!("failed to derive test App `{app}`: {error}"));
    fs::write(&output, plan).unwrap();
    output
}

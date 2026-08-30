use std::{
    collections::hash_map::DefaultHasher,
    env, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

pub(crate) fn plan_for_home(app: &str, home: &Path) -> PathBuf {
    match app {
        "base" => resolve("lenso-agent", fixture_configurations(), home),
        "model-switch" => resolve(
            "model-switch",
            &[
                (
                    "lenso.agent.loop/agent.toml",
                    "model = \"fixture/readme-summary-v1\"\n",
                ),
                (
                    "lenso.agent.model.fixture/model.toml",
                    "model = \"fixture/readme-summary-v1\"\nallowed_models = [\"fixture/alternate-v1\"]\n",
                ),
            ],
            home,
        ),
        "interaction-resume-budget" => resolve_interaction_resume_budget(home),
        app @ ("interaction-resume-limit" | "interaction-total-tool-limit") => {
            interaction_limit(home, app)
        }
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
        "openai-compatible" => resolve_openai_compatible(app, home),
        _ => panic!("unsupported test App `{app}`"),
    }
}

fn resolve_openai_compatible(app: &str, home: &Path) -> PathBuf {
    resolve(
        app,
        &[
            ("lenso.agent.loop/agent.toml", "model = \"gpt-4o-mini\"\n"),
            (
                "lenso.agent.loop/researcher.toml",
                "model = \"gpt-4o-mini\"\n",
            ),
            (
                "lenso.agent.loop/reviewer.toml",
                "model = \"gpt-4o-mini\"\n",
            ),
            (
                "lenso.agent.model.openai-compatible/model.toml",
                "api_key_ref = \"model/openai-api-key\"\nbase_url = \"https://api.openai.com/v1\"\nmodel = \"gpt-4o-mini\"\n",
            ),
            (
                "lenso.secrets.env/secrets.toml",
                "[references]\n\"model/openai-api-key\" = \"OPENAI_API_KEY\"\n",
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
    )
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

fn interaction_limit(home: &Path, app: &str) -> PathBuf {
    let limit = match app {
        "interaction-resume-limit" => "max_user_resumes = 0",
        "interaction-total-tool-limit" => "max_total_tool_calls = 4",
        _ => unreachable!("caller selects an interaction-limit fixture"),
    };
    let agent = format!("model = \"fixture/readme-summary-v1\"\n{limit}\n");
    resolve(
        app,
        &[
            ("lenso.agent.loop/agent.toml", &agent),
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

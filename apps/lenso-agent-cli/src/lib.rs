use std::{env, path::PathBuf};

mod authority;
pub mod generation;
mod plugin_profiles;
pub mod plugins;
pub mod provenance;
pub mod tui;

const DEFAULT_APP: &str = "headless-readonly";
const SUPPORTED_APPS: &[&str] = &[
    DEFAULT_APP,
    "headless-coding",
    "headless-local-coding",
    "openai-readonly",
    "openai-codex-direct",
    "openai-codex-direct-skills",
    "openai-codex-direct-coding",
    "openai-codex-direct-local-coding",
];

pub(crate) fn default_plan() -> PathBuf {
    env::var_os("LENSO_RESOLVED_PLAN").map_or_else(
        || app_plan(DEFAULT_APP).expect("the default App must be supported"),
        PathBuf::from,
    )
}

pub(crate) fn app_plan(app: &str) -> Result<PathBuf, String> {
    if !SUPPORTED_APPS.contains(&app) {
        return Err(format!(
            "unknown App `{app}`; choose one of: {}",
            SUPPORTED_APPS.join(", ")
        ));
    }
    Ok(PathBuf::from("composition")
        .join(app)
        .join("resolved-plan.json"))
}

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use lenso_authoring::CargoAppDefinition;

mod authority;
pub mod channel;
pub mod channel_host;
pub mod discord;
pub mod generation;
mod plugin_profiles;
pub mod plugins;
pub mod provenance;
pub mod telegram;
#[cfg(test)]
mod test_support;
pub mod tui;

/// Loads an exact Plan override or resolves the product App Definition in memory.
pub fn plan_bytes(explicit_plan: Option<&Path>) -> Result<Vec<u8>, String> {
    explicit_plan
        .map(PathBuf::from)
        .or_else(|| env::var_os("LENSO_RESOLVED_PLAN").map(PathBuf::from))
        .map_or_else(resolve_base_plan, |plan| {
            fs::read(&plan).map_err(|error| format!("failed to read {}: {error}", plan.display()))
        })
}

fn resolve_base_plan() -> Result<Vec<u8>, String> {
    let definition = env::var_os("LENSO_APP_DEFINITION")
        .map_or_else(|| PathBuf::from("lenso.app.json"), PathBuf::from);
    let app = CargoAppDefinition::load(&definition)
        .map_err(|error| format!("failed to load {}: {error}", definition.display()))?;
    let root = definition.parent().unwrap_or_else(|| Path::new("."));
    app.resolve_canonical(root).map_err(|error| {
        format!(
            "failed to resolve the App from {}: {error}",
            definition.display()
        )
    })
}

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
    let definition = app_definition_path();
    let app = CargoAppDefinition::load(&definition)
        .map_err(|error| format!("failed to load {}: {error}", definition.display()))?;
    let catalog = generation::linked_module_catalog()?;
    app.resolve_with_catalog_canonical(&catalog)
        .map_err(|error| {
            format!(
                "failed to resolve the App from {}: {error}",
                definition.display()
            )
        })
}

pub(crate) fn app_definition_path() -> PathBuf {
    env::var_os("LENSO_APP_DEFINITION")
        .map_or_else(|| PathBuf::from("lenso.app.json"), PathBuf::from)
}

pub(crate) fn existing_app_definition_path() -> Option<PathBuf> {
    let path = app_definition_path();
    (env::var_os("LENSO_APP_DEFINITION").is_some() || path.is_file()).then_some(path)
}

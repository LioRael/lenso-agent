use std::{
    env, fs,
    path::{Path, PathBuf},
};

mod authority;
pub mod channel;
pub mod channel_host;
pub mod discord;
pub mod generation;
mod generation_authority;
mod plugin_root;
pub mod provenance;
pub mod telegram;
#[cfg(test)]
mod test_support;
pub mod tui;

/// Loads an exact diagnostic Plan override or derives the App from the current Host and Plugin Root.
pub fn plan_bytes(explicit_plan: Option<&Path>) -> Result<Vec<u8>, String> {
    ensure_host_catalog()?;
    explicit_plan
        .map(PathBuf::from)
        .or_else(|| env::var_os("LENSO_RESOLVED_PLAN").map(PathBuf::from))
        .map_or_else(resolve_base_plan, |plan| {
            fs::read(&plan).map_err(|error| format!("failed to read {}: {error}", plan.display()))
        })
}

fn ensure_host_catalog() -> Result<(), String> {
    let path = Path::new(".lenso/host-catalog.json");
    let catalog = generation::linked_host_catalog()?;
    let mut bytes = serde_json::to_vec_pretty(&catalog)
        .map_err(|error| format!("failed to encode the Host Catalog: {error}"))?;
    bytes.push(b'\n');
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    let parent = path.parent().expect("Host Catalog has a parent");
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let temporary = parent.join("host-catalog.json.tmp");
    fs::write(&temporary, &bytes)
        .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("failed to publish {}: {error}", path.display()))
}

fn resolve_base_plan() -> Result<Vec<u8>, String> {
    derived_plan_bytes(&plugin_root_path())
}

/// Derives the immutable runtime Plan from this Host and one Plugin Root.
///
/// The returned bytes are execution evidence, never authoring input.
pub fn derived_plan_bytes(plugin_root: &Path) -> Result<Vec<u8>, String> {
    let snapshot = plugin_root::snapshot(plugin_root)?;
    let plan = generation::resolve_host_plan(&snapshot)?;
    serde_json::to_vec(&plan).map_err(|error| format!("failed to encode the derived App: {error}"))
}

pub(crate) fn plugin_root_path() -> PathBuf {
    env::var_os("LENSO_PLUGIN_ROOT").map_or_else(|| PathBuf::from("plugins"), PathBuf::from)
}

use std::{
    env, fs,
    path::{Path, PathBuf},
};

mod authority;
mod directories;
pub mod generation;
mod generation_authority;
mod host;
mod plugin_root;
mod profile;
pub mod provenance;
mod runtime_state;
#[cfg(test)]
mod test_support;

pub use directories::{AGENT_HOME_ENV, AgentDirectories};
pub use host::{
    AgentHost, AgentHostBuilder, AgentSurface, AgentSurfaceKind, ChannelSurface,
    ConfiguredAgentHost, DiscordSurface, HeadlessSurface, Profile, TelegramSurface, TuiSurface,
    WebSurface,
};

/// Loads an exact diagnostic Plan override or derives the App from the current Host and Plugin Root.
pub fn plan_bytes(explicit_plan: Option<&Path>) -> Result<Vec<u8>, String> {
    plan_bytes_for_profile(explicit_plan, None)
}

/// Loads one exact Plan or derives an App through a named Session Profile.
pub fn plan_bytes_for_profile(
    explicit_plan: Option<&Path>,
    profile_name: Option<&str>,
) -> Result<Vec<u8>, String> {
    let directories = AgentDirectories::resolve()?;
    plan_bytes_for_profile_in(&directories, explicit_plan, profile_name)
}

pub(crate) fn plan_bytes_for_profile_in(
    directories: &AgentDirectories,
    explicit_plan: Option<&Path>,
    profile_name: Option<&str>,
) -> Result<Vec<u8>, String> {
    ensure_host_catalog(directories)?;
    let explicit_plan = explicit_plan
        .map(PathBuf::from)
        .or_else(|| env::var_os("LENSO_RESOLVED_PLAN").map(PathBuf::from));
    if explicit_plan.is_some() && profile_name.is_some() {
        return Err("--profile conflicts with an exact resolved Plan".to_owned());
    }
    explicit_plan.map_or_else(
        || resolve_base_plan(directories, profile_name),
        |plan| {
            fs::read(&plan).map_err(|error| format!("failed to read {}: {error}", plan.display()))
        },
    )
}

fn ensure_host_catalog(directories: &AgentDirectories) -> Result<(), String> {
    let plugin_root = directories.plugins();
    fs::create_dir_all(&plugin_root).map_err(|error| {
        format!(
            "failed to create visible Plugin Root {}: {error}",
            plugin_root.display()
        )
    })?;
    let path = directories.host_catalog();
    let catalog = generation::linked_host_catalog_in(directories)?;
    let mut bytes = serde_json::to_vec_pretty(&catalog)
        .map_err(|error| format!("failed to encode the Host Catalog: {error}"))?;
    bytes.push(b'\n');
    if fs::read(&path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    let parent = path.parent().expect("Host Catalog has a parent");
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let temporary = parent.join("host-catalog.json.tmp");
    fs::write(&temporary, &bytes)
        .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, &path)
        .map_err(|error| format!("failed to publish {}: {error}", path.display()))
}

fn resolve_base_plan(
    directories: &AgentDirectories,
    profile_name: Option<&str>,
) -> Result<Vec<u8>, String> {
    let plugin_root = directories.plugins();
    let snapshot = plugin_root::snapshot(&plugin_root)?;
    let plan = if let Some(profile_name) = profile_name {
        let profile = profile::select(profile_name, &snapshot, &directories.profiles())?;
        generation::resolve_host_plan_for_agent_in(directories, profile.root(), profile.agent())?
    } else {
        generation::resolve_host_plan_in(directories, &snapshot)?
    };
    serde_json::to_vec(&plan).map_err(|error| format!("failed to encode the derived App: {error}"))
}

/// Derives the immutable runtime Plan from this Host and one Plugin Root.
///
/// The returned bytes are execution evidence, never authoring input.
pub fn derived_plan_bytes(plugin_root: &Path) -> Result<Vec<u8>, String> {
    let directories = AgentDirectories::resolve()?;
    derived_plan_bytes_in(&directories, plugin_root)
}

/// Derives a Plan against one explicit Agent Home.
pub fn derived_plan_bytes_for_home(home: &Path, plugin_root: &Path) -> Result<Vec<u8>, String> {
    let directories = AgentDirectories::from_home(home)?;
    derived_plan_bytes_in(&directories, plugin_root)
}

fn derived_plan_bytes_in(
    directories: &AgentDirectories,
    plugin_root: &Path,
) -> Result<Vec<u8>, String> {
    let snapshot = plugin_root::snapshot(plugin_root)?;
    let plan = generation::resolve_host_plan_in(directories, &snapshot)?;
    serde_json::to_vec(&plan).map_err(|error| format!("failed to encode the derived App: {error}"))
}

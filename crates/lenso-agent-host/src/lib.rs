use std::{
    env, fs,
    path::{Path, PathBuf},
};

mod authority;
mod directories;
pub mod generation;
mod generation_authority;
mod host;
mod official_prompts;
mod online_generation;
mod plugin_root;
mod profile;
pub mod provenance;
mod provider_catalog;
mod runtime_state;
#[cfg(test)]
mod test_support;

pub use directories::{AGENT_HOME_ENV, AgentDirectories};
pub use host::{
    AcpSurface, AgentHost, AgentHostBuilder, AgentSurface, AgentSurfaceKind, ChannelSurface,
    ConfiguredAgentHost, DiscordSurface, HeadlessSurface, Profile, TelegramSurface, TuiSurface,
    WebSurface,
};
pub use official_prompts::migrate_legacy_official_files;
pub use provider_catalog::{
    ModelAuthentication, ModelCapabilities, ModelCatalogEntry, ModelInputModality, ModelLimits,
    ModelProviderCatalogEntry, ModelProviderReadiness, ModelProviderReadinessStatus,
    ModelReasoningControl, ModelServiceTierControl, ModelWireProtocol, ProviderModelCatalog,
    ResolvedTurnProfile,
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
    if let Some(profile_name) = profile_name {
        official_prompts::prepare_named_profile(directories.home(), profile_name)?;
    }
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

/// Validates one staged Plugin Root through the same Profile-aware Host resolution used online.
///
/// This is an authoring preflight only. Runtime authority still comes from the
/// canonical snapshot taken by the reconciler under its authority fence.
pub fn validate_desired_plugin_root_for_home(
    home: &Path,
    profile_name: Option<&str>,
) -> Result<(), String> {
    let (root, plan) = resolve_desired_plugin_root_for_home(home, profile_name)?;
    plugin_root::plan_resources_from_snapshot(&root, &plan).map(drop)
}

/// One immutable, Profile-aware Desired State captured for an authoring receipt.
#[derive(Clone, Debug)]
pub struct DesiredPluginRootSnapshot {
    plan: lenso_app_plan::ResolvedAppPlan,
    plugin_root: lenso_app_plan::authoring::PluginRootSnapshot,
    plugin_root_revision: String,
    desired_state_digest: String,
    plan_digest: String,
    disableable_instance_keys: Vec<String>,
}

impl DesiredPluginRootSnapshot {
    pub const fn plan(&self) -> &lenso_app_plan::ResolvedAppPlan {
        &self.plan
    }

    /// Returns the complete canonical Plugin Root behind this Desired State.
    pub const fn plugin_root(&self) -> &lenso_app_plan::authoring::PluginRootSnapshot {
        &self.plugin_root
    }

    pub fn plugin_root_revision(&self) -> &str {
        &self.plugin_root_revision
    }

    pub fn desired_state_digest(&self) -> &str {
        &self.desired_state_digest
    }

    pub fn plan_digest(&self) -> &str {
        &self.plan_digest
    }

    pub fn disableable_instance_keys(&self) -> &[String] {
        &self.disableable_instance_keys
    }
}

/// Captures the exact Desired State returned by a mutation receipt.
///
/// `home` provides the Plugin Root and Profile; `authority_home` identifies the
/// live runtime authority. Callers must hold [`PluginRootMutationFence`] for
/// `authority_home` while taking a post-commit snapshot so its identity cannot
/// race another authoring client.
pub fn snapshot_desired_plugin_root_for_home(
    home: &Path,
    authority_home: &Path,
    profile_name: Option<&str>,
) -> Result<DesiredPluginRootSnapshot, String> {
    let (root, plan) = resolve_desired_plugin_root_for_home(home, profile_name)?;
    let plugin_root_revision = root.revision()?;
    let resources = plugin_root::plan_resources_from_snapshot(&root, &plan)?;
    let authority_directories = AgentDirectories::from_home(authority_home)?;
    let authority =
        generation_authority::load_generation_authority_unfenced(&authority_directories.runtime());
    let (desired_state_digest, plan_digest) = generation::desired_generation_identity(
        &authority.resolution_authority_digest,
        &plan,
        &resources,
    )?;
    let disableable_instance_keys = root
        .root()
        .instances()
        .iter()
        .map(|instance| instance.id().to_string())
        .chain(root.root().disabled().iter().map(ToString::to_string))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    Ok(DesiredPluginRootSnapshot {
        plan,
        plugin_root: root.root().clone(),
        plugin_root_revision,
        desired_state_digest,
        plan_digest,
        disableable_instance_keys,
    })
}

fn resolve_desired_plugin_root_for_home(
    home: &Path,
    profile_name: Option<&str>,
) -> Result<
    (
        plugin_root::PluginRootContents,
        lenso_app_plan::ResolvedAppPlan,
    ),
    String,
> {
    let directories = AgentDirectories::from_home(home)?;
    let root = plugin_root::snapshot_with_resources(&directories.plugins())?;
    let plan = if let Some(profile_name) = profile_name {
        let profile = profile::select(profile_name, root.root(), &directories.profiles())?;
        generation::resolve_host_plan_for_agent_in(&directories, profile.root(), profile.agent())?
    } else {
        generation::resolve_host_plan_in(&directories, root.root())?
    };
    Ok((root, plan))
}

/// Exclusive cross-process fence for publishing one Plugin Root mutation.
///
/// Hold this value from the canonical pre-mutation snapshot through the final
/// filesystem commit. Online reconcilers keep serving the current Generation
/// and retry after the fence is released.
#[derive(Debug)]
pub struct PluginRootMutationFence {
    _authority: authority::AuthorityFence,
}

/// Fences one Plugin Root authoring transaction against every online reconciler.
pub fn fence_plugin_root_mutation_for_home(home: &Path) -> Result<PluginRootMutationFence, String> {
    let directories = AgentDirectories::from_home(home)?;
    let coordinator = authority::AuthorityCoordinator::prepare(&directories.runtime())?;
    let authority = coordinator.transition()?;
    Ok(PluginRootMutationFence {
        _authority: authority,
    })
}

fn derived_plan_bytes_in(
    directories: &AgentDirectories,
    plugin_root: &Path,
) -> Result<Vec<u8>, String> {
    let snapshot = plugin_root::snapshot(plugin_root)?;
    let plan = generation::resolve_host_plan_in(directories, &snapshot)?;
    serde_json::to_vec(&plan).map_err(|error| format!("failed to encode the derived App: {error}"))
}

//! Explicit deployment of Agent DX resources emitted by an App build.
//!
//! File conventions can produce a candidate Tool Provider and an optional
//! Profile source, or a Profile-only composition. This module is the only
//! place that imports those bytes into an Agent Home. It verifies the immutable
//! distribution, stages every actual Plugin candidate, and asks the ordinary
//! Host resolver to admit the resulting Profile before publishing visible
//! Plugin Root or Profile files.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as FmtWrite,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::{
    ensure_unmanaged_plugin_root, fence_plugin_root_mutation_for_home,
    profile::validate_profile_name,
};

/// Schema emitted by the optional Agent convention support package.
pub const APP_DEPLOYMENT_SCHEMA: &str = "lenso.agent.deployment@1";
/// Profile-only deployment resources have no Plugin or Bundle and therefore
/// use a separate schema rather than weakening the established v1 identity
/// contract for Tool Provider contributions.
pub const PROFILE_ONLY_APP_DEPLOYMENT_SCHEMA: &str = "lenso.agent.deployment@2";
const APP_RESOURCES_SCHEMA: &str = "lenso.app-resources.v1";
const MAX_RESOURCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ARCHIVE_FILES: usize = 4_096;
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;

/// A read-only explanation of one Agent contribution selected from an App
/// distribution. It does not imply that the contribution has been installed
/// or granted to a model.
#[derive(Clone, Debug, Serialize)]
pub struct AppDeploymentInspection {
    /// Resource-only contributions have no Plugin identity. The Engine-assigned
    /// contribution identity makes their source inventory auditable without
    /// representing them as installed Plugins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contribution_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    pub profile: Option<String>,
    pub resource_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_path: Option<PathBuf>,
}

/// Receipt for one explicit local Agent Home publication.
#[derive(Clone, Debug, Serialize)]
pub struct AppDeploymentApplied {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contribution_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    pub profile: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct ResourceDocument {
    schema: String,
    resources: Vec<ResourceEntry>,
}

#[derive(Clone, Debug, Deserialize)]
struct ResourceEntry {
    owner: String,
    schema: String,
    path: String,
    sha256: String,
    size: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct BundleEntry {
    plugin_id: String,
    path: String,
    manifest_digest: String,
}

#[derive(Clone, Debug)]
struct DeploymentDocument {
    contribution_id: String,
    plugin: Option<DeploymentPlugin>,
    profile: Option<DeploymentProfile>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginDeploymentDocument {
    schema: String,
    plugin: DeploymentPlugin,
    #[serde(default)]
    profile: Option<DeploymentProfile>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileOnlyDeploymentDocument {
    schema: String,
    contribution: DeploymentContribution,
    profile: DeploymentProfile,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentContribution {
    id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentPlugin {
    id: String,
    instance: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentProfile {
    name: String,
    source_toml: String,
    #[serde(default)]
    instructions: Option<String>,
}

#[derive(Clone, Debug)]
struct LoadedDeployment {
    document: DeploymentDocument,
    resource_path: PathBuf,
    bundle: Option<LoadedBundle>,
}

#[derive(Clone, Debug)]
struct LoadedBundle {
    path: PathBuf,
    manifest_digest: String,
}

/// Reads and verifies the Agent deployment resources in an App distribution
/// without touching an Agent Home.
pub fn inspect_app_deployments(
    distribution: &Path,
) -> Result<Vec<AppDeploymentInspection>, String> {
    load_deployments(distribution).map(|deployments| {
        deployments
            .into_iter()
            .map(|deployment| {
                let document = deployment.document;
                let contribution_id = document
                    .plugin
                    .is_none()
                    .then_some(document.contribution_id.clone());
                let plugin_id = document.plugin.as_ref().map(|plugin| plugin.id.clone());
                let instance = document
                    .plugin
                    .as_ref()
                    .map(|plugin| plugin.instance.clone());
                AppDeploymentInspection {
                    contribution_id,
                    plugin_id,
                    instance,
                    profile: document.profile.map(|profile| profile.name),
                    resource_path: deployment.resource_path,
                    bundle_path: deployment.bundle.map(|bundle| bundle.path),
                }
            })
            .collect()
    })
}

/// Imports verified Agent DX resources into one unmanaged Agent Home.
///
/// The source App distribution remains immutable. Existing Plugin and Profile
/// paths are never overwritten; callers must use the ordinary Plugin/Profile
/// lifecycle to change or remove an already installed contribution.
pub fn apply_app_deployments(
    distribution: &Path,
    home: &Path,
) -> Result<Vec<AppDeploymentApplied>, String> {
    ensure_unmanaged_plugin_root(home)?;
    let _fence = fence_plugin_root_mutation_for_home(home)?;
    ensure_unmanaged_plugin_root(home)?;
    let deployments = load_deployments(distribution)?;
    ensure_targets_are_new(home, &deployments)?;

    let stage = stage_home(home)?;
    let plugins = stage.path().join("plugins");
    let profiles = stage.path().join("profiles");
    let mut applied = Vec::with_capacity(deployments.len());
    for deployment in &deployments {
        if deployment.document.plugin.is_some() {
            stage_bundle(deployment, &plugins)?;
        }
        let profile = deployment
            .document
            .profile
            .as_ref()
            .map(|profile| render_profile(profile, deployment.document.plugin.as_ref()))
            .transpose()?;
        if let Some((name, document)) = &profile {
            fs::write(profiles.join(format!("{name}.toml")), document)
                .map_err(|error| format!("failed to stage Agent Profile `{name}`: {error}"))?;
        }
        applied.push(AppDeploymentApplied {
            contribution_id: deployment
                .document
                .plugin
                .is_none()
                .then_some(deployment.document.contribution_id.clone()),
            plugin_id: deployment
                .document
                .plugin
                .as_ref()
                .map(|plugin| plugin.id.clone()),
            instance: deployment
                .document
                .plugin
                .as_ref()
                .map(|plugin| plugin.instance.clone()),
            profile: profile.map(|(name, _)| name),
        });
    }

    // Validate both the ordinary App and every Profile through the same Host
    // resolution and Adapter admission path that will own execution later.
    crate::validate_desired_plugin_root_for_home(stage.path(), None)?;
    for receipt in &applied {
        if let Some(profile) = &receipt.profile {
            crate::validate_desired_plugin_root_for_home(stage.path(), Some(profile))?;
        }
    }

    for receipt in &applied {
        if let Some(plugin_id) = &receipt.plugin_id {
            let staged_plugin = plugins.join(plugin_id);
            let target = home.join("plugins").join(plugin_id);
            let parent = target
                .parent()
                .ok_or_else(|| format!("Plugin target has no parent: {}", target.display()))?;
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
            fs::rename(&staged_plugin, &target).map_err(|error| {
                format!(
                    "failed to publish Agent Plugin {plugin_id} into {}: {error}",
                    target.display()
                )
            })?;
        }
        if let Some(profile) = &receipt.profile {
            let staged_profile = profiles.join(format!("{profile}.toml"));
            let target = home.join("profiles").join(format!("{profile}.toml"));
            let parent = target
                .parent()
                .ok_or_else(|| format!("Profile target has no parent: {}", target.display()))?;
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
            fs::rename(&staged_profile, &target)
                .map_err(|error| format!("failed to publish Agent Profile `{profile}`: {error}"))?;
        }
    }
    Ok(applied)
}

fn load_deployments(distribution: &Path) -> Result<Vec<LoadedDeployment>, String> {
    let distribution = fs::canonicalize(distribution).map_err(|error| {
        format!(
            "failed to resolve App distribution {}: {error}",
            distribution.display()
        )
    })?;
    if !distribution.is_dir() {
        return Err(format!(
            "App distribution must be a directory: {}",
            distribution.display()
        ));
    }
    let resources: ResourceDocument = read_json(&distribution, "resources.json")?;
    if resources.schema != APP_RESOURCES_SCHEMA {
        return Err("unsupported App resource inventory schema".to_owned());
    }
    let bundles: Vec<BundleEntry> = read_json(&distribution, "bundles.json")?;
    let mut bundles_by_plugin = BTreeMap::new();
    for bundle in bundles {
        if bundles_by_plugin
            .insert(bundle.plugin_id.clone(), bundle)
            .is_some()
        {
            return Err("App distribution has duplicate Bundle Plugin identities".to_owned());
        }
    }

    let mut deployments = Vec::new();
    let mut contribution_ids = BTreeSet::new();
    let mut profile_names = BTreeSet::new();
    for resource in resources.resources.into_iter().filter(|resource| {
        resource.schema == APP_DEPLOYMENT_SCHEMA
            || resource.schema == PROFILE_ONLY_APP_DEPLOYMENT_SCHEMA
    }) {
        let resource_path = resolve_regular_file(&distribution, &resource.path)?;
        let bytes = fs::read(&resource_path)
            .map_err(|error| format!("failed to read {}: {error}", resource_path.display()))?;
        if bytes.len() as u64 != resource.size || resource.size > MAX_RESOURCE_BYTES {
            return Err(format!(
                "Agent deployment resource has an invalid size: {}",
                resource.path
            ));
        }
        if hash(&bytes) != resource.sha256 {
            return Err(format!(
                "Agent deployment resource digest does not match: {}",
                resource.path
            ));
        }
        let document = decode_deployment(&resource.schema, &bytes).map_err(|error| {
            format!(
                "invalid Agent deployment resource {}: {error}",
                resource.path
            )
        })?;
        validate_deployment(&document)?;
        if let Some(profile) = &document.profile {
            render_profile(profile, document.plugin.as_ref())?;
        }
        if resource.owner != document.contribution_id {
            return Err(
                "Agent deployment resource owner does not match its contribution identity"
                    .to_owned(),
            );
        }
        if !contribution_ids.insert(document.contribution_id.clone()) {
            return Err(
                "App distribution declares an Agent contribution more than once".to_owned(),
            );
        }
        if let Some(profile) = &document.profile
            && !profile_names.insert(profile.name.clone())
        {
            return Err("App distribution declares an Agent Profile more than once".to_owned());
        }
        let bundle = document
            .plugin
            .as_ref()
            .map(|plugin| -> Result<LoadedBundle, String> {
                let bundle = bundles_by_plugin.get(&plugin.id).ok_or_else(|| {
                    format!(
                        "Agent deployment has no matching Plugin Bundle: {}",
                        plugin.id
                    )
                })?;
                Ok(LoadedBundle {
                    path: resolve_regular_file(&distribution, &bundle.path)?,
                    manifest_digest: bundle.manifest_digest.clone(),
                })
            })
            .transpose()?;
        deployments.push(LoadedDeployment {
            document,
            resource_path: PathBuf::from(resource.path),
            bundle,
        });
    }
    if deployments.is_empty() {
        return Err("App distribution has no Agent deployment resources".to_owned());
    }
    Ok(deployments)
}

fn decode_deployment(schema: &str, bytes: &[u8]) -> Result<DeploymentDocument, String> {
    match schema {
        APP_DEPLOYMENT_SCHEMA => {
            let document: PluginDeploymentDocument = serde_json::from_slice(bytes)
                .map_err(|error| format!("parse v1 Tool Provider deployment: {error}"))?;
            if document.schema != APP_DEPLOYMENT_SCHEMA {
                return Err(
                    "v1 deployment payload schema does not match its resource inventory".to_owned(),
                );
            }
            Ok(DeploymentDocument {
                contribution_id: document.plugin.id.clone(),
                plugin: Some(document.plugin),
                profile: document.profile,
            })
        }
        PROFILE_ONLY_APP_DEPLOYMENT_SCHEMA => {
            let document: ProfileOnlyDeploymentDocument = serde_json::from_slice(bytes)
                .map_err(|error| format!("parse v2 Profile-only deployment: {error}"))?;
            if document.schema != PROFILE_ONLY_APP_DEPLOYMENT_SCHEMA {
                return Err(
                    "v2 deployment payload schema does not match its resource inventory".to_owned(),
                );
            }
            Ok(DeploymentDocument {
                contribution_id: document.contribution.id,
                plugin: None,
                profile: Some(document.profile),
            })
        }
        _ => Err(format!(
            "unsupported Agent deployment resource schema: {schema}"
        )),
    }
}

fn validate_deployment(deployment: &DeploymentDocument) -> Result<(), String> {
    if !valid_plugin_id(&deployment.contribution_id) {
        return Err("Agent deployment has an invalid contribution identity".to_owned());
    }
    if let Some(plugin) = &deployment.plugin
        && (!valid_plugin_id(&plugin.id) || plugin.instance != "default")
    {
        return Err("Agent deployment has an invalid Plugin identity or instance".to_owned());
    }
    if deployment.plugin.is_none() && deployment.profile.is_none() {
        return Err("Agent deployment must contain a Plugin or a Profile".to_owned());
    }
    if let Some(profile) = &deployment.profile {
        validate_profile_name(&profile.name)?;
        if profile.source_toml.len() > 256 * 1024 {
            return Err("Agent deployment Profile source exceeds 256 KiB".to_owned());
        }
        if let Some(instructions) = &profile.instructions
            && (instructions.trim().is_empty() || instructions.len() > 65_536)
        {
            return Err(
                "Agent deployment instructions must contain 1 through 65536 characters".to_owned(),
            );
        }
    }
    Ok(())
}

fn render_profile(
    source: &DeploymentProfile,
    plugin: Option<&DeploymentPlugin>,
) -> Result<(String, String), String> {
    validate_profile_name(&source.name)?;
    let mut document: toml::Value = toml::from_str(&source.source_toml)
        .map_err(|error| format!("invalid Agent Profile source: {error}"))?;
    let table = document
        .as_table_mut()
        .ok_or_else(|| "Agent Profile source must be a TOML table".to_owned())?;
    let lenso = table
        .remove("lenso")
        .ok_or_else(|| "Agent Profile source requires a [lenso] table".to_owned())?;
    let lenso = lenso
        .as_table()
        .ok_or_else(|| "Agent Profile [lenso] must be a table".to_owned())?;
    if lenso.len() != 1
        || lenso.get("profile").and_then(toml::Value::as_str) != Some(source.name.as_str())
    {
        return Err("Agent Profile [lenso] must contain only its matching profile name".to_owned());
    }
    let instances = table
        .get("instances")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| "Agent Profile source requires instances = [...]".to_owned())?;
    if instances.iter().any(|instance| {
        instance
            .as_str()
            .is_none_or(|instance| instance.trim().is_empty())
    }) {
        return Err("Agent Profile instances must be nonempty strings".to_owned());
    }
    if !table
        .get("allowed_tools")
        .is_some_and(toml::Value::is_array)
    {
        return Err(
            "Agent Profile source must declare allowed_tools explicitly before it selects convention Tools"
                .to_owned(),
        );
    }
    if let Some(instructions) = &source.instructions {
        if table
            .get("instructions")
            .and_then(toml::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(
                "Agent Profile source cannot set instructions when instructions.md is present"
                    .to_owned(),
            );
        }
        table.insert(
            "instructions".to_owned(),
            toml::Value::String(instructions.clone()),
        );
    }
    if let Some(plugin) = plugin {
        let generated_instance = format!("{}/{}", plugin.id, plugin.instance);
        let instances = table
            .get_mut("instances")
            .and_then(toml::Value::as_array_mut)
            .expect("validated Agent Profile instances remain an array");
        if !instances
            .iter()
            .any(|instance| instance.as_str() == Some(generated_instance.as_str()))
        {
            instances.push(toml::Value::String(generated_instance));
        }
    }
    toml::to_string_pretty(&document)
        .map(|document| (source.name.clone(), document))
        .map_err(|error| format!("failed to render Agent Profile: {error}"))
}

fn ensure_targets_are_new(home: &Path, deployments: &[LoadedDeployment]) -> Result<(), String> {
    for deployment in deployments {
        if let Some(plugin) = &deployment.document.plugin {
            let path = home.join("plugins").join(&plugin.id);
            if path.try_exists().map_err(|error| error.to_string())? {
                return Err(format!(
                    "refusing to overwrite existing Agent Plugin directory: {}",
                    path.display()
                ));
            }
        }
        if let Some(profile) = &deployment.document.profile {
            let path = home.join("profiles").join(format!("{}.toml", profile.name));
            if path.try_exists().map_err(|error| error.to_string())? {
                return Err(format!(
                    "refusing to overwrite existing Agent Profile: {}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn stage_home(home: &Path) -> Result<tempfile::TempDir, String> {
    let parent = home
        .parent()
        .ok_or_else(|| format!("Agent Home has no parent: {}", home.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create Agent Home parent {}: {error}",
            parent.display()
        )
    })?;
    let stage = tempfile::Builder::new()
        .prefix(".lenso-agent-dx-")
        .tempdir_in(parent)
        .map_err(|error| format!("failed to stage Agent Home: {error}"))?;
    for name in ["plugins", "profiles"] {
        let source = home.join(name);
        let destination = stage.path().join(name);
        if source.try_exists().map_err(|error| error.to_string())? {
            copy_tree(&source, &destination, &mut 0, &mut 0, 0)?;
        } else {
            fs::create_dir(&destination)
                .map_err(|error| format!("failed to create {}: {error}", destination.display()))?;
        }
    }
    Ok(stage)
}

fn stage_bundle(deployment: &LoadedDeployment, plugins: &Path) -> Result<(), String> {
    let plugin = deployment
        .document
        .plugin
        .as_ref()
        .ok_or_else(|| "Profile-only deployment has no Plugin Bundle to stage".to_owned())?;
    let bundle_source = deployment
        .bundle
        .as_ref()
        .ok_or_else(|| "Agent deployment has no matching Plugin Bundle".to_owned())?;
    let plugin_directory = plugins.join(&plugin.id);
    fs::create_dir(&plugin_directory)
        .map_err(|error| format!("failed to stage Agent Plugin directory: {error}"))?;
    let bundle = plugin_directory.join("plugin.lenso-plugin");
    extract_bundle(&bundle_source.path, &bundle)?;
    let verified = lenso_plugin_bundle::verify_bundle_directory(&bundle)
        .map_err(|error| format!("failed to verify Agent Plugin Bundle: {error}"))?;
    if verified.plugin_id != plugin.id || verified.manifest_digest != bundle_source.manifest_digest
    {
        return Err("Agent Plugin Bundle identity does not match the App distribution".to_owned());
    }
    // An App distribution can propose a provider but cannot make it visible to
    // the default Agent. The authored Profile explicitly selects this marker
    // and binds its allowlist; a tools-only contribution remains disabled until
    // the user configures it through the ordinary Agent lifecycle.
    fs::write(plugin_directory.join("default.toml"), "")
        .map_err(|error| format!("failed to stage Agent Plugin configuration: {error}"))?;
    fs::write(plugin_directory.join("default.disabled"), "")
        .map_err(|error| format!("failed to stage disabled Agent Plugin marker: {error}"))
}

fn read_json<T: for<'de> Deserialize<'de>>(root: &Path, name: &str) -> Result<T, String> {
    let path = resolve_regular_file(root, name)?;
    let bytes =
        fs::read(&path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("invalid {}: {error}", path.display()))
}

fn resolve_regular_file(root: &Path, relative: impl AsRef<Path>) -> Result<PathBuf, String> {
    let relative = relative.as_ref();
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "distribution path must be a relative file: {}",
            relative.display()
        ));
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("failed to inspect {}: {error}", current.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "distribution path cannot traverse a symbolic link: {}",
                relative.display()
            ));
        }
    }
    if !fs::symlink_metadata(&current)
        .map_err(|error| format!("failed to inspect {}: {error}", current.display()))?
        .file_type()
        .is_file()
    {
        return Err(format!(
            "distribution path must be a regular file: {}",
            relative.display()
        ));
    }
    Ok(current)
}

fn extract_bundle(archive: &Path, destination: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|error| {
        format!(
            "failed to open App Plugin Bundle {}: {error}",
            archive.display()
        )
    })?;
    let mut archive = ZipArchive::new(file).map_err(|error| {
        format!(
            "failed to read App Plugin Bundle {}: {error}",
            archive.display()
        )
    })?;
    if archive.len() > MAX_ARCHIVE_FILES {
        return Err("App Plugin Bundle exceeds the file limit".to_owned());
    }
    fs::create_dir(destination)
        .map_err(|error| format!("failed to create Bundle staging directory: {error}"))?;
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("failed to read App Plugin Bundle entry: {error}"))?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| "App Plugin Bundle contains an unsafe path".to_owned())?;
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err("App Plugin Bundle contains an invalid path".to_owned());
        }
        if entry.is_dir() {
            fs::create_dir_all(destination.join(relative))
                .map_err(|error| format!("failed to create Bundle directory: {error}"))?;
            continue;
        }
        if !entry.is_file()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
        {
            return Err("App Plugin Bundle contains a non-file entry".to_owned());
        }
        #[cfg(unix)]
        let executable = entry.unix_mode().is_some_and(|mode| mode & 0o111 != 0);
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| "App Plugin Bundle size overflow".to_owned())?;
        if total > MAX_ARCHIVE_BYTES {
            return Err("App Plugin Bundle exceeds the size limit".to_owned());
        }
        let output = destination.join(relative);
        let parent = output
            .parent()
            .ok_or_else(|| "App Plugin Bundle entry has no parent".to_owned())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create Bundle parent: {error}"))?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&output)
            .map_err(|error| {
                format!("failed to create Bundle file {}: {error}", output.display())
            })?;
        std::io::copy(&mut entry, &mut file).map_err(|error| {
            format!(
                "failed to extract Bundle file {}: {error}",
                output.display()
            )
        })?;
        file.flush().map_err(|error| {
            format!("failed to flush Bundle file {}: {error}", output.display())
        })?;
        // `OpenOptions::create_new` uses the current umask and consequently
        // loses the executable bit carried by a Process implementation in a
        // verified Bundle. Keep the extracted Plugin private to the Agent
        // owner, then restore execution only when the archive entry declared
        // it. This never forwards setuid, group, or world permissions.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = if executable { 0o700 } else { 0o600 };
            fs::set_permissions(&output, fs::Permissions::from_mode(mode)).map_err(|error| {
                format!(
                    "failed to set Bundle file permissions {}: {error}",
                    output.display()
                )
            })?;
        }
    }
    Ok(())
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    files: &mut usize,
    bytes: &mut u64,
    depth: usize,
) -> Result<(), String> {
    if depth > 32 {
        return Err("Agent Home staging exceeds 32 directory levels".to_owned());
    }
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("failed to inspect {}: {error}", source.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "Agent Home staging rejects symbolic links: {}",
            source.display()
        ));
    }
    if metadata.is_dir() {
        fs::create_dir(destination)
            .map_err(|error| format!("failed to create {}: {error}", destination.display()))?;
        let mut entries = fs::read_dir(source)
            .map_err(|error| format!("failed to read {}: {error}", source.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("failed to enumerate {}: {error}", source.display()))?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            copy_tree(
                &entry.path(),
                &destination.join(entry.file_name()),
                files,
                bytes,
                depth + 1,
            )?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(format!(
            "Agent Home staging rejects special files: {}",
            source.display()
        ));
    }
    *files += 1;
    *bytes = bytes
        .checked_add(metadata.len())
        .ok_or_else(|| "Agent Home staging size overflow".to_owned())?;
    if *files > MAX_ARCHIVE_FILES || *bytes > MAX_ARCHIVE_BYTES {
        return Err("Agent Home staging exceeds its resource limits".to_owned());
    }
    fs::copy(source, destination)
        .map_err(|error| format!("failed to copy {}: {error}", source.display()))?;
    Ok(())
}

fn valid_plugin_id(value: &str) -> bool {
    lenso_app_authoring::identity::classify_existing_plugin_id(value).is_ok()
}

fn hash(bytes: &[u8]) -> String {
    let bytes = Sha256::digest(bytes);
    let mut digest = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut digest, "{byte:02x}").expect("writing to a String cannot fail");
    }
    format!("sha256:{digest}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_rendering_requires_an_explicit_tool_allowlist_and_injects_the_provider() {
        let profile = DeploymentProfile {
            name: "orders".to_owned(),
            source_toml:
                "instances = []\nallowed_tools = [\"greet\"]\n\n[lenso]\nprofile = \"orders\"\n"
                    .to_owned(),
            instructions: Some("Follow the order rules.".to_owned()),
        };
        let plugin = DeploymentPlugin {
            id: "example.orders".to_owned(),
            instance: "default".to_owned(),
        };
        let (name, rendered) = render_profile(&profile, Some(&plugin)).unwrap();
        assert_eq!(name, "orders");
        assert!(rendered.contains("example.orders/default"));
        assert!(rendered.contains("Follow the order rules."));
        assert!(!rendered.contains("[lenso]"));

        let missing_policy = DeploymentProfile {
            source_toml: "instances = []\n\n[lenso]\nprofile = \"orders\"\n".to_owned(),
            ..profile
        };
        assert!(render_profile(&missing_policy, Some(&plugin)).is_err());
    }

    #[test]
    fn profile_only_resources_need_no_bundle_and_preserve_declared_instances() {
        let temporary = tempfile::tempdir().unwrap();
        let distribution = temporary.path().join("distribution");
        let owner = "example.assistant";
        let relative = format!("resources/{owner}/lenso-agent-deployment.json");
        let source = "instances = [\"lenso.agent.loop/agent\"]\nallowed_tools = []\n\n[lenso]\nprofile = \"assistant\"\n";
        let deployment = serde_json::json!({
            "schema": PROFILE_ONLY_APP_DEPLOYMENT_SCHEMA,
            "contribution": {"id": owner},
            "profile": {
                "name": "assistant",
                "source_toml": source,
                "instructions": "Answer only from the approved records.\n",
            },
        });
        let bytes = serde_json::to_vec(&deployment).unwrap();
        let path = distribution.join(&relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &bytes).unwrap();
        fs::write(
            distribution.join("resources.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": APP_RESOURCES_SCHEMA,
                "resources": [{
                    "owner": owner,
                    "schema": PROFILE_ONLY_APP_DEPLOYMENT_SCHEMA,
                    "path": relative,
                    "sha256": hash(&bytes),
                    "size": bytes.len(),
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(distribution.join("bundles.json"), "[]").unwrap();

        let inspected = inspect_app_deployments(&distribution).unwrap();
        assert_eq!(inspected.len(), 1);
        assert_eq!(inspected[0].contribution_id.as_deref(), Some(owner));
        assert!(inspected[0].plugin_id.is_none());
        assert!(inspected[0].instance.is_none());
        assert!(inspected[0].bundle_path.is_none());
        assert_eq!(inspected[0].profile.as_deref(), Some("assistant"));

        let profile = DeploymentProfile {
            name: "assistant".to_owned(),
            source_toml: source.to_owned(),
            instructions: Some("Answer only from the approved records.\n".to_owned()),
        };
        let (_, rendered) = render_profile(&profile, None).unwrap();
        let rendered: toml::Value = toml::from_str(&rendered).unwrap();
        assert_eq!(
            rendered["instances"].as_array().unwrap(),
            &[toml::Value::String("lenso.agent.loop/agent".to_owned())]
        );
        assert!(!rendered.to_string().contains("example.assistant/default"));
    }

    #[cfg(unix)]
    #[test]
    fn extraction_restores_only_owner_execution_for_process_entries() {
        use std::{io::Write, os::unix::fs::PermissionsExt};

        let temporary = tempfile::tempdir().unwrap();
        let archive = temporary.path().join("provider.lenso-plugin");
        let file = File::create(&archive).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file(
                "implementations/process/plugin",
                zip::write::SimpleFileOptions::default().unix_permissions(0o755),
            )
            .unwrap();
        writer.write_all(b"fixture").unwrap();
        writer.finish().unwrap();

        let destination = temporary.path().join("bundle");
        extract_bundle(&archive, &destination).unwrap();
        let mode = fs::metadata(destination.join("implementations/process/plugin"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}

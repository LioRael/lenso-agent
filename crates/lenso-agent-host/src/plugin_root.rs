use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use lenso_app_plan::{
    ExecutionClassId, ResolvedAppPlan,
    authoring::{PluginDescriptor, PluginInstanceId, PluginRootInstance, PluginRootSnapshot},
};
use lenso_plugin_bundle::{
    ImplementationPolicy, RuntimeAdmission, read_bundle_manifest, resolve_implementation,
    verify_bundle_directory,
};
use lenso_plugin_control_plane::{PlanArtifact, sha256_digest};
use lenso_runtime_codec::{ArtifactHandle, InstanceResourceCatalog, InstanceResources};

const BUNDLE_NAME: &str = "plugin.lenso-plugin";
const MAX_CONFIGURATION_BYTES: u64 = 256 * 1024;
const MAX_CONFIGURATION_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PLUGIN_ROOT_ENTRIES: usize = 16_384;
const MAX_PLUGIN_ROOT_INSTANCES: usize = 4_096;
const MAX_PLUGIN_ROOT_PLUGINS: usize = 1_024;
const MAX_PROBE_DEPTH: usize = 64;
const MAX_PROBE_ENTRIES: usize = MAX_PLUGIN_ROOT_ENTRIES + 2;
const MAX_RESOURCE_FILES: usize = 4_096;
const MAX_RESOURCE_FILE_BYTES: u64 = 1024 * 1024;
const MAX_RESOURCE_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ROOT_RESOURCE_FILES: usize = 4_096;
const MAX_ROOT_RESOURCE_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RESOURCE_DEPTH: usize = 32;

static CANONICAL_SNAPSHOTS: AtomicU64 = AtomicU64::new(0);
static METADATA_PROBES: AtomicU64 = AtomicU64::new(0);
static RESOURCE_DIRECTORY_READS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
std::thread_local! {
    // Unit-test before/after assertions must not observe snapshots from parallel test threads.
    static TEST_CANONICAL_SNAPSHOTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

pub(crate) fn snapshot(path: &Path) -> Result<PluginRootSnapshot, String> {
    snapshot_with_resources(path).map(|snapshot| snapshot.root)
}

#[derive(Clone, Debug)]
pub(crate) struct PluginRootContents {
    root: PluginRootSnapshot,
    resources: BTreeMap<String, InstanceResources>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DesiredStateProbe(String);

/// Captures only filesystem identity metadata. The result is a wakeup hint,
/// never Plugin authority; every changed or failed probe is followed by the
/// complete canonical snapshot and Ready Gate path.
pub(crate) fn desired_state_probe(
    plugin_root: &Path,
    selected_profile: Option<&Path>,
) -> Result<DesiredStateProbe, String> {
    METADATA_PROBES.fetch_add(1, Ordering::Relaxed);
    let mut entries = Vec::new();
    let mut budget = ProbeBudget::default();
    probe_path(
        plugin_root,
        Path::new("plugins"),
        0,
        &mut budget,
        &mut entries,
    )?;
    if let Some(profile) = selected_profile {
        probe_path(profile, Path::new("profile"), 0, &mut budget, &mut entries)?;
    }
    entries.sort();
    serde_json::to_vec(&entries)
        .map(|bytes| DesiredStateProbe(sha256_digest(&bytes)))
        .map_err(|error| format!("failed to identify Plugin consistency metadata: {error}"))
}

fn probe_path(
    path: &Path,
    relative: &Path,
    depth: usize,
    budget: &mut ProbeBudget,
    entries: &mut Vec<(String, &'static str, u64, Option<u128>)>,
) -> Result<(), String> {
    if depth > MAX_PROBE_DEPTH {
        return Err(format!(
            "Plugin consistency metadata exceeds {MAX_PROBE_DEPTH} levels: {}",
            path.display()
        ));
    }
    let relative = probe_relative_path(relative)?;
    budget.admit(&relative, path)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            entries.push((relative, "missing", 0, None));
            return Ok(());
        }
        Err(error) => return Err(format!("failed to inspect {}: {error}", path.display())),
    };
    let file_type = metadata.file_type();
    let kind = if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_dir() {
        "directory"
    } else if file_type.is_file() {
        "file"
    } else {
        "special"
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    entries.push((relative.clone(), kind, metadata.len(), modified));
    if !file_type.is_dir() || file_type.is_symlink() {
        return Ok(());
    }
    let mut children = read_entries_bounded(
        path,
        MAX_PROBE_ENTRIES.saturating_sub(budget.entries),
        "Plugin consistency metadata",
    )?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let child_path = child.path();
        let name = utf8_name(&child_path, &child.file_name())?;
        if is_ignored_os_metadata(&name) {
            continue;
        }
        probe_path(
            &child_path,
            &Path::new(&relative).join(name),
            depth + 1,
            budget,
            entries,
        )?;
    }
    Ok(())
}

#[derive(Debug, Default)]
struct ProbeBudget {
    entries: usize,
}

impl ProbeBudget {
    fn admit(&mut self, _relative: &str, path: &Path) -> Result<(), String> {
        self.entries = self.entries.saturating_add(1);
        if self.entries > MAX_PROBE_ENTRIES {
            return Err(format!(
                "Plugin consistency metadata exceeds {MAX_PROBE_ENTRIES} entries: {}",
                path.display()
            ));
        }
        Ok(())
    }
}

fn probe_relative_path(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("Plugin consistency path is not UTF-8: {}", path.display()))
}

impl PluginRootContents {
    pub(crate) const fn root(&self) -> &PluginRootSnapshot {
        &self.root
    }

    pub(crate) fn revision(&self) -> Result<String, String> {
        revision(&self.root)
    }
}

pub(crate) fn revision(root: &PluginRootSnapshot) -> Result<String, String> {
    serde_json::to_vec(root)
        .map(|bytes| sha256_digest(&bytes))
        .map_err(|error| format!("failed to identify Plugin Root revision: {error}"))
}

/// Reads and validates one complete Plugin Root, retaining immutable resource
/// bytes so Generation resolution never needs to read them a second time.
pub(crate) fn snapshot_with_resources(path: &Path) -> Result<PluginRootContents, String> {
    record_canonical_snapshot();
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(format!(
                "Plugin Root must be a regular directory: {}",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PluginRootContents {
                root: PluginRootSnapshot::default(),
                resources: BTreeMap::new(),
            });
        }
        Err(error) => return Err(format!("failed to inspect {}: {error}", path.display())),
    }

    let mut releases = Vec::new();
    let mut instances = Vec::new();
    let mut disabled = Vec::new();
    let mut resources = BTreeMap::new();
    let mut budget = PluginRootBudget::default();
    let mut normalized = BTreeMap::new();
    let mut entries = read_entries_bounded(path, budget.remaining_entries(), "Plugin Root")?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let entry_path = entry.path();
        let name = utf8_name(&entry_path, &entry.file_name())?;
        if is_ignored_os_metadata(&name) {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect {}: {error}", entry.path().display()))?;
        if !file_type.is_dir() {
            return Err(format!(
                "unknown Plugin Root entry: {}",
                entry.path().display()
            ));
        }
        let plugin_id = name;
        validate_identity(&plugin_id, "Plugin ID")?;
        reject_case_collision(&mut normalized, &plugin_id, "Plugin ID")?;
        budget.admit_entry(&entry_path)?;
        budget.admit_plugin(&plugin_id, &entry_path)?;
        scan_plugin(
            &entry.path(),
            &plugin_id,
            &mut releases,
            &mut instances,
            &mut disabled,
            &mut resources,
            &mut budget,
        )?;
    }
    Ok(PluginRootContents {
        root: PluginRootSnapshot::new(releases, instances, disabled),
        resources,
    })
}

pub(crate) fn plan_artifacts(
    path: &Path,
    plan: &ResolvedAppPlan,
) -> Result<Vec<PlanArtifact>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut artifacts = Vec::new();
    for instance in plan.plugin_instances() {
        let Some((plugin_id, _)) = instance.instance_key().split_once('/') else {
            continue;
        };
        let bundle = path.join(plugin_id).join(BUNDLE_NAME);
        if !bundle.exists() {
            continue;
        }
        verify_bundle_directory(&bundle).map_err(|error| {
            format!(
                "failed to verify Plugin Bundle {}: {error}",
                bundle.display()
            )
        })?;
        let manifest = read_bundle_manifest(&bundle).map_err(|error| {
            format!(
                "failed to read Plugin Manifest {}: {error}",
                bundle.display()
            )
        })?;
        if manifest.plugin_id() != plugin_id {
            return Err(format!(
                "Plugin Bundle ID `{}` does not match directory `{plugin_id}`",
                manifest.plugin_id()
            ));
        }
        let selected = resolve_implementation(&manifest, &implementation_policy())
            .map_err(|error| format!("failed to select Plugin implementation: {error}"))?;
        if instance.package_revision() != selected.artifact.digest {
            return Err(format!(
                "Plugin Instance `{}` does not select the verified Artifact digest",
                instance.instance_key()
            ));
        }
        let artifact_path = bundle.join(&selected.artifact.path);
        let handle = ArtifactHandle::open(
            &artifact_path,
            &selected.artifact.digest,
            selected.artifact.size,
        )
        .map_err(|error| {
            format!(
                "failed to admit Plugin Artifact {}: {error:?}",
                artifact_path.display()
            )
        })?;
        artifacts.push(PlanArtifact {
            instance_key: instance.instance_key().to_owned(),
            plugin_id: plugin_id.to_owned(),
            artifact_id: "main".to_owned(),
            media_type: selected.artifact.media_type,
            target: selected.artifact.target,
            handle,
        });
    }
    Ok(artifacts)
}

pub(crate) fn plan_resources(
    path: &Path,
    plan: &ResolvedAppPlan,
) -> Result<InstanceResourceCatalog, String> {
    let contents = snapshot_with_resources(path)?;
    plan_resources_from_snapshot(&contents, plan)
}

pub(crate) fn plan_resources_from_snapshot(
    contents: &PluginRootContents,
    plan: &ResolvedAppPlan,
) -> Result<InstanceResourceCatalog, String> {
    let mut catalog = InstanceResourceCatalog::new();
    for instance in plan.plugin_instances() {
        let Some(resources) = contents.resources.get(instance.instance_key()) else {
            continue;
        };
        if instance.execution_class().as_str() != "lenso.native-rust@1" {
            return Err(format!(
                "Plugin Instance `{}` uses resources, but execution class `{}` does not yet expose immutable Instance resources",
                instance.instance_key(),
                instance.execution_class().as_str()
            ));
        }
        catalog = catalog
            .with_resources(instance.instance_key(), resources.clone())
            .map_err(|error| format!("invalid Plugin resources: {error:?}"))?;
    }
    Ok(catalog)
}

fn scan_plugin(
    path: &Path,
    plugin_id: &str,
    releases: &mut Vec<PluginDescriptor>,
    instances: &mut Vec<PluginRootInstance>,
    disabled: &mut Vec<PluginInstanceId>,
    resources: &mut BTreeMap<String, InstanceResources>,
    budget: &mut PluginRootBudget,
) -> Result<(), String> {
    let mut normalized = BTreeMap::new();
    let mut configured_instances = BTreeSet::new();
    let mut resource_directories = BTreeMap::<String, PathBuf>::new();
    let mut entries = read_entries_bounded(path, budget.remaining_entries(), "Plugin Root")?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let entry_path = entry.path();
        let name = utf8_name(&entry_path, &entry.file_name())?;
        if is_ignored_os_metadata(&name) {
            continue;
        }
        budget.admit_entry(&entry_path)?;
        reject_case_collision(&mut normalized, &name, "Plugin filename")?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect {}: {error}", entry_path.display()))?;
        if name == BUNDLE_NAME {
            if !file_type.is_dir() {
                return Err(format!(
                    "Plugin Bundle must be a regular directory: {}",
                    entry_path.display()
                ));
            }
            admit_descendant_entries(&entry_path, budget)?;
            releases.push(read_bundle_descriptor(&entry_path, plugin_id)?);
        } else if file_type.is_dir() {
            validate_instance(&name)?;
            resource_directories.insert(name, entry_path);
        } else if !file_type.is_file() {
            return Err(format!(
                "Plugin entries cannot be symlinks or special files: {}",
                entry_path.display()
            ));
        } else if let Some(instance) = name.strip_suffix(".toml") {
            validate_instance(instance)?;
            budget.admit_instance(plugin_id, instance, &entry_path)?;
            configured_instances.insert(instance.to_owned());
            instances.push(
                PluginRootInstance::new(plugin_id, instance)
                    .with_configuration(read_configuration(&entry_path, budget)?),
            );
        } else if let Some(instance) = name.strip_suffix(".disabled") {
            validate_instance(instance)?;
            budget.admit_instance(plugin_id, instance, &entry_path)?;
            let length = fs::metadata(&entry_path)
                .map_err(|error| format!("failed to inspect {}: {error}", entry_path.display()))?
                .len();
            if length != 0 {
                return Err(format!(
                    "disabled marker must be empty: {}",
                    entry_path.display()
                ));
            }
            disabled.push(PluginInstanceId::new(plugin_id, instance));
        } else {
            return Err(format!("unknown Plugin file: {}", entry_path.display()));
        }
    }
    for (instance, resource_directory) in resource_directories {
        if !configured_instances.contains(&instance) {
            return Err(format!(
                "orphan Plugin resource directory without `{instance}.toml`: {}",
                resource_directory.display()
            ));
        }
        let instance_resources = read_resource_directory(&resource_directory, budget)?;
        resources.insert(format!("{plugin_id}/{instance}"), instance_resources);
    }
    Ok(())
}

#[derive(Debug, Default)]
struct PluginRootBudget {
    configuration_bytes: u64,
    entries: usize,
    instances: BTreeSet<String>,
    plugins: BTreeSet<String>,
    resource_bytes: u64,
    resource_files: usize,
}

impl PluginRootBudget {
    fn remaining_entries(&self) -> usize {
        MAX_PLUGIN_ROOT_ENTRIES.saturating_sub(self.entries)
    }

    fn admit_entry(&mut self, path: &Path) -> Result<(), String> {
        self.entries = self.entries.saturating_add(1);
        if self.entries > MAX_PLUGIN_ROOT_ENTRIES {
            return Err(format!(
                "Plugin Root exceeds {MAX_PLUGIN_ROOT_ENTRIES} filesystem entries: {}",
                path.display()
            ));
        }
        Ok(())
    }

    fn admit_plugin(&mut self, plugin_id: &str, path: &Path) -> Result<(), String> {
        self.plugins.insert(plugin_id.to_owned());
        if self.plugins.len() > MAX_PLUGIN_ROOT_PLUGINS {
            return Err(format!(
                "Plugin Root exceeds {MAX_PLUGIN_ROOT_PLUGINS} Plugins: {}",
                path.display()
            ));
        }
        Ok(())
    }

    fn admit_instance(
        &mut self,
        plugin_id: &str,
        instance: &str,
        path: &Path,
    ) -> Result<(), String> {
        self.instances.insert(format!("{plugin_id}/{instance}"));
        if self.instances.len() > MAX_PLUGIN_ROOT_INSTANCES {
            return Err(format!(
                "Plugin Root exceeds {MAX_PLUGIN_ROOT_INSTANCES} Instance entries: {}",
                path.display()
            ));
        }
        Ok(())
    }

    fn admit_resource(&mut self, bytes: u64, path: &Path) -> Result<(), String> {
        self.resource_files = self.resource_files.saturating_add(1);
        if self.resource_files > MAX_ROOT_RESOURCE_FILES {
            return Err(format!(
                "Plugin Root resources exceed {MAX_ROOT_RESOURCE_FILES} files: {}",
                path.display()
            ));
        }
        self.resource_bytes = self
            .resource_bytes
            .checked_add(bytes)
            .ok_or_else(|| "Plugin Root resource size overflow".to_owned())?;
        if self.resource_bytes > MAX_ROOT_RESOURCE_TOTAL_BYTES {
            return Err(format!(
                "Plugin Root resources exceed 16 MiB in aggregate: {}",
                path.display()
            ));
        }
        Ok(())
    }

    fn admit_configuration(&mut self, bytes: u64, path: &Path) -> Result<(), String> {
        self.configuration_bytes = self
            .configuration_bytes
            .checked_add(bytes)
            .ok_or_else(|| "Plugin Root configuration size overflow".to_owned())?;
        if self.configuration_bytes > MAX_CONFIGURATION_TOTAL_BYTES {
            return Err(format!(
                "Plugin Root configurations exceed 16 MiB in aggregate: {}",
                path.display()
            ));
        }
        Ok(())
    }
}

fn admit_descendant_entries(root: &Path, budget: &mut PluginRootBudget) -> Result<(), String> {
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    while let Some((directory, depth)) = pending.pop() {
        if depth > MAX_PROBE_DEPTH {
            return Err(format!(
                "Plugin Root exceeds {MAX_PROBE_DEPTH} levels: {}",
                directory.display()
            ));
        }
        let entries = read_entries_bounded(&directory, budget.remaining_entries(), "Plugin Root")?;
        for entry in entries {
            let path = entry.path();
            budget.admit_entry(&path)?;
            let file_type = entry
                .file_type()
                .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
            if file_type.is_dir() {
                pending.push((path, depth + 1));
            }
        }
    }
    Ok(())
}

fn read_resource_directory(
    path: &Path,
    root_budget: &mut PluginRootBudget,
) -> Result<InstanceResources, String> {
    RESOURCE_DIRECTORY_READS.fetch_add(1, Ordering::Relaxed);
    let mut files = Vec::new();
    let mut pending = vec![(path.to_path_buf(), PathBuf::new(), 0_usize)];
    let mut total_size = 0_u64;
    while let Some((directory, relative, depth)) = pending.pop() {
        if depth > MAX_RESOURCE_DEPTH {
            return Err(format!(
                "Plugin resource directory exceeds {MAX_RESOURCE_DEPTH} levels: {}",
                directory.display()
            ));
        }
        let mut entries =
            read_entries_bounded(&directory, root_budget.remaining_entries(), "Plugin Root")?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let entry_path = entry.path();
            let name = utf8_name(&entry_path, &entry.file_name())?;
            if is_ignored_os_metadata(&name) {
                continue;
            }
            root_budget.admit_entry(&entry_path)?;
            let resource_path = relative.join(&name);
            let file_type = entry
                .file_type()
                .map_err(|error| format!("failed to inspect {}: {error}", entry_path.display()))?;
            if file_type.is_dir() {
                pending.push((entry_path, resource_path, depth + 1));
                continue;
            }
            if !file_type.is_file() {
                return Err(format!(
                    "Plugin resources cannot contain symlinks or special files: {}",
                    entry_path.display()
                ));
            }
            if files.len() == MAX_RESOURCE_FILES {
                return Err(format!(
                    "Plugin resources exceed {MAX_RESOURCE_FILES} files: {}",
                    path.display()
                ));
            }
            let metadata = fs::symlink_metadata(&entry_path)
                .map_err(|error| format!("failed to inspect {}: {error}", entry_path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                return Err(format!(
                    "Plugin resources must be regular files: {}",
                    entry_path.display()
                ));
            }
            if metadata.len() > MAX_RESOURCE_FILE_BYTES {
                return Err(format!(
                    "Plugin resource exceeds 1 MiB: {}",
                    entry_path.display()
                ));
            }
            let bytes = fs::read(&entry_path)
                .map_err(|error| format!("failed to read {}: {error}", entry_path.display()))?;
            let byte_count = u64::try_from(bytes.len())
                .map_err(|_| format!("Plugin resource is too large: {}", entry_path.display()))?;
            if byte_count > MAX_RESOURCE_FILE_BYTES {
                return Err(format!(
                    "Plugin resource exceeds 1 MiB: {}",
                    entry_path.display()
                ));
            }
            root_budget.admit_resource(byte_count, &entry_path)?;
            total_size = total_size
                .checked_add(byte_count)
                .ok_or_else(|| format!("Plugin resource size overflow: {}", path.display()))?;
            if total_size > MAX_RESOURCE_TOTAL_BYTES {
                return Err(format!(
                    "Plugin resources exceed 16 MiB: {}",
                    path.display()
                ));
            }
            files.push((normalized_resource_path(&resource_path)?, bytes));
        }
    }
    InstanceResources::from_files(files)
        .map_err(|error| format!("invalid Plugin resources {}: {error:?}", path.display()))
}

pub(crate) fn io_telemetry() -> (u64, u64, u64) {
    (
        canonical_snapshot_count(),
        METADATA_PROBES.load(Ordering::Relaxed),
        RESOURCE_DIRECTORY_READS.load(Ordering::Relaxed),
    )
}

fn record_canonical_snapshot() {
    CANONICAL_SNAPSHOTS.fetch_add(1, Ordering::Relaxed);
    #[cfg(test)]
    TEST_CANONICAL_SNAPSHOTS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(test))]
fn canonical_snapshot_count() -> u64 {
    CANONICAL_SNAPSHOTS.load(Ordering::Relaxed)
}

#[cfg(test)]
fn canonical_snapshot_count() -> u64 {
    TEST_CANONICAL_SNAPSHOTS.with(std::cell::Cell::get)
}

fn normalized_resource_path(path: &Path) -> Result<String, String> {
    path.components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .ok_or_else(|| format!("Plugin resource path is not UTF-8: {}", path.display()))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|components| components.join("/"))
}

fn read_bundle_descriptor(path: &Path, plugin_id: &str) -> Result<PluginDescriptor, String> {
    let verified = verify_bundle_directory(path)
        .map_err(|error| format!("failed to verify Plugin Bundle {}: {error}", path.display()))?;
    if verified.plugin_id != plugin_id {
        return Err(format!(
            "Plugin Bundle ID `{}` does not match directory `{plugin_id}`",
            verified.plugin_id
        ));
    }
    let manifest = read_bundle_manifest(path)
        .map_err(|error| format!("invalid Plugin Manifest {}: {error}", path.display()))?;
    let descriptor = resolve_implementation(&manifest, &implementation_policy())
        .map_err(|error| format!("failed to select Plugin implementation: {error}"))?
        .descriptor;
    if descriptor.plugin_id() != plugin_id
        || descriptor.release_version() != verified.release_version
    {
        return Err("Plugin Descriptor identity does not match the verified Bundle".to_owned());
    }
    Ok(descriptor)
}

fn implementation_policy() -> ImplementationPolicy {
    ImplementationPolicy {
        host_target: format!(
            "{}-unknown-{}",
            std::env::consts::ARCH,
            std::env::consts::OS
        ),
        runtimes: [
            ("lenso.quickjs@1", "lenso.quickjs@1"),
            ("lenso.process@1", "lenso.process-stdio@2"),
            ("lenso.bun-process@1", "lenso.bun-authoring@2"),
            ("lenso.wasm-component@1", "lenso.wasm-component@1"),
        ]
        .into_iter()
        .map(|(execution_class, runtime_profile)| RuntimeAdmission {
            execution_class: ExecutionClassId::new(execution_class),
            runtime_profile: runtime_profile.to_owned(),
        })
        .collect(),
    }
}

fn read_configuration(
    path: &Path,
    budget: &mut PluginRootBudget,
) -> Result<serde_json::Value, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
    if metadata.len() > MAX_CONFIGURATION_BYTES {
        return Err(format!(
            "Plugin configuration exceeds 256 KiB: {}",
            path.display()
        ));
    }
    budget.admit_configuration(metadata.len(), path)?;
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let table: toml::Table = toml::from_str(&text)
        .map_err(|error| format!("invalid Plugin configuration {}: {error}", path.display()))?;
    serde_json::to_value(table)
        .map_err(|error| format!("failed to normalize {}: {error}", path.display()))
}

fn read_entries_bounded(
    path: &Path,
    maximum: usize,
    label: &str,
) -> Result<Vec<fs::DirEntry>, String> {
    let entries = fs::read_dir(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut bounded = Vec::with_capacity(maximum.min(256));
    for entry in entries {
        if bounded.len() == maximum {
            return Err(format!(
                "{label} exceeds its {maximum}-entry remaining budget: {}",
                path.display()
            ));
        }
        bounded.push(entry.map_err(|error| format!("failed to read {}: {error}", path.display()))?);
    }
    Ok(bounded)
}

fn is_ignored_os_metadata(name: &str) -> bool {
    name == ".DS_Store"
}

fn utf8_name(path: &Path, name: &std::ffi::OsStr) -> Result<String, String> {
    name.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("Plugin path is not UTF-8: {}", path.display()))
}

fn validate_instance(instance: &str) -> Result<(), String> {
    validate_identity(instance, "Instance key")?;
    if instance.starts_with('.') || instance == "plugin" {
        return Err(format!("reserved Plugin Instance key `{instance}`"));
    }
    Ok(())
}

fn validate_identity(value: &str, label: &str) -> Result<(), String> {
    if value.trim() != value
        || value.is_empty()
        || value == "."
        || value == ".."
        || value.contains(['/', '\0', '\\'])
    {
        return Err(format!("invalid {label} `{value}`"));
    }
    Ok(())
}

fn reject_case_collision(
    normalized: &mut BTreeMap<String, String>,
    value: &str,
    label: &str,
) -> Result<(), String> {
    let key = value.to_lowercase();
    if let Some(previous) = normalized.insert(key, value.to_owned())
        && previous != value
    {
        return Err(format!(
            "case-colliding {label}s `{previous}` and `{value}`"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use lenso_app_plan::{
        CapabilityEndpointPlan, ExecutionClassId, PluginInstancePlan, ResolvedAppPlan,
        authoring::PluginContract,
    };
    use lenso_plugin_bundle::{
        SourcePluginImplementation, SourcePluginReleaseBuild, build_source_plugin_release_bundle,
    };

    use super::{
        MAX_CONFIGURATION_TOTAL_BYTES, MAX_PLUGIN_ROOT_INSTANCES, MAX_PROBE_DEPTH,
        MAX_PROBE_ENTRIES, MAX_ROOT_RESOURCE_FILES, PluginRootBudget, desired_state_probe,
        plan_resources, plan_resources_from_snapshot, read_bundle_descriptor, read_entries_bounded,
        snapshot, snapshot_with_resources,
    };

    #[test]
    fn missing_root_is_the_empty_root() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("plugins");
        assert!(snapshot(&root).unwrap().instances().is_empty());
    }

    #[test]
    fn macos_metadata_at_plugin_root_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("plugins");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join(".DS_Store"), b"Finder metadata").unwrap();

        let snapshot = snapshot(&root).unwrap();

        assert!(snapshot.instances().is_empty());
    }

    #[test]
    fn macos_metadata_inside_plugin_directory_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let plugin = directory.path().join("plugins/example.plugin");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join(".DS_Store"), b"Finder metadata").unwrap();

        let snapshot = snapshot(&directory.path().join("plugins")).unwrap();

        assert!(snapshot.instances().is_empty());
    }

    #[test]
    fn reads_builtin_configuration_from_plugin_directory() {
        let directory = tempfile::tempdir().unwrap();
        let plugin = directory.path().join("plugins/lenso.agent.text-tools");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("default.toml"), "").unwrap();
        let root = snapshot(&directory.path().join("plugins")).unwrap();
        assert_eq!(root.instances().len(), 1);
        assert_eq!(
            root.instances()[0].id().plugin_id(),
            "lenso.agent.text-tools"
        );
        let plan = crate::generation::resolve_host_plan(&root).unwrap();
        let plan = serde_json::to_value(plan).unwrap();
        assert!(
            plan["plugin_instances"]
                .as_array()
                .unwrap()
                .iter()
                .any(|instance| { instance["instance_key"] == "lenso.agent.text-tools/default" })
        );
        assert!(
            plan["capability_bindings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|binding| {
                    binding["consumer_instance"] == "lenso.agent.tools/tools"
                        && binding["provider_instance"] == "lenso.agent.text-tools/default"
                })
        );
    }

    #[test]
    fn snapshots_instance_resource_directories_as_immutable_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("plugins");
        let plugin = root.join("lenso.agent.text-tools");
        let resources = plugin.join("default/prompts");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(plugin.join("default.toml"), "").unwrap();
        let prompt = resources.join("system.md");
        std::fs::write(&prompt, "generation one").unwrap();
        std::fs::write(resources.join(".DS_Store"), "Finder metadata").unwrap();

        let plugin_root = snapshot(&root).unwrap();
        let plan = crate::generation::resolve_host_plan(&plugin_root).unwrap();
        let catalog = plan_resources(&root, &plan).unwrap();
        let first = catalog
            .for_instance("lenso.agent.text-tools/default")
            .clone();

        std::fs::write(prompt, "generation two").unwrap();
        let second = plan_resources(&root, &plan).unwrap();

        assert_eq!(
            first.read_text("prompts/system.md").unwrap(),
            "generation one"
        );
        assert_ne!(
            first.digest(),
            second
                .for_instance("lenso.agent.text-tools/default")
                .digest()
        );
        assert_eq!(first.file_count(), 1);
    }

    #[test]
    fn generation_resolution_reuses_the_canonical_resource_read() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("plugins");
        let plugin = root.join("lenso.agent.text-tools");
        std::fs::create_dir_all(plugin.join("default/prompts")).unwrap();
        std::fs::write(plugin.join("default.toml"), "").unwrap();
        let prompt = plugin.join("default/prompts/system.md");
        std::fs::write(&prompt, "canonical snapshot").unwrap();

        let contents = snapshot_with_resources(&root).unwrap();
        let plan = crate::generation::resolve_host_plan(contents.root()).unwrap();
        std::fs::write(prompt, "later live bytes").unwrap();
        let resources = plan_resources_from_snapshot(&contents, &plan).unwrap();

        assert_eq!(
            resources
                .for_instance("lenso.agent.text-tools/default")
                .read_text("prompts/system.md")
                .unwrap(),
            "canonical snapshot"
        );
    }

    #[test]
    fn rejects_orphan_resource_directory() {
        let directory = tempfile::tempdir().unwrap();
        let resources = directory
            .path()
            .join("plugins/lenso.agent.text-tools/default");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(resources.join("prompt.md"), "orphan").unwrap();

        let error = snapshot(&directory.path().join("plugins")).unwrap_err();

        assert!(error.contains("orphan Plugin resource directory"));
    }

    #[test]
    fn rejects_resources_for_an_adapter_without_snapshot_support() {
        let directory = tempfile::tempdir().unwrap();
        let plugin = directory.path().join("plugins/example.quickjs");
        std::fs::create_dir_all(plugin.join("default")).unwrap();
        std::fs::write(plugin.join("default.toml"), "").unwrap();
        std::fs::write(plugin.join("default/prompt.md"), "hello").unwrap();
        let plan = ResolvedAppPlan::new(
            vec![
                PluginInstancePlan::new("example.quickjs/default", "example.quickjs")
                    .with_execution_class(ExecutionClassId::new("lenso.quickjs@1")),
            ],
            vec![],
        );

        let error = plan_resources(&directory.path().join("plugins"), &plan).unwrap_err();

        assert!(error.contains("does not yet expose immutable Instance resources"));
    }

    #[test]
    fn rejects_an_oversized_resource_file() {
        let directory = tempfile::tempdir().unwrap();
        let plugin = directory.path().join("plugins/lenso.agent.text-tools");
        std::fs::create_dir_all(plugin.join("default")).unwrap();
        std::fs::write(plugin.join("default.toml"), "").unwrap();
        std::fs::write(
            plugin.join("default/large.bin"),
            vec![0_u8; 1024 * 1024 + 1],
        )
        .unwrap();

        let error = snapshot(&directory.path().join("plugins")).unwrap_err();

        assert!(error.contains("exceeds 1 MiB"));
    }

    #[test]
    fn consistency_probe_rejects_excessive_depth_and_entries() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("plugins");
        let mut nested = root.clone();
        for depth in 0..=MAX_PROBE_DEPTH {
            nested = nested.join(format!("level-{depth}"));
        }
        std::fs::create_dir_all(&nested).unwrap();

        let depth_error = desired_state_probe(&root, None).unwrap_err();
        assert!(depth_error.contains("levels"));

        std::fs::remove_dir_all(&root).unwrap();
        std::fs::create_dir(&root).unwrap();
        for index in 0..MAX_PROBE_ENTRIES {
            std::fs::write(root.join(format!("entry-{index}")), "").unwrap();
        }

        let entries_error = desired_state_probe(&root, None).unwrap_err();
        assert!(
            entries_error.contains("remaining budget"),
            "{entries_error}"
        );
    }

    #[test]
    fn directory_reads_stop_at_the_remaining_budget_before_sorting() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..5 {
            std::fs::write(directory.path().join(format!("entry-{index}")), "").unwrap();
        }

        let error = read_entries_bounded(directory.path(), 4, "fixture").unwrap_err();

        assert!(error.contains("4-entry remaining budget"));
    }

    #[test]
    fn canonical_snapshot_rejects_multi_instance_resource_amplification() {
        let directory = tempfile::tempdir().unwrap();
        let plugin = directory.path().join("plugins/lenso.agent.text-tools");
        std::fs::create_dir_all(&plugin).unwrap();
        for index in 0..=16 {
            let instance = format!("instance-{index}");
            std::fs::write(plugin.join(format!("{instance}.toml")), "").unwrap();
            let resources = plugin.join(&instance);
            std::fs::create_dir(&resources).unwrap();
            std::fs::write(resources.join("resource.bin"), vec![0_u8; 1024 * 1024]).unwrap();
        }

        let error = snapshot(&directory.path().join("plugins")).unwrap_err();

        assert!(error.contains("16 MiB in aggregate"));
    }

    #[test]
    fn canonical_snapshot_budgets_instances_and_resource_files_globally() {
        let mut budget = PluginRootBudget::default();
        for index in 0..MAX_PLUGIN_ROOT_INSTANCES {
            budget
                .admit_instance(
                    "example.plugin",
                    &format!("instance-{index}"),
                    Path::new("root"),
                )
                .unwrap();
        }
        assert!(
            budget
                .admit_instance("example.plugin", "overflow", Path::new("root"))
                .unwrap_err()
                .contains("Instance entries")
        );

        let mut budget = PluginRootBudget::default();
        for _ in 0..MAX_ROOT_RESOURCE_FILES {
            budget.admit_resource(0, Path::new("resource")).unwrap();
        }
        assert!(
            budget
                .admit_resource(0, Path::new("overflow"))
                .unwrap_err()
                .contains("resources exceed")
        );

        let mut budget = PluginRootBudget::default();
        budget
            .admit_configuration(MAX_CONFIGURATION_TOTAL_BYTES, Path::new("configuration"))
            .unwrap();
        assert!(
            budget
                .admit_configuration(1, Path::new("overflow"))
                .unwrap_err()
                .contains("configurations exceed")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_inside_resource_directory() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let plugin = directory.path().join("plugins/lenso.agent.text-tools");
        std::fs::create_dir_all(plugin.join("default")).unwrap();
        std::fs::write(plugin.join("default.toml"), "").unwrap();
        std::fs::write(directory.path().join("secret"), "not admitted").unwrap();
        symlink(
            directory.path().join("secret"),
            plugin.join("default/secret"),
        )
        .unwrap();

        let error = snapshot(&directory.path().join("plugins")).unwrap_err();

        assert!(error.contains("cannot contain symlinks"));
    }

    #[test]
    fn selects_one_v4_implementation_before_plugin_root_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("plugin.js");
        std::fs::write(&artifact, "export function invoke() {}\n").unwrap();
        let bundle = directory.path().join("plugin.lenso-plugin");
        let contract = PluginContract::new("example.multi", "1.0.0", "tool-providers")
            .with_authoring_version(2)
            .with_capability(CapabilityEndpointPlan::new(
                "example.echo@1",
                "1.0.0",
                ["echo"],
            ));
        build_source_plugin_release_bundle(&SourcePluginReleaseBuild {
            contract,
            implementations: vec![SourcePluginImplementation {
                id: "quickjs".to_owned(),
                host_targets: vec!["*".to_owned()],
                artifact,
                bundle_path: "implementations/quickjs/plugin.js".to_owned(),
                media_type: "application/javascript".to_owned(),
                target: "javascript-es2023".to_owned(),
                entrypoint: "plugin.js".to_owned(),
                execution_class: ExecutionClassId::new("lenso.quickjs@1"),
                runtime_profile: "lenso.quickjs@1".to_owned(),
            }],
            output: bundle.clone(),
        })
        .unwrap();

        let selected = read_bundle_descriptor(&bundle, "example.multi").unwrap();
        assert_eq!(selected.execution_class().as_str(), "lenso.quickjs@1");
    }
}

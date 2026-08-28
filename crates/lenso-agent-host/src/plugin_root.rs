use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use lenso_app_plan::{
    ExecutionClassId, ResolvedAppPlan,
    authoring::{PluginDescriptor, PluginInstanceId, PluginRootInstance, PluginRootSnapshot},
};
use lenso_plugin_bundle::{
    ImplementationPolicy, read_bundle_manifest, resolve_implementation, verify_bundle_directory,
};
use lenso_plugin_control_plane::PlanArtifact;
use lenso_runtime_codec::{ArtifactHandle, InstanceResourceCatalog, InstanceResources};

const BUNDLE_NAME: &str = "plugin.lenso-plugin";
const MAX_CONFIGURATION_BYTES: u64 = 256 * 1024;
const MAX_RESOURCE_FILES: usize = 4_096;
const MAX_RESOURCE_FILE_BYTES: u64 = 1024 * 1024;
const MAX_RESOURCE_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RESOURCE_DEPTH: usize = 32;

pub(crate) fn snapshot(path: &Path) -> Result<PluginRootSnapshot, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(format!(
                "Plugin Root must be a regular directory: {}",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PluginRootSnapshot::default());
        }
        Err(error) => return Err(format!("failed to inspect {}: {error}", path.display())),
    }

    let mut releases = Vec::new();
    let mut instances = Vec::new();
    let mut disabled = Vec::new();
    let mut normalized = BTreeMap::new();
    let mut entries = read_entries(path)?;
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
        scan_plugin(
            &entry.path(),
            &plugin_id,
            &mut releases,
            &mut instances,
            &mut disabled,
        )?;
    }
    Ok(PluginRootSnapshot::new(releases, instances, disabled))
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
    let mut catalog = InstanceResourceCatalog::new();
    for instance in plan.plugin_instances() {
        let Some((plugin_id, instance_name)) = instance.instance_key().split_once('/') else {
            continue;
        };
        let resource_directory = path.join(plugin_id).join(instance_name);
        match fs::symlink_metadata(&resource_directory) {
            Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            }
            Ok(_) => {
                return Err(format!(
                    "Plugin resource path must be a regular directory: {}",
                    resource_directory.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "failed to inspect {}: {error}",
                    resource_directory.display()
                ));
            }
        }
        if instance.execution_class().as_str() != "lenso.native-rust@1" {
            return Err(format!(
                "Plugin Instance `{}` uses resources, but execution class `{}` does not yet expose immutable Instance resources",
                instance.instance_key(),
                instance.execution_class().as_str()
            ));
        }
        let resources = read_resource_directory(&resource_directory)?;
        catalog = catalog
            .with_resources(instance.instance_key(), resources)
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
) -> Result<(), String> {
    let mut normalized = BTreeMap::new();
    let mut configured_instances = BTreeSet::new();
    let mut resource_directories = BTreeMap::<String, PathBuf>::new();
    let mut entries = read_entries(path)?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let entry_path = entry.path();
        let name = utf8_name(&entry_path, &entry.file_name())?;
        if is_ignored_os_metadata(&name) {
            continue;
        }
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
            configured_instances.insert(instance.to_owned());
            instances.push(
                PluginRootInstance::new(plugin_id, instance)
                    .with_configuration(read_configuration(&entry_path)?),
            );
        } else if let Some(instance) = name.strip_suffix(".disabled") {
            validate_instance(instance)?;
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
        read_resource_directory(&resource_directory)?;
    }
    Ok(())
}

fn read_resource_directory(path: &Path) -> Result<InstanceResources, String> {
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
        let mut entries = read_entries(&directory)?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let entry_path = entry.path();
            let name = utf8_name(&entry_path, &entry.file_name())?;
            if is_ignored_os_metadata(&name) {
                continue;
            }
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
        execution_classes: [
            "lenso.quickjs@1",
            "lenso.process@1",
            "lenso.wasm-component@1",
        ]
        .into_iter()
        .map(ExecutionClassId::new)
        .collect(),
    }
}

fn read_configuration(path: &Path) -> Result<serde_json::Value, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
    if metadata.len() > MAX_CONFIGURATION_BYTES {
        return Err(format!(
            "Plugin configuration exceeds 256 KiB: {}",
            path.display()
        ));
    }
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let table: toml::Table = toml::from_str(&text)
        .map_err(|error| format!("invalid Plugin configuration {}: {error}", path.display()))?;
    serde_json::to_value(table)
        .map_err(|error| format!("failed to normalize {}: {error}", path.display()))
}

fn read_entries(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    fs::read_dir(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to read {}: {error}", path.display()))
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
    use lenso_app_plan::{
        CapabilityEndpointPlan, ExecutionClassId, PluginInstancePlan, ResolvedAppPlan,
        authoring::PluginContract,
    };
    use lenso_plugin_bundle::{
        SourcePluginImplementation, SourcePluginReleaseBuild, build_source_plugin_release_bundle,
    };

    use super::{plan_resources, read_bundle_descriptor, snapshot};

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
    fn selects_one_v3_implementation_before_plugin_root_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("plugin.js");
        std::fs::write(&artifact, "export function invoke() {}\n").unwrap();
        let bundle = directory.path().join("plugin.lenso-plugin");
        let contract =
            PluginContract::new("example.multi", "1.0.0", "tool-providers").with_capability(
                CapabilityEndpointPlan::new("example.echo@1", "1.0.0", ["echo"]),
            );
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
            }],
            output: bundle.clone(),
        })
        .unwrap();

        let selected = read_bundle_descriptor(&bundle, "example.multi").unwrap();
        assert_eq!(selected.execution_class().as_str(), "lenso.quickjs@1");
    }
}

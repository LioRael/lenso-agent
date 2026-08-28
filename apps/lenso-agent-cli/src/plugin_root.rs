use std::{collections::BTreeMap, fs, path::Path};

use lenso_app_plan::{
    ExecutionClassId, ResolvedAppPlan,
    authoring::{PluginDescriptor, PluginInstanceId, PluginRootInstance, PluginRootSnapshot},
};
use lenso_plugin_bundle::{
    ImplementationPolicy, read_bundle_manifest, resolve_implementation, verify_bundle_directory,
};
use lenso_plugin_control_plane::PlanArtifact;
use lenso_runtime_codec::ArtifactHandle;

const BUNDLE_NAME: &str = "plugin.lenso-plugin";
const MAX_CONFIGURATION_BYTES: u64 = 256 * 1024;

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
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect {}: {error}", entry.path().display()))?;
        if !file_type.is_dir() {
            return Err(format!(
                "unknown Plugin Root entry: {}",
                entry.path().display()
            ));
        }
        let plugin_id = utf8_name(&entry.path(), &entry.file_name())?;
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

fn scan_plugin(
    path: &Path,
    plugin_id: &str,
    releases: &mut Vec<PluginDescriptor>,
    instances: &mut Vec<PluginRootInstance>,
    disabled: &mut Vec<PluginInstanceId>,
) -> Result<(), String> {
    let mut normalized = BTreeMap::new();
    let mut entries = read_entries(path)?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let entry_path = entry.path();
        let name = utf8_name(&entry_path, &entry.file_name())?;
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
        } else if !file_type.is_file() {
            return Err(format!(
                "Plugin entries cannot be symlinks or special files: {}",
                entry_path.display()
            ));
        } else if let Some(instance) = name.strip_suffix(".toml") {
            validate_instance(instance)?;
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
    Ok(())
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
        CapabilityEndpointPlan, ExecutionClassId, authoring::PluginContract,
    };
    use lenso_plugin_bundle::{
        SourcePluginImplementation, SourcePluginReleaseBuild, build_source_plugin_release_bundle,
    };

    use super::{read_bundle_descriptor, snapshot};

    #[test]
    fn missing_root_is_the_empty_root() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("plugins");
        assert!(snapshot(&root).unwrap().instances().is_empty());
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
    fn selects_one_v3_implementation_before_plugin_root_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("plugin.js");
        std::fs::write(&artifact, "export function invoke() {}\n").unwrap();
        let bundle = directory.path().join("plugin.lenso-plugin");
        let contract = PluginContract::new("example.multi", "1.0.0", "tool-providers")
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
            }],
            output: bundle.clone(),
        })
        .unwrap();

        let selected = read_bundle_descriptor(&bundle, "example.multi").unwrap();
        assert_eq!(selected.execution_class().as_str(), "lenso.quickjs@1");
    }
}

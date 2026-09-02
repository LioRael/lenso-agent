use std::{fs, path::PathBuf};

use lenso_app_plan::ResolvedAppPlan;
use lenso_kernel::RuntimeFailure;
use lenso_runtime_codec::{InstanceResourceCatalog, InstanceResources};

const DIRECT_MODEL_PLUGIN: &str = "lenso.agent.model.openai-codex-direct";
const SNAPSHOT_RESOURCE_PATH: &str = "model-catalog.json";
const MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) fn inject_selected_catalog_snapshot(
    plan: &ResolvedAppPlan,
    resources: InstanceResourceCatalog,
) -> Result<InstanceResourceCatalog, String> {
    let Some(selected) = crate::provider_catalog::selected_model_instance(plan)? else {
        return Ok(resources);
    };
    let instance = plan
        .plugin_instances()
        .iter()
        .find(|instance| instance.instance_key() == selected)
        .ok_or_else(|| format!("selected Model Instance `{selected}` is absent from the Plan"))?;
    if instance.package_id() != DIRECT_MODEL_PLUGIN {
        return Ok(resources);
    }
    let configuration = serde_json::from_str::<serde_json::Value>(instance.configuration())
        .map_err(|error| format!("selected direct Model configuration is invalid: {error}"))?;
    let Some(path) = configuration
        .get("catalog_snapshot_path")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
    else {
        return Ok(resources);
    };
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(resources),
        Err(error) => {
            return Err(format!(
                "failed to inspect selected Model catalog snapshot {}: {error}",
                path.display()
            ));
        }
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_SNAPSHOT_BYTES {
        return Err(format!(
            "selected Model catalog snapshot must be a bounded regular file: {}",
            path.display()
        ));
    }
    let snapshot = fs::read(&path).map_err(|error| {
        format!(
            "failed to read selected Model catalog snapshot {}: {error}",
            path.display()
        )
    })?;
    if u64::try_from(snapshot.len()).unwrap_or(u64::MAX) > MAX_SNAPSHOT_BYTES {
        return Err(format!(
            "selected Model catalog snapshot exceeds 8 MiB: {}",
            path.display()
        ));
    }

    let mut rebuilt = InstanceResourceCatalog::new();
    for plan_instance in plan.plugin_instances() {
        let current = resources.for_instance(plan_instance.instance_key());
        let mut files = current
            .paths()
            .map(|resource_path| {
                current
                    .read(resource_path)
                    .map(|bytes| (resource_path.to_owned(), bytes.to_vec()))
            })
            .collect::<Result<Vec<_>, RuntimeFailure>>()
            .map_err(|error| format!("failed to copy immutable Plugin resources: {error:?}"))?;
        if plan_instance.instance_key() == selected {
            if files
                .iter()
                .any(|(resource_path, _)| resource_path == SNAPSHOT_RESOURCE_PATH)
            {
                return Err(format!(
                    "selected Model Instance `{selected}` already owns reserved resource `{SNAPSHOT_RESOURCE_PATH}`"
                ));
            }
            files.push((SNAPSHOT_RESOURCE_PATH.to_owned(), snapshot.clone()));
        }
        if !files.is_empty() {
            let snapshot = InstanceResources::from_files(files)
                .map_err(|error| format!("invalid selected Model resources: {error:?}"))?;
            rebuilt = rebuilt
                .with_resources(plan_instance.instance_key(), snapshot)
                .map_err(|error| format!("invalid selected Model resource catalog: {error:?}"))?;
        }
    }
    Ok(rebuilt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directories::AgentDirectories;
    use lenso_app_plan::authoring::PluginRootSnapshot;

    #[test]
    fn equivalent_catalog_bytes_keep_identity_and_changed_bytes_replace_it() {
        let directory = tempfile::tempdir().unwrap();
        let directories = AgentDirectories::from_home(directory.path()).unwrap();
        let plan =
            crate::generation::resolve_host_plan_in(&directories, &PluginRootSnapshot::default())
                .unwrap();
        let selected = crate::provider_catalog::selected_model_instance(&plan)
            .unwrap()
            .unwrap();
        let path = directories.model_catalog_snapshot();
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        fs::write(&path, br#"{"schema":"one"}"#).unwrap();
        let first =
            inject_selected_catalog_snapshot(&plan, InstanceResourceCatalog::new()).unwrap();
        let equivalent =
            inject_selected_catalog_snapshot(&plan, InstanceResourceCatalog::new()).unwrap();
        assert_eq!(
            first.for_instance(&selected).digest(),
            equivalent.for_instance(&selected).digest()
        );
        fs::write(&path, br#"{"schema":"two"}"#).unwrap();
        let second =
            inject_selected_catalog_snapshot(&plan, InstanceResourceCatalog::new()).unwrap();

        assert_ne!(
            first.for_instance(&selected).digest(),
            second.for_instance(&selected).digest()
        );
        assert_eq!(
            second
                .for_instance(&selected)
                .read(SNAPSHOT_RESOURCE_PATH)
                .unwrap(),
            br#"{"schema":"two"}"#
        );
    }
}

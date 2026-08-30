//! Repository-scoped hierarchical workspace instructions.

use std::{
    cell::RefCell,
    fs,
    path::{Component, Path, PathBuf},
    rc::Rc,
};

use futures::future::ready;
use lenso::prelude::*;
use lenso_capability_agent_prompt_provider::{
    self as prompt_contract, ContributeRequest, ContributeResponse,
    ContributeResponseContributionsItem, ContributeResponseContributionsItemKind,
    PromptProviderProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceInstructionsConfig {
    working_directory: PathBuf,
    file_name: String,
    max_ancestor_depth: usize,
    max_file_bytes: usize,
    max_total_bytes: usize,
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct WorkspaceInstructionsPlugin {
    #[config]
    config: WorkspaceInstructionsConfig,
    contributions: Rc<RefCell<Option<Vec<ContributeResponseContributionsItem>>>>,
}

impl Lifecycle for WorkspaceInstructionsPlugin {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        self.contributions
            .replace(Some(load_instructions(&self.config)?));
        Ok(())
    }
}

#[lenso::provides(prompt_contract::PromptProvider)]
impl PromptProviderProvider for WorkspaceInstructionsPlugin {
    fn contribute(
        &self,
        _context: InvocationContext,
        _request: ContributeRequest,
    ) -> lenso_kernel::NativeRequestFuture<prompt_contract::PromptProvider> {
        let result = self
            .contributions
            .borrow()
            .clone()
            .ok_or(RuntimeFailure::Unavailable {
                capability: prompt_contract::CAPABILITY_ID,
            });
        Box::pin(ready(
            result.map(|contributions| Ok(ContributeResponse { contributions })),
        ))
    }
}

fn validate_config(config: &WorkspaceInstructionsConfig) -> Result<(), RuntimeFailure> {
    let file = Path::new(&config.file_name);
    if config.working_directory.as_os_str().is_empty()
        || !matches!(
            file.components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        )
        || config.file_name.starts_with('.')
        || !(1..=64).contains(&config.max_ancestor_depth)
        || !(1..=262_144).contains(&config.max_file_bytes)
        || !(1..=1_048_576).contains(&config.max_total_bytes)
        || config.max_file_bytes > config.max_total_bytes
    {
        return Err(invalid_plan(
            "workspace instruction configuration limits are invalid",
        ));
    }
    Ok(())
}

fn load_instructions(
    config: &WorkspaceInstructionsConfig,
) -> Result<Vec<ContributeResponseContributionsItem>, RuntimeFailure> {
    validate_config(config)?;
    let working_directory = fs::canonicalize(&config.working_directory).map_err(|error| {
        plugin_failure(format!(
            "workspace working directory `{}` is unavailable: {error}",
            config.working_directory.display()
        ))
    })?;
    if !working_directory.is_dir() {
        return Err(plugin_failure(
            "workspace working directory is not a directory",
        ));
    }

    let directories = instruction_directories(&working_directory, config.max_ancestor_depth);
    let mut total_bytes = 0_usize;
    let mut contributions = Vec::new();
    for (index, directory) in directories.into_iter().enumerate() {
        let path = directory.join(&config.file_name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(plugin_failure(format!(
                    "failed to inspect workspace instruction `{}`: {error}",
                    path.display()
                )));
            }
        };
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(plugin_failure(format!(
                "workspace instruction must be a regular non-symlink file: {}",
                path.display()
            )));
        }
        let file_bytes = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
        if file_bytes == 0 || file_bytes > config.max_file_bytes {
            return Err(plugin_failure(format!(
                "workspace instruction exceeds its file limit: {}",
                path.display()
            )));
        }
        let content = fs::read_to_string(&path).map_err(|error| {
            plugin_failure(format!(
                "workspace instruction is not readable UTF-8 `{}`: {error}",
                path.display()
            ))
        })?;
        total_bytes = total_bytes.saturating_add(content.len());
        if total_bytes > config.max_total_bytes {
            return Err(plugin_failure(
                "workspace instructions exceed their aggregate limit",
            ));
        }
        contributions.push(ContributeResponseContributionsItem {
            id: format!("workspace.instructions.{index}"),
            version: format!("{:x}", Sha256::digest(content.as_bytes())),
            kind: ContributeResponseContributionsItemKind::Instruction,
            content,
        });
    }
    Ok(contributions)
}

fn instruction_directories(working_directory: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut path = working_directory.to_path_buf();
    let mut leaf_to_root = Vec::new();
    let mut found_repository = false;
    for _ in 0..max_depth {
        leaf_to_root.push(path.clone());
        if path.join(".git").exists() {
            found_repository = true;
            break;
        }
        let Some(parent) = path.parent() else {
            break;
        };
        path = parent.to_path_buf();
    }
    if !found_repository {
        return vec![working_directory.to_path_buf()];
    }
    leaf_to_root.reverse();
    leaf_to_root
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

fn plugin_failure(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(path: &Path) -> WorkspaceInstructionsConfig {
        WorkspaceInstructionsConfig {
            working_directory: path.to_path_buf(),
            file_name: "AGENTS.md".to_owned(),
            max_ancestor_depth: 16,
            max_file_bytes: 1024,
            max_total_bytes: 4096,
        }
    }

    #[test]
    fn loads_repository_instructions_from_root_to_leaf() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join(".git")).unwrap();
        fs::write(directory.path().join("AGENTS.md"), "root").unwrap();
        let nested = directory.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(directory.path().join("a/AGENTS.md"), "child").unwrap();

        let contributions = load_instructions(&config(&nested)).unwrap();

        assert_eq!(
            contributions
                .iter()
                .map(|item| item.content.as_str())
                .collect::<Vec<_>>(),
            ["root", "child"]
        );
        assert!(contributions.iter().all(|item| {
            item.version.len() == 64
                && item
                    .version
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }));
    }

    #[test]
    fn a_non_repository_reads_only_the_working_directory() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("AGENTS.md"), "local").unwrap();
        assert_eq!(
            load_instructions(&config(directory.path())).unwrap().len(),
            1
        );
    }

    #[test]
    fn descriptor_exposes_only_a_prompt_provider() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(
            descriptor["plugin_id"],
            "lenso.agent.workspace-instructions"
        );
        assert_eq!(
            descriptor["provided_capabilities"][0]["capability_id"],
            "lenso.agent.prompt-provider@1"
        );
        assert_eq!(descriptor["required_capabilities"], serde_json::json!([]));
    }
}

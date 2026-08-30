//! Bounded workspace file suggestion Plugin for the Agent TUI.

use futures::future::ready;
use lenso_capability_tui_suggestion::{
    self as suggestion_contract, SnapshotRequest, SnapshotResponse, SuggestionItem as Suggestion,
    SuggestionKind, SuggestionProvider, validate_snapshot_suggestions,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};
use std::{fs, path::PathBuf};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceSuggestionsConfig {
    root: PathBuf,
    max_files: usize,
    max_entries: usize,
    exclude_directories: Vec<String>,
}

fn validate_config(config: &WorkspaceSuggestionsConfig) -> Result<(), RuntimeFailure> {
    if config.max_files == 0 || config.max_files > 2_048 {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "max_files must be between 1 and 2048".to_owned(),
        });
    }
    if config.max_entries == 0 || config.max_entries > 100_000 {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "max_entries must be between 1 and 100000".to_owned(),
        });
    }
    if config.exclude_directories.len() > 64
        || config.exclude_directories.iter().any(|name| {
            name.is_empty()
                || name == "."
                || name == ".."
                || name.contains('/')
                || name.contains('\\')
        })
    {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "exclude_directories must contain at most 64 directory names".to_owned(),
        });
    }
    Ok(())
}

#[lenso::plugin(
    configuration_schema = "config.schema.json",
    configuration_defaults = "config.defaults.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct WorkspaceSuggestions {
    #[config]
    config: WorkspaceSuggestionsConfig,
}

impl WorkspaceSuggestions {
    fn snapshot_files(&self) -> Result<Vec<Suggestion>, RuntimeFailure> {
        let root =
            fs::canonicalize(&self.config.root).map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("workspace suggestion root is unavailable: {error}"),
            })?;
        if !root.is_dir() {
            return Err(RuntimeFailure::PluginFailure {
                detail: "workspace suggestion root is not a directory".to_owned(),
            });
        }
        let mut pending = vec![root.clone()];
        let mut paths = Vec::new();
        let mut entries_seen = 0usize;
        while let Some(directory) = pending.pop() {
            let mut entries = fs::read_dir(&directory)
                .map_err(|error| RuntimeFailure::PluginFailure {
                    detail: format!("failed to read workspace suggestions: {error}"),
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| RuntimeFailure::PluginFailure {
                    detail: format!("failed to read workspace suggestions: {error}"),
                })?;
            entries.sort_by_key(fs::DirEntry::file_name);
            for entry in entries.into_iter().rev() {
                if entries_seen == self.config.max_entries {
                    pending.clear();
                    break;
                }
                entries_seen += 1;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') {
                    continue;
                }
                let file_type =
                    entry
                        .file_type()
                        .map_err(|error| RuntimeFailure::PluginFailure {
                            detail: format!("failed to inspect workspace suggestion: {error}"),
                        })?;
                if file_type.is_symlink() {
                    continue;
                }
                if file_type.is_dir() {
                    if self
                        .config
                        .exclude_directories
                        .iter()
                        .any(|excluded| excluded.as_str() == name)
                    {
                        continue;
                    }
                    pending.push(entry.path());
                } else if file_type.is_file() {
                    let path = entry
                        .path()
                        .strip_prefix(&root)
                        .map_err(|_| RuntimeFailure::PluginFailure {
                            detail: "workspace suggestion escaped its configured root".to_owned(),
                        })?
                        .to_string_lossy()
                        .replace('\\', "/");
                    paths.push(path);
                    if paths.len() == self.config.max_files {
                        pending.clear();
                        break;
                    }
                }
            }
        }
        paths.sort();
        let suggestions = paths
            .into_iter()
            .enumerate()
            .map(|(index, path)| Suggestion {
                id: format!("workspace.file.{index}"),
                kind: SuggestionKind::File,
                label: path.clone(),
                insert_text: format!("@{path}"),
                description: "Workspace file".to_owned(),
            })
            .collect::<Vec<_>>();
        validate_snapshot_suggestions(&suggestions)
            .map_err(|detail| RuntimeFailure::PluginFailure { detail })?;
        Ok(suggestions)
    }
}

#[lenso::provides(suggestion_contract::Suggestion)]
impl SuggestionProvider for WorkspaceSuggestions {
    fn snapshot(
        &self,
        _context: InvocationContext,
        _request: SnapshotRequest,
    ) -> lenso_kernel::NativeRequestFuture<suggestion_contract::Suggestion> {
        Box::pin(ready(
            self.snapshot_files()
                .map(|suggestions| Ok(SnapshotResponse { suggestions })),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_files_stably_and_skips_excluded_or_hidden_directories() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::create_dir_all(temp.path().join("target")).unwrap();
        fs::create_dir_all(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join("src/lib.rs"), "pub fn run() {}").unwrap();
        fs::write(temp.path().join("README.md"), "read me").unwrap();
        fs::write(temp.path().join("target/output"), "ignored").unwrap();
        fs::write(temp.path().join(".git/config"), "ignored").unwrap();
        let plugin = WorkspaceSuggestions {
            config: WorkspaceSuggestionsConfig {
                root: temp.path().to_owned(),
                max_files: 16,
                max_entries: 64,
                exclude_directories: vec!["target".to_owned()],
            },
        };

        let suggestions = plugin.snapshot_files().unwrap();
        assert_eq!(
            suggestions
                .iter()
                .map(|suggestion| suggestion.label.as_str())
                .collect::<Vec<_>>(),
            vec!["README.md", "src/lib.rs"]
        );
    }

    #[test]
    fn entry_budget_bounds_the_snapshot_walk() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            fs::write(temp.path().join(name), name).unwrap();
        }
        let plugin = WorkspaceSuggestions {
            config: WorkspaceSuggestionsConfig {
                root: temp.path().to_owned(),
                max_files: 16,
                max_entries: 2,
                exclude_directories: Vec::new(),
            },
        };
        assert_eq!(plugin.snapshot_files().unwrap().len(), 2);
    }
}

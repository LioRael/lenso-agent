//! Read-only, workspace-rooted Tool Provider Module.

use futures::future::{LocalBoxFuture, ready};
use lenso::prelude::*;
use lenso_capability_agent_tool_provider::{
    self as tool_provider_contract, CatalogError, CatalogRequest, CatalogResponse, ContentType,
    ExecuteError, ExecuteRequest, ExecuteResponse, ToolDefinition, ToolProviderProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// Stable Tool name for listing one workspace directory.
pub const LIST_TOOL: &str = "list";
/// Stable Tool name for bounded literal search.
pub const SEARCH_TOOL: &str = "search";
/// Stable Tool name for reading one UTF-8 file.
pub const READ_TOOL: &str = "read";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceConfig {
    root: PathBuf,
    max_output_bytes: usize,
    max_list_entries: usize,
    max_search_entries: usize,
    max_search_bytes: usize,
    max_search_matches: usize,
}

fn validate_config(config: &WorkspaceConfig) -> Result<(), RuntimeFailure> {
    for (valid, message) in [
        (
            config.max_output_bytes > 0 && config.max_output_bytes <= 1_048_576,
            "max_output_bytes must be between 1 and 1048576",
        ),
        (
            config.max_list_entries > 0 && config.max_list_entries <= 10_000,
            "max_list_entries must be between 1 and 10000",
        ),
        (
            config.max_search_entries > 0 && config.max_search_entries <= 100_000,
            "max_search_entries must be between 1 and 100000",
        ),
        (
            config.max_search_bytes > 0 && config.max_search_bytes <= 1_073_741_824,
            "max_search_bytes must be between 1 and 1073741824",
        ),
        (
            config.max_search_matches > 0 && config.max_search_matches <= 10_000,
            "max_search_matches must be between 1 and 10000",
        ),
    ] {
        if !valid {
            return Err(invalid_plan(format!("workspace-read {message}")));
        }
    }
    Ok(())
}

#[lenso::module(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct WorkspaceProvider {
    #[config]
    config: WorkspaceConfig,
}

#[derive(Debug)]
enum WorkspaceReadFailure {
    Domain(ExecuteError),
    Runtime(RuntimeFailure),
}
impl From<ExecuteError> for WorkspaceReadFailure {
    fn from(error: ExecuteError) -> Self {
        Self::Domain(error)
    }
}

#[derive(serde::Serialize)]
struct ListEntry {
    path: String,
    kind: &'static str,
    size_bytes: Option<u64>,
}

#[derive(serde::Serialize)]
struct SearchMatch {
    path: String,
    line: usize,
    excerpt: String,
}

impl WorkspaceProvider {
    fn canonical_root(&self) -> Result<PathBuf, RuntimeFailure> {
        let root =
            fs::canonicalize(&self.config.root).map_err(|error| RuntimeFailure::ModuleFailure {
                detail: format!("workspace root is unavailable: {error}"),
            })?;
        if !root.is_dir() {
            return Err(RuntimeFailure::ModuleFailure {
                detail: "workspace root is not a directory".to_owned(),
            });
        }
        Ok(root)
    }

    fn resolve(root: &Path, path: &str) -> Result<PathBuf, ExecuteError> {
        if path.is_empty() {
            return Err(ExecuteError::InvalidArguments);
        }
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(ExecuteError::PermissionDenied);
        }
        let mut resolved = root.to_path_buf();
        for part in relative.components() {
            if let Component::Normal(part) = part {
                resolved.push(part);
                if fs::symlink_metadata(&resolved)
                    .map_err(|error| map_path_error(&error))?
                    .file_type()
                    .is_symlink()
                {
                    return Err(ExecuteError::PermissionDenied);
                }
            }
        }
        Ok(resolved)
    }

    fn list(&self, arguments_json: &str) -> Result<ExecuteResponse, WorkspaceReadFailure> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Arguments {
            #[serde(default = "default_workspace_path")]
            path: String,
        }
        let arguments = serde_json::from_str::<Arguments>(arguments_json)
            .map_err(|_| ExecuteError::InvalidArguments)?;
        let root = self
            .canonical_root()
            .map_err(WorkspaceReadFailure::Runtime)?;
        let directory = Self::resolve(&root, &arguments.path)?;
        if !directory.is_dir() {
            return Err(ExecuteError::PermissionDenied.into());
        }
        let mut entries = Vec::new();
        for entry in fs::read_dir(directory).map_err(|_| ExecuteError::PermissionDenied)? {
            let entry = entry.map_err(|_| ExecuteError::PermissionDenied)?;
            let file_type = entry
                .file_type()
                .map_err(|_| ExecuteError::PermissionDenied)?;
            if file_type.is_symlink() {
                return Err(ExecuteError::PermissionDenied.into());
            }
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if entries.len() == self.config.max_list_entries {
                return Err(ExecuteError::OutputLimitExceeded.into());
            }
            let path = entry
                .path()
                .strip_prefix(&root)
                .map_err(|_| ExecuteError::PermissionDenied)?
                .to_string_lossy()
                .into_owned();
            let (kind, size_bytes) = if file_type.is_dir() {
                ("directory", None)
            } else if file_type.is_file() {
                (
                    "file",
                    Some(
                        entry
                            .metadata()
                            .map_err(|_| ExecuteError::PermissionDenied)?
                            .len(),
                    ),
                )
            } else {
                return Err(ExecuteError::PermissionDenied.into());
            };
            entries.push(ListEntry {
                path,
                kind,
                size_bytes,
            });
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        self.json_response(
            &entries,
            &serde_json::json!({"path": arguments.path, "entries": entries.len()}),
        )
    }

    fn search(&self, arguments_json: &str) -> Result<ExecuteResponse, WorkspaceReadFailure> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Arguments {
            query: String,
            #[serde(default = "default_workspace_path")]
            path: String,
        }
        let arguments = serde_json::from_str::<Arguments>(arguments_json)
            .map_err(|_| ExecuteError::InvalidArguments)?;
        if arguments.query.is_empty() {
            return Err(ExecuteError::InvalidArguments.into());
        }
        let root = self
            .canonical_root()
            .map_err(WorkspaceReadFailure::Runtime)?;
        let requested = Self::resolve(&root, &arguments.path)?;
        let mut files = Vec::new();
        let mut visited_entries = 0;
        self.collect_files(&root, &requested, &mut visited_entries, &mut files)?;
        files.sort();
        let mut scanned_bytes = 0usize;
        let mut matches = Vec::new();
        for file in files {
            let file_bytes = usize::try_from(
                fs::metadata(&file)
                    .map_err(|_| ExecuteError::PermissionDenied)?
                    .len(),
            )
            .map_err(|_| ExecuteError::OutputLimitExceeded)?;
            scanned_bytes = scanned_bytes
                .checked_add(file_bytes)
                .ok_or(ExecuteError::OutputLimitExceeded)?;
            if scanned_bytes > self.config.max_search_bytes {
                return Err(ExecuteError::OutputLimitExceeded.into());
            }
            let bytes = fs::read(&file).map_err(|_| ExecuteError::PermissionDenied)?;
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let path = file
                .strip_prefix(&root)
                .map_err(|_| ExecuteError::PermissionDenied)?
                .to_string_lossy()
                .into_owned();
            for (index, line) in text.lines().enumerate() {
                if line.contains(&arguments.query) {
                    if matches.len() == self.config.max_search_matches {
                        return Err(ExecuteError::OutputLimitExceeded.into());
                    }
                    matches.push(SearchMatch {
                        path: path.clone(),
                        line: index + 1,
                        excerpt: line.trim().to_owned(),
                    });
                }
            }
        }
        self.json_response(&matches, &serde_json::json!({"path": arguments.path, "query": arguments.query, "matches": matches.len(), "scanned_bytes": scanned_bytes, "visited_entries": visited_entries}))
    }

    fn collect_files(
        &self,
        root: &Path,
        requested: &Path,
        visited: &mut usize,
        files: &mut Vec<PathBuf>,
    ) -> Result<(), ExecuteError> {
        let metadata = fs::symlink_metadata(requested).map_err(|error| map_path_error(&error))?;
        if metadata.file_type().is_symlink() {
            return Err(ExecuteError::PermissionDenied);
        }
        if metadata.is_file() {
            files.push(requested.to_path_buf());
            return Ok(());
        }
        if !metadata.is_dir() {
            return Err(ExecuteError::PermissionDenied);
        }
        let mut children = fs::read_dir(requested)
            .map_err(|_| ExecuteError::PermissionDenied)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ExecuteError::PermissionDenied)?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            *visited += 1;
            if *visited > self.config.max_search_entries {
                return Err(ExecuteError::OutputLimitExceeded);
            }
            let file_type = child
                .file_type()
                .map_err(|_| ExecuteError::PermissionDenied)?;
            if file_type.is_symlink() {
                return Err(ExecuteError::PermissionDenied);
            }
            if child.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = child.path();
            if !path.starts_with(root) {
                return Err(ExecuteError::PermissionDenied);
            }
            if file_type.is_dir() {
                self.collect_files(root, &path, visited, files)?;
            } else if file_type.is_file() {
                files.push(path);
            } else {
                return Err(ExecuteError::PermissionDenied);
            }
        }
        Ok(())
    }

    fn read_text(&self, arguments_json: &str) -> Result<ExecuteResponse, WorkspaceReadFailure> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Arguments {
            path: String,
        }
        let arguments = serde_json::from_str::<Arguments>(arguments_json)
            .map_err(|_| ExecuteError::InvalidArguments)?;
        let root = self
            .canonical_root()
            .map_err(WorkspaceReadFailure::Runtime)?;
        let resolved = Self::resolve(&root, &arguments.path)?;
        if !resolved.is_file() {
            return Err(ExecuteError::PermissionDenied.into());
        }
        if fs::metadata(&resolved)
            .map_err(|_| ExecuteError::NotFound)?
            .len()
            > self.config.max_output_bytes as u64
        {
            return Err(ExecuteError::OutputLimitExceeded.into());
        }
        let content = fs::read_to_string(&resolved)
            .map_err(|_| execution_failed("not_utf8", "workspace file is not valid UTF-8"))?;
        Ok(ExecuteResponse {
            content,
            content_type: ContentType::Text,
            metadata_json: serde_json::json!({"path": arguments.path})
                .to_string()
                .try_into()
                .expect("serde_json values must produce valid JSON"),
        })
    }

    fn json_response<T: serde::Serialize>(
        &self,
        value: &T,
        metadata: &serde_json::Value,
    ) -> Result<ExecuteResponse, WorkspaceReadFailure> {
        let content = serde_json::to_string(value).map_err(|_| {
            execution_failed("json_encode", "workspace result could not be encoded")
        })?;
        if content.len() > self.config.max_output_bytes {
            return Err(ExecuteError::OutputLimitExceeded.into());
        }
        Ok(ExecuteResponse {
            content,
            content_type: ContentType::Text,
            metadata_json: metadata
                .to_string()
                .try_into()
                .expect("serde_json values must produce valid JSON"),
        })
    }
}

#[lenso::provides(tool_provider_contract::ToolProvider)]
impl ToolProviderProvider for WorkspaceProvider {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> LocalBoxFuture<'static, Result<Result<CatalogResponse, CatalogError>, RuntimeFailure>>
    {
        Box::pin(ready(Ok(Ok(CatalogResponse {
            tools: vec![
                ToolDefinition {
                    name: LIST_TOOL.to_owned(),
                    description: "List one directory below the selected workspace root. Hidden entries are omitted.".to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{"path":{"default":".","minLength":1,"type":"string"}},"type":"object"}"#.to_owned().try_into().expect("static Tool schema must be valid JSON"),
                },
                ToolDefinition {
                    name: SEARCH_TOOL.to_owned(),
                    description: "Search UTF-8 workspace files recursively for a case-sensitive literal string. Hidden entries are omitted.".to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{"path":{"default":".","minLength":1,"type":"string"},"query":{"minLength":1,"type":"string"}},"required":["query"],"type":"object"}"#.to_owned().try_into().expect("static Tool schema must be valid JSON"),
                },
                ToolDefinition {
                    name: READ_TOOL.to_owned(),
                    description: "Read one UTF-8 text file below the selected workspace root.".to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{"path":{"minLength":1,"type":"string"}},"required":["path"],"type":"object"}"#.to_owned().try_into().expect("static Tool schema must be valid JSON"),
                },
            ],
        }))))
    }
    fn execute(
        &self,
        _context: InvocationContext,
        request: ExecuteRequest,
    ) -> LocalBoxFuture<'static, Result<Result<ExecuteResponse, ExecuteError>, RuntimeFailure>>
    {
        let result = match request.name.as_str() {
            LIST_TOOL => self.list(request.arguments_json.as_str()),
            SEARCH_TOOL => self.search(request.arguments_json.as_str()),
            READ_TOOL => self.read_text(request.arguments_json.as_str()),
            _ => Err(ExecuteError::NotFound.into()),
        };
        Box::pin(ready(match result {
            Ok(response) => Ok(Ok(response)),
            Err(WorkspaceReadFailure::Domain(error)) => Ok(Err(error)),
            Err(WorkspaceReadFailure::Runtime(error)) => Err(error),
        }))
    }
}

impl Lifecycle for WorkspaceProvider {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        self.canonical_root().map(|_| ())
    }
}

fn default_workspace_path() -> String {
    ".".to_owned()
}

fn map_path_error(error: &std::io::Error) -> ExecuteError {
    if error.kind() == std::io::ErrorKind::NotFound {
        ExecuteError::NotFound
    } else {
        ExecuteError::PermissionDenied
    }
}

fn execution_failed(reason_code: &str, message: &str) -> WorkspaceReadFailure {
    WorkspaceReadFailure::Domain(ExecuteError::ExecutionFailed {
        payload: lenso_capability_agent_tool_provider::ExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: message.to_owned(),
            details_json: "{}"
                .to_owned()
                .try_into()
                .expect("static details must be valid JSON"),
        },
    })
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(root: PathBuf) -> WorkspaceProvider {
        WorkspaceProvider {
            config: WorkspaceConfig {
                root,
                max_output_bytes: 4096,
                max_list_entries: 16,
                max_search_entries: 64,
                max_search_bytes: 4096,
                max_search_matches: 16,
            },
        }
    }

    #[test]
    fn lists_stably_and_omits_hidden_entries() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("z.txt"), "z").unwrap();
        fs::create_dir(temp.path().join("a-dir")).unwrap();
        fs::write(temp.path().join(".hidden"), "secret").unwrap();
        let provider = provider(temp.path().into());
        let result: serde_json::Value =
            serde_json::from_str(&provider.list("{}").unwrap().content).unwrap();
        assert_eq!(result[0]["path"], "a-dir");
        assert_eq!(result[0]["kind"], "directory");
        assert_eq!(result[1]["path"], "z.txt");
        assert_eq!(result.as_array().unwrap().len(), 2);
        assert_eq!(
            provider.read_text(r#"{"path":".hidden"}"#).unwrap().content,
            "secret"
        );
    }

    #[test]
    fn searches_recursively_and_skips_hidden_and_binary_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/b.rs"), "none\nneedle b\n").unwrap();
        fs::write(temp.path().join("a.txt"), "needle a\n").unwrap();
        fs::write(temp.path().join(".hidden"), "needle hidden\n").unwrap();
        fs::write(temp.path().join("binary"), [0xff, 0xfe]).unwrap();
        let result: serde_json::Value = serde_json::from_str(
            &provider(temp.path().into())
                .search(r#"{"query":"needle"}"#)
                .unwrap()
                .content,
        )
        .unwrap();
        assert_eq!(result[0]["path"], "a.txt");
        assert_eq!(result[0]["line"], 1);
        assert_eq!(result[1]["path"], "src/b.rs");
        assert_eq!(result[1]["line"], 2);
        assert_eq!(result.as_array().unwrap().len(), 2);
    }

    #[test]
    fn reads_utf8_and_rejects_escape() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("README.md"), "# Fixture\n").unwrap();
        fs::write(temp.path().join("secret"), "secret").unwrap();
        let provider = provider(root);
        assert_eq!(
            provider
                .read_text(r#"{"path":"README.md"}"#)
                .unwrap()
                .content,
            "# Fixture\n"
        );
        assert!(matches!(
            provider.read_text(r#"{"path":"../secret"}"#),
            Err(WorkspaceReadFailure::Domain(ExecuteError::PermissionDenied))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("target"), "needle").unwrap();
        symlink("target", temp.path().join("link")).unwrap();
        let provider = provider(temp.path().into());
        assert!(matches!(
            provider.list("{}"),
            Err(WorkspaceReadFailure::Domain(ExecuteError::PermissionDenied))
        ));
        assert!(matches!(
            provider.search(r#"{"query":"needle"}"#),
            Err(WorkspaceReadFailure::Domain(ExecuteError::PermissionDenied))
        ));
        assert!(matches!(
            provider.read_text(r#"{"path":"link"}"#),
            Err(WorkspaceReadFailure::Domain(ExecuteError::PermissionDenied))
        ));
    }

    #[test]
    fn enforces_budgets_and_argument_shape() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("a"), "needle one\nneedle two").unwrap();
        fs::write(temp.path().join("b"), "needle three").unwrap();
        let mut provider = provider(temp.path().into());
        provider.config.max_list_entries = 1;
        assert!(matches!(
            provider.list("{}"),
            Err(WorkspaceReadFailure::Domain(
                ExecuteError::OutputLimitExceeded
            ))
        ));
        provider.config.max_list_entries = 16;
        provider.config.max_search_matches = 1;
        assert!(matches!(
            provider.search(r#"{"query":"needle"}"#),
            Err(WorkspaceReadFailure::Domain(
                ExecuteError::OutputLimitExceeded
            ))
        ));
        provider.config.max_search_matches = 16;
        provider.config.max_search_entries = 1;
        assert!(matches!(
            provider.search(r#"{"query":"absent"}"#),
            Err(WorkspaceReadFailure::Domain(
                ExecuteError::OutputLimitExceeded
            ))
        ));
        provider.config.max_search_entries = 64;
        provider.config.max_search_bytes = 1;
        assert!(matches!(
            provider.search(r#"{"query":"absent"}"#),
            Err(WorkspaceReadFailure::Domain(
                ExecuteError::OutputLimitExceeded
            ))
        ));
        provider.config.max_search_bytes = 4096;
        provider.config.max_output_bytes = 4;
        assert!(matches!(
            provider.read_text(r#"{"path":"a"}"#),
            Err(WorkspaceReadFailure::Domain(
                ExecuteError::OutputLimitExceeded
            ))
        ));
        assert!(matches!(
            provider.search(r#"{"query":""}"#),
            Err(WorkspaceReadFailure::Domain(ExecuteError::InvalidArguments))
        ));
        assert!(matches!(
            provider.search(r#"{"path":"..","query":"needle"}"#),
            Err(WorkspaceReadFailure::Domain(ExecuteError::PermissionDenied))
        ));
    }

    #[test]
    fn workspace_loss_is_runtime_failure() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let provider = provider(root.clone());
        fs::remove_dir(root).unwrap();
        assert!(matches!(
            provider.list("{}"),
            Err(WorkspaceReadFailure::Runtime(_))
        ));
        assert!(matches!(
            provider.search(r#"{"query":"x"}"#),
            Err(WorkspaceReadFailure::Runtime(_))
        ));
        assert!(matches!(
            provider.read_text(r#"{"path":"x"}"#),
            Err(WorkspaceReadFailure::Runtime(_))
        ));
    }
}

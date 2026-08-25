//! Opt-in, workspace-rooted mutation Tool Provider Module.

use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

use futures::future::{LocalBoxFuture, ready};
use lenso::prelude::*;
use lenso_capability_agent_tool_provider::{
    self as tool_provider_contract, CatalogError, CatalogRequest, CatalogResponse,
    CatalogResponseToolsItem, ExecuteError, ExecuteRequest, ExecuteResponse,
    ExecuteResponseContentType, ToolProviderProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};
use sha2::{Digest, Sha256};

/// Stable Tool name for unique exact text replacement.
pub const EDIT_TOOL: &str = "edit";
/// Stable Tool name for create-only UTF-8 file writes.
pub const CREATE_FILE_TOOL: &str = "create_file";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceEditConfig {
    root: PathBuf,
    max_file_bytes: usize,
    max_edit_bytes: usize,
}

fn validate_config(config: &WorkspaceEditConfig) -> Result<(), RuntimeFailure> {
    if config.max_file_bytes == 0 || config.max_file_bytes > 16_777_216 {
        return Err(invalid_plan(
            "workspace-edit max_file_bytes must be between 1 and 16777216",
        ));
    }
    if config.max_edit_bytes == 0 || config.max_edit_bytes > 262_144 {
        return Err(invalid_plan(
            "workspace-edit max_edit_bytes must be between 1 and 262144",
        ));
    }
    Ok(())
}

#[lenso::module(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct WorkspaceEditProvider {
    #[config]
    config: WorkspaceEditConfig,
}

#[derive(Debug)]
enum WorkspaceEditFailure {
    Domain(ExecuteError),
    Runtime(RuntimeFailure),
}

impl From<ExecuteError> for WorkspaceEditFailure {
    fn from(error: ExecuteError) -> Self {
        Self::Domain(error)
    }
}

impl WorkspaceEditProvider {
    fn canonical_root(&self) -> Result<PathBuf, RuntimeFailure> {
        let root =
            fs::canonicalize(&self.config.root).map_err(|error| RuntimeFailure::ModuleFailure {
                detail: format!("workspace edit root is unavailable: {error}"),
            })?;
        if !root.is_dir() {
            return Err(RuntimeFailure::ModuleFailure {
                detail: "workspace edit root is not a directory".to_owned(),
            });
        }
        Ok(root)
    }

    fn resolve_target(root: &Path, path: &str) -> Result<PathBuf, ExecuteError> {
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
        let normal_parts = relative
            .components()
            .filter_map(|part| match part {
                Component::Normal(part) => Some(part),
                Component::CurDir => None,
                _ => unreachable!("components were validated above"),
            })
            .collect::<Vec<_>>();
        if normal_parts.is_empty() {
            return Err(ExecuteError::InvalidArguments);
        }

        let mut target = root.to_path_buf();
        for (index, part) in normal_parts.iter().enumerate() {
            target.push(part);
            if index + 1 == normal_parts.len() {
                break;
            }
            let metadata = fs::symlink_metadata(&target).map_err(|error| map_path_error(&error))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ExecuteError::PermissionDenied);
            }
        }
        Ok(target)
    }

    fn write_text(&self, arguments_json: &str) -> Result<ExecuteResponse, WorkspaceEditFailure> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Arguments {
            path: String,
            content: String,
        }

        let arguments = serde_json::from_str::<Arguments>(arguments_json)
            .map_err(|_| ExecuteError::InvalidArguments)?;
        if arguments.content.len() > self.config.max_file_bytes {
            return Err(ExecuteError::OutputLimitExceeded.into());
        }
        let root = self
            .canonical_root()
            .map_err(WorkspaceEditFailure::Runtime)?;
        let target = Self::resolve_target(&root, &arguments.path)?;
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ExecuteError::PermissionDenied.into());
            }
            Ok(_) => return Err(execution_failed("target_exists", "target already exists")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ExecuteError::PermissionDenied.into()),
        }
        let parent = target.parent().ok_or(ExecuteError::PermissionDenied)?;
        Self::persist_new(parent, &target, arguments.content.as_bytes())?;
        Ok(success_response(
            "created",
            &arguments.path,
            arguments.content.as_bytes(),
            None,
        ))
    }

    fn edit_text(&self, arguments_json: &str) -> Result<ExecuteResponse, WorkspaceEditFailure> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Arguments {
            path: String,
            old_text: String,
            new_text: String,
        }

        let arguments = serde_json::from_str::<Arguments>(arguments_json)
            .map_err(|_| ExecuteError::InvalidArguments)?;
        let edit_bytes = arguments
            .old_text
            .len()
            .checked_add(arguments.new_text.len())
            .ok_or(ExecuteError::OutputLimitExceeded)?;
        if arguments.old_text.is_empty() || arguments.old_text == arguments.new_text {
            return Err(ExecuteError::InvalidArguments.into());
        }
        if edit_bytes > self.config.max_edit_bytes {
            return Err(ExecuteError::OutputLimitExceeded.into());
        }

        let root = self
            .canonical_root()
            .map_err(WorkspaceEditFailure::Runtime)?;
        let target = Self::resolve_target(&root, &arguments.path)?;
        let metadata = fs::symlink_metadata(&target).map_err(|error| map_path_error(&error))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ExecuteError::PermissionDenied.into());
        }
        if metadata.len() > self.config.max_file_bytes as u64 {
            return Err(ExecuteError::OutputLimitExceeded.into());
        }
        let original = fs::read_to_string(&target).map_err(|error| map_read_error(&error))?;
        let mut matches = original.match_indices(&arguments.old_text);
        let Some((start, _)) = matches.next() else {
            return Err(execution_failed(
                "old_text_not_found",
                "old_text was not found",
            ));
        };
        if matches.next().is_some() {
            return Err(execution_failed(
                "old_text_not_unique",
                "old_text matches more than once",
            ));
        }
        let final_len = original
            .len()
            .checked_sub(arguments.old_text.len())
            .and_then(|length| length.checked_add(arguments.new_text.len()))
            .ok_or(ExecuteError::OutputLimitExceeded)?;
        if final_len > self.config.max_file_bytes {
            return Err(ExecuteError::OutputLimitExceeded.into());
        }
        let mut updated = String::with_capacity(final_len);
        updated.push_str(&original[..start]);
        updated.push_str(&arguments.new_text);
        updated.push_str(&original[start + arguments.old_text.len()..]);
        let parent = target.parent().ok_or(ExecuteError::PermissionDenied)?;
        Self::persist_replacement(parent, &target, &original, updated.as_bytes(), &metadata)?;
        Ok(success_response(
            "edited",
            &arguments.path,
            updated.as_bytes(),
            Some(original.len()),
        ))
    }

    fn persist_new(
        parent: &Path,
        target: &Path,
        content: &[u8],
    ) -> Result<(), WorkspaceEditFailure> {
        let mut temporary = new_workspace_file(parent)
            .map_err(|error| runtime_io("create temporary workspace file", &error))?;
        temporary
            .write_all(content)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| runtime_io("write temporary workspace file", &error))?;
        temporary.persist_noclobber(target).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                execution_failed("target_exists", "target already exists")
            } else {
                runtime_io("persist new workspace file", &error.error)
            }
        })?;
        sync_directory(parent)?;
        Ok(())
    }

    fn persist_replacement(
        parent: &Path,
        target: &Path,
        original: &str,
        updated: &[u8],
        metadata: &fs::Metadata,
    ) -> Result<(), WorkspaceEditFailure> {
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| runtime_io("create temporary workspace file", &error))?;
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .and_then(|()| temporary.write_all(updated))
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| runtime_io("write temporary workspace file", &error))?;
        let current = fs::read_to_string(target)
            .map_err(|error| runtime_io("recheck workspace file", &error))?;
        if current != original {
            return Err(execution_failed(
                "content_changed",
                "target changed during the edit",
            ));
        }
        temporary
            .persist(target)
            .map_err(|error| runtime_io("replace workspace file", &error.error))?;
        sync_directory(parent)?;
        Ok(())
    }
}

#[lenso::provides(tool_provider_contract::ToolProvider)]
impl ToolProviderProvider for WorkspaceEditProvider {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> LocalBoxFuture<'static, Result<Result<CatalogResponse, CatalogError>, RuntimeFailure>>
    {
        Box::pin(ready(Ok(Ok(CatalogResponse {
            tools: vec![
                CatalogResponseToolsItem {
                    name: EDIT_TOOL.to_owned(),
                    description: "Replace one unique, exact UTF-8 string in an existing workspace file. The call fails if old_text is absent or not unique.".to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{"new_text":{"type":"string"},"old_text":{"minLength":1,"type":"string"},"path":{"minLength":1,"type":"string"}},"required":["path","old_text","new_text"],"type":"object"}"#.to_owned(),
                },
                CatalogResponseToolsItem {
                    name: CREATE_FILE_TOOL.to_owned(),
                    description: "Create one new UTF-8 workspace file below an existing directory. Existing targets are never overwritten.".to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{"content":{"type":"string"},"path":{"minLength":1,"type":"string"}},"required":["path","content"],"type":"object"}"#.to_owned(),
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
            EDIT_TOOL => self.edit_text(&request.arguments_json),
            CREATE_FILE_TOOL => self.write_text(&request.arguments_json),
            _ => Err(ExecuteError::NotFound.into()),
        };
        Box::pin(ready(match result {
            Ok(response) => Ok(Ok(response)),
            Err(WorkspaceEditFailure::Domain(error)) => Ok(Err(error)),
            Err(WorkspaceEditFailure::Runtime(error)) => Err(error),
        }))
    }
}

impl Lifecycle for WorkspaceEditProvider {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        self.canonical_root().map(|_| ())
    }
}

fn success_response(
    operation: &str,
    path: &str,
    content: &[u8],
    previous_bytes: Option<usize>,
) -> ExecuteResponse {
    ExecuteResponse {
        content: format!("{operation} {path}"),
        content_type: ExecuteResponseContentType::Text,
        metadata_json: serde_json::json!({
            "operation": operation,
            "path": path,
            "previous_bytes": previous_bytes,
            "bytes_written": content.len(),
            "sha256": format!("{:x}", Sha256::digest(content)),
        })
        .to_string(),
    }
}

fn map_path_error(error: &std::io::Error) -> ExecuteError {
    if error.kind() == std::io::ErrorKind::NotFound {
        ExecuteError::NotFound
    } else {
        ExecuteError::PermissionDenied
    }
}

fn execution_failed(reason_code: &str, message: &str) -> WorkspaceEditFailure {
    WorkspaceEditFailure::Domain(ExecuteError::ExecutionFailed {
        payload: lenso_capability_agent_tool_provider::ExecuteErrorExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: message.to_owned(),
            details_json: "{}".to_owned(),
        },
    })
}

fn map_read_error(error: &std::io::Error) -> WorkspaceEditFailure {
    match error.kind() {
        std::io::ErrorKind::InvalidData => {
            execution_failed("not_utf8", "target is not valid UTF-8")
        }
        std::io::ErrorKind::NotFound => WorkspaceEditFailure::Domain(ExecuteError::NotFound),
        std::io::ErrorKind::PermissionDenied => {
            WorkspaceEditFailure::Domain(ExecuteError::PermissionDenied)
        }
        _ => runtime_io("read workspace file", error),
    }
}

fn runtime_io(operation: &str, error: &std::io::Error) -> WorkspaceEditFailure {
    WorkspaceEditFailure::Runtime(RuntimeFailure::ModuleFailure {
        detail: format!("{operation} failed: {error}"),
    })
}

fn sync_directory(directory: &Path) -> Result<(), WorkspaceEditFailure> {
    fs::File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| runtime_io("sync workspace directory", &error))
}

fn new_workspace_file(directory: &Path) -> std::io::Result<tempfile::NamedTempFile> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o666))
            .tempfile_in(directory)
    }
    #[cfg(not(unix))]
    {
        tempfile::NamedTempFile::new_in(directory)
    }
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(root: PathBuf) -> WorkspaceEditProvider {
        WorkspaceEditProvider {
            config: WorkspaceEditConfig {
                root,
                max_file_bytes: 4096,
                max_edit_bytes: 2048,
            },
        }
    }

    #[test]
    fn creates_a_new_file_without_overwriting() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("notes")).unwrap();
        let provider = provider(temporary.path().to_path_buf());
        let response = provider
            .write_text(r#"{"path":"notes/new.txt","content":"first\n"}"#)
            .unwrap();
        assert_eq!(
            fs::read_to_string(temporary.path().join("notes/new.txt")).unwrap(),
            "first\n"
        );
        assert!(response.metadata_json.contains("sha256"));
        assert!(matches!(
            provider.write_text(r#"{"path":"notes/new.txt","content":"second\n"}"#),
            Err(WorkspaceEditFailure::Domain(
                ExecuteError::ExecutionFailed { .. }
            ))
        ));
        assert_eq!(
            fs::read_to_string(temporary.path().join("notes/new.txt")).unwrap(),
            "first\n"
        );
    }

    #[test]
    fn edits_one_unique_exact_match() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("note.txt");
        fs::write(&target, "before middle after\n").unwrap();
        let provider = provider(temporary.path().to_path_buf());
        let response = provider
            .edit_text(r#"{"path":"note.txt","old_text":"middle","new_text":"updated"}"#)
            .unwrap();
        assert_eq!(
            fs::read_to_string(target).unwrap(),
            "before updated after\n"
        );
        let metadata: serde_json::Value = serde_json::from_str(&response.metadata_json).unwrap();
        assert_eq!(metadata["operation"], "edited");
        assert_eq!(metadata["previous_bytes"], 20);
    }

    #[test]
    fn rejects_absent_or_ambiguous_replacements_without_changing_the_file() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("note.txt");
        fs::write(&target, "same same\n").unwrap();
        let provider = provider(temporary.path().to_path_buf());
        for arguments in [
            r#"{"path":"note.txt","old_text":"missing","new_text":"x"}"#,
            r#"{"path":"note.txt","old_text":"same","new_text":"x"}"#,
        ] {
            assert!(matches!(
                provider.edit_text(arguments),
                Err(WorkspaceEditFailure::Domain(
                    ExecuteError::ExecutionFailed { .. }
                ))
            ));
        }
        assert_eq!(fs::read_to_string(target).unwrap(), "same same\n");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_traversal_and_symlink_targets() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(temporary.path().join("outside"), "outside").unwrap();
        symlink(temporary.path().join("outside"), root.join("link")).unwrap();
        let provider = provider(root);
        assert!(matches!(
            provider.write_text(r#"{"path":"../escape","content":"x"}"#),
            Err(WorkspaceEditFailure::Domain(ExecuteError::PermissionDenied))
        ));
        assert!(matches!(
            provider.edit_text(r#"{"path":"link","old_text":"outside","new_text":"x"}"#),
            Err(WorkspaceEditFailure::Domain(ExecuteError::PermissionDenied))
        ));
        assert!(matches!(
            provider.write_text(r#"{"path":"link","content":"x"}"#),
            Err(WorkspaceEditFailure::Domain(ExecuteError::PermissionDenied))
        ));
    }

    #[test]
    fn enforces_input_and_final_file_budgets() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("note.txt");
        fs::write(&target, "short").unwrap();
        let mut provider = provider(temporary.path().to_path_buf());
        provider.config.max_file_bytes = 5;
        provider.config.max_edit_bytes = 4;
        assert!(matches!(
            provider.write_text(r#"{"path":"large","content":"123456"}"#),
            Err(WorkspaceEditFailure::Domain(
                ExecuteError::OutputLimitExceeded
            ))
        ));
        assert!(matches!(
            provider.edit_text(r#"{"path":"note.txt","old_text":"short","new_text":"longer"}"#),
            Err(WorkspaceEditFailure::Domain(
                ExecuteError::OutputLimitExceeded
            ))
        ));
    }

    #[test]
    fn missing_root_is_a_runtime_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let provider = provider(root.clone());
        fs::remove_dir(root).unwrap();
        assert!(matches!(
            provider.write_text(r#"{"path":"note.txt","content":"x"}"#),
            Err(WorkspaceEditFailure::Runtime(_))
        ));
    }

    #[test]
    fn rejects_non_utf8_edit_targets() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("binary"), [0xff, 0xfe]).unwrap();
        let provider = provider(temporary.path().to_path_buf());
        assert!(matches!(
            provider.edit_text(r#"{"path":"binary","old_text":"x","new_text":"y"}"#),
            Err(WorkspaceEditFailure::Domain(
                ExecuteError::ExecutionFailed { .. }
            ))
        ));
    }
}

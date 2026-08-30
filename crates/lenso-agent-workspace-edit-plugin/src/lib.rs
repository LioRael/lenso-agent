//! Opt-in, workspace-rooted mutation Tool Provider Plugin.

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

use fs2::FileExt;
use futures::future::{LocalBoxFuture, ready};
use lenso::prelude::*;
use lenso_agent_native_support::WorkspaceScope;
use lenso_capability_agent_tool_provider::{
    self as tool_provider_contract, CatalogError, CatalogRequest, CatalogResponse, ContentType,
    ExecuteError, ExecuteRequest, ExecuteResponse, ToolDefinition, ToolExecutionClass,
    ToolProviderProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};
use sha2::{Digest, Sha256};

/// Stable Tool name for unique exact text replacement.
pub const EDIT_TOOL: &str = "edit";
/// Stable Tool name for create-only UTF-8 file writes.
pub const CREATE_FILE_TOOL: &str = "create_file";
/// Starts one explicit reversible set of Workspace edits.
pub const CHECKPOINT_CREATE_TOOL: &str = "checkpoint_create";
/// Renders the current changes recorded by one checkpoint.
pub const CHECKPOINT_REVIEW_TOOL: &str = "checkpoint_review";
/// Accepts current changes and removes their stored preimages.
pub const CHECKPOINT_ACCEPT_TOOL: &str = "checkpoint_accept";
/// Restores recorded preimages when no target changed outside the checkpoint.
pub const CHECKPOINT_RESTORE_TOOL: &str = "checkpoint_restore";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceEditConfig {
    root: PathBuf,
    #[serde(default)]
    delegated_root: Option<PathBuf>,
    max_file_bytes: usize,
    max_edit_bytes: usize,
    checkpoint_directory: PathBuf,
    require_checkpoint: bool,
    max_checkpoints: usize,
    max_review_bytes: usize,
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
    if config.checkpoint_directory.as_os_str().is_empty()
        || !(1..=1_000).contains(&config.max_checkpoints)
        || !(1_024..=1_048_576).contains(&config.max_review_bytes)
    {
        return Err(invalid_plan("workspace checkpoint limits are invalid"));
    }
    Ok(())
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct WorkspaceCheckpoint {
    schema_version: u32,
    checkpoint_id: String,
    workspace_root: String,
    files: Vec<CheckpointFile>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct CheckpointFile {
    path: String,
    original: Option<String>,
    known_digests: BTreeSet<String>,
}

#[lenso::plugin(
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
            fs::canonicalize(&self.config.root).map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("workspace edit root is unavailable: {error}"),
            })?;
        if !root.is_dir() {
            return Err(RuntimeFailure::PluginFailure {
                detail: "workspace edit root is not a directory".to_owned(),
            });
        }
        Ok(root)
    }

    fn invocation_root(&self, context: &InvocationContext) -> Result<PathBuf, RuntimeFailure> {
        let root = self.canonical_root()?;
        let Some(scope) = context
            .typed_extension::<WorkspaceScope>()
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("Workspace scope is invalid: {error}"),
            })?
        else {
            return Ok(root);
        };
        let scoped = fs::canonicalize(&scope.absolute_path).map_err(|error| {
            RuntimeFailure::PluginFailure {
                detail: format!("scoped Workspace is unavailable: {error}"),
            }
        })?;
        if scoped == root {
            return Ok(root);
        }
        let delegated =
            self.config
                .delegated_root
                .as_ref()
                .ok_or_else(|| RuntimeFailure::PluginFailure {
                    detail: "Workspace scope is outside the configured edit root".to_owned(),
                })?;
        let delegated =
            fs::canonicalize(delegated).map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("delegated Workspace root is unavailable: {error}"),
            })?;
        if !scoped.starts_with(&delegated) || !scoped.join(".git").exists() {
            return Err(RuntimeFailure::PluginFailure {
                detail: "Workspace scope is not an authorized delegated Git worktree".to_owned(),
            });
        }
        Ok(scoped)
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

    fn checkpoint_store(&self) -> Result<PathBuf, RuntimeFailure> {
        fs::create_dir_all(&self.config.checkpoint_directory).map_err(|error| {
            RuntimeFailure::PluginFailure {
                detail: format!("failed to create Workspace checkpoint directory: {error}"),
            }
        })?;
        let metadata =
            fs::symlink_metadata(&self.config.checkpoint_directory).map_err(|error| {
                RuntimeFailure::PluginFailure {
                    detail: format!("failed to inspect Workspace checkpoint directory: {error}"),
                }
            })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(RuntimeFailure::PluginFailure {
                detail: "Workspace checkpoint path is not a regular directory".to_owned(),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &self.config.checkpoint_directory,
                fs::Permissions::from_mode(0o700),
            )
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("failed to protect Workspace checkpoint directory: {error}"),
            })?;
        }
        fs::canonicalize(&self.config.checkpoint_directory).map_err(|error| {
            RuntimeFailure::PluginFailure {
                detail: format!("failed to resolve Workspace checkpoint directory: {error}"),
            }
        })
    }

    fn lock_checkpoint_store(&self) -> Result<(PathBuf, fs::File), RuntimeFailure> {
        let store = self.checkpoint_store()?;
        let lock_path = store.join(".checkpoints.lock");
        if fs::symlink_metadata(&lock_path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
        {
            return Err(RuntimeFailure::PluginFailure {
                detail: "Workspace checkpoint lock is not a regular file".to_owned(),
            });
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("failed to open Workspace checkpoint lock: {error}"),
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            lock.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|error| RuntimeFailure::PluginFailure {
                    detail: format!("failed to protect Workspace checkpoint lock: {error}"),
                })?;
        }
        lock.lock_exclusive()
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("failed to lock Workspace checkpoints: {error}"),
            })?;
        Ok((store, lock))
    }

    fn create_checkpoint_at(&self, root: &Path) -> Result<ExecuteResponse, WorkspaceEditFailure> {
        let root = root.to_str().ok_or_else(|| {
            WorkspaceEditFailure::Runtime(RuntimeFailure::PluginFailure {
                detail: "Workspace root must be valid UTF-8 for checkpointing".to_owned(),
            })
        })?;
        let (store, lock) = self
            .lock_checkpoint_store()
            .map_err(WorkspaceEditFailure::Runtime)?;
        let count = fs::read_dir(&store)
            .map_err(|error| runtime_io("list Workspace checkpoints", &error))?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "json")
            })
            .count();
        if count >= self.config.max_checkpoints {
            return Err(execution_failed(
                "checkpoint_limit_reached",
                "Workspace checkpoint limit reached",
            ));
        }
        let checkpoint_id = uuid::Uuid::new_v4().to_string();
        let checkpoint = WorkspaceCheckpoint {
            schema_version: 1,
            checkpoint_id: checkpoint_id.clone(),
            workspace_root: root.to_owned(),
            files: Vec::new(),
        };
        write_checkpoint(&store, &checkpoint)?;
        FileExt::unlock(&lock)
            .map_err(|error| runtime_io("unlock Workspace checkpoints", &error))?;
        Ok(ExecuteResponse {
            content_blocks: None,
            content: format!("created checkpoint {checkpoint_id}"),
            content_type: ContentType::Text,
            metadata_json: serde_json::json!({
                "operation": "checkpoint_created",
                "checkpoint_id": checkpoint_id,
                "files": 0
            })
            .to_string()
            .try_into()
            .expect("checkpoint metadata is valid JSON"),
        })
    }

    fn record_checkpoint(
        &self,
        checkpoint_id: Option<&str>,
        root: &Path,
        path: &str,
        original: Option<&str>,
        updated: &str,
    ) -> Result<(), WorkspaceEditFailure> {
        let Some(checkpoint_id) = checkpoint_id else {
            if self.config.require_checkpoint {
                return Err(execution_failed(
                    "checkpoint_required",
                    "Create a Workspace checkpoint and pass its ID before editing",
                ));
            }
            return Ok(());
        };
        validate_checkpoint_id(checkpoint_id)?;
        let root_text = root.to_str().ok_or_else(|| {
            WorkspaceEditFailure::Runtime(RuntimeFailure::PluginFailure {
                detail: "Workspace root must be valid UTF-8 for checkpointing".to_owned(),
            })
        })?;
        let (store, lock) = self
            .lock_checkpoint_store()
            .map_err(WorkspaceEditFailure::Runtime)?;
        let mut checkpoint = read_checkpoint(&store, checkpoint_id)?;
        if checkpoint.workspace_root != root_text {
            return Err(ExecuteError::PermissionDenied.into());
        }
        let original_digest = original.map(content_digest);
        let updated_digest = content_digest(updated);
        if let Some(file) = checkpoint.files.iter_mut().find(|file| file.path == path) {
            if file.original.as_deref().map(content_digest) != original_digest
                && !file.known_digests.contains(
                    &original_digest
                        .clone()
                        .unwrap_or_else(|| "missing".to_owned()),
                )
            {
                return Err(execution_failed(
                    "checkpoint_content_conflict",
                    "Workspace file changed outside this checkpoint",
                ));
            }
            if let Some(digest) = original_digest {
                file.known_digests.insert(digest);
            }
            file.known_digests.insert(updated_digest);
        } else {
            let mut known_digests = BTreeSet::new();
            if let Some(digest) = original_digest {
                known_digests.insert(digest);
            }
            known_digests.insert(updated_digest);
            checkpoint.files.push(CheckpointFile {
                path: path.to_owned(),
                original: original.map(ToOwned::to_owned),
                known_digests,
            });
            checkpoint
                .files
                .sort_by(|left, right| left.path.cmp(&right.path));
        }
        write_checkpoint(&store, &checkpoint)?;
        FileExt::unlock(&lock)
            .map_err(|error| runtime_io("unlock Workspace checkpoints", &error))?;
        Ok(())
    }

    fn review_checkpoint_at(
        &self,
        root: &Path,
        checkpoint_id: &str,
    ) -> Result<ExecuteResponse, WorkspaceEditFailure> {
        validate_checkpoint_id(checkpoint_id)?;
        let (store, lock) = self
            .lock_checkpoint_store()
            .map_err(WorkspaceEditFailure::Runtime)?;
        let checkpoint = read_checkpoint(&store, checkpoint_id)?;
        ensure_checkpoint_root(&checkpoint, root)?;
        let mut review = String::new();
        let mut conflicts = 0_usize;
        let mut changes = 0_usize;
        for file in &checkpoint.files {
            let current =
                read_optional_checkpoint_target(root, &file.path, self.config.max_file_bytes)?;
            if current == file.original {
                continue;
            }
            changes += 1;
            if current
                .as_deref()
                .map(content_digest)
                .is_some_and(|digest| !file.known_digests.contains(&digest))
                || (current.is_none() && file.original.is_some())
            {
                conflicts += 1;
            }
            review.push_str(&render_file_diff(
                &file.path,
                file.original.as_deref(),
                current.as_deref(),
            ));
            if review.len() > self.config.max_review_bytes {
                return Err(ExecuteError::OutputLimitExceeded.into());
            }
        }
        FileExt::unlock(&lock)
            .map_err(|error| runtime_io("unlock Workspace checkpoints", &error))?;
        if review.is_empty() {
            review.push_str("No changes recorded for this checkpoint.\n");
        }
        Ok(ExecuteResponse {
            content_blocks: None,
            content: review,
            content_type: ContentType::Text,
            metadata_json: serde_json::json!({
                "operation": "checkpoint_reviewed",
                "checkpoint_id": checkpoint_id,
                "changes": changes,
                "conflicts": conflicts
            })
            .to_string()
            .try_into()
            .expect("checkpoint metadata is valid JSON"),
        })
    }

    fn finish_checkpoint_at(
        &self,
        root: &Path,
        checkpoint_id: &str,
        restore: bool,
    ) -> Result<ExecuteResponse, WorkspaceEditFailure> {
        validate_checkpoint_id(checkpoint_id)?;
        let (store, lock) = self
            .lock_checkpoint_store()
            .map_err(WorkspaceEditFailure::Runtime)?;
        let checkpoint = read_checkpoint(&store, checkpoint_id)?;
        ensure_checkpoint_root(&checkpoint, root)?;
        if restore {
            let mut current_files = Vec::with_capacity(checkpoint.files.len());
            for file in &checkpoint.files {
                let current =
                    read_optional_checkpoint_target(root, &file.path, self.config.max_file_bytes)?;
                let safe = current == file.original
                    || current
                        .as_deref()
                        .map(content_digest)
                        .is_some_and(|digest| file.known_digests.contains(&digest))
                    || (current.is_none() && file.original.is_none());
                if !safe {
                    return Err(execution_failed(
                        "checkpoint_content_conflict",
                        "Workspace file changed outside this checkpoint; nothing was restored",
                    ));
                }
                current_files.push(current);
            }
            for (file, current) in checkpoint.files.iter().zip(current_files) {
                restore_checkpoint_file(root, file, current.as_deref())?;
            }
        }
        fs::remove_file(checkpoint_path(&store, checkpoint_id))
            .map_err(|error| runtime_io("remove Workspace checkpoint", &error))?;
        sync_directory(&store)?;
        FileExt::unlock(&lock)
            .map_err(|error| runtime_io("unlock Workspace checkpoints", &error))?;
        let operation = if restore {
            "checkpoint_restored"
        } else {
            "checkpoint_accepted"
        };
        Ok(ExecuteResponse {
            content_blocks: None,
            content: format!("{operation} {checkpoint_id}"),
            content_type: ContentType::Text,
            metadata_json: serde_json::json!({
                "operation": operation,
                "checkpoint_id": checkpoint_id,
                "files": checkpoint.files.len()
            })
            .to_string()
            .try_into()
            .expect("checkpoint metadata is valid JSON"),
        })
    }

    fn write_text_at(
        &self,
        root: &Path,
        arguments_json: &str,
    ) -> Result<ExecuteResponse, WorkspaceEditFailure> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Arguments {
            path: String,
            content: String,
            #[serde(default)]
            checkpoint_id: Option<String>,
        }

        let arguments = serde_json::from_str::<Arguments>(arguments_json)
            .map_err(|_| ExecuteError::InvalidArguments)?;
        if arguments.content.len() > self.config.max_file_bytes {
            return Err(ExecuteError::OutputLimitExceeded.into());
        }
        let target = Self::resolve_target(root, &arguments.path)?;
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ExecuteError::PermissionDenied.into());
            }
            Ok(_) => return Err(execution_failed("target_exists", "target already exists")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ExecuteError::PermissionDenied.into()),
        }
        let parent = target.parent().ok_or(ExecuteError::PermissionDenied)?;
        self.record_checkpoint(
            arguments.checkpoint_id.as_deref(),
            root,
            &arguments.path,
            None,
            &arguments.content,
        )?;
        Self::persist_new(parent, &target, arguments.content.as_bytes())?;
        Ok(success_response(
            "created",
            &arguments.path,
            arguments.content.as_bytes(),
            None,
            Some(1),
        ))
    }

    fn edit_text_at(
        &self,
        root: &Path,
        arguments_json: &str,
    ) -> Result<ExecuteResponse, WorkspaceEditFailure> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Arguments {
            path: String,
            old_text: String,
            new_text: String,
            #[serde(default)]
            checkpoint_id: Option<String>,
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

        let target = Self::resolve_target(root, &arguments.path)?;
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
        self.record_checkpoint(
            arguments.checkpoint_id.as_deref(),
            root,
            &arguments.path,
            Some(&original),
            &updated,
        )?;
        Self::persist_replacement(parent, &target, &original, updated.as_bytes(), &metadata)?;
        Ok(success_response(
            "edited",
            &arguments.path,
            updated.as_bytes(),
            Some(original.len()),
            Some(
                original[..start]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count()
                    + 1,
            ),
        ))
    }

    #[cfg(test)]
    fn create_checkpoint(&self) -> Result<ExecuteResponse, WorkspaceEditFailure> {
        let root = self
            .canonical_root()
            .map_err(WorkspaceEditFailure::Runtime)?;
        self.create_checkpoint_at(&root)
    }

    #[cfg(test)]
    fn review_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> Result<ExecuteResponse, WorkspaceEditFailure> {
        let root = self
            .canonical_root()
            .map_err(WorkspaceEditFailure::Runtime)?;
        self.review_checkpoint_at(&root, checkpoint_id)
    }

    #[cfg(test)]
    fn finish_checkpoint(
        &self,
        checkpoint_id: &str,
        restore: bool,
    ) -> Result<ExecuteResponse, WorkspaceEditFailure> {
        let root = self
            .canonical_root()
            .map_err(WorkspaceEditFailure::Runtime)?;
        self.finish_checkpoint_at(&root, checkpoint_id, restore)
    }

    #[cfg(test)]
    fn write_text(&self, arguments_json: &str) -> Result<ExecuteResponse, WorkspaceEditFailure> {
        let root = self
            .canonical_root()
            .map_err(WorkspaceEditFailure::Runtime)?;
        self.write_text_at(&root, arguments_json)
    }

    #[cfg(test)]
    fn edit_text(&self, arguments_json: &str) -> Result<ExecuteResponse, WorkspaceEditFailure> {
        let root = self
            .canonical_root()
            .map_err(WorkspaceEditFailure::Runtime)?;
        self.edit_text_at(&root, arguments_json)
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

fn validate_checkpoint_id(checkpoint_id: &str) -> Result<(), WorkspaceEditFailure> {
    uuid::Uuid::parse_str(checkpoint_id)
        .map(|_| ())
        .map_err(|_| ExecuteError::InvalidArguments.into())
}

fn checkpoint_path(store: &Path, checkpoint_id: &str) -> PathBuf {
    store.join(format!("{checkpoint_id}.json"))
}

fn read_checkpoint(
    store: &Path,
    checkpoint_id: &str,
) -> Result<WorkspaceCheckpoint, WorkspaceEditFailure> {
    let path = checkpoint_path(store, checkpoint_id);
    let metadata = fs::symlink_metadata(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => WorkspaceEditFailure::Domain(ExecuteError::NotFound),
        _ => runtime_io("inspect Workspace checkpoint", &error),
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 32_000_000 {
        return Err(WorkspaceEditFailure::Runtime(
            RuntimeFailure::PluginFailure {
                detail: "Workspace checkpoint is not a bounded regular file".to_owned(),
            },
        ));
    }
    let checkpoint = serde_json::from_slice::<WorkspaceCheckpoint>(
        &fs::read(&path).map_err(|error| runtime_io("read Workspace checkpoint", &error))?,
    )
    .map_err(|error| {
        WorkspaceEditFailure::Runtime(RuntimeFailure::PluginFailure {
            detail: format!("Workspace checkpoint is corrupt: {error}"),
        })
    })?;
    if checkpoint.schema_version != 1 || checkpoint.checkpoint_id != checkpoint_id {
        return Err(WorkspaceEditFailure::Runtime(
            RuntimeFailure::PluginFailure {
                detail: "Workspace checkpoint identity is invalid".to_owned(),
            },
        ));
    }
    Ok(checkpoint)
}

fn write_checkpoint(
    store: &Path,
    checkpoint: &WorkspaceCheckpoint,
) -> Result<(), WorkspaceEditFailure> {
    let encoded = serde_json::to_vec(checkpoint).map_err(|error| {
        WorkspaceEditFailure::Runtime(RuntimeFailure::PluginFailure {
            detail: format!("failed to encode Workspace checkpoint: {error}"),
        })
    })?;
    if encoded.len() > 32_000_000 {
        return Err(ExecuteError::OutputLimitExceeded.into());
    }
    let mut temporary = tempfile::NamedTempFile::new_in(store)
        .map_err(|error| runtime_io("create Workspace checkpoint", &error))?;
    temporary
        .write_all(&encoded)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| runtime_io("write Workspace checkpoint", &error))?;
    temporary
        .persist(checkpoint_path(store, &checkpoint.checkpoint_id))
        .map_err(|error| runtime_io("persist Workspace checkpoint", &error.error))?;
    sync_directory(store)
}

fn ensure_checkpoint_root(
    checkpoint: &WorkspaceCheckpoint,
    root: &Path,
) -> Result<(), WorkspaceEditFailure> {
    if root.to_str() == Some(&checkpoint.workspace_root) {
        Ok(())
    } else {
        Err(ExecuteError::PermissionDenied.into())
    }
}

fn content_digest(content: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(content.as_bytes()))
}

fn read_optional_checkpoint_target(
    root: &Path,
    path: &str,
    max_file_bytes: usize,
) -> Result<Option<String>, WorkspaceEditFailure> {
    let target = WorkspaceEditProvider::resolve_target(root, path)?;
    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(runtime_io("inspect checkpoint target", &error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ExecuteError::PermissionDenied.into());
    }
    if metadata.len() > max_file_bytes as u64 {
        return Err(ExecuteError::OutputLimitExceeded.into());
    }
    fs::read_to_string(target)
        .map(Some)
        .map_err(|error| map_read_error(&error))
}

fn restore_checkpoint_file(
    root: &Path,
    file: &CheckpointFile,
    current: Option<&str>,
) -> Result<(), WorkspaceEditFailure> {
    if current == file.original.as_deref() {
        return Ok(());
    }
    let target = WorkspaceEditProvider::resolve_target(root, &file.path)?;
    match &file.original {
        Some(original) => {
            let metadata = fs::symlink_metadata(&target)
                .map_err(|error| runtime_io("inspect checkpoint restore target", &error))?;
            let parent = target.parent().ok_or(ExecuteError::PermissionDenied)?;
            WorkspaceEditProvider::persist_replacement(
                parent,
                &target,
                current.unwrap_or_default(),
                original.as_bytes(),
                &metadata,
            )
        }
        None => {
            fs::remove_file(&target)
                .map_err(|error| runtime_io("remove checkpoint-created file", &error))?;
            sync_directory(target.parent().ok_or(ExecuteError::PermissionDenied)?)
        }
    }
}

fn render_file_diff(path: &str, original: Option<&str>, current: Option<&str>) -> String {
    let old = original.unwrap_or_default();
    let new = current.unwrap_or_default();
    let old_lines = old.lines().collect::<Vec<_>>();
    let new_lines = new.lines().collect::<Vec<_>>();
    let prefix = old_lines
        .iter()
        .zip(&new_lines)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = old_lines[prefix..]
        .iter()
        .rev()
        .zip(new_lines[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let old_end = old_lines.len().saturating_sub(suffix);
    let new_end = new_lines.len().saturating_sub(suffix);
    let mut diff = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -{},{} +{},{} @@\n",
        prefix + 1,
        old_end.saturating_sub(prefix),
        prefix + 1,
        new_end.saturating_sub(prefix)
    );
    for line in &old_lines[prefix..old_end] {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in &new_lines[prefix..new_end] {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

fn checkpoint_tool_schema() -> String {
    r#"{"additionalProperties":false,"properties":{"checkpoint_id":{"type":"string"}},"required":["checkpoint_id"],"type":"object"}"#
        .to_owned()
}

fn checkpoint_argument(arguments_json: &str) -> Result<String, WorkspaceEditFailure> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Arguments {
        checkpoint_id: String,
    }
    serde_json::from_str::<Arguments>(arguments_json)
        .map(|arguments| arguments.checkpoint_id)
        .map_err(|_| ExecuteError::InvalidArguments.into())
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
                ToolDefinition {
                    name: EDIT_TOOL.to_owned(),
                    description: "Replace one unique, exact UTF-8 string in an existing workspace file. Pass checkpoint_id when the Profile requires reversible edits.".to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{"checkpoint_id":{"type":"string"},"new_text":{"type":"string"},"old_text":{"minLength":1,"type":"string"},"path":{"minLength":1,"type":"string"}},"required":["path","old_text","new_text"],"type":"object"}"#.to_owned().try_into().expect("static Tool schema must be valid JSON"),
                    execution: ToolExecutionClass::Exclusive,
                },
                ToolDefinition {
                    name: CREATE_FILE_TOOL.to_owned(),
                    description: "Create one new UTF-8 workspace file below an existing directory. Pass checkpoint_id when the Profile requires reversible edits.".to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{"checkpoint_id":{"type":"string"},"content":{"type":"string"},"path":{"minLength":1,"type":"string"}},"required":["path","content"],"type":"object"}"#.to_owned().try_into().expect("static Tool schema must be valid JSON"),
                    execution: ToolExecutionClass::Exclusive,
                },
                ToolDefinition {
                    name: CHECKPOINT_CREATE_TOOL.to_owned(),
                    description: "Start one explicit reversible Workspace change set before editing files.".to_owned(),
                    input_schema_json: r#"{"additionalProperties":false,"properties":{},"type":"object"}"#.to_owned().try_into().expect("static Tool schema must be valid JSON"),
                    execution: ToolExecutionClass::Exclusive,
                },
                ToolDefinition {
                    name: CHECKPOINT_REVIEW_TOOL.to_owned(),
                    description: "Review the bounded unified diff and conflict count for one Workspace checkpoint.".to_owned(),
                    input_schema_json: checkpoint_tool_schema().try_into().expect("static Tool schema must be valid JSON"),
                    execution: ToolExecutionClass::Exclusive,
                },
                ToolDefinition {
                    name: CHECKPOINT_ACCEPT_TOOL.to_owned(),
                    description: "Accept the current Workspace changes and delete their stored checkpoint preimages.".to_owned(),
                    input_schema_json: checkpoint_tool_schema().try_into().expect("static Tool schema must be valid JSON"),
                    execution: ToolExecutionClass::Exclusive,
                },
                ToolDefinition {
                    name: CHECKPOINT_RESTORE_TOOL.to_owned(),
                    description: "Restore every file in one Workspace checkpoint, but only when no file has an external content conflict.".to_owned(),
                    input_schema_json: checkpoint_tool_schema().try_into().expect("static Tool schema must be valid JSON"),
                    execution: ToolExecutionClass::Exclusive,
                },
            ],
        }))))
    }

    fn execute(
        &self,
        context: InvocationContext,
        request: ExecuteRequest,
    ) -> LocalBoxFuture<'static, Result<Result<ExecuteResponse, ExecuteError>, RuntimeFailure>>
    {
        let result = match self.invocation_root(&context) {
            Ok(root) => match request.name.as_str() {
                EDIT_TOOL => self.edit_text_at(&root, request.arguments_json.as_str()),
                CREATE_FILE_TOOL => self.write_text_at(&root, request.arguments_json.as_str()),
                CHECKPOINT_CREATE_TOOL => serde_json::from_str::<
                    serde_json::Map<String, serde_json::Value>,
                >(request.arguments_json.as_str())
                .map_err(|_| ExecuteError::InvalidArguments.into())
                .and_then(|arguments| {
                    if arguments.is_empty() {
                        self.create_checkpoint_at(&root)
                    } else {
                        Err(ExecuteError::InvalidArguments.into())
                    }
                }),
                CHECKPOINT_REVIEW_TOOL => checkpoint_argument(request.arguments_json.as_str())
                    .and_then(|checkpoint_id| self.review_checkpoint_at(&root, &checkpoint_id)),
                CHECKPOINT_ACCEPT_TOOL => checkpoint_argument(request.arguments_json.as_str())
                    .and_then(|checkpoint_id| {
                        self.finish_checkpoint_at(&root, &checkpoint_id, false)
                    }),
                CHECKPOINT_RESTORE_TOOL => checkpoint_argument(request.arguments_json.as_str())
                    .and_then(|checkpoint_id| {
                        self.finish_checkpoint_at(&root, &checkpoint_id, true)
                    }),
                _ => Err(ExecuteError::NotFound.into()),
            },
            Err(error) => Err(WorkspaceEditFailure::Runtime(error)),
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
        self.canonical_root()?;
        self.checkpoint_store().map(|_| ())
    }
}

fn success_response(
    operation: &str,
    path: &str,
    content: &[u8],
    previous_bytes: Option<usize>,
    start_line: Option<usize>,
) -> ExecuteResponse {
    ExecuteResponse {
        content_blocks: None,
        content: format!("{operation} {path}"),
        content_type: ContentType::Text,
        metadata_json: serde_json::json!({
            "operation": operation,
            "path": path,
            "previous_bytes": previous_bytes,
            "bytes_written": content.len(),
            "start_line": start_line,
            "sha256": format!("{:x}", Sha256::digest(content)),
        })
        .to_string()
        .try_into()
        .expect("serde_json values must produce valid JSON"),
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
    WorkspaceEditFailure::Runtime(RuntimeFailure::PluginFailure {
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
        let checkpoint_directory = root.join(".test-checkpoints");
        WorkspaceEditProvider {
            config: WorkspaceEditConfig {
                root,
                delegated_root: None,
                max_file_bytes: 4096,
                max_edit_bytes: 2048,
                checkpoint_directory,
                require_checkpoint: false,
                max_checkpoints: 16,
                max_review_bytes: 16_384,
            },
        }
    }

    fn checkpoint_id(provider: &WorkspaceEditProvider) -> String {
        let response = provider.create_checkpoint().unwrap();
        serde_json::from_str::<serde_json::Value>(response.metadata_json.as_str()).unwrap()
            ["checkpoint_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn checkpoint_reviews_and_restores_edited_and_created_files() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("note.txt"), "before\n").unwrap();
        let provider = provider(temporary.path().to_path_buf());
        let checkpoint_id = checkpoint_id(&provider);

        provider
            .edit_text(
                &serde_json::json!({
                    "path": "note.txt",
                    "old_text": "before",
                    "new_text": "after",
                    "checkpoint_id": checkpoint_id
                })
                .to_string(),
            )
            .unwrap();
        provider
            .write_text(
                &serde_json::json!({
                    "path": "created.txt",
                    "content": "new\n",
                    "checkpoint_id": checkpoint_id
                })
                .to_string(),
            )
            .unwrap();

        let review = provider.review_checkpoint(&checkpoint_id).unwrap();
        assert!(review.content.contains("--- a/note.txt"));
        assert!(review.content.contains("-before"));
        assert!(review.content.contains("+after"));
        let metadata: serde_json::Value =
            serde_json::from_str(review.metadata_json.as_str()).unwrap();
        assert_eq!(metadata["changes"], 2);
        assert_eq!(metadata["conflicts"], 0);

        provider.finish_checkpoint(&checkpoint_id, true).unwrap();
        assert_eq!(
            fs::read_to_string(temporary.path().join("note.txt")).unwrap(),
            "before\n"
        );
        assert!(!temporary.path().join("created.txt").exists());
    }

    #[test]
    fn checkpoint_restore_rejects_every_write_when_external_content_conflicts() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("first.txt"), "one\n").unwrap();
        fs::write(temporary.path().join("second.txt"), "two\n").unwrap();
        let provider = provider(temporary.path().to_path_buf());
        let checkpoint_id = checkpoint_id(&provider);
        for (path, old_text, new_text) in [
            ("first.txt", "one", "changed-one"),
            ("second.txt", "two", "changed-two"),
        ] {
            provider
                .edit_text(
                    &serde_json::json!({
                        "path": path,
                        "old_text": old_text,
                        "new_text": new_text,
                        "checkpoint_id": checkpoint_id
                    })
                    .to_string(),
                )
                .unwrap();
        }
        fs::write(temporary.path().join("second.txt"), "external\n").unwrap();

        assert!(matches!(
            provider.finish_checkpoint(&checkpoint_id, true),
            Err(WorkspaceEditFailure::Domain(
                ExecuteError::ExecutionFailed { .. }
            ))
        ));
        assert_eq!(
            fs::read_to_string(temporary.path().join("first.txt")).unwrap(),
            "changed-one\n"
        );
        assert_eq!(
            fs::read_to_string(temporary.path().join("second.txt")).unwrap(),
            "external\n"
        );
    }

    #[test]
    fn required_checkpoint_rejects_untracked_edits_and_accept_keeps_changes() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("note.txt"), "before\n").unwrap();
        let mut provider = provider(temporary.path().to_path_buf());
        provider.config.require_checkpoint = true;
        assert!(matches!(
            provider.edit_text(r#"{"path":"note.txt","old_text":"before","new_text":"after"}"#),
            Err(WorkspaceEditFailure::Domain(
                ExecuteError::ExecutionFailed { .. }
            ))
        ));
        let checkpoint_id = checkpoint_id(&provider);
        provider
            .edit_text(
                &serde_json::json!({
                    "path": "note.txt",
                    "old_text": "before",
                    "new_text": "after",
                    "checkpoint_id": checkpoint_id
                })
                .to_string(),
            )
            .unwrap();
        provider.finish_checkpoint(&checkpoint_id, false).unwrap();
        assert_eq!(
            fs::read_to_string(temporary.path().join("note.txt")).unwrap(),
            "after\n"
        );
        assert!(matches!(
            provider.review_checkpoint(&checkpoint_id),
            Err(WorkspaceEditFailure::Domain(ExecuteError::NotFound))
        ));
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
        assert!(response.metadata_json.as_str().contains("sha256"));
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
        let metadata: serde_json::Value =
            serde_json::from_str(response.metadata_json.as_str()).unwrap();
        assert_eq!(metadata["operation"], "edited");
        assert_eq!(metadata["previous_bytes"], 20);
        assert_eq!(metadata["start_line"], 1);
    }

    #[test]
    fn edit_metadata_reports_the_real_start_line() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("note.txt");
        fs::write(&target, "first\nsecond\nthird\n").unwrap();
        let provider = provider(temporary.path().to_path_buf());
        let response = provider
            .edit_text(r#"{"path":"note.txt","old_text":"second","new_text":"updated"}"#)
            .unwrap();
        let metadata: serde_json::Value =
            serde_json::from_str(response.metadata_json.as_str()).unwrap();
        assert_eq!(metadata["start_line"], 2);
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

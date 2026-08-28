//! Workspace-rooted read Capability reserved for reviewed guest imports.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use futures::future::{LocalBoxFuture, ready};
use lenso::prelude::*;
use lenso_capability_agent_workspace_read::{
    self as workspace_read_contract, ReadTextError, ReadTextErrorExecutionFailedPayload,
    ReadTextRequest, ReadTextResponse, WorkspaceReadProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceImportReadConfig {
    root: PathBuf,
    max_output_bytes: usize,
}

fn validate_config(config: &WorkspaceImportReadConfig) -> Result<(), RuntimeFailure> {
    if config.max_output_bytes == 0 || config.max_output_bytes > 1_048_576 {
        return Err(invalid_plan(
            "workspace-import-read max_output_bytes must be between 1 and 1048576",
        ));
    }
    Ok(())
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    configuration_defaults = "config.defaults.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct WorkspaceImportReader {
    #[config]
    config: WorkspaceImportReadConfig,
}

impl WorkspaceImportReader {
    fn canonical_root(&self) -> Result<PathBuf, RuntimeFailure> {
        let root =
            fs::canonicalize(&self.config.root).map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("workspace import root is unavailable: {error}"),
            })?;
        if !root.is_dir() {
            return Err(RuntimeFailure::PluginFailure {
                detail: "workspace import root is not a directory".to_owned(),
            });
        }
        Ok(root)
    }

    fn resolve(root: &Path, path: &str) -> Result<PathBuf, ReadTextError> {
        if path.is_empty() {
            return Err(ReadTextError::InvalidArguments);
        }
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(ReadTextError::PermissionDenied);
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
                    return Err(ReadTextError::PermissionDenied);
                }
            }
        }
        Ok(resolved)
    }
}

#[lenso::provides(workspace_read_contract::WorkspaceRead)]
impl WorkspaceReadProvider for WorkspaceImportReader {
    fn read_text(
        &self,
        _context: InvocationContext,
        request: ReadTextRequest,
    ) -> LocalBoxFuture<'static, Result<Result<ReadTextResponse, ReadTextError>, RuntimeFailure>>
    {
        let result = (|| -> Result<ReadTextResponse, WorkspaceImportReadFailure> {
            let root = self.canonical_root()?;
            let resolved = Self::resolve(&root, &request.path).map_err(domain_failure)?;
            if !resolved.is_file() {
                return Err(domain_failure(ReadTextError::PermissionDenied));
            }
            if fs::metadata(&resolved)
                .map_err(|_| domain_failure(ReadTextError::NotFound))?
                .len()
                > self.config.max_output_bytes as u64
            {
                return Err(domain_failure(ReadTextError::OutputLimitExceeded));
            }
            let content = fs::read_to_string(&resolved).map_err(|_| {
                domain_failure(ReadTextError::ExecutionFailed {
                    payload: ReadTextErrorExecutionFailedPayload {
                        reason_code: "not_utf8".to_owned(),
                        message: "workspace file is not valid UTF-8".to_owned(),
                        details_json: "{}"
                            .try_into()
                            .expect("static error details must be valid JSON"),
                    },
                })
            })?;
            Ok(ReadTextResponse {
                content,
                metadata_json: serde_json::json!({"path": request.path})
                    .to_string()
                    .try_into()
                    .expect("serde_json values must produce valid JSON"),
            })
        })();
        Box::pin(ready(match result {
            Ok(response) => Ok(Ok(response)),
            Err(WorkspaceImportReadFailure::Domain(error)) => Ok(Err(error)),
            Err(WorkspaceImportReadFailure::Runtime(error)) => Err(error),
        }))
    }
}

impl Lifecycle for WorkspaceImportReader {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        self.canonical_root().map(|_| ())
    }
}

#[derive(Debug)]
enum WorkspaceImportReadFailure {
    Domain(ReadTextError),
    Runtime(RuntimeFailure),
}

impl From<RuntimeFailure> for WorkspaceImportReadFailure {
    fn from(error: RuntimeFailure) -> Self {
        Self::Runtime(error)
    }
}

fn domain_failure(error: ReadTextError) -> WorkspaceImportReadFailure {
    WorkspaceImportReadFailure::Domain(error)
}

fn map_path_error(error: &std::io::Error) -> ReadTextError {
    if error.kind() == std::io::ErrorKind::NotFound {
        ReadTextError::NotFound
    } else {
        ReadTextError::PermissionDenied
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
    use lenso_kernel::CancellationToken;

    fn provider(root: PathBuf) -> WorkspaceImportReader {
        WorkspaceImportReader {
            config: WorkspaceImportReadConfig {
                root,
                max_output_bytes: 4096,
            },
        }
    }

    #[test]
    fn preserves_success_domain_and_runtime_outcomes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("README.md"), "# Capability Fixture\n").unwrap();
        let provider = provider(root.clone());
        let context = || InvocationContext::new(1, None, CancellationToken::new());

        let success = futures::executor::block_on(provider.read_text(
            context(),
            ReadTextRequest {
                path: "README.md".to_owned(),
            },
        ))
        .unwrap()
        .unwrap();
        assert_eq!(success.content, "# Capability Fixture\n");

        let domain = futures::executor::block_on(provider.read_text(
            context(),
            ReadTextRequest {
                path: "../outside".to_owned(),
            },
        ))
        .unwrap()
        .unwrap_err();
        assert_eq!(domain, ReadTextError::PermissionDenied);

        fs::remove_file(root.join("README.md")).unwrap();
        fs::remove_dir(root).unwrap();
        let runtime = futures::executor::block_on(provider.read_text(
            context(),
            ReadTextRequest {
                path: "README.md".to_owned(),
            },
        ))
        .unwrap_err();
        assert!(matches!(runtime, RuntimeFailure::PluginFailure { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("target"), "secret").unwrap();
        symlink("target", temp.path().join("link")).unwrap();
        let result = futures::executor::block_on(provider(temp.path().into()).read_text(
            InvocationContext::new(1, None, CancellationToken::new()),
            ReadTextRequest {
                path: "link".to_owned(),
            },
        ))
        .unwrap()
        .unwrap_err();
        assert_eq!(result, ReadTextError::PermissionDenied);
    }
}

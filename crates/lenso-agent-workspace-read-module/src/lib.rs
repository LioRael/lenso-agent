//! Read-only, workspace-rooted Tool Provider Module.

use std::{fs, path::PathBuf, rc::Rc};

use futures::future::{LocalBoxFuture, ready};
use lenso_capability_agent_tool_provider::{
    CatalogError, CatalogRequest, CatalogResponse, CatalogResponseToolsItem, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecuteResponseContentType, ToolProviderEndpoint,
    ToolProviderProvider,
};
use lenso_kernel::{
    InvocationContext, ModuleFuture, ModuleLifecycle, NativeRequestEndpoint, PrepareContext,
    RuntimeFailure,
};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance};

/// Runtime package identity selected by App Composition.
pub const PACKAGE_ID: &str = "lenso.agent.workspace-read";
/// Exact linked package version.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Stable Tool name contributed by this Module.
pub const READ_TEXT_TOOL: &str = "workspace.read_text";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceConfig {
    root: PathBuf,
    max_output_bytes: usize,
}

/// Native factory for a workspace-rooted read-only Tool Provider.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceReadFactory;

impl NativeModuleFactory for WorkspaceReadFactory {
    fn package_id(&self) -> &'static str {
        PACKAGE_ID
    }

    fn package_version(&self) -> &'static str {
        PACKAGE_VERSION
    }

    fn instantiate(
        &self,
        context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        if context.entrypoint() != "default" {
            return Err(invalid_plan("unsupported workspace-read entrypoint"));
        }
        let config =
            serde_json::from_str::<WorkspaceConfig>(context.configuration()).map_err(|error| {
                invalid_plan(format!("invalid workspace-read configuration: {error}"))
            })?;
        if config.max_output_bytes == 0 || config.max_output_bytes > 1_048_576 {
            return Err(invalid_plan(
                "workspace-read max_output_bytes must be between 1 and 1048576",
            ));
        }
        let provider = WorkspaceProvider { config };
        let endpoint =
            Rc::new(ToolProviderEndpoint::new(provider.clone())) as Rc<dyn NativeRequestEndpoint>;
        Ok(NativeModuleInstance::with_lifecycle(
            vec![endpoint],
            WorkspaceLifecycle { provider },
        ))
    }
}

#[derive(Clone, Debug)]
struct WorkspaceProvider {
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

    fn read_text(&self, arguments_json: &str) -> Result<ExecuteResponse, WorkspaceReadFailure> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Arguments {
            path: String,
        }

        let arguments = serde_json::from_str::<Arguments>(arguments_json)
            .map_err(|_| ExecuteError::InvalidArguments)?;
        if arguments.path.is_empty() {
            return Err(ExecuteError::InvalidArguments.into());
        }
        let root = self
            .canonical_root()
            .map_err(WorkspaceReadFailure::Runtime)?;
        let requested = root.join(&arguments.path);
        let resolved = fs::canonicalize(requested).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => ExecuteError::NotFound,
            _ => ExecuteError::PermissionDenied,
        })?;
        if !resolved.starts_with(&root) || !resolved.is_file() {
            return Err(ExecuteError::PermissionDenied.into());
        }
        let metadata = fs::metadata(&resolved).map_err(|_| ExecuteError::NotFound)?;
        if metadata.len() > self.config.max_output_bytes as u64 {
            return Err(ExecuteError::OutputLimitExceeded.into());
        }
        let content = fs::read_to_string(&resolved).map_err(|_| {
            WorkspaceReadFailure::Domain(ExecuteError::ExecutionFailed {
                payload: lenso_capability_agent_tool_provider::ExecuteErrorExecutionFailedPayload {
                    reason_code: "not_utf8".to_owned(),
                    message: "workspace file is not valid UTF-8".to_owned(),
                    details_json: "{}".to_owned(),
                },
            })
        })?;
        Ok(ExecuteResponse {
            content,
            content_type: ExecuteResponseContentType::Text,
            metadata_json: serde_json::json!({ "path": arguments.path }).to_string(),
        })
    }
}

impl ToolProviderProvider for WorkspaceProvider {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> LocalBoxFuture<'static, Result<Result<CatalogResponse, CatalogError>, RuntimeFailure>>
    {
        Box::pin(ready(Ok(Ok(CatalogResponse {
            tools: vec![CatalogResponseToolsItem {
                name: READ_TEXT_TOOL.to_owned(),
                description: "Read one UTF-8 text file below the selected workspace root."
                    .to_owned(),
                input_schema_json: r#"{"additionalProperties":false,"properties":{"path":{"minLength":1,"type":"string"}},"required":["path"],"type":"object"}"#.to_owned(),
            }],
        }))))
    }

    fn execute(
        &self,
        _context: InvocationContext,
        request: ExecuteRequest,
    ) -> LocalBoxFuture<'static, Result<Result<ExecuteResponse, ExecuteError>, RuntimeFailure>>
    {
        let result = if request.name == READ_TEXT_TOOL {
            self.read_text(&request.arguments_json)
        } else {
            Err(ExecuteError::NotFound.into())
        };
        let result = match result {
            Ok(response) => Ok(Ok(response)),
            Err(WorkspaceReadFailure::Domain(error)) => Ok(Err(error)),
            Err(WorkspaceReadFailure::Runtime(error)) => Err(error),
        };
        Box::pin(ready(result))
    }
}

#[derive(Debug)]
struct WorkspaceLifecycle {
    provider: WorkspaceProvider,
}

impl ModuleLifecycle for WorkspaceLifecycle {
    fn prepare(&self, _context: PrepareContext) -> ModuleFuture {
        Box::pin(ready(self.provider.canonical_root().map(|_| ())))
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

    #[test]
    fn reads_utf8_below_root_and_rejects_escape() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("README.md"), "# Fixture\n").unwrap();
        fs::write(temporary.path().join("secret.txt"), "secret").unwrap();
        let provider = WorkspaceProvider {
            config: WorkspaceConfig {
                root: workspace,
                max_output_bytes: 1024,
            },
        };
        let response = provider.read_text(r#"{"path":"README.md"}"#).unwrap();
        assert_eq!(response.content, "# Fixture\n");
        assert!(matches!(
            provider.read_text(r#"{"path":"../secret.txt"}"#),
            Err(WorkspaceReadFailure::Domain(ExecuteError::PermissionDenied))
        ));
    }

    #[test]
    fn rejects_malformed_arguments_and_oversized_output() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("large.txt"), "oversized").unwrap();
        let provider = WorkspaceProvider {
            config: WorkspaceConfig {
                root: temporary.path().to_path_buf(),
                max_output_bytes: 4,
            },
        };
        assert!(matches!(
            provider.read_text(r#"{"unknown":true}"#),
            Err(WorkspaceReadFailure::Domain(ExecuteError::InvalidArguments))
        ));
        assert!(matches!(
            provider.read_text(r#"{"path":"large.txt"}"#),
            Err(WorkspaceReadFailure::Domain(
                ExecuteError::OutputLimitExceeded
            ))
        ));
    }

    #[test]
    fn workspace_loss_remains_a_runtime_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let provider = WorkspaceProvider {
            config: WorkspaceConfig {
                root: root.clone(),
                max_output_bytes: 1024,
            },
        };
        fs::remove_dir(&root).unwrap();
        assert!(matches!(
            provider.read_text(r#"{"path":"README.md"}"#),
            Err(WorkspaceReadFailure::Runtime(_))
        ));
    }
}

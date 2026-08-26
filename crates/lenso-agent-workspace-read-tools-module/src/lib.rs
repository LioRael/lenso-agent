//! Narrow Tools Capability projection over one reviewed `WorkspaceRead` provider.

use futures::future::ready;
use lenso::prelude::*;
use lenso_capability_agent_tool_hook as hook_contract;
use lenso_capability_agent_tools::{
    self as tools_contract, CatalogRequest, CatalogResponse, CatalogResponseToolsItem,
    CatalogResponseToolsItemExecution, ExecuteError, ExecuteErrorToolErrorPayload, ExecuteRequest,
    ExecuteResponse, ExecuteResponseContentType, ToolsProvider,
};
use lenso_capability_agent_workspace_read::{
    self as workspace_read_contract, ReadTextError, ReadTextRequest, WorkspaceReadInvocationError,
};
use lenso_kernel::InvocationContext;

const READ_TEXT_TOOL: &str = "read_text";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadTextArguments {
    path: String,
}

#[lenso::module]
#[derive(Clone, Debug)]
struct WorkspaceReadToolsModule {
    hooks: ManyPort<hook_contract::ToolHookClient>,
    workspace: Port<workspace_read_contract::WorkspaceReadClient>,
}

#[lenso::provides(tools_contract::Tools)]
impl ToolsProvider for WorkspaceReadToolsModule {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<tools_contract::ToolsCatalog> {
        Box::pin(ready(Ok(Ok(CatalogResponse {
            tools: vec![CatalogResponseToolsItem {
                name: READ_TEXT_TOOL.to_owned(),
                description: "Read one UTF-8 file below the reviewed workspace root without following symlinks.".to_owned(),
                input_schema_json: serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "path": { "type": "string", "minLength": 1, "maxLength": 4096 }
                    },
                    "required": ["path"]
                })
                .to_string()
                .try_into()
                .expect("workspace read Tool schema must be valid JSON"),
                execution: CatalogResponseToolsItemExecution::ParallelSafe,
            }],
        }))))
    }

    fn execute(
        &self,
        context: InvocationContext,
        request: ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<tools_contract::ToolsExecute> {
        if request.name != READ_TEXT_TOOL {
            return Box::pin(ready(Ok(Err(ExecuteError::UnknownTool))));
        }
        let Ok(arguments_json) =
            hook_contract::normalize_arguments(request.arguments_json.as_str())
        else {
            return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
        };
        let Ok(arguments) = serde_json::from_str::<ReadTextArguments>(&arguments_json) else {
            return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
        };
        if arguments.path.is_empty() || arguments.path.len() > 4096 {
            return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
        }
        let workspace = self.workspace.clone();
        let hooks = self.hooks.clone();
        Box::pin(async move {
            let execution =
                hook_contract::start_hooks(&hooks, &context, request.name, arguments_json).await?;
            if let Some(block) = &execution.block {
                hook_contract::finish_hooks(
                    &hooks,
                    &context,
                    &execution,
                    hook_contract::HookTerminal::DomainError,
                    "",
                    "{}",
                    block.provider_code,
                )
                .await?;
                return Ok(Err(tool_error(
                    block.provider_code,
                    &block.message,
                    &block.details_json,
                )));
            }
            let result = workspace
                .read_text_with_context(
                    context.clone(),
                    ReadTextRequest {
                        path: arguments.path,
                    },
                )
                .await;
            match result {
                Ok(response) => {
                    hook_contract::finish_hooks(
                        &hooks,
                        &context,
                        &execution,
                        hook_contract::HookTerminal::Success,
                        &response.content,
                        response.metadata_json.as_str(),
                        "",
                    )
                    .await?;
                    Ok(Ok(ExecuteResponse {
                        content: response.content,
                        content_type: ExecuteResponseContentType::Text,
                        metadata_json: response.metadata_json,
                    }))
                }
                Err(WorkspaceReadInvocationError::Domain(error)) => {
                    let error = map_workspace_error(error);
                    let provider_code = match &error {
                        ExecuteError::ToolError { payload } => payload.provider_code.as_str(),
                        _ => "workspace_read_error",
                    };
                    hook_contract::finish_hooks(
                        &hooks,
                        &context,
                        &execution,
                        hook_contract::HookTerminal::DomainError,
                        "",
                        "{}",
                        provider_code,
                    )
                    .await?;
                    Ok(Err(error))
                }
                Err(WorkspaceReadInvocationError::Runtime(error)) => {
                    hook_contract::finish_hooks(
                        &hooks,
                        &context,
                        &execution,
                        hook_contract::HookTerminal::RuntimeFailure,
                        "",
                        "{}",
                        "runtime_failure",
                    )
                    .await?;
                    Err(error)
                }
            }
        })
    }
}

fn tool_error(provider_code: &str, message: &str, details_json: &str) -> ExecuteError {
    ExecuteError::ToolError {
        payload: ExecuteErrorToolErrorPayload {
            provider_code: provider_code.to_owned(),
            message: message.to_owned(),
            details_json: details_json
                .to_owned()
                .try_into()
                .expect("Hook details must be JSON"),
        },
    }
}

fn map_workspace_error(error: ReadTextError) -> ExecuteError {
    let (provider_code, message, details_json) = match error {
        ReadTextError::ExecutionFailed { payload } => {
            (payload.reason_code, payload.message, payload.details_json)
        }
        ReadTextError::InvalidArguments => (
            "invalid_arguments".to_owned(),
            "Workspace read arguments are invalid".to_owned(),
            static_details(),
        ),
        ReadTextError::NotFound => (
            "not_found".to_owned(),
            "Workspace file was not found".to_owned(),
            static_details(),
        ),
        ReadTextError::OutputLimitExceeded => (
            "output_limit_exceeded".to_owned(),
            "Workspace file exceeds the output limit".to_owned(),
            static_details(),
        ),
        ReadTextError::PermissionDenied => (
            "permission_denied".to_owned(),
            "Workspace path is not admitted".to_owned(),
            static_details(),
        ),
        ReadTextError::Unknown(unknown) => (
            unknown.code,
            "Workspace provider returned an unknown error".to_owned(),
            unknown.payload.map_or_else(static_details, |value| {
                value
                    .to_string()
                    .try_into()
                    .expect("JSON value must remain valid")
            }),
        ),
    };
    ExecuteError::ToolError {
        payload: ExecuteErrorToolErrorPayload {
            provider_code,
            message,
            details_json,
        },
    }
}

fn static_details() -> tools_contract::RawJson {
    "{}".to_owned()
        .try_into()
        .expect("static details must be valid JSON")
}

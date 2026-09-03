//! Removable Console Agent Tool Provider backed by Host-routed App Agents.

use lenso::{PluginError, Port};
use lenso_capability_agent_tool_provider as provider_contract;
use lenso_capability_agent_tool_target as target_contract;

#[lenso::plugin]
#[derive(Clone, Debug)]
struct ConsoleAppTools {
    target: Port<target_contract::ToolTargetClient>,
}

#[lenso::provides(provider_contract::ToolProvider)]
impl ConsoleAppTools {
    async fn catalog(
        &self,
        _context: lenso::Ctx,
        _request: provider_contract::CatalogRequest,
    ) -> lenso::PluginResult<provider_contract::CatalogResponse, provider_contract::CatalogError>
    {
        let response = self
            .target
            .catalog(target_contract::CatalogRequest {})
            .await
            .map_err(map_catalog_invocation_error)?;
        Ok(provider_contract::CatalogResponse {
            tools: response
                .tools
                .into_iter()
                .map(|tool| provider_contract::ToolDefinition {
                    description: tool.description,
                    execution: match tool.execution {
                        target_contract::ToolExecutionClass::ParallelSafe => {
                            provider_contract::ToolExecutionClass::ParallelSafe
                        }
                        target_contract::ToolExecutionClass::Exclusive => {
                            provider_contract::ToolExecutionClass::Exclusive
                        }
                    },
                    input_schema_json: tool.input_schema_json,
                    name: tool.name,
                })
                .collect(),
        })
    }

    async fn execute(
        &self,
        _context: lenso::Ctx,
        request: provider_contract::ExecuteRequest,
    ) -> lenso::PluginResult<provider_contract::ExecuteResponse, provider_contract::ExecuteError>
    {
        let response = self
            .target
            .execute(target_contract::ExecuteRequest {
                arguments_json: request.arguments_json,
                name: request.name,
            })
            .await
            .map_err(map_execute_invocation_error)?;
        serde_json::from_str(response.response_json.as_str()).map_err(|error| {
            PluginError::runtime(lenso_kernel::RuntimeFailure::PluginFailure {
                detail: format!("App Agent Tool target returned an invalid response: {error}"),
            })
        })
    }
}

fn map_catalog_invocation_error(
    error: target_contract::ToolTargetCatalogInvocationError,
) -> PluginError<provider_contract::CatalogError> {
    match error {
        target_contract::ToolTargetCatalogInvocationError::Domain(_) => {
            PluginError::domain(provider_contract::CatalogError::CatalogInvalid)
        }
        target_contract::ToolTargetCatalogInvocationError::Runtime(error) => {
            PluginError::runtime(error)
        }
    }
}

fn map_execute_invocation_error(
    error: target_contract::ToolTargetExecuteInvocationError,
) -> PluginError<provider_contract::ExecuteError> {
    match error {
        target_contract::ToolTargetExecuteInvocationError::Runtime(error) => {
            PluginError::runtime(error)
        }
        target_contract::ToolTargetExecuteInvocationError::Domain(error) => {
            PluginError::domain(match error {
                target_contract::ExecuteError::InvalidRequest => {
                    provider_contract::ExecuteError::InvalidArguments
                }
                target_contract::ExecuteError::PermissionDenied => {
                    provider_contract::ExecuteError::PermissionDenied
                }
                target_contract::ExecuteError::TargetNotFound
                | target_contract::ExecuteError::ToolNotFound => {
                    provider_contract::ExecuteError::NotFound
                }
                target_contract::ExecuteError::ExecutionFailed { payload } => {
                    execution_failed(payload.reason_code, payload.message, payload.details_json)
                }
                target_contract::ExecuteError::StaleCatalog => execution_failed(
                    "stale_catalog",
                    "The target App Agent changed after this Console Generation was resolved",
                    "null".parse().expect("null is portable JSON"),
                ),
                target_contract::ExecuteError::Unknown(error) => execution_failed(
                    error.code,
                    "The App Agent Tool target returned an unknown error",
                    serde_json::to_string(&error.payload)
                        .unwrap_or_else(|_| "null".to_owned())
                        .parse()
                        .unwrap_or_else(|_| "null".parse().expect("null is portable JSON")),
                ),
            })
        }
    }
}

fn execution_failed(
    reason_code: impl Into<String>,
    message: impl Into<String>,
    details_json: provider_contract::RawJson,
) -> provider_contract::ExecuteError {
    provider_contract::ExecuteError::ExecutionFailed {
        payload: provider_contract::ExecutionFailedPayload {
            details_json,
            message: message.into(),
            reason_code: reason_code.into(),
        },
    }
}

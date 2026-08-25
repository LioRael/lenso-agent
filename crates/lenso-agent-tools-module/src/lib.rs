//! Aggregate Tool Runtime Module.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use lenso::prelude::*;
use lenso_capability_agent_tool_provider as provider_contract;
use lenso_capability_agent_tools::{
    self as tools_contract, CatalogRequest, CatalogResponse, CatalogResponseToolsItem,
    ExecuteError, ExecuteErrorToolErrorPayload, ExecuteRequest, ExecuteResponse,
    ExecuteResponseContentType, ToolsCatalog, ToolsExecute, ToolsProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};

#[lenso::module(lifecycle)]
#[derive(Clone, Debug)]
struct ToolsModule {
    providers: ManyPort<provider_contract::ToolProviderClient>,
    state: Rc<RefCell<Option<ToolRuntimeState>>>,
}

#[derive(Debug)]
struct ToolRuntimeState {
    catalog: Vec<CatalogResponseToolsItem>,
    routes: BTreeMap<String, usize>,
}

#[lenso::provides(tools_contract::Tools)]
impl ToolsProvider for ToolsModule {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<ToolsCatalog> {
        let result = self
            .state
            .borrow()
            .as_ref()
            .map(|state| CatalogResponse {
                tools: state.catalog.clone(),
            })
            .ok_or(RuntimeFailure::Unavailable {
                capability: lenso_capability_agent_tools::CAPABILITY_ID,
            });
        Box::pin(futures::future::ready(result.map(Ok)))
    }

    fn execute(
        &self,
        context: InvocationContext,
        request: ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<ToolsExecute> {
        let route = self
            .state
            .borrow()
            .as_ref()
            .and_then(|state| state.routes.get(&request.name).copied());
        let Some(index) = route else {
            return Box::pin(futures::future::ready(Ok(Err(ExecuteError::UnknownTool))));
        };
        let providers = self.providers.clone();
        Box::pin(async move {
            match providers[index]
                .execute_with_context(
                    context,
                    provider_contract::ExecuteRequest {
                        name: request.name,
                        arguments_json: request.arguments_json,
                    },
                )
                .await
            {
                Ok(response) => Ok(Ok(convert_execute_response(response))),
                Err(provider_contract::ToolProviderExecuteInvocationError::Domain(error)) => {
                    Ok(Err(convert_execute_error(error)))
                }
                Err(provider_contract::ToolProviderExecuteInvocationError::Runtime(error)) => {
                    Err(error)
                }
            }
        })
    }
}

impl Lifecycle for ToolsModule {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        let mut catalog = Vec::new();
        let mut routes = BTreeMap::new();
        for (index, provider) in self.providers.iter().enumerate() {
            let response = provider
                .catalog(provider_contract::CatalogRequest {})
                .await
                .map_err(|error| match error {
                    provider_contract::ToolProviderCatalogInvocationError::Domain(_) => {
                        RuntimeFailure::ModuleFailure {
                            detail: format!("Tool Provider {index} returned an invalid catalog"),
                        }
                    }
                    provider_contract::ToolProviderCatalogInvocationError::Runtime(error) => error,
                })?;
            for tool in response.tools {
                if routes.insert(tool.name.clone(), index).is_some() {
                    return Err(RuntimeFailure::InvalidResolvedPlan {
                        detail: format!("duplicate Tool name `{}`", tool.name),
                    });
                }
                catalog.push(CatalogResponseToolsItem {
                    name: tool.name,
                    description: tool.description,
                    input_schema_json: tool.input_schema_json,
                });
            }
        }
        catalog.sort_by(|left, right| left.name.cmp(&right.name));
        self.state
            .replace(Some(ToolRuntimeState { catalog, routes }));
        Ok(())
    }
}

fn convert_execute_response(response: provider_contract::ExecuteResponse) -> ExecuteResponse {
    ExecuteResponse {
        content: response.content,
        content_type: match response.content_type {
            provider_contract::ExecuteResponseContentType::Text => ExecuteResponseContentType::Text,
        },
        metadata_json: response.metadata_json,
    }
}

fn convert_execute_error(error: provider_contract::ExecuteError) -> ExecuteError {
    use provider_contract::ExecuteError as ProviderError;
    match error {
        ProviderError::InvalidArguments => ExecuteError::InvalidArguments,
        ProviderError::NotFound => tool_error("not_found", "Tool resource was not found", "{}"),
        ProviderError::OutputLimitExceeded => {
            tool_error("output_limit_exceeded", "Tool output limit exceeded", "{}")
        }
        ProviderError::PermissionDenied => {
            tool_error("permission_denied", "Tool permission denied", "{}")
        }
        ProviderError::ExecutionFailed { payload } => tool_error(
            &payload.reason_code,
            &payload.message,
            &payload.details_json,
        ),
        ProviderError::Unknown(unknown) => tool_error(
            &unknown.code,
            "Tool Provider returned an unknown Domain Error",
            &unknown
                .payload
                .map_or_else(|| "{}".to_owned(), |value| value.to_string()),
        ),
    }
}

fn tool_error(code: &str, message: &str, details_json: &str) -> ExecuteError {
    ExecuteError::ToolError {
        payload: ExecuteErrorToolErrorPayload {
            provider_code: code.to_owned(),
            message: message.to_owned(),
            details_json: details_json.to_owned(),
        },
    }
}

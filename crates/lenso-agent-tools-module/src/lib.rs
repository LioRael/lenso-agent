//! Aggregate Tool Runtime Module.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use lenso_capability_agent_tool_provider as provider_contract;
use lenso_capability_agent_tools::{
    CatalogRequest, CatalogResponse, CatalogResponseToolsItem, ExecuteError,
    ExecuteErrorToolErrorPayload, ExecuteRequest, ExecuteResponse, ExecuteResponseContentType,
    ToolsCatalog, ToolsEndpoint, ToolsExecute, ToolsProvider,
};
use lenso_kernel::{
    ActivateContext, InvocationContext, ModuleFuture, ModuleLifecycle, NativeRequestEndpoint,
    NativeRequestHandle, RuntimeFailure,
};
use lenso_native_adapter::{NativeModuleFactoryContext, NativeModuleInstance};

/// Instantiates deterministic Tool aggregation and dispatch.
#[lenso_native_adapter::module(
    descriptor = r#"{"provided_capabilities":[{"capability_id":"lenso.agent.tools@1","descriptor_version":"1.0.0","operations":["catalog","execute"],"operation_kinds":{},"default_admission":{"queue_capacity":4,"max_concurrency":1},"operation_admissions":{},"event_admission":null,"cross_lane_transfer":false}],"required_capabilities":[{"capability_id":"lenso.agent.tool-provider@1","descriptor_version":"1.0.0","cardinality":"many"}]}"#
)]
fn instantiate(
    context: NativeModuleFactoryContext<'_>,
) -> Result<NativeModuleInstance, RuntimeFailure> {
    if context.entrypoint() != "default" || context.configuration() != "{}" {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "Tools Module requires entrypoint `default` and empty configuration".to_owned(),
        });
    }
    let state = Rc::new(RefCell::new(None));
    let endpoint = Rc::new(ToolsEndpoint::new(AggregateTools {
        state: state.clone(),
    })) as Rc<dyn NativeRequestEndpoint>;
    Ok(NativeModuleInstance::with_lifecycle(
        vec![endpoint],
        ToolsLifecycle { state },
    ))
}

#[derive(Debug)]
struct ToolRuntimeState {
    catalog: Vec<CatalogResponseToolsItem>,
    routes: BTreeMap<String, usize>,
    execute_handles: Vec<Rc<NativeRequestHandle<provider_contract::ToolProviderExecute>>>,
}

#[derive(Clone, Debug)]
struct AggregateTools {
    state: Rc<RefCell<Option<ToolRuntimeState>>>,
}

impl ToolsProvider for AggregateTools {
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
        let route = self.state.borrow().as_ref().and_then(|state| {
            state
                .routes
                .get(&request.name)
                .and_then(|index| state.execute_handles.get(*index))
                .cloned()
        });
        let Some(handle) = route else {
            return Box::pin(futures::future::ready(Ok(Err(ExecuteError::UnknownTool))));
        };
        Box::pin(async move {
            let result = handle
                .invoke_with_context(
                    provider_contract::EXECUTE_OPERATION,
                    context,
                    provider_contract::ExecuteRequest {
                        name: request.name,
                        arguments_json: request.arguments_json,
                    },
                )
                .await?;
            Ok(result
                .map(convert_execute_response)
                .map_err(convert_execute_error))
        })
    }
}

#[derive(Debug)]
struct ToolsLifecycle {
    state: Rc<RefCell<Option<ToolRuntimeState>>>,
}

impl ModuleLifecycle for ToolsLifecycle {
    fn activate(&self, context: ActivateContext) -> ModuleFuture {
        let state = self.state.clone();
        let catalog_handles = match context
            .dependencies()
            .many::<provider_contract::ToolProviderCatalog>()
        {
            Ok(handles) => handles,
            Err(error) => return Box::pin(futures::future::ready(Err(error))),
        };
        let execute_handles = match context
            .dependencies()
            .many::<provider_contract::ToolProviderExecute>()
        {
            Ok(handles) => handles,
            Err(error) => return Box::pin(futures::future::ready(Err(error))),
        };
        Box::pin(async move {
            if catalog_handles.len() != execute_handles.len() {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: "Tool Provider catalog/execute bindings are inconsistent".to_owned(),
                });
            }
            let mut catalog = Vec::new();
            let mut routes = BTreeMap::new();
            for (index, handle) in catalog_handles.iter().enumerate() {
                let response = handle
                    .invoke(
                        provider_contract::CATALOG_OPERATION,
                        provider_contract::CatalogRequest {},
                    )
                    .await?
                    .map_err(|_| RuntimeFailure::ModuleFailure {
                        detail: format!("Tool Provider {index} returned an invalid catalog"),
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
            state.replace(Some(ToolRuntimeState {
                catalog,
                routes,
                execute_handles: execute_handles.into_iter().map(Rc::new).collect(),
            }));
            Ok(())
        })
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

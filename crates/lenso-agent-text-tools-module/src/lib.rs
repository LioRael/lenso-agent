//! Removable, stateless text Tool Provider Plugin Module.

use std::rc::Rc;

use futures::future::{LocalBoxFuture, ready};
use lenso_capability_agent_tool_provider::{
    CatalogError, CatalogRequest, CatalogResponse, CatalogResponseToolsItem, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecuteResponseContentType, ToolProviderEndpoint,
    ToolProviderProvider,
};
use lenso_kernel::{InvocationContext, NativeRequestEndpoint, RuntimeFailure};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance};

/// Runtime package identity selected by the Plugin contribution.
pub const PACKAGE_ID: &str = "lenso.agent.text-tools";
/// Exact linked package version.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Exact Host Build identity referenced by reviewed Plugin Manifests.
pub const FACTORY_IDENTITY: &str = "lenso.agent.text-tools@0.1.0";
/// Stable Tool name exposed only while the Plugin is active.
pub const UPPERCASE_TOOL: &str = "text.uppercase";

const MAX_TEXT_BYTES: usize = 4_096;

/// Native factory linked into the Host but activated only by Plugin Composition.
#[derive(Clone, Debug, Default)]
pub struct TextToolsFactory;

impl NativeModuleFactory for TextToolsFactory {
    fn package_id(&self) -> &'static str {
        PACKAGE_ID
    }

    fn package_version(&self) -> &'static str {
        PACKAGE_VERSION
    }

    fn factory_identity(&self) -> String {
        FACTORY_IDENTITY.to_owned()
    }

    fn instantiate(
        &self,
        context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        if context.entrypoint() != "default" || context.configuration() != "{}" {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: "text-tools requires the default entrypoint and empty configuration"
                    .to_owned(),
            });
        }
        let endpoint =
            Rc::new(ToolProviderEndpoint::new(TextToolsProvider)) as Rc<dyn NativeRequestEndpoint>;
        Ok(NativeModuleInstance::new(vec![endpoint]))
    }
}

#[derive(Clone, Debug)]
struct TextToolsProvider;

impl ToolProviderProvider for TextToolsProvider {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> LocalBoxFuture<'static, Result<Result<CatalogResponse, CatalogError>, RuntimeFailure>>
    {
        Box::pin(ready(Ok(Ok(CatalogResponse {
            tools: vec![CatalogResponseToolsItem {
                name: UPPERCASE_TOOL.to_owned(),
                description: "Convert one bounded UTF-8 string to uppercase.".to_owned(),
                input_schema_json: r#"{"additionalProperties":false,"properties":{"text":{"maxLength":4096,"type":"string"}},"required":["text"],"type":"object"}"#.to_owned(),
            }],
        }))))
    }

    fn execute(
        &self,
        _context: InvocationContext,
        request: ExecuteRequest,
    ) -> LocalBoxFuture<'static, Result<Result<ExecuteResponse, ExecuteError>, RuntimeFailure>>
    {
        let result = execute_now(&request);
        Box::pin(ready(Ok(result)))
    }
}

fn execute_now(request: &ExecuteRequest) -> Result<ExecuteResponse, ExecuteError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Arguments {
        text: String,
    }
    if request.name != UPPERCASE_TOOL {
        return Err(ExecuteError::NotFound);
    }
    let arguments = serde_json::from_str::<Arguments>(&request.arguments_json)
        .map_err(|_| ExecuteError::InvalidArguments)?;
    if arguments.text.len() > MAX_TEXT_BYTES {
        return Err(ExecuteError::OutputLimitExceeded);
    }
    let content = arguments.text.to_uppercase();
    if content.len() > MAX_TEXT_BYTES {
        return Err(ExecuteError::OutputLimitExceeded);
    }
    Ok(ExecuteResponse {
        content,
        content_type: ExecuteResponseContentType::Text,
        metadata_json: r#"{"operation":"uppercase"}"#.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uppercase_is_bounded_and_rejects_unknown_tools_or_arguments() {
        let response = execute_now(&ExecuteRequest {
            name: UPPERCASE_TOOL.to_owned(),
            arguments_json: r#"{"text":"Lenso plugin"}"#.to_owned(),
        })
        .unwrap();
        assert_eq!(response.content, "LENSO PLUGIN");
        assert!(matches!(
            execute_now(&ExecuteRequest {
                name: "text.unknown".to_owned(),
                arguments_json: "{}".to_owned(),
            }),
            Err(ExecuteError::NotFound)
        ));
        assert!(matches!(
            execute_now(&ExecuteRequest {
                name: UPPERCASE_TOOL.to_owned(),
                arguments_json: r#"{"extra":true,"text":"x"}"#.to_owned(),
            }),
            Err(ExecuteError::InvalidArguments)
        ));
    }
}

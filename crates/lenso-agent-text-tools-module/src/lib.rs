//! Removable, stateless text Tool Provider Plugin Module.

use futures::future::ready;
use lenso_capability_agent_tool_provider::{
    self as tool_provider_contract, CatalogRequest, CatalogResponse, CatalogResponseToolsItem,
    ExecuteError, ExecuteRequest, ExecuteResponse, ExecuteResponseContentType,
    ToolProviderProvider,
};
use lenso_kernel::InvocationContext;
use schemars::JsonSchema;

/// Stable Tool name exposed only while the Plugin is active.
pub const UPPERCASE_TOOL: &str = "uppercase";

const MAX_TEXT_BYTES: usize = 4_096;

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UppercaseArguments {
    #[schemars(length(max = 4096))]
    text: String,
}

fn uppercase(arguments: UppercaseArguments) -> Result<ExecuteResponse, ExecuteError> {
    if arguments.text.len() > MAX_TEXT_BYTES {
        return Err(ExecuteError::OutputLimitExceeded);
    }
    let UppercaseArguments { text } = arguments;
    let content = text.to_uppercase();
    if content.len() > MAX_TEXT_BYTES {
        return Err(ExecuteError::OutputLimitExceeded);
    }
    Ok(ExecuteResponse {
        content,
        content_type: ExecuteResponseContentType::Text,
        metadata_json: r#"{"operation":"uppercase"}"#.to_owned(),
    })
}

#[lenso::module]
#[derive(Clone, Copy, Debug)]
struct TextTools {}

#[lenso::provides(tool_provider_contract::ToolProvider)]
impl ToolProviderProvider for TextTools {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_provider_contract::ToolProviderCatalog> {
        static INPUT_SCHEMA: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let input_schema_json = INPUT_SCHEMA
            .get_or_init(|| {
                serde_json::to_string(&schemars::schema_for!(UppercaseArguments))
                    .expect("derived Tool input Schema must serialize")
            })
            .clone();
        Box::pin(ready(Ok(Ok(CatalogResponse {
            tools: vec![CatalogResponseToolsItem {
                name: UPPERCASE_TOOL.to_owned(),
                description: "Convert one bounded UTF-8 string to uppercase.".to_owned(),
                input_schema_json,
            }],
        }))))
    }

    fn execute(
        &self,
        _context: InvocationContext,
        request: ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_provider_contract::ToolProviderExecute> {
        Box::pin(ready(if request.name == UPPERCASE_TOOL {
            let Ok(arguments) = serde_json::from_str(&request.arguments_json) else {
                return Box::pin(ready(Ok(Err(ExecuteError::InvalidArguments))));
            };
            Ok(uppercase(arguments))
        } else {
            Ok(Err(ExecuteError::NotFound))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_kernel::CancellationToken;

    #[test]
    fn uppercase_is_bounded() {
        let response = uppercase(UppercaseArguments {
            text: "Lenso plugin".to_owned(),
        })
        .unwrap();
        assert_eq!(response.content, "LENSO PLUGIN");

        let oversized = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(matches!(
            uppercase(UppercaseArguments { text: oversized }),
            Err(ExecuteError::OutputLimitExceeded)
        ));
    }

    #[test]
    fn generated_provider_derives_schema_and_dispatches_safely() {
        let context = || InvocationContext::new(1, None, CancellationToken::new());
        let catalog = futures::executor::block_on(ToolProviderProvider::catalog(
            &TextTools {},
            context(),
            CatalogRequest {},
        ))
        .unwrap()
        .unwrap();
        assert_eq!(catalog.tools.len(), 1);
        let schema: serde_json::Value =
            serde_json::from_str(&catalog.tools[0].input_schema_json).unwrap();
        assert_eq!(schema["properties"]["text"]["maxLength"], 4096);
        assert_eq!(schema["additionalProperties"], false);

        let unknown = futures::executor::block_on(ToolProviderProvider::execute(
            &TextTools {},
            context(),
            ExecuteRequest {
                name: "missing_tool".to_owned(),
                arguments_json: "{}".to_owned(),
            },
        ))
        .unwrap();
        assert!(matches!(unknown, Err(ExecuteError::NotFound)));

        let invalid = futures::executor::block_on(ToolProviderProvider::execute(
            &TextTools {},
            context(),
            ExecuteRequest {
                name: UPPERCASE_TOOL.to_owned(),
                arguments_json: r#"{"extra":true,"text":"x"}"#.to_owned(),
            },
        ))
        .unwrap();
        assert!(matches!(invalid, Err(ExecuteError::InvalidArguments)));
    }

    #[test]
    fn generated_module_descriptor_is_package_owned_and_complete() {
        let descriptor: serde_json::Value =
            serde_json::from_str(MODULE_DESCRIPTOR_JSON).expect("descriptor must be valid JSON");
        assert_eq!(descriptor["package_id"], "lenso.agent.text-tools");
        assert_eq!(descriptor["package_revision"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            descriptor["provided_capabilities"][0]["capability_id"],
            "lenso.agent.tool-provider@1"
        );
        assert_eq!(
            descriptor["provided_capabilities"][0]["operations"],
            serde_json::json!(["catalog", "execute"])
        );
        assert_eq!(descriptor["required_capabilities"], serde_json::json!([]));
    }
}

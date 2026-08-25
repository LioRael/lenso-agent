//! Removable, stateless text Tool Provider Plugin Module.

use lenso_agent_tool_sdk::prelude::*;
use schemars::JsonSchema;

const MAX_TEXT_BYTES: usize = 4_096;

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UppercaseArguments {
    #[schemars(length(max = 4096))]
    text: String,
}

#[lenso::module]
#[derive(Clone, Copy, Debug)]
struct TextTools {}

#[lenso_agent_tool_sdk::tool_provider]
impl TextTools {
    #[tool(
        name = "uppercase",
        description = "Convert one bounded UTF-8 string to uppercase."
    )]
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
            content_type: ContentType::Text,
            metadata_json: r#"{"operation":"uppercase"}"#
                .try_into()
                .expect("static Tool metadata must be valid JSON"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso::{Ctx, ModuleError};
    use lenso_capability_agent_tool_provider::{CatalogRequest, ExecuteRequest};
    use lenso_kernel::CancellationToken;

    #[test]
    fn uppercase_is_bounded() {
        let response = TextTools::uppercase(UppercaseArguments {
            text: "Lenso plugin".to_owned(),
        })
        .unwrap();
        assert_eq!(response.content, "LENSO PLUGIN");

        let oversized = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(matches!(
            TextTools::uppercase(UppercaseArguments { text: oversized }),
            Err(ExecuteError::OutputLimitExceeded)
        ));
    }

    #[test]
    fn generated_provider_derives_schema_and_dispatches_safely() {
        let context = || Ctx::new(1, None, CancellationToken::new());
        let catalog =
            futures::executor::block_on(TextTools {}.catalog(context(), CatalogRequest {}))
                .unwrap();
        assert_eq!(catalog.tools.len(), 1);
        let schema: serde_json::Value =
            serde_json::from_str(catalog.tools[0].input_schema_json.as_str()).unwrap();
        assert_eq!(schema["properties"]["text"]["maxLength"], 4096);
        assert_eq!(schema["additionalProperties"], false);
        let tool_name = catalog.tools[0].name.clone();

        let valid = futures::executor::block_on(TextTools {}.execute(
            context(),
            ExecuteRequest {
                name: tool_name.clone(),
                arguments_json: r#"{"text":"Lenso plugin"}"#.try_into().unwrap(),
            },
        ))
        .unwrap();
        assert_eq!(valid.content, "LENSO PLUGIN");

        let unknown = futures::executor::block_on(TextTools {}.execute(
            context(),
            ExecuteRequest {
                name: "missing_tool".to_owned(),
                arguments_json: "{}".to_owned().try_into().unwrap(),
            },
        ));
        assert!(matches!(
            unknown,
            Err(ModuleError::Domain(ExecuteError::NotFound))
        ));

        let invalid = futures::executor::block_on(TextTools {}.execute(
            context(),
            ExecuteRequest {
                name: tool_name,
                arguments_json: r#"{"extra":true,"text":"x"}"#.to_owned().try_into().unwrap(),
            },
        ));
        assert!(matches!(
            invalid,
            Err(ModuleError::Domain(ExecuteError::InvalidArguments))
        ));
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

//! Removable, stateless text Tool Provider Plugin Module.

use lenso_agent_module as agent;

/// Stable Tool name exposed only while the Plugin is active.
pub const UPPERCASE_TOOL: &str = "text.uppercase";

const MAX_TEXT_BYTES: usize = 4_096;

#[derive(agent::JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UppercaseArguments {
    #[schemars(length(max = 4096))]
    text: String,
}

#[agent::tool(
    name = UPPERCASE_TOOL,
    description = "Convert one bounded UTF-8 string to uppercase."
)]
fn uppercase(arguments: UppercaseArguments) -> Result<agent::ToolOutput, agent::ToolError> {
    if arguments.text.len() > MAX_TEXT_BYTES {
        return Err(agent::ToolError::OutputLimitExceeded);
    }
    let UppercaseArguments { text } = arguments;
    let content = text.to_uppercase();
    if content.len() > MAX_TEXT_BYTES {
        return Err(agent::ToolError::OutputLimitExceeded);
    }
    Ok(agent::ToolOutput {
        content,
        content_type: agent::ToolOutputType::Text,
        metadata_json: r#"{"operation":"uppercase"}"#.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent::__private::{
        CancellationToken, CatalogRequest, ExecuteRequest, InvocationContext, ToolProviderProvider,
    };

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
            Err(agent::ToolError::OutputLimitExceeded)
        ));
    }

    #[test]
    fn generated_provider_derives_schema_and_dispatches_safely() {
        let context = || InvocationContext::new(1, None, CancellationToken::new());
        let catalog = futures::executor::block_on(ToolProviderProvider::catalog(
            &__LensoToolProvider_uppercase,
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
            &__LensoToolProvider_uppercase,
            context(),
            ExecuteRequest {
                name: "text.unknown".to_owned(),
                arguments_json: "{}".to_owned(),
            },
        ))
        .unwrap();
        assert!(matches!(unknown, Err(agent::ToolError::NotFound)));

        let invalid = futures::executor::block_on(ToolProviderProvider::execute(
            &__LensoToolProvider_uppercase,
            context(),
            ExecuteRequest {
                name: UPPERCASE_TOOL.to_owned(),
                arguments_json: r#"{"extra":true,"text":"x"}"#.to_owned(),
            },
        ))
        .unwrap();
        assert!(matches!(invalid, Err(agent::ToolError::InvalidArguments)));
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

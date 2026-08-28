use lenso_agent_tool_sdk::prelude::*;
use schemars::JsonSchema;

const MAX_TEXT_BYTES: usize = 4_096;

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UppercaseArguments {
    #[schemars(length(max = 4096))]
    text: String,
}

#[lenso::plugin]
#[derive(Clone, Copy, Debug, Default)]
struct TextTools {}

#[lenso_agent_tool_sdk::tool_provider]
impl TextTools {
    #[tool(
        name = "uppercase",
        description = "Convert one bounded UTF-8 string to uppercase.",
        execution = "parallel_safe"
    )]
    fn uppercase(arguments: UppercaseArguments) -> Result<ExecuteResponse, ExecuteError> {
        if arguments.text.len() > MAX_TEXT_BYTES {
            return Err(ExecuteError::OutputLimitExceeded);
        }
        let content = arguments.text.to_uppercase();
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
    use lenso::{InvocationOutcome, JsonRequestHandler};

    #[test]
    fn generated_portable_dispatcher_preserves_tool_behavior() {
        let response = TextTools::default().invoke(
            "lenso.agent.tool-provider@2",
            "execute",
            lenso::__private::serde_json::json!({
                "name": "uppercase",
                "arguments_json": r#"{"text":"Lenso plugin"}"#,
            }),
        );
        let InvocationOutcome::Success(response) = response else {
            panic!("expected successful Tool response");
        };
        assert_eq!(response["content"], "LENSO PLUGIN");

        let invalid = TextTools::default().invoke(
            "lenso.agent.tool-provider@2",
            "execute",
            lenso::__private::serde_json::json!({
                "name": "uppercase",
                "arguments_json": "{}",
            }),
        );
        assert!(matches!(
            invalid,
            InvocationOutcome::DomainError(error) if error == "invalid_arguments"
        ));
    }
}

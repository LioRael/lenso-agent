use serde::{Deserialize, Serialize};

wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

const CAPABILITY: &str = "lenso.agent.tool-provider@1";
const TOOL: &str = "uppercase";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogRequest {}

#[derive(Serialize)]
struct CatalogResponse {
    tools: Vec<ToolDefinition>,
}

#[derive(Serialize)]
struct ToolDefinition {
    name: &'static str,
    description: &'static str,
    input_schema_json: &'static str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecuteRequest {
    name: String,
    arguments_json: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UppercaseArguments {
    text: String,
}

#[derive(Serialize)]
struct ExecuteResponse {
    content_type: &'static str,
    content: String,
    metadata_json: &'static str,
}

struct ExternalTextTools;

impl Guest for ExternalTextTools {
    fn describe() -> String {
        r#"{"abi":"lenso.json-request@1","capabilities":[{"capability_id":"lenso.agent.tool-provider@1","descriptor_version":"1.0.0","request_operations":["catalog","execute"]}]}"#.to_owned()
    }

    fn invoke(
        capability: String,
        operation: String,
        request_json: String,
    ) -> Result<String, String> {
        if capability != CAPABILITY {
            return Err("\"not_found\"".to_owned());
        }
        match operation.as_str() {
            "catalog" => {
                serde_json::from_str::<CatalogRequest>(&request_json)
                    .map_err(|_| "\"catalog_invalid\"".to_owned())?;
                serde_json::to_string(&CatalogResponse {
                    tools: vec![ToolDefinition {
                        name: TOOL,
                        description: "Convert one UTF-8 string to uppercase.",
                        input_schema_json: r#"{"additionalProperties":false,"properties":{"text":{"maxLength":4096,"type":"string"}},"required":["text"],"type":"object"}"#,
                    }],
                })
                .map_err(|_| "\"catalog_invalid\"".to_owned())
            }
            "execute" => {
                let request = serde_json::from_str::<ExecuteRequest>(&request_json)
                    .map_err(|_| "\"invalid_arguments\"".to_owned())?;
                if request.name != TOOL {
                    return Err("\"not_found\"".to_owned());
                }
                let arguments = serde_json::from_str::<UppercaseArguments>(&request.arguments_json)
                    .map_err(|_| "\"invalid_arguments\"".to_owned())?;
                if arguments.text.len() > 4_096 {
                    return Err("\"output_limit_exceeded\"".to_owned());
                }
                serde_json::to_string(&ExecuteResponse {
                    content_type: "text",
                    content: arguments.text.to_uppercase(),
                    metadata_json: r#"{"provider":"external-wasm"}"#,
                })
                .map_err(|_| "\"execution_failed\"".to_owned())
            }
            _ => Err("\"not_found\"".to_owned()),
        }
    }
}

export!(ExternalTextTools);

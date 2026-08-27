use serde::{Deserialize, Serialize};

wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

const CAPABILITY: &str = "lenso.agent.tool-provider@2";
const TOOL: &str = "uppercase";

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

struct PluginComponent;

lenso_guest_sdk::guest_request_plugin! {
impl Guest for PluginComponent {
    provides: {
        capability_id: "lenso.agent.tool-provider@2",
        descriptor_version: "2.0.0",
        requests: ["catalog", "execute"],
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
            "catalog" => Ok(r#"{"tools":[{"name":"uppercase","description":"Convert one UTF-8 string to uppercase.","input_schema_json":"{\"additionalProperties\":false,\"properties\":{\"text\":{\"maxLength\":4096,\"type\":\"string\"}},\"required\":[\"text\"],\"type\":\"object\"}","execution":"parallel_safe"}]}"#.to_owned()),
            "execute" => {
                let request = serde_json::from_str::<ExecuteRequest>(&request_json)
                    .map_err(|_| "\"invalid_arguments\"".to_owned())?;
                if request.name != TOOL {
                    return Err("\"not_found\"".to_owned());
                }
                let arguments = serde_json::from_str::<UppercaseArguments>(&request.arguments_json)
                    .map_err(|_| "\"invalid_arguments\"".to_owned())?;
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
}

export!(PluginComponent);

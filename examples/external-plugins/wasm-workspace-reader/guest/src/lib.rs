use lenso_capability_agent_workspace_read::{ReadTextRequest, WorkspaceReadGuestClient};
use lenso_guest_sdk::GuestContext;
use serde::{Deserialize, Serialize};

wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

lenso_guest_sdk::wasm_host! {
    struct WasmHost {
        bindings: host_bindings,
        invoke: host_invoke,
        stream_open: host_stream_open,
        stream_send: host_stream_send,
        stream_receive: host_stream_receive,
        stream_close_send: host_stream_close_send,
        stream_cancel: host_stream_cancel,
    }
}

const CAPABILITY: &str = "lenso.agent.tool-provider@2";
const TOOL: &str = "plugin_workspace_read_text";

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
    execution: &'static str,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExecuteRequest {
    name: String,
    arguments_json: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    path: String,
}

#[derive(Deserialize, Serialize)]
struct ExecuteResponse {
    content_type: String,
    content: String,
    metadata_json: String,
}

struct ExternalWorkspaceReader;

impl Guest for ExternalWorkspaceReader {
    fn describe() -> String {
        r#"{"abi":"lenso.json-host-imports@1","capabilities":[{"capability_id":"lenso.agent.tool-provider@2","descriptor_version":"2.0.0","request_operations":["catalog","execute"]}],"required_capabilities":[{"capability_id":"lenso.agent.workspace-read@1","descriptor_version":"1.0.0","cardinality":"one"}]}"#.to_owned()
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
                        description: "Read UTF-8 text through the Host-selected read-only workspace provider.",
                        input_schema_json: r#"{"additionalProperties":false,"properties":{"path":{"minLength":1,"type":"string"}},"required":["path"],"type":"object"}"#,
                        execution: "parallel_safe",
                    }],
                })
                .map_err(|_| "\"catalog_invalid\"".to_owned())
            }
            "execute" => execute(&request_json),
            _ => Err("\"not_found\"".to_owned()),
        }
    }

    fn stream_open(_: String, _: String, _: String) -> Result<u64, String> {
        Err("unsupported Stream Operation".to_owned())
    }

    fn stream_send(_: u64, _: String) -> Result<(), String> {
        Err("unknown stream".to_owned())
    }

    fn stream_receive(_: u64) -> Result<String, String> {
        Err("unknown stream".to_owned())
    }

    fn stream_close_send(_: u64) -> Result<(), String> {
        Err("unknown stream".to_owned())
    }

    fn stream_cancel(_: u64) {}
}

fn execute(request_json: &str) -> Result<String, String> {
    let request = serde_json::from_str::<ExecuteRequest>(request_json)
        .map_err(|_| "\"invalid_arguments\"".to_owned())?;
    if request.name != TOOL {
        return Err("\"not_found\"".to_owned());
    }
    let arguments = serde_json::from_str::<ReadArguments>(&request.arguments_json)
        .map_err(|_| "\"invalid_arguments\"".to_owned())?;
    let context = GuestContext::load(WasmHost)
        .map_err(|error| format!(r#"{{"execution_failed":{{"reason_code":"host_bindings","message":"{error:?}","details_json":"{{}}"}}}}"#))?;
    let workspace = WorkspaceReadGuestClient::from_context(&context)
        .map_err(|error| format!(r#"{{"execution_failed":{{"reason_code":"host_binding","message":"{error:?}","details_json":"{{}}"}}}}"#))?;
    let response = workspace
        .read_text(&ReadTextRequest {
            path: arguments.path,
        })
        .map_err(|error| format!(r#"{{"execution_failed":{{"reason_code":"workspace_read","message":"{error:?}","details_json":"{{}}"}}}}"#))?;
    serde_json::to_string(&ExecuteResponse {
        content_type: "text".to_owned(),
        content: response.content,
        metadata_json: serde_json::json!({
            "provider": "external-wasm",
            "authority": "host-selected-workspace-read",
            "host_metadata": response.metadata_json
        })
        .to_string(),
    })
    .map_err(|_| "\"execution_failed\"".to_owned())
}

export!(ExternalWorkspaceReader);

//! Tool Provider whose schema makes post-transform validation observable.

use lenso::prelude::*;
use lenso_capability_agent_tool_provider::{
    self as tools, CatalogError, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ToolDefinition, ToolExecutionClass,
};

#[derive(Clone, Debug, Default, serde::Deserialize, lenso::PluginConfig)]
#[serde(deny_unknown_fields)]
struct SecretToolConfig {}

#[lenso::plugin]
#[derive(Clone, Debug)]
struct SecretTool {
    #[config]
    config: SecretToolConfig,
}

pub fn link() {
    link_plugin();
}

#[lenso::provides(tools::ToolProvider)]
impl SecretTool {
    async fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> PluginResult<CatalogResponse, CatalogError> {
        let _ = &self.config;
        Ok(CatalogResponse {
            tools: vec![ToolDefinition {
                name: "read_secret".to_owned(),
                description: "Fixture Tool that accepts only final approved arguments.".to_owned(),
                input_schema_json: serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id"],
                    "properties": { "id": { "const": "approved" } }
                })
                .to_string()
                .try_into()
                .expect("fixture schema is valid JSON"),
                execution: ToolExecutionClass::Exclusive,
            }],
        })
    }

    async fn execute(
        &self,
        _context: Ctx,
        request: ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        let _ = &self.config;
        if request.name != "read_secret" {
            return Err(PluginError::domain(ExecuteError::NotFound));
        }
        let arguments = serde_json::from_str::<serde_json::Value>(request.arguments_json.as_str())
            .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))?;
        if arguments != serde_json::json!({"id": "approved"}) {
            return Err(PluginError::domain(ExecuteError::InvalidArguments));
        }
        Ok(ExecuteResponse {
            content_type: ContentType::Text,
            content: "SECRET_RESULT: approved".to_owned(),
            content_blocks: None,
            metadata_json: serde_json::json!({"provider": "example.secret-tool"})
                .to_string()
                .try_into()
                .expect("fixture metadata is valid JSON"),
        })
    }
}

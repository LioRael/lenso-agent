//! A Tool Provider with a visible side effect for final-policy proof.

use std::{fs::OpenOptions, io::Write, path::Path};

use lenso::prelude::*;
use lenso_capability_agent_tool_provider::{
    self as tool_provider, CatalogError, CatalogRequest, CatalogResponse, ContentType,
    ExecuteError, ExecuteRequest, ExecuteResponse, ToolDefinition, ToolExecutionClass,
};

pub const INPUT_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["key"],"properties":{"key":{"const":"alpha"}}}"#;

#[derive(Clone, Debug, serde::Deserialize, PluginConfig)]
#[serde(deny_unknown_fields)]
struct PolicyToolConfig {
    effect_path: String,
}

#[lenso::plugin(validate = validate_config)]
#[derive(Clone, Debug)]
struct PolicyTool {
    #[config]
    config: PolicyToolConfig,
}

fn validate_config(config: &PolicyToolConfig) -> Result<(), RuntimeFailure> {
    if !Path::new(&config.effect_path).is_absolute() || config.effect_path.len() > 4_096 {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "external policy Tool requires an absolute bounded effect path".to_owned(),
        });
    }
    Ok(())
}

#[lenso::provides(tool_provider::ToolProvider)]
impl PolicyTool {
    async fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> PluginResult<CatalogResponse, CatalogError> {
        Ok(CatalogResponse {
            tools: vec![ToolDefinition {
                name: "read_policy_value".to_owned(),
                description: "Reads the fixture policy value after final authorization.".to_owned(),
                input_schema_json: INPUT_SCHEMA
                    .to_owned()
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
        if request.name != "read_policy_value"
            || request.arguments_json.as_str() != r#"{"key":"alpha"}"#
        {
            return Err(PluginError::domain(ExecuteError::InvalidArguments));
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.effect_path)
            .map_err(|error| {
                PluginError::runtime(RuntimeFailure::PluginFailure {
                    detail: format!("external policy Tool cannot open effect path: {error}"),
                })
            })?;
        writeln!(file, "executed-alpha").map_err(|error| {
            PluginError::runtime(RuntimeFailure::PluginFailure {
                detail: format!("external policy Tool cannot record effect: {error}"),
            })
        })?;
        Ok(ExecuteResponse {
            content_type: ContentType::Text,
            content: "policy value alpha".to_owned(),
            content_blocks: None,
            metadata_json: r#"{"provider":"example.policy-tool"}"#
                .try_into()
                .expect("fixture metadata is valid JSON"),
        })
    }
}

pub fn link() {
    link_plugin();
}

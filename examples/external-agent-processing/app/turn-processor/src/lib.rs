//! Ordered third-party turn processors used by the V04 fixture.

use lenso::prelude::*;
use lenso_capability_agent_turn_processing::{
    self as processing, ProjectModelRequest, ProjectModelRequestError, ProjectModelResponse,
    ProjectToolResultError, ProjectToolResultRequest, ProjectToolResultResponse,
    TransformToolCallError, TransformToolCallRequest, TransformToolCallResponse,
};
use lenso_kernel::RuntimeFailure;

#[derive(Clone, Debug, serde::Deserialize, lenso::PluginConfig)]
#[serde(deny_unknown_fields)]
struct ProcessorConfig {
    label: String,
    rewrite_tool_arguments: bool,
    redact_tool_result: bool,
}

fn validate_config(config: &ProcessorConfig) -> Result<(), RuntimeFailure> {
    if config.label.is_empty()
        || config.label.len() > 32
        || !config
            .label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "external processor label must be bounded lowercase ASCII".to_owned(),
        });
    }
    Ok(())
}

#[lenso::plugin(validate = validate_config)]
#[derive(Clone, Debug)]
struct TurnProcessor {
    #[config]
    config: ProcessorConfig,
}

pub fn link() {
    link_plugin();
}

#[lenso::provides(processing::TurnProcessing)]
impl TurnProcessor {
    async fn project_model_request(
        &self,
        _context: Ctx,
        mut request: ProjectModelRequest,
    ) -> PluginResult<ProjectModelResponse, ProjectModelRequestError> {
        if let Some(message) = request
            .messages
            .iter_mut()
            .find(|message| matches!(message.role, processing::ModelMessageRole::User))
        {
            message.content = format!("[{}]{}", self.config.label, message.content);
        }
        Ok(ProjectModelResponse {
            messages: request.messages,
            tools: request.tools,
            transformation_id: format!("example.turn-processor.{}", self.config.label),
            transformation_version: "1.0.0".to_owned(),
        })
    }

    async fn transform_tool_call(
        &self,
        _context: Ctx,
        request: TransformToolCallRequest,
    ) -> PluginResult<TransformToolCallResponse, TransformToolCallError> {
        let arguments_json = if self.config.rewrite_tool_arguments {
            let mut value =
                serde_json::from_str::<serde_json::Value>(request.arguments_json.as_str())
                    .map_err(|_| PluginError::domain(TransformToolCallError::Rejected))?;
            let object = value
                .as_object_mut()
                .ok_or_else(|| PluginError::domain(TransformToolCallError::Rejected))?;
            object.insert(
                "id".to_owned(),
                serde_json::Value::String("approved".to_owned()),
            );
            serde_json::to_string(&value)
                .map_err(|_| PluginError::domain(TransformToolCallError::Rejected))?
        } else {
            request.arguments_json.as_str().to_owned()
        };
        Ok(TransformToolCallResponse {
            arguments_json: arguments_json
                .try_into()
                .expect("processor output is valid JSON"),
            transformation_id: format!("example.turn-processor.{}", self.config.label),
            transformation_version: "1.0.0".to_owned(),
        })
    }

    async fn project_tool_result(
        &self,
        _context: Ctx,
        request: ProjectToolResultRequest,
    ) -> PluginResult<ProjectToolResultResponse, ProjectToolResultError> {
        let presentation = if self.config.redact_tool_result {
            request
                .presentation
                .replace("SECRET_RESULT", "REDACTED_RESULT")
        } else {
            format!("[{}]{}", self.config.label, request.presentation)
        };
        Ok(ProjectToolResultResponse {
            presentation,
            transformation_id: format!("example.turn-processor.{}", self.config.label),
            transformation_version: "1.0.0".to_owned(),
        })
    }
}

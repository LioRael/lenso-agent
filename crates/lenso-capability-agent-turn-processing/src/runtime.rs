//! Ordered processor-chain orchestration lives here rather than in a Host.

use lenso_capability_agent_model::{
    CompleteMessageInput, CompleteMessageRole, CompleteTool, ModelImage as ModelRequestImage,
};
use lenso_kernel::RuntimeFailure;
use lenso_plugin_authoring::ManyPort;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::{
    ModelImage, ModelMessage, ModelMessageRole, ModelTool, ProjectModelRequest,
    ProjectToolResultRequest, TransformToolCallRequest, TurnProcessingClient,
    TurnProcessingProjectModelRequestInvocationError,
    TurnProcessingProjectToolResultInvocationError, TurnProcessingTransformToolCallInvocationError,
};

const MAX_TRANSFORMATION_ID_BYTES: usize = 128;
const MAX_TRANSFORMATION_VERSION_BYTES: usize = 64;
const MAX_MODEL_MESSAGES: usize = 256;
const MAX_MODEL_TOOLS: usize = 256;
const MAX_MODEL_CONTENT_BYTES: usize = 1_048_576;
const MAX_TOOL_SCHEMA_BYTES: usize = 65_536;
const MAX_TOOL_ARGUMENT_BYTES: usize = 262_144;
const MAX_RESULT_PRESENTATION_BYTES: usize = 1_048_576;

/// The one narrow stage whose values were transformed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStage {
    ModelRequest,
    ToolCall,
    ToolResult,
}

/// Evidence for one successful deterministic processor step.
///
/// Digests deliberately cover the wire representation instead of retaining a
/// second copy of potentially sensitive prompt, Tool argument, or Tool result
/// content in the trace.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationTrace {
    pub stage: ProcessingStage,
    pub provider_instance: String,
    pub transformation_id: String,
    pub transformation_version: String,
    pub input_digest: String,
    pub output_digest: String,
}

/// The final value plus the exact resolved processor order that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedProcessing<T> {
    pub value: T,
    pub trace: Vec<TransformationTrace>,
}

/// Converts one native Model request's mutable presentation fields to the
/// portable processing contract. Model selection and execution controls stay
/// outside the returned value.
#[must_use]
pub fn model_request_from_model(
    turn_id: impl Into<String>,
    messages: &[CompleteMessageInput],
    tools: &[CompleteTool],
) -> ProjectModelRequest {
    ProjectModelRequest {
        turn_id: turn_id.into(),
        messages: messages.iter().map(model_message_from_model).collect(),
        tools: tools.iter().map(model_tool_from_model).collect(),
    }
}

/// Converts a processor result back to native Model presentation fields after
/// enforcing the same bounded portable shape at the native boundary.
pub fn model_request_to_model(
    request: &ProjectModelRequest,
) -> Result<(Vec<CompleteMessageInput>, Vec<CompleteTool>), RuntimeFailure> {
    validate_model_request(request)?;
    let messages = request
        .messages
        .iter()
        .map(model_message_to_model)
        .collect::<Result<Vec<_>, _>>()?;
    let tools = request
        .tools
        .iter()
        .map(model_tool_to_model)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((messages, tools))
}

/// Applies every selected context projector in resolved Plan order. A failure
/// stops the chain; it is never retried or converted into a partial success.
pub async fn apply_model_request_processors(
    processors: &ManyPort<TurnProcessingClient>,
    context: &lenso_kernel::InvocationContext,
    request: ProjectModelRequest,
) -> Result<AppliedProcessing<ProjectModelRequest>, RuntimeFailure> {
    let mut value = request;
    validate_model_request(&value)?;
    let mut trace = Vec::with_capacity(processors.len());
    for (index, processor) in processors.iter().enumerate() {
        ensure_active(context)?;
        let input_digest = digest(&value)?;
        let response = processor
            .project_model_request_with_context(context.clone(), value.clone())
            .await
            .map_err(|error| {
                processor_model_request_failure(index, processor.provider_instance(), error)
            })?;
        validate_transformation(
            &response.transformation_id,
            &response.transformation_version,
        )?;
        value = ProjectModelRequest {
            turn_id: value.turn_id,
            messages: response.messages,
            tools: response.tools,
        };
        validate_model_request(&value)?;
        trace.push(TransformationTrace {
            stage: ProcessingStage::ModelRequest,
            provider_instance: processor.provider_instance().to_owned(),
            transformation_id: response.transformation_id,
            transformation_version: response.transformation_version,
            input_digest,
            output_digest: digest(&value)?,
        });
    }
    Ok(AppliedProcessing { value, trace })
}

/// Applies every selected pre-authorization Tool argument transformer in
/// resolved Plan order. Each response is canonicalized before it reaches the
/// next processor. The Tool aggregate is responsible for schema validation
/// against the selected Tool definition after this function returns.
pub async fn apply_tool_call_processors(
    processors: &ManyPort<TurnProcessingClient>,
    context: &lenso_kernel::InvocationContext,
    mut request: TransformToolCallRequest,
) -> Result<AppliedProcessing<TransformToolCallRequest>, RuntimeFailure> {
    request.arguments_json = canonical_json(request.arguments_json.as_str())?
        .try_into()
        .map_err(|_| invalid_processing("Tool arguments are not portable JSON"))?;
    validate_tool_call(&request)?;
    let mut trace = Vec::with_capacity(processors.len());
    for (index, processor) in processors.iter().enumerate() {
        ensure_active(context)?;
        let input_digest = digest(&request)?;
        let response = processor
            .transform_tool_call_with_context(context.clone(), request.clone())
            .await
            .map_err(|error| {
                processor_tool_call_failure(index, processor.provider_instance(), error)
            })?;
        validate_transformation(
            &response.transformation_id,
            &response.transformation_version,
        )?;
        request.arguments_json = canonical_json(response.arguments_json.as_str())?
            .try_into()
            .map_err(|_| invalid_processing("Tool arguments are not portable JSON"))?;
        validate_tool_call(&request)?;
        trace.push(TransformationTrace {
            stage: ProcessingStage::ToolCall,
            provider_instance: processor.provider_instance().to_owned(),
            transformation_id: response.transformation_id,
            transformation_version: response.transformation_version,
            input_digest,
            output_digest: digest(&request)?,
        });
    }
    Ok(AppliedProcessing {
        value: request,
        trace,
    })
}

/// Applies presentation-only result projectors. The immutable factual result
/// stays in `value.fact`; only the Model-visible presentation can change.
pub async fn apply_tool_result_processors(
    processors: &ManyPort<TurnProcessingClient>,
    context: &lenso_kernel::InvocationContext,
    mut request: ProjectToolResultRequest,
) -> Result<AppliedProcessing<ProjectToolResultRequest>, RuntimeFailure> {
    validate_tool_result_projection(&request)?;
    let fact = request.fact.clone();
    let mut trace = Vec::with_capacity(processors.len());
    for (index, processor) in processors.iter().enumerate() {
        ensure_active(context)?;
        let input_digest = digest(&request)?;
        let response = processor
            .project_tool_result_with_context(context.clone(), request.clone())
            .await
            .map_err(|error| {
                processor_tool_result_failure(index, processor.provider_instance(), error)
            })?;
        validate_transformation(
            &response.transformation_id,
            &response.transformation_version,
        )?;
        request.presentation = response.presentation;
        if request.fact != fact {
            return Err(invalid_processing(
                "Tool result processor attempted to replace an immutable factual result",
            ));
        }
        validate_tool_result_projection(&request)?;
        trace.push(TransformationTrace {
            stage: ProcessingStage::ToolResult,
            provider_instance: processor.provider_instance().to_owned(),
            transformation_id: response.transformation_id,
            transformation_version: response.transformation_version,
            input_digest,
            output_digest: digest(&request)?,
        });
    }
    Ok(AppliedProcessing {
        value: request,
        trace,
    })
}

/// Canonicalizes one complete JSON value so evidence and approval comparisons
/// are insensitive to object-key order.
pub fn canonical_json(value: &str) -> Result<String, RuntimeFailure> {
    if value.len() > MAX_TOOL_ARGUMENT_BYTES {
        return Err(invalid_processing(
            "Tool arguments exceed the portable size limit",
        ));
    }
    let mut value = serde_json::from_str::<serde_json::Value>(value)
        .map_err(|_| invalid_processing("Tool arguments are not valid JSON"))?;
    sort_json(&mut value);
    serde_json::to_string(&value)
        .map_err(|_| invalid_processing("Tool arguments cannot be canonicalized"))
}

fn model_message_from_model(message: &CompleteMessageInput) -> ModelMessage {
    ModelMessage {
        arguments_json: message
            .arguments_json
            .as_ref()
            .map(|value| Some(value.as_str().to_owned())),
        content: message.content.clone(),
        images: message.images.as_ref().map(|images| {
            Some(
                images
                    .iter()
                    .map(|image| ModelImage {
                        media_type: image.media_type.clone(),
                        data_base64: image.data_base64.clone(),
                    })
                    .collect(),
            )
        }),
        role: match message.role {
            CompleteMessageRole::System => ModelMessageRole::System,
            CompleteMessageRole::User => ModelMessageRole::User,
            CompleteMessageRole::Assistant => ModelMessageRole::Assistant,
            CompleteMessageRole::Tool => ModelMessageRole::Tool,
        },
        tool_call_id: message
            .tool_call_id
            .as_ref()
            .map(|value| Some(value.clone())),
        tool_name: message.tool_name.as_ref().map(|value| Some(value.clone())),
    }
}

fn model_message_to_model(message: &ModelMessage) -> Result<CompleteMessageInput, RuntimeFailure> {
    Ok(CompleteMessageInput {
        arguments_json: message
            .arguments_json
            .clone()
            .flatten()
            .map(|value| {
                value.try_into().map_err(|_| {
                    invalid_processing("Model message arguments are not portable JSON")
                })
            })
            .transpose()?,
        content: message.content.clone(),
        images: message.images.clone().flatten().map(|images| {
            images
                .into_iter()
                .map(|image| ModelRequestImage {
                    media_type: image.media_type,
                    data_base64: image.data_base64,
                })
                .collect()
        }),
        role: match message.role {
            ModelMessageRole::System => CompleteMessageRole::System,
            ModelMessageRole::User => CompleteMessageRole::User,
            ModelMessageRole::Assistant => CompleteMessageRole::Assistant,
            ModelMessageRole::Tool => CompleteMessageRole::Tool,
        },
        tool_call_id: message.tool_call_id.clone().flatten(),
        tool_name: message.tool_name.clone().flatten(),
    })
}

fn model_tool_from_model(tool: &CompleteTool) -> ModelTool {
    ModelTool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        input_schema_json: tool.input_schema_json.clone(),
    }
}

fn model_tool_to_model(tool: &ModelTool) -> Result<CompleteTool, RuntimeFailure> {
    Ok(CompleteTool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        input_schema_json: tool
            .input_schema_json
            .as_str()
            .try_into()
            .map_err(|_| invalid_processing("Model Tool schema is not portable JSON"))?,
    })
}

fn validate_model_request(request: &ProjectModelRequest) -> Result<(), RuntimeFailure> {
    valid_identifier(
        &request.turn_id,
        MAX_TRANSFORMATION_ID_BYTES,
        "Turn identifier",
    )?;
    if request.messages.len() > MAX_MODEL_MESSAGES || request.tools.len() > MAX_MODEL_TOOLS {
        return Err(invalid_processing(
            "Model request projection exceeds the portable item limit",
        ));
    }
    for message in &request.messages {
        if message.content.len() > MAX_MODEL_CONTENT_BYTES
            || message
                .tool_call_id
                .clone()
                .flatten()
                .is_some_and(|value| value.len() > 128)
            || message
                .tool_name
                .clone()
                .flatten()
                .is_some_and(|value| value.len() > 128)
            || message
                .arguments_json
                .clone()
                .flatten()
                .is_some_and(|value| value.len() > MAX_TOOL_ARGUMENT_BYTES)
        {
            return Err(invalid_processing(
                "Model message exceeds a portable field limit",
            ));
        }
        if let Some(images) = message.images.clone().flatten()
            && (images.len() > 16
                || images.iter().any(|image| {
                    image.media_type.is_empty()
                        || image.media_type.len() > 128
                        || image.data_base64.is_empty()
                        || image.data_base64.len() > 2_796_204
                }))
        {
            return Err(invalid_processing("Model image projection is invalid"));
        }
    }
    for tool in &request.tools {
        valid_identifier(&tool.name, MAX_TRANSFORMATION_ID_BYTES, "Model Tool name")?;
        if tool.description.len() > 4_096
            || tool.input_schema_json.as_str().len() > MAX_TOOL_SCHEMA_BYTES
        {
            return Err(invalid_processing(
                "Model Tool projection exceeds a portable field limit",
            ));
        }
        let _: serde_json::Value = serde_json::from_str(tool.input_schema_json.as_str())
            .map_err(|_| invalid_processing("Model Tool schema is not valid JSON"))?;
    }
    Ok(())
}

fn validate_tool_call(request: &TransformToolCallRequest) -> Result<(), RuntimeFailure> {
    valid_identifier(
        &request.turn_id,
        MAX_TRANSFORMATION_ID_BYTES,
        "Turn identifier",
    )?;
    valid_identifier(
        &request.tool_call_id,
        MAX_TRANSFORMATION_ID_BYTES,
        "Tool call identifier",
    )?;
    valid_identifier(&request.tool_name, MAX_TRANSFORMATION_ID_BYTES, "Tool name")?;
    if request.arguments_json.as_str().len() > MAX_TOOL_ARGUMENT_BYTES {
        return Err(invalid_processing(
            "Tool arguments exceed the portable size limit",
        ));
    }
    let _: serde_json::Value = serde_json::from_str(request.arguments_json.as_str())
        .map_err(|_| invalid_processing("Tool arguments are not valid JSON"))?;
    Ok(())
}

fn validate_tool_result_projection(
    request: &ProjectToolResultRequest,
) -> Result<(), RuntimeFailure> {
    valid_identifier(
        &request.turn_id,
        MAX_TRANSFORMATION_ID_BYTES,
        "Turn identifier",
    )?;
    if request.presentation.len() > MAX_RESULT_PRESENTATION_BYTES {
        return Err(invalid_processing(
            "Tool result presentation exceeds the portable size limit",
        ));
    }
    validate_tool_call(&TransformToolCallRequest {
        turn_id: request.turn_id.clone(),
        tool_call_id: request.fact.tool_call_id.clone(),
        tool_name: request.fact.tool_name.clone(),
        arguments_json: request.fact.arguments_json.clone(),
    })?;
    if request.fact.content.len() > MAX_RESULT_PRESENTATION_BYTES
        || request.fact.metadata_json.as_str().len() > MAX_TOOL_SCHEMA_BYTES
        || request.fact.provider_code.len() > MAX_TRANSFORMATION_ID_BYTES
    {
        return Err(invalid_processing(
            "Tool result fact exceeds a portable field limit",
        ));
    }
    let _: serde_json::Value = serde_json::from_str(request.fact.metadata_json.as_str())
        .map_err(|_| invalid_processing("Tool result metadata is not valid JSON"))?;
    Ok(())
}

fn validate_transformation(id: &str, version: &str) -> Result<(), RuntimeFailure> {
    valid_identifier(id, MAX_TRANSFORMATION_ID_BYTES, "Transformation identifier")?;
    valid_identifier(
        version,
        MAX_TRANSFORMATION_VERSION_BYTES,
        "Transformation version",
    )
}

fn valid_identifier(value: &str, max: usize, label: &str) -> Result<(), RuntimeFailure> {
    if value.is_empty()
        || value.len() > max
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'@' | b'/')
        })
    {
        return Err(invalid_processing(format!("{label} is invalid")));
    }
    Ok(())
}

fn ensure_active(context: &lenso_kernel::InvocationContext) -> Result<(), RuntimeFailure> {
    if context.is_cancelled() {
        Err(RuntimeFailure::Cancelled {
            request_id: context.request_id(),
        })
    } else {
        Ok(())
    }
}

fn processor_model_request_failure(
    index: usize,
    provider: &str,
    error: TurnProcessingProjectModelRequestInvocationError,
) -> RuntimeFailure {
    match error {
        TurnProcessingProjectModelRequestInvocationError::Runtime(error) => error,
        TurnProcessingProjectModelRequestInvocationError::Domain(error) => {
            RuntimeFailure::PluginFailure {
                detail: format!(
                    "Turn processor `{provider}` rejected model request stage {index}: {error:?}"
                ),
            }
        }
    }
}

fn processor_tool_call_failure(
    index: usize,
    provider: &str,
    error: TurnProcessingTransformToolCallInvocationError,
) -> RuntimeFailure {
    match error {
        TurnProcessingTransformToolCallInvocationError::Runtime(error) => error,
        TurnProcessingTransformToolCallInvocationError::Domain(error) => {
            RuntimeFailure::PluginFailure {
                detail: format!(
                    "Turn processor `{provider}` rejected Tool call stage {index}: {error:?}"
                ),
            }
        }
    }
}

fn processor_tool_result_failure(
    index: usize,
    provider: &str,
    error: TurnProcessingProjectToolResultInvocationError,
) -> RuntimeFailure {
    match error {
        TurnProcessingProjectToolResultInvocationError::Runtime(error) => error,
        TurnProcessingProjectToolResultInvocationError::Domain(error) => {
            RuntimeFailure::PluginFailure {
                detail: format!(
                    "Turn processor `{provider}` rejected Tool result stage {index}: {error:?}"
                ),
            }
        }
    }
}

fn digest(value: &impl Serialize) -> Result<String, RuntimeFailure> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| invalid_processing("Turn processing value cannot be encoded for evidence"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn sort_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => values.iter_mut().for_each(sort_json),
        serde_json::Value::Object(object) => {
            let previous = std::mem::take(object);
            let mut entries = previous.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            for (key, mut value) in entries {
                sort_json(&mut value);
                object.insert(key, value);
            }
        }
        _ => {}
    }
}

fn invalid_processing(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use lenso_capability_agent_model::{CompleteMessageInput, CompleteMessageRole, CompleteTool};

    use super::{canonical_json, model_request_from_model, model_request_to_model};

    #[test]
    fn canonical_json_sorts_nested_object_keys() {
        assert_eq!(
            canonical_json(r#"{"z":{"b":2,"a":1},"a":[{"d":4,"c":3}]}"#).unwrap(),
            r#"{"a":[{"c":3,"d":4}],"z":{"a":1,"b":2}}"#
        );
        assert!(canonical_json("not-json").is_err());
    }

    #[test]
    fn model_projection_round_trip_preserves_optional_wire_fields() {
        let messages = vec![CompleteMessageInput {
            images: None,
            role: CompleteMessageRole::Tool,
            content: "factual result".to_owned(),
            tool_call_id: Some("call-1".to_owned()),
            tool_name: None,
            arguments_json: Some(r#"{"x":1}"#.try_into().unwrap()),
        }];
        let tools = vec![CompleteTool {
            name: "read".to_owned(),
            description: "Read a record".to_owned(),
            input_schema_json: r#"{"type":"object"}"#.try_into().unwrap(),
        }];

        let projected = model_request_from_model("turn-1", &messages, &tools);
        let (messages_after, tools_after) = model_request_to_model(&projected).unwrap();

        assert_eq!(messages_after, messages);
        assert_eq!(tools_after, tools);
    }
}

//! Model-backed Session title and latest-turn preview projection Adapter.

use lenso::prelude::*;
use lenso_capability_agent_model::{
    self as model_contract, CompleteMessageInput, CompleteMessageKind, CompleteMessageRole,
    CompleteOpen, ModelEvent, ModelInvocationError,
};
use lenso_capability_agent_session_presentation::{
    self as presentation_contract, ProjectError, ProjectRequest, ProjectResponse,
};
use lenso_kernel::{RuntimeFailure, StreamEvent};

const MAX_MODEL_RESPONSE_CHARACTERS: usize = 16_384;

#[derive(Clone, Debug, serde::Deserialize, lenso::PluginConfig)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "the public configuration uses consistent max_* limit names"
)]
struct PresentationModelConfig {
    model: String,
    instruction: String,
    temperature: f64,
    max_output_tokens: i64,
    max_input_characters: usize,
    max_title_characters: usize,
    max_preview_characters: usize,
}

#[lenso::plugin(validate = validate_config)]
#[derive(Clone, Debug)]
struct ModelSessionPresentationPlugin {
    #[config]
    config: PresentationModelConfig,
    model: Port<model_contract::ModelClient>,
}

fn validate_config(config: &PresentationModelConfig) -> Result<(), RuntimeFailure> {
    if config.model.trim() != config.model
        || config.model.is_empty()
        || config.model.len() > 256
        || config.instruction.trim().is_empty()
        || config.instruction.chars().count() > 8_192
        || !config.temperature.is_finite()
        || !(0.0..=2.0).contains(&config.temperature)
        || !(16..=4_096).contains(&config.max_output_tokens)
        || !(256..=524_288).contains(&config.max_input_characters)
        || !(16..=256).contains(&config.max_title_characters)
        || !(32..=1_024).contains(&config.max_preview_characters)
    {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "Model Session Presentation configuration is invalid".to_owned(),
        });
    }
    Ok(())
}

#[lenso::provides(presentation_contract::SessionPresentation)]
impl ModelSessionPresentationPlugin {
    async fn project(
        &self,
        context: Ctx,
        request: ProjectRequest,
    ) -> PluginResult<ProjectResponse, ProjectError> {
        validate_request(&self.config, &request).map_err(PluginError::domain)?;
        let completion =
            collect_model_response(&self.model, context, model_request(&self.config, &request))
                .await?;
        project_response(&self.config, &request, &completion).map_err(PluginError::domain)
    }
}

fn validate_request(
    config: &PresentationModelConfig,
    request: &ProjectRequest,
) -> Result<(), ProjectError> {
    if request.session_id.trim().is_empty()
        || request.turn_id.trim().is_empty()
        || request.user_input.trim().is_empty()
        || request.assistant_output.trim().is_empty()
    {
        return Err(ProjectError::InvalidTurn);
    }
    let input_characters = request
        .user_input
        .chars()
        .count()
        .saturating_add(request.assistant_output.chars().count());
    if input_characters > config.max_input_characters {
        return Err(ProjectError::ContentTooLarge);
    }
    Ok(())
}

fn model_request(config: &PresentationModelConfig, request: &ProjectRequest) -> CompleteOpen {
    let fixed_instruction = format!(
        "{}\n\nReturn exactly one JSON object and no prose or Markdown. The object must have \"title\" (a string or null) and \"latest_preview\" (a string). Use null for \"title\" when current_title is present. Keep title within {} characters and latest_preview within {} characters.",
        config.instruction, config.max_title_characters, config.max_preview_characters
    );
    let input = serde_json::json!({
        "session_id": request.session_id,
        "turn_id": request.turn_id,
        "user_input": request.user_input,
        "assistant_output": request.assistant_output,
        "current_title": request.current_title,
    })
    .to_string();
    CompleteOpen {
        model: config.model.clone(),
        messages: vec![
            CompleteMessageInput {
                role: CompleteMessageRole::System,
                content: fixed_instruction,
                tool_call_id: None,
                tool_name: None,
                arguments_json: None,
            },
            CompleteMessageInput {
                role: CompleteMessageRole::User,
                content: input,
                tool_call_id: None,
                tool_name: None,
                arguments_json: None,
            },
        ],
        tools: Vec::new(),
        temperature: config.temperature,
        max_output_tokens: config.max_output_tokens,
    }
}

async fn collect_model_response(
    model: &model_contract::ModelClient,
    context: Ctx,
    request: CompleteOpen,
) -> PluginResult<String, ProjectError> {
    let stream = model
        .complete_with_context(context, request)
        .await
        .map_err(map_model_open_error)?;
    stream.close_send().await.map_err(PluginError::runtime)?;
    let mut output = String::new();
    loop {
        match stream.receive().await.map_err(PluginError::runtime)? {
            ModelEvent::Message(message) => match message.kind {
                CompleteMessageKind::TextDelta => {
                    if output
                        .chars()
                        .count()
                        .saturating_add(message.text.chars().count())
                        > MAX_MODEL_RESPONSE_CHARACTERS
                    {
                        return Err(PluginError::domain(ProjectError::ProjectionFailed));
                    }
                    output.push_str(&message.text);
                }
                CompleteMessageKind::ReasoningSummaryDelta | CompleteMessageKind::Usage => {}
                CompleteMessageKind::ToolCall => {
                    return Err(PluginError::domain(ProjectError::ProjectionFailed));
                }
            },
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => return Ok(output),
            StreamEvent::Terminal(Err(_)) => {
                return Err(PluginError::domain(ProjectError::ProjectionFailed));
            }
        }
    }
}

fn map_model_open_error(error: ModelInvocationError) -> PluginError<ProjectError> {
    match error {
        ModelInvocationError::Domain(_) => PluginError::domain(ProjectError::ProjectionFailed),
        ModelInvocationError::Runtime(error) => PluginError::runtime(error),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelProjection {
    #[serde(default)]
    title: Option<String>,
    latest_preview: String,
}

fn project_response(
    config: &PresentationModelConfig,
    request: &ProjectRequest,
    completion: &str,
) -> Result<ProjectResponse, ProjectError> {
    let source = json_source(completion).ok_or(ProjectError::ProjectionFailed)?;
    let projection = serde_json::from_str::<ModelProjection>(source)
        .map_err(|_| ProjectError::ProjectionFailed)?;
    let title = match request.current_title.as_ref().and_then(Option::as_deref) {
        Some(current) => current.to_owned(),
        None => bounded_text(
            projection.title.as_deref().unwrap_or_default(),
            config.max_title_characters,
        )?,
    };
    let latest_preview = bounded_text(&projection.latest_preview, config.max_preview_characters)?;
    Ok(ProjectResponse {
        title,
        latest_preview,
    })
}

fn json_source(value: &str) -> Option<&str> {
    let value = value.trim();
    if !value.starts_with("```") {
        return (!value.is_empty()).then_some(value);
    }
    let body = value
        .strip_prefix("```json")
        .or_else(|| value.strip_prefix("```"))?;
    body.strip_suffix("```").map(str::trim)
}

fn bounded_text(value: &str, limit: usize) -> Result<String, ProjectError> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Err(ProjectError::ProjectionFailed);
    }
    Ok(normalized.chars().take(limit).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PresentationModelConfig {
        PresentationModelConfig {
            model: "fixture/readme-summary-v1".to_owned(),
            instruction: "Create concise Session display metadata.".to_owned(),
            temperature: 0.0,
            max_output_tokens: 256,
            max_input_characters: 16_384,
            max_title_characters: 32,
            max_preview_characters: 64,
        }
    }

    fn request(current_title: Option<String>) -> ProjectRequest {
        ProjectRequest {
            session_id: "session-1".to_owned(),
            turn_id: "turn-1".to_owned(),
            user_input: "Design model-backed titles".to_owned(),
            assistant_output: "Added one replaceable Adapter.".to_owned(),
            current_title: Some(current_title),
        }
    }

    #[test]
    fn constructs_a_tool_free_bounded_model_request() {
        let request = model_request(&config(), &request(None));
        assert_eq!(request.model, "fixture/readme-summary-v1");
        assert!(request.tools.is_empty());
        assert!(request.temperature.abs() < f64::EPSILON);
        assert_eq!(request.max_output_tokens, 256);
        assert!(
            request.messages[0]
                .content
                .contains("Return exactly one JSON object")
        );
    }

    #[test]
    fn accepts_direct_or_fenced_json_and_bounds_fields() {
        let projected = project_response(
            &config(),
            &request(None),
            r#"```json
{"title":"  Model   presentation  ","latest_preview":"  Generated   concise metadata.  "}
```"#,
        )
        .unwrap();
        assert_eq!(projected.title, "Model presentation");
        assert_eq!(projected.latest_preview, "Generated concise metadata.");
    }

    #[test]
    fn preserves_an_existing_title_regardless_of_model_output() {
        let projected = project_response(
            &config(),
            &request(Some("User title".to_owned())),
            r#"{"title":"Replacement","latest_preview":"Updated preview"}"#,
        )
        .unwrap();
        assert_eq!(projected.title, "User title");
        assert_eq!(projected.latest_preview, "Updated preview");
    }

    #[test]
    fn rejects_prose_unknown_fields_and_empty_projection_text() {
        assert!(project_response(&config(), &request(None), "Here is the result").is_err());
        assert!(
            project_response(
                &config(),
                &request(None),
                r#"{"title":"Title","latest_preview":"Preview","extra":true}"#,
            )
            .is_err()
        );
        assert!(
            project_response(
                &config(),
                &request(None),
                r#"{"title":" ","latest_preview":"Preview"}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_invalid_configuration_before_readiness() {
        let invalid = PresentationModelConfig {
            model: String::new(),
            ..config()
        };
        assert!(matches!(
            validate_config(&invalid),
            Err(RuntimeFailure::InvalidResolvedPlan { .. })
        ));
    }
}

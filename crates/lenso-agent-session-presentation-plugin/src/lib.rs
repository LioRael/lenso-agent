//! Deterministic local Session presentation projection Adapter.

use lenso::prelude::*;
use lenso_capability_agent_session_presentation::{
    self as presentation_contract, ProjectError, ProjectRequest, ProjectResponse,
};
use lenso_kernel::RuntimeFailure;

#[derive(Clone, Debug, serde::Deserialize, lenso::PluginConfig)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "the public configuration uses consistent max_* limit names"
)]
struct PresentationConfig {
    max_input_characters: usize,
    max_title_characters: usize,
    max_preview_characters: usize,
}

#[lenso::plugin(validate = validate_config)]
#[derive(Clone, Debug)]
struct SessionPresentationPlugin {
    #[config]
    config: PresentationConfig,
}

fn validate_config(config: &PresentationConfig) -> Result<(), RuntimeFailure> {
    if !(256..=524_288).contains(&config.max_input_characters)
        || !(16..=256).contains(&config.max_title_characters)
        || !(32..=1_024).contains(&config.max_preview_characters)
    {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "Session Presentation limits are invalid".to_owned(),
        });
    }
    Ok(())
}

#[lenso::provides(presentation_contract::SessionPresentation)]
impl SessionPresentationPlugin {
    #[allow(clippy::unused_async_trait_impl)]
    async fn project(
        &self,
        _: Ctx,
        request: ProjectRequest,
    ) -> PluginResult<ProjectResponse, ProjectError> {
        project_turn(&self.config, request).map_err(PluginError::domain)
    }
}

fn project_turn(
    config: &PresentationConfig,
    request: ProjectRequest,
) -> Result<ProjectResponse, ProjectError> {
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

    let title = request.current_title.flatten().unwrap_or_else(|| {
        bounded_text(&request.user_input, config.max_title_characters, "New chat")
    });
    let latest_preview = bounded_text(
        &request.assistant_output,
        config.max_preview_characters,
        "Completed turn",
    );
    Ok(ProjectResponse {
        title,
        latest_preview,
    })
}

fn bounded_text(value: &str, limit: usize, fallback: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return fallback.to_owned();
    }
    if normalized.chars().count() <= limit {
        normalized
    } else {
        normalized.chars().take(limit).collect::<String>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PresentationConfig {
        PresentationConfig {
            max_input_characters: 16_384,
            max_title_characters: 32,
            max_preview_characters: 64,
        }
    }

    #[test]
    fn projects_first_title_and_latest_preview() {
        let response = project_turn(
            &config(),
            ProjectRequest {
                session_id: "session-1".to_owned(),
                turn_id: "turn-1".to_owned(),
                user_input: "  Design   a Session title Plugin  ".to_owned(),
                assistant_output: "  Keep presentation separate from compaction.  ".to_owned(),
                current_title: Some(None),
            },
        )
        .unwrap();

        assert_eq!(response.title, "Design a Session title Plugin");
        assert_eq!(
            response.latest_preview,
            "Keep presentation separate from compaction."
        );
    }

    #[test]
    fn preserves_an_existing_title() {
        let response = project_turn(
            &config(),
            ProjectRequest {
                session_id: "session-1".to_owned(),
                turn_id: "turn-2".to_owned(),
                user_input: "Continue".to_owned(),
                assistant_output: "Done".to_owned(),
                current_title: Some(Some("Chosen title".to_owned())),
            },
        )
        .unwrap();
        assert_eq!(response.title, "Chosen title");
    }

    #[test]
    fn rejects_oversized_turns() {
        let result = project_turn(
            &PresentationConfig {
                max_input_characters: 256,
                ..config()
            },
            ProjectRequest {
                session_id: "session-1".to_owned(),
                turn_id: "turn-1".to_owned(),
                user_input: "x".repeat(257),
                assistant_output: "done".to_owned(),
                current_title: Some(None),
            },
        );
        assert!(matches!(result, Err(ProjectError::ContentTooLarge)));
    }

    #[test]
    fn rejects_invalid_configuration_before_readiness() {
        let result = validate_config(&PresentationConfig {
            max_input_characters: 255,
            ..config()
        });
        assert!(matches!(
            result,
            Err(RuntimeFailure::InvalidResolvedPlan { .. })
        ));
    }
}

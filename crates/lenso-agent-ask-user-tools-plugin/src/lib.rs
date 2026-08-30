//! Tool projection over the portable User Interaction Capability.

use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicU64, Ordering},
};

use lenso::prelude::*;
use lenso_capability_agent_tool_provider::{
    self as tool_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
};
use lenso_capability_agent_tools::USER_INTERACTION_COMPLETED_METADATA_KEY;
use lenso_capability_agent_user_interaction::{
    self as interaction_contract, AskError, AskRequest, InteractionOption, InteractionQuestion,
    UserInteractionAskInvocationError,
};

pub const ASK_USER_TOOL: &str = "ask_user";
static NEXT_INTERACTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AskUserArguments {
    questions: Vec<AskUserQuestion>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AskUserQuestion {
    id: String,
    header: String,
    question: String,
    #[serde(default)]
    options: Vec<AskUserOption>,
    #[serde(default)]
    multi_select: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AskUserOption {
    label: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    preview: Option<String>,
}

#[lenso::plugin]
#[derive(Clone, Debug)]
struct AskUserToolsPlugin {
    interaction: Port<interaction_contract::UserInteractionClient>,
}

#[lenso::provides(tool_contract::ToolProvider)]
impl AskUserToolsPlugin {
    fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> impl std::future::Future<Output = PluginResult<CatalogResponse, tool_contract::CatalogError>>
    {
        let _ = self;
        futures::future::ready(Ok(CatalogResponse {
            tools: vec![ToolDefinition {
                name: ASK_USER_TOOL.to_owned(),
                description:
                    "Ask the user one or more blocking choice questions when a decision is required. Every question also allows an Other answer."
                        .to_owned(),
                input_schema_json: serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "questions": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": 8,
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "id": { "type": "string", "minLength": 1, "maxLength": 128 },
                                    "header": { "type": "string", "minLength": 1, "maxLength": 64 },
                                    "question": { "type": "string", "minLength": 1, "maxLength": 4096 },
                                    "options": {
                                        "type": "array",
                                        "maxItems": 16,
                                        "items": {
                                            "type": "object",
                                            "additionalProperties": false,
                                            "properties": {
                                                "label": { "type": "string", "minLength": 1, "maxLength": 256 },
                                                "description": { "type": "string", "maxLength": 1024 },
                                                "preview": { "type": "string", "maxLength": 16384 }
                                            },
                                            "required": ["label"]
                                        }
                                    },
                                    "multi_select": { "type": "boolean", "default": false }
                                },
                                "required": ["id", "header", "question"]
                            }
                        }
                    },
                    "required": ["questions"]
                })
                .to_string()
                .try_into()
                .expect("ask_user schema must be valid JSON"),
                execution: ToolExecutionClass::Exclusive,
            }],
        }))
    }

    async fn execute(
        &self,
        context: Ctx,
        request: ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        if request.name != ASK_USER_TOOL {
            return Err(PluginError::domain(ExecuteError::NotFound));
        }
        let arguments = serde_json::from_str::<AskUserArguments>(request.arguments_json.as_str())
            .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))?;
        if !valid_arguments(&arguments) {
            return Err(PluginError::domain(ExecuteError::InvalidArguments));
        }
        let sequence = NEXT_INTERACTION_ID.fetch_add(1, Ordering::Relaxed);
        let interaction_id = format!("ask-{}-{sequence}", context.request_id());
        match self
            .interaction
            .ask_with_context(
                context,
                AskRequest {
                    interaction_id: interaction_id.clone(),
                    questions: arguments
                        .questions
                        .into_iter()
                        .map(|question| InteractionQuestion {
                            question_id: question.id,
                            header: question.header,
                            prompt: question.question,
                            multi_select: question.multi_select,
                            options: question
                                .options
                                .into_iter()
                                .map(|option| InteractionOption {
                                    option_id: option.label.clone(),
                                    label: option.label,
                                    description: option.description,
                                    preview: Some(option.preview),
                                })
                                .collect(),
                        })
                        .collect(),
                },
            )
            .await
        {
            Ok(response) => Ok(ExecuteResponse {
                content_blocks: None,
                content: serde_json::to_string(&response).map_err(|error| {
                    PluginError::runtime(lenso_kernel::RuntimeFailure::PluginFailure {
                        detail: format!("failed to encode ask_user answers: {error}"),
                    })
                })?,
                content_type: ContentType::Text,
                metadata_json: serde_json::json!({
                    "interaction_id": interaction_id,
                    (USER_INTERACTION_COMPLETED_METADATA_KEY): true
                })
                .to_string()
                .try_into()
                .expect("ask_user metadata must be valid JSON"),
            }),
            Err(UserInteractionAskInvocationError::Domain(error)) => {
                Err(PluginError::domain(map_interaction_error(&error)))
            }
            Err(UserInteractionAskInvocationError::Runtime(error)) => {
                Err(PluginError::runtime(error))
            }
        }
    }
}

fn valid_arguments(arguments: &AskUserArguments) -> bool {
    let question_ids = arguments
        .questions
        .iter()
        .map(|question| question.id.as_str())
        .collect::<BTreeSet<_>>();
    !arguments.questions.is_empty()
        && arguments.questions.len() <= 8
        && question_ids.len() == arguments.questions.len()
        && arguments.questions.iter().all(|question| {
            let labels = question
                .options
                .iter()
                .map(|option| option.label.as_str())
                .collect::<BTreeSet<_>>();
            !question.id.trim().is_empty()
                && question.id.len() <= 128
                && !question.header.trim().is_empty()
                && question.header.len() <= 64
                && !question.question.trim().is_empty()
                && question.question.len() <= 4096
                && question.options.len() <= 16
                && labels.len() == question.options.len()
                && question.options.iter().all(|option| {
                    !option.label.trim().is_empty()
                        && option.label.len() <= 256
                        && option.description.len() <= 1024
                        && option
                            .preview
                            .as_ref()
                            .is_none_or(|preview| !question.multi_select && preview.len() <= 16_384)
                })
        })
}

fn map_interaction_error(error: &AskError) -> ExecuteError {
    let (reason_code, message) = match error {
        AskError::Unavailable => (
            "interaction_unavailable",
            "This Agent surface cannot ask the user an interactive question.",
        ),
        AskError::InvalidRequest => ("interaction_invalid", "The user question was invalid."),
        AskError::TooManyPending => (
            "interaction_busy",
            "Too many user questions are already pending.",
        ),
        AskError::Timeout => ("interaction_timeout", "The user question timed out."),
        AskError::Unknown(_) => ("interaction_unknown", "User interaction failed."),
    };
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: message.to_owned(),
            details_json: "{}"
                .to_owned()
                .try_into()
                .expect("static interaction details must be valid JSON"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_exposes_one_exclusive_tool_provider_with_interaction_dependency() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(descriptor["plugin_id"], "lenso.agent.ask-user-tools");
        assert_eq!(
            descriptor["required_capabilities"][0]["capability_id"],
            "lenso.agent.user-interaction@2"
        );
        assert_eq!(
            descriptor["provided_capabilities"][0]["capability_id"],
            "lenso.agent.tool-provider@2"
        );
    }

    #[test]
    fn headless_unavailable_becomes_a_stable_tool_error() {
        let ExecuteError::ExecutionFailed { payload } =
            map_interaction_error(&AskError::Unavailable)
        else {
            panic!("unavailable interaction must be a structured Tool error");
        };
        assert_eq!(payload.reason_code, "interaction_unavailable");
    }

    #[test]
    fn previews_are_rejected_for_multi_select_questions() {
        assert!(!valid_arguments(&AskUserArguments {
            questions: vec![AskUserQuestion {
                id: "mode".to_owned(),
                header: "Mode".to_owned(),
                question: "Choose".to_owned(),
                multi_select: true,
                options: vec![AskUserOption {
                    label: "safe".to_owned(),
                    description: String::new(),
                    preview: Some("preview".to_owned()),
                }],
            }],
        }));
    }
}

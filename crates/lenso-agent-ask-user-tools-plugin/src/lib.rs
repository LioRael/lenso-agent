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
use lenso_capability_agent_user_interaction::{
    self as interaction_contract, AskError, AskRequest, UserInteractionAskInvocationError,
};

pub const ASK_USER_TOOL: &str = "ask_user";
static NEXT_INTERACTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AskUserArguments {
    question: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default = "default_allow_freeform")]
    allow_freeform: bool,
}

fn default_allow_freeform() -> bool {
    true
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
                    "Ask the user one blocking question when their input is required to continue."
                        .to_owned(),
                input_schema_json: serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "question": { "type": "string", "minLength": 1, "maxLength": 4096 },
                        "options": {
                            "type": "array",
                            "maxItems": 16,
                            "items": { "type": "string", "minLength": 1, "maxLength": 256 }
                        },
                        "allow_freeform": { "type": "boolean", "default": true }
                    },
                    "required": ["question"]
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
                    prompt: arguments.question,
                    options: arguments.options,
                    allow_freeform: arguments.allow_freeform,
                },
            )
            .await
        {
            Ok(response) => Ok(ExecuteResponse {
                content: response.answer,
                content_type: ContentType::Text,
                metadata_json: serde_json::json!({ "interaction_id": interaction_id })
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
    let options = arguments.options.iter().collect::<BTreeSet<_>>();
    !arguments.question.trim().is_empty()
        && arguments.question.len() <= 4096
        && arguments.options.len() <= 16
        && arguments
            .options
            .iter()
            .all(|option| !option.trim().is_empty() && option.len() <= 256)
        && options.len() == arguments.options.len()
        && (arguments.allow_freeform || !arguments.options.is_empty())
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
            "lenso.agent.user-interaction@1"
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
    fn closed_choice_requires_at_least_one_option() {
        assert!(!valid_arguments(&AskUserArguments {
            question: "Choose".to_owned(),
            options: Vec::new(),
            allow_freeform: false,
        }));
    }
}

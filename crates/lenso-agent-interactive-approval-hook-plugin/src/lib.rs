//! Inline approval over the portable User Interaction Capability.

use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicU64, Ordering},
};

use lenso::prelude::*;
use lenso_capability_agent_tool_hook::{
    self as hook_contract, AfterExecuteRequest, AfterExecuteResponse, BeforeExecuteRequest,
    BeforeExecuteResponse, HookDecision, ToolHookProvider,
};
use lenso_capability_agent_user_interaction::{
    self as interaction_contract, AskRequest, InteractionOption, InteractionQuestion,
    UserInteractionAskInvocationError,
};
use lenso_kernel::RuntimeFailure;

static NEXT_APPROVAL_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum PolicyDecision {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InteractiveApprovalConfig {
    default_decision: PolicyDecision,
    #[serde(default)]
    allow_tools: Vec<String>,
    #[serde(default)]
    ask_tools: Vec<String>,
    #[serde(default)]
    deny_tools: Vec<String>,
    max_preview_bytes: usize,
}

#[lenso::plugin(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct InteractiveApprovalHookPlugin {
    #[config]
    config: InteractiveApprovalConfig,
    interaction: Port<interaction_contract::UserInteractionClient>,
}

#[lenso::provides(hook_contract::ToolHook)]
impl ToolHookProvider for InteractiveApprovalHookPlugin {
    fn before_execute(
        &self,
        context: Ctx,
        request: BeforeExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<hook_contract::ToolHookBeforeExecute> {
        let config = self.config.clone();
        let interaction = self.interaction.clone();
        Box::pin(async move { approve(&config, &interaction, context, request).await })
    }

    fn after_execute(
        &self,
        _context: Ctx,
        _request: AfterExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<hook_contract::ToolHookAfterExecute> {
        Box::pin(async { Ok(Ok(AfterExecuteResponse {})) })
    }
}

fn validate_config(config: &InteractiveApprovalConfig) -> Result<(), RuntimeFailure> {
    if !(256..=16_384).contains(&config.max_preview_bytes) {
        return Err(invalid_plan(
            "interactive approval preview limit is invalid",
        ));
    }
    let mut names = BTreeSet::new();
    for name in config
        .allow_tools
        .iter()
        .chain(&config.ask_tools)
        .chain(&config.deny_tools)
    {
        if name.trim().is_empty() || !names.insert(name) {
            return Err(invalid_plan(
                "interactive approval Tool names must be non-empty and disjoint",
            ));
        }
    }
    Ok(())
}

async fn approve(
    config: &InteractiveApprovalConfig,
    interaction: &interaction_contract::UserInteractionClient,
    context: Ctx,
    request: BeforeExecuteRequest,
) -> Result<Result<BeforeExecuteResponse, hook_contract::BeforeExecuteError>, RuntimeFailure> {
    match policy_for(config, &request.tool_name) {
        PolicyDecision::Allow => {
            return Ok(Ok(response(
                HookDecision::Allow,
                "policy_allow",
                "Tool is allowed",
            )));
        }
        PolicyDecision::Deny => {
            return Ok(Ok(response(
                HookDecision::Deny,
                "policy_deny",
                "Tool is denied by policy",
            )));
        }
        PolicyDecision::Ask => {}
    }
    let sequence = NEXT_APPROVAL_ID.fetch_add(1, Ordering::Relaxed);
    let interaction_id = format!("approval-{}-{sequence}", context.request_id());
    let preview = truncate_utf8(request.arguments_json.as_str(), config.max_preview_bytes);
    let result = interaction
        .ask_with_context(
            context,
            AskRequest {
                interaction_id,
                questions: vec![InteractionQuestion {
                    question_id: "approval".to_owned(),
                    header: "Tool approval".to_owned(),
                    prompt: format!("Allow `{}` to run once?", request.tool_name),
                    multi_select: false,
                    options: vec![
                        InteractionOption {
                            option_id: "approve".to_owned(),
                            label: "Approve once".to_owned(),
                            description: "Run this exact Tool call once.".to_owned(),
                            preview: Some(Some(preview)),
                        },
                        InteractionOption {
                            option_id: "deny".to_owned(),
                            label: "Deny".to_owned(),
                            description: "Do not run this Tool call.".to_owned(),
                            preview: Some(None),
                        },
                    ],
                }],
            },
        )
        .await;
    match result {
        Ok(answer)
            if answer.answers.first().is_some_and(|answer| {
                answer
                    .selected_option_ids
                    .iter()
                    .any(|option| option == "approve")
            }) =>
        {
            Ok(Ok(response(
                HookDecision::Allow,
                "interactive_approved",
                "User approved this Tool call",
            )))
        }
        Ok(_) => Ok(Ok(response(
            HookDecision::Deny,
            "interactive_denied",
            "User denied this Tool call",
        ))),
        Err(UserInteractionAskInvocationError::Domain(_)) => Ok(Ok(response(
            HookDecision::Ask,
            "interactive_approval_unavailable",
            "This surface could not complete interactive approval",
        ))),
        Err(UserInteractionAskInvocationError::Runtime(error)) => Err(error),
    }
}

fn policy_for(config: &InteractiveApprovalConfig, tool_name: &str) -> PolicyDecision {
    if config.deny_tools.iter().any(|name| name == tool_name) {
        PolicyDecision::Deny
    } else if config.ask_tools.iter().any(|name| name == tool_name) {
        PolicyDecision::Ask
    } else if config.allow_tools.iter().any(|name| name == tool_name) {
        PolicyDecision::Allow
    } else {
        config.default_decision
    }
}

fn response(decision: HookDecision, reason_code: &str, message: &str) -> BeforeExecuteResponse {
    BeforeExecuteResponse {
        decision,
        reason_code: reason_code.to_owned(),
        message: message.to_owned(),
        context_json: "{}".to_owned().try_into().expect("static JSON is valid"),
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…", &value[..end])
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_policy_precedes_the_default() {
        let config = InteractiveApprovalConfig {
            default_decision: PolicyDecision::Ask,
            allow_tools: vec!["read_text".to_owned()],
            ask_tools: vec![],
            deny_tools: vec!["danger".to_owned()],
            max_preview_bytes: 1024,
        };
        assert!(matches!(
            policy_for(&config, "read_text"),
            PolicyDecision::Allow
        ));
        assert!(matches!(
            policy_for(&config, "danger"),
            PolicyDecision::Deny
        ));
        assert!(matches!(
            policy_for(&config, "write_text"),
            PolicyDecision::Ask
        ));
    }

    #[test]
    fn preview_truncation_preserves_utf8_boundaries() {
        assert_eq!(truncate_utf8("ab你好", 5), "ab你\n…");
    }

    #[test]
    fn descriptor_exposes_a_hook_with_user_interaction_dependency() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(
            descriptor["plugin_id"],
            "lenso.agent.interactive-approval-hook"
        );
        assert_eq!(
            descriptor["required_capabilities"][0]["capability_id"],
            "lenso.agent.user-interaction@2"
        );
        assert_eq!(
            descriptor["provided_capabilities"][0]["capability_id"],
            "lenso.agent.tool-hook@1"
        );
    }
}

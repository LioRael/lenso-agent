//! Shared host-side orchestration for ordered Tool Hooks.

use lenso_kernel::{InvocationContext, RuntimeFailure};
use lenso_module_authoring::ManyPort;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::{
    AfterExecuteRequest, BeforeExecuteRequest, HookDecision, HookOutcome,
    ToolHookAfterExecuteInvocationError, ToolHookBeforeExecuteInvocationError, ToolHookClient,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookBlock {
    pub provider_code: &'static str,
    pub message: String,
    pub details_json: String,
}

#[derive(Clone, Debug)]
pub struct HookExecution {
    execution_id: String,
    tool_name: String,
    arguments_json: String,
    contexts: Vec<String>,
    pub block: Option<HookBlock>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookTerminal {
    Success,
    DomainError,
    RuntimeFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NormalizeArgumentsError;

pub fn normalize_arguments(arguments_json: &str) -> Result<String, NormalizeArgumentsError> {
    let mut value =
        serde_json::from_str::<Value>(arguments_json).map_err(|_| NormalizeArgumentsError)?;
    sort_json(&mut value);
    serde_json::to_string(&value).map_err(|_| NormalizeArgumentsError)
}

fn sort_json(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(sort_json),
        Value::Object(object) => {
            let previous = std::mem::take(object);
            let mut entries = previous.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut sorted = Map::new();
            for (key, mut value) in entries {
                sort_json(&mut value);
                sorted.insert(key, value);
            }
            *object = sorted;
        }
        _ => {}
    }
}

pub async fn start_hooks(
    hooks: &ManyPort<ToolHookClient>,
    context: &InvocationContext,
    tool_name: String,
    arguments_json: String,
) -> Result<HookExecution, RuntimeFailure> {
    let execution_id = Uuid::new_v4().to_string();
    let mut contexts = Vec::with_capacity(hooks.len());
    let mut selected: Option<(u8, &'static str, String, String)> = None;
    let mut evidence = Vec::with_capacity(hooks.len());
    for (index, hook) in hooks.iter().enumerate() {
        let response = match hook
            .before_execute_with_context(
                context.clone(),
                BeforeExecuteRequest {
                    execution_id: execution_id.clone(),
                    tool_name: tool_name.clone(),
                    arguments_json: arguments_json
                        .clone()
                        .try_into()
                        .expect("normalized arguments must remain JSON"),
                },
            )
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let failure = hook_failure("before_execute", index, error);
                let partial = HookExecution {
                    execution_id: execution_id.clone(),
                    tool_name: tool_name.clone(),
                    arguments_json: arguments_json.clone(),
                    contexts,
                    block: None,
                };
                finish_hooks(
                    hooks,
                    context,
                    &partial,
                    HookTerminal::RuntimeFailure,
                    "",
                    "{}",
                    "hook_failure",
                )
                .await?;
                return Err(failure);
            }
        };
        let rank = match response.decision {
            HookDecision::Allow => 0,
            HookDecision::Ask => 1,
            HookDecision::Deny => 2,
        };
        let code = if rank == 2 {
            "hook_denied"
        } else {
            "approval_required"
        };
        if rank > 0 && selected.as_ref().is_none_or(|current| rank > current.0) {
            selected = Some((
                rank,
                code,
                response.message.clone(),
                response.reason_code.clone(),
            ));
        }
        evidence.push(json!({
            "hook_index": index,
            "decision": match rank { 0 => "allow", 1 => "ask", _ => "deny" },
            "reason_code": response.reason_code,
            "context": serde_json::from_str::<Value>(response.context_json.as_str())
                .unwrap_or(Value::Null),
        }));
        contexts.push(response.context_json.to_string());
    }
    let block = selected.map(|(_, provider_code, message, reason_code)| HookBlock {
        provider_code,
        message,
        details_json: json!({ "reason_code": reason_code, "hooks": evidence }).to_string(),
    });
    Ok(HookExecution {
        execution_id,
        tool_name,
        arguments_json,
        contexts,
        block,
    })
}

pub async fn finish_hooks(
    hooks: &ManyPort<ToolHookClient>,
    context: &InvocationContext,
    execution: &HookExecution,
    terminal: HookTerminal,
    terminal_content: &str,
    metadata_json: &str,
    provider_code: &str,
) -> Result<(), RuntimeFailure> {
    for (index, (hook, hook_context)) in hooks.iter().zip(&execution.contexts).enumerate() {
        hook.after_execute_with_context(
            context.clone(),
            AfterExecuteRequest {
                execution_id: execution.execution_id.clone(),
                tool_name: execution.tool_name.clone(),
                arguments_json: execution
                    .arguments_json
                    .clone()
                    .try_into()
                    .expect("normalized arguments must remain JSON"),
                context_json: hook_context
                    .clone()
                    .try_into()
                    .expect("Hook context must be JSON"),
                outcome: match terminal {
                    HookTerminal::Success => HookOutcome::Success,
                    HookTerminal::DomainError => HookOutcome::DomainError,
                    HookTerminal::RuntimeFailure => HookOutcome::RuntimeFailure,
                },
                content: terminal_content.to_owned(),
                metadata_json: metadata_json.to_owned().try_into().map_err(|_| {
                    RuntimeFailure::ModuleFailure {
                        detail: "Tool Provider returned invalid metadata JSON".to_owned(),
                    }
                })?,
                provider_code: provider_code.to_owned(),
            },
        )
        .await
        .map_err(|error| after_hook_failure(index, error))?;
    }
    Ok(())
}

fn hook_failure(
    operation: &str,
    index: usize,
    error: ToolHookBeforeExecuteInvocationError,
) -> RuntimeFailure {
    match error {
        ToolHookBeforeExecuteInvocationError::Runtime(error) => error,
        ToolHookBeforeExecuteInvocationError::Domain(_) => RuntimeFailure::ModuleFailure {
            detail: format!("Tool Hook {index} failed during {operation}"),
        },
    }
}

fn after_hook_failure(index: usize, error: ToolHookAfterExecuteInvocationError) -> RuntimeFailure {
    match error {
        ToolHookAfterExecuteInvocationError::Runtime(error) => error,
        ToolHookAfterExecuteInvocationError::Domain(_) => RuntimeFailure::ModuleFailure {
            detail: format!("Tool Hook {index} failed during after_execute"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_arguments;

    #[test]
    fn arguments_are_normalized_recursively_and_invalid_json_is_rejected() {
        assert_eq!(
            normalize_arguments(r#"{"z":{"b":2,"a":1},"a":[{"d":4,"c":3}]}"#).unwrap(),
            r#"{"a":[{"c":3,"d":4}],"z":{"a":1,"b":2}}"#
        );
        assert!(normalize_arguments("not-json").is_err());
    }
}

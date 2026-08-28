//! Command-backed Adapter for model, remote, or custom context compaction.

use lenso::prelude::*;
use lenso_agent_native_support::{CommandAdapterConfig, CommandOutcome, invoke_command};
use lenso_capability_agent_context_compaction::{
    self as context_contract, CompactError, CompactRequest, CompactResponse,
};
use lenso_kernel::RuntimeFailure;

fn validate_config(config: &CommandAdapterConfig) -> Result<(), RuntimeFailure> {
    config.validate()
}

#[lenso::plugin(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct ContextCompactionCommandPlugin {
    #[config]
    config: CommandAdapterConfig,
}

#[lenso::provides(context_contract::ContextCompaction)]
impl ContextCompactionCommandPlugin {
    async fn compact(
        &self,
        context: Ctx,
        request: CompactRequest,
    ) -> PluginResult<CompactResponse, CompactError> {
        match invoke_command(&self.config, &context, "context.compact", &request)
            .await
            .map_err(PluginError::runtime)?
        {
            CommandOutcome::Result(response) => Ok(response),
            CommandOutcome::DomainError(error) if error == "invalid_context" => {
                Err(PluginError::domain(CompactError::InvalidContext))
            }
            CommandOutcome::DomainError(error) if error == "context_too_large" => {
                Err(PluginError::domain(CompactError::ContextTooLarge))
            }
            CommandOutcome::DomainError(error) if error == "compaction_failed" => {
                Err(PluginError::domain(CompactError::CompactionFailed))
            }
            CommandOutcome::DomainError(error) => {
                Err(PluginError::runtime(RuntimeFailure::PluginFailure {
                    detail: format!("Context Compaction command returned unknown `{error}` error"),
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_capability_agent_context_compaction::{ContextMessage, ContextMessageRole};
    use lenso_kernel::{CancellationToken, InvocationContext};

    #[tokio::test(flavor = "current_thread")]
    async fn command_may_supply_model_or_remote_compaction() {
        let plugin = ContextCompactionCommandPlugin {
            config: CommandAdapterConfig {
                program: "/bin/sh".into(),
                arguments: vec![
                    "-c".to_owned(),
                    r#"read request
case "$request" in *'"operation":"context.compact"'*) ;; *) exit 9;; esac
printf '%s\n' '{"protocol":"lenso.agent.command-adapter@1","result":{"summary":"remote summary","retained_messages":[{"role":"user","content":"recent question"},{"role":"assistant","content":"recent answer"}]}}'"#.to_owned(),
                ],
                timeout_ms: 1_000,
                max_response_bytes: 16_384,
            },
        };
        let response = plugin
            .compact(
                InvocationContext::new(1, None, CancellationToken::new()),
                CompactRequest {
                    session_id: "session-1".to_owned(),
                    previous_summary: Some(None),
                    messages: vec![
                        ContextMessage {
                            role: ContextMessageRole::User,
                            content: "old question".to_owned(),
                        },
                        ContextMessage {
                            role: ContextMessageRole::Assistant,
                            content: "old answer".to_owned(),
                        },
                    ],
                    target_summary_characters: 4096,
                },
            )
            .await
            .unwrap();
        assert_eq!(response.summary, "remote summary");
        assert_eq!(response.retained_messages.len(), 2);
    }
}

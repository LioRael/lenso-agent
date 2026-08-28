//! Command-backed Adapter for local, remote, or MCP-bridged Memory providers.

use lenso::prelude::*;
use lenso_agent_native_support::{CommandAdapterConfig, CommandOutcome, invoke_command};
use lenso_capability_agent_memory::{
    self as memory_contract, ForgetError, ForgetRequest, ForgetResponse, ObserveError,
    ObserveRequest, ObserveResponse, RecallError, RecallRequest, RecallResponse, RememberError,
    RememberRequest, RememberResponse,
};
use lenso_kernel::RuntimeFailure;

fn validate_config(config: &CommandAdapterConfig) -> Result<(), RuntimeFailure> {
    config.validate()
}

#[lenso::plugin(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct MemoryCommandPlugin {
    #[config]
    config: CommandAdapterConfig,
}

fn unknown_domain(operation: &str, error: &str) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: format!("Memory command returned unknown `{error}` error for {operation}"),
    }
}

#[lenso::provides(memory_contract::Memory)]
impl MemoryCommandPlugin {
    async fn observe(
        &self,
        context: Ctx,
        request: ObserveRequest,
    ) -> PluginResult<ObserveResponse, ObserveError> {
        match invoke_command(&self.config, &context, "memory.observe", &request)
            .await
            .map_err(PluginError::runtime)?
        {
            CommandOutcome::Result(response) => Ok(response),
            CommandOutcome::DomainError(error) if error == "invalid_request" => {
                Err(PluginError::domain(ObserveError::InvalidRequest))
            }
            CommandOutcome::DomainError(error) if error == "content_too_large" => {
                Err(PluginError::domain(ObserveError::ContentTooLarge))
            }
            CommandOutcome::DomainError(error) => {
                Err(PluginError::runtime(unknown_domain("observe", &error)))
            }
        }
    }

    async fn recall(
        &self,
        context: Ctx,
        request: RecallRequest,
    ) -> PluginResult<RecallResponse, RecallError> {
        match invoke_command(&self.config, &context, "memory.recall", &request)
            .await
            .map_err(PluginError::runtime)?
        {
            CommandOutcome::Result(response) => Ok(response),
            CommandOutcome::DomainError(error) if error == "invalid_request" => {
                Err(PluginError::domain(RecallError::InvalidRequest))
            }
            CommandOutcome::DomainError(error) if error == "content_too_large" => {
                Err(PluginError::domain(RecallError::ContentTooLarge))
            }
            CommandOutcome::DomainError(error) => {
                Err(PluginError::runtime(unknown_domain("recall", &error)))
            }
        }
    }

    async fn remember(
        &self,
        context: Ctx,
        request: RememberRequest,
    ) -> PluginResult<RememberResponse, RememberError> {
        match invoke_command(&self.config, &context, "memory.remember", &request)
            .await
            .map_err(PluginError::runtime)?
        {
            CommandOutcome::Result(response) => Ok(response),
            CommandOutcome::DomainError(error) if error == "invalid_request" => {
                Err(PluginError::domain(RememberError::InvalidRequest))
            }
            CommandOutcome::DomainError(error) if error == "content_too_large" => {
                Err(PluginError::domain(RememberError::ContentTooLarge))
            }
            CommandOutcome::DomainError(error) => {
                Err(PluginError::runtime(unknown_domain("remember", &error)))
            }
        }
    }

    async fn forget(
        &self,
        context: Ctx,
        request: ForgetRequest,
    ) -> PluginResult<ForgetResponse, ForgetError> {
        match invoke_command(&self.config, &context, "memory.forget", &request)
            .await
            .map_err(PluginError::runtime)?
        {
            CommandOutcome::Result(response) => Ok(response),
            CommandOutcome::DomainError(error) if error == "invalid_request" => {
                Err(PluginError::domain(ForgetError::InvalidRequest))
            }
            CommandOutcome::DomainError(error) if error == "content_too_large" => {
                Err(PluginError::domain(ForgetError::ContentTooLarge))
            }
            CommandOutcome::DomainError(error) => {
                Err(PluginError::runtime(unknown_domain("forget", &error)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_kernel::{CancellationToken, InvocationContext};

    fn plugin(script: &str) -> MemoryCommandPlugin {
        MemoryCommandPlugin {
            config: CommandAdapterConfig {
                program: "/bin/sh".into(),
                arguments: vec!["-c".to_owned(), script.to_owned()],
                timeout_ms: 1_000,
                max_response_bytes: 16_384,
            },
        }
    }

    fn context() -> InvocationContext {
        InvocationContext::new(1, None, CancellationToken::new())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_command_implements_memory_without_sqlite_internals() {
        let plugin = plugin(
            r#"read request
case "$request" in *'"operation":"memory.recall"'*) ;; *) exit 9;; esac
printf '%s\n' '{"protocol":"lenso.agent.command-adapter@1","result":{"items":[{"memory_id":"remote-1","content":"portable memory","source":{"session_id":"source-1","turn_id":"turn-1"},"confidence_milli":900}]}}'"#,
        );
        let response = plugin
            .recall(
                context(),
                RecallRequest {
                    session_id: "session-1".to_owned(),
                    query: "portable".to_owned(),
                    max_items: 4,
                    max_characters: 4096,
                },
            )
            .await
            .unwrap();
        assert_eq!(response.items[0].memory_id, "remote-1");
        assert_eq!(response.items[0].content, "portable memory");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_domain_errors_remain_typed() {
        let plugin = plugin(
            r#"read request
printf '%s\n' '{"protocol":"lenso.agent.command-adapter@1","error":"content_too_large"}'"#,
        );
        let error = plugin
            .remember(
                context(),
                RememberRequest {
                    session_id: "session-1".to_owned(),
                    content: "large".to_owned(),
                    confidence_milli: 800,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            PluginError::Domain(RememberError::ContentTooLarge)
        ));
    }
}

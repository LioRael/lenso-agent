//! Bounded command Adapter for typed Agent lifecycle events.

use std::{path::PathBuf, process::Stdio, time::Duration};

use lenso_capability_agent_lifecycle::{
    self as lifecycle_contract, LifecycleProvider, ObserveError, ObserveRequest, ObserveResponse,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};
use tokio::{io::AsyncWriteExt, process::Command};

const MAX_TOTAL_ARGUMENT_BYTES: usize = 16_384;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandConfig {
    program: PathBuf,
    arguments: Vec<String>,
    timeout_ms: u64,
}

fn validate_config(config: &CommandConfig) -> Result<(), RuntimeFailure> {
    let argument_bytes = config.arguments.iter().map(String::len).sum::<usize>();
    if !config.program.is_absolute()
        || config.arguments.len() > 64
        || argument_bytes > MAX_TOTAL_ARGUMENT_BYTES
        || !(1..=60_000).contains(&config.timeout_ms)
    {
        return Err(invalid_plan(
            "lifecycle command requires an absolute program, bounded arguments, and a 1..60000ms timeout",
        ));
    }
    Ok(())
}

#[lenso::plugin(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct LifecycleCommandPlugin {
    #[config]
    config: CommandConfig,
}

#[lenso::provides(lifecycle_contract::Lifecycle)]
impl LifecycleProvider for LifecycleCommandPlugin {
    fn observe(
        &self,
        context: InvocationContext,
        request: ObserveRequest,
    ) -> lenso_kernel::NativeRequestFuture<lifecycle_contract::Lifecycle> {
        let config = self.config.clone();
        Box::pin(async move { invoke(&config, &context, &request).await })
    }
}

async fn invoke(
    config: &CommandConfig,
    context: &InvocationContext,
    request: &ObserveRequest,
) -> Result<Result<ObserveResponse, ObserveError>, RuntimeFailure> {
    let payload = serde_json::to_vec(request).map_err(|error| RuntimeFailure::Internal {
        detail: format!("failed to encode lifecycle command input: {error}"),
    })?;
    let mut child = Command::new(&config.program)
        .args(&config.arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(command_failure)?;
    let mut stdin = child.stdin.take().ok_or_else(|| RuntimeFailure::Internal {
        detail: "lifecycle command has no stdin pipe".to_owned(),
    })?;
    stdin.write_all(&payload).await.map_err(command_failure)?;
    stdin.write_all(b"\n").await.map_err(command_failure)?;
    drop(stdin);

    let cancellation = context.cancellation();
    let request_id = context.request_id();
    let wait = child.wait();
    tokio::pin!(wait);
    let timeout = tokio::time::sleep(Duration::from_millis(config.timeout_ms));
    tokio::pin!(timeout);
    let cancelled = cancellation.cancelled();
    tokio::pin!(cancelled);
    let status = tokio::select! {
        () = &mut cancelled => return Err(RuntimeFailure::Cancelled { request_id }),
        () = &mut timeout => return Ok(Err(ObserveError::ObserverRejected)),
        status = &mut wait => status.map_err(command_failure)?,
    };
    if status.success() {
        Ok(Ok(ObserveResponse {}))
    } else {
        Ok(Err(ObserveError::ObserverRejected))
    }
}

fn command_failure(error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: format!("lifecycle command failed: {error}"),
    }
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_capability_agent_lifecycle::LifecycleEventKind;
    use lenso_kernel::CancellationToken;

    fn request() -> ObserveRequest {
        ObserveRequest {
            event_id: "session/session-1/session-started".to_owned(),
            kind: LifecycleEventKind::SessionStarted,
            session_id: "session-1".to_owned(),
            turn_id: Some(None),
            occurred_at: "2026-08-29T00:00:00Z".to_owned(),
            generation_spec_digest: format!("sha256:{}", "a".repeat(64)),
            payload_json: "{}".to_owned().try_into().unwrap(),
        }
    }

    #[test]
    fn command_configuration_rejects_shell_style_program_lookup() {
        let error = validate_config(&CommandConfig {
            program: "sh".into(),
            arguments: Vec::new(),
            timeout_ms: 1000,
        })
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn command_receives_one_json_event_and_controls_acceptance() {
        let context = InvocationContext::new(1, None, CancellationToken::new());
        let accepted = invoke(
            &CommandConfig {
                program: "/bin/sh".into(),
                arguments: vec!["-c".to_owned(), "read event; test -n \"$event\"".to_owned()],
                timeout_ms: 1000,
            },
            &context,
            &request(),
        )
        .await
        .unwrap();
        assert!(accepted.is_ok());

        let rejected = invoke(
            &CommandConfig {
                program: "/bin/sh".into(),
                arguments: vec!["-c".to_owned(), "exit 7".to_owned()],
                timeout_ms: 1000,
            },
            &context,
            &request(),
        )
        .await
        .unwrap();
        assert!(matches!(rejected, Err(ObserveError::ObserverRejected)));
    }
}

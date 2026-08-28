//! Small native-runtime helpers shared by Agent Harness Plugins.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::VecDeque,
    path::PathBuf,
    process::Stdio,
    time::Duration,
};

use futures::future::{LocalBoxFuture, ready};
use lenso_kernel::{NativeStreamItem, NativeStreamSession, RuntimeFailure};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

const COMMAND_PROTOCOL: &str = "lenso.agent.command-adapter@1";
const MAX_ARGUMENT_BYTES: usize = 16_384;

/// Shared bounded process configuration for command-backed Agent Adapters.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAdapterConfig {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub timeout_ms: u64,
    pub max_response_bytes: usize,
}

impl CommandAdapterConfig {
    /// Rejects shell lookup and unbounded command resources before readiness.
    pub fn validate(&self) -> Result<(), RuntimeFailure> {
        let argument_bytes = self.arguments.iter().map(String::len).sum::<usize>();
        if !self.program.is_absolute()
            || self.arguments.len() > 64
            || argument_bytes > MAX_ARGUMENT_BYTES
            || !(1..=120_000).contains(&self.timeout_ms)
            || !(1_024..=16_777_216).contains(&self.max_response_bytes)
        {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: "command Adapter requires an absolute program and bounded arguments, timeout, and response size".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandResponse<T> {
    protocol: String,
    result: Option<T>,
    error: Option<String>,
}

/// Successful result or one stable domain-error code returned by the command.
#[derive(Debug)]
pub enum CommandOutcome<T> {
    Result(T),
    DomainError(String),
}

/// Invokes one exact executable with a bounded JSON stdin/stdout exchange.
pub async fn invoke_command<Request, Response>(
    config: &CommandAdapterConfig,
    context: &lenso_kernel::InvocationContext,
    operation: &str,
    request: &Request,
) -> Result<CommandOutcome<Response>, RuntimeFailure>
where
    Request: Serialize,
    Response: DeserializeOwned,
{
    #[derive(Serialize)]
    struct Envelope<'a, T> {
        protocol: &'static str,
        operation: &'a str,
        request: &'a T,
    }
    let mut payload = serde_json::to_vec(&Envelope {
        protocol: COMMAND_PROTOCOL,
        operation,
        request,
    })
    .map_err(|error| RuntimeFailure::Internal {
        detail: format!("failed to encode command Adapter input: {error}"),
    })?;
    payload.push(b'\n');
    let mut child = Command::new(&config.program)
        .args(&config.arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(command_failure)?;
    let mut stdin = child.stdin.take().ok_or_else(|| RuntimeFailure::Internal {
        detail: "command Adapter has no stdin pipe".to_owned(),
    })?;
    stdin.write_all(&payload).await.map_err(command_failure)?;
    drop(stdin);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RuntimeFailure::Internal {
            detail: "command Adapter has no stdout pipe".to_owned(),
        })?;
    let limit = u64::try_from(config.max_response_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let read = async move {
        let mut output = Vec::new();
        stdout
            .take(limit)
            .read_to_end(&mut output)
            .await
            .map_err(command_failure)?;
        let status = child.wait().await.map_err(command_failure)?;
        Ok::<_, RuntimeFailure>((status, output))
    };
    let cancellation = context.cancellation();
    let request_id = context.request_id();
    let (status, output) = tokio::select! {
        () = cancellation.cancelled() => return Err(RuntimeFailure::Cancelled { request_id }),
        () = tokio::time::sleep(Duration::from_millis(config.timeout_ms)) => {
            return Err(RuntimeFailure::PluginFailure { detail: "command Adapter timed out".to_owned() });
        }
        result = read => result?,
    };
    if !status.success() {
        return Err(RuntimeFailure::PluginFailure {
            detail: format!("command Adapter exited with {status}"),
        });
    }
    if output.len() > config.max_response_bytes {
        return Err(RuntimeFailure::PluginFailure {
            detail: "command Adapter response exceeded its configured bound".to_owned(),
        });
    }
    let response =
        serde_json::from_slice::<CommandResponse<Response>>(&output).map_err(|error| {
            RuntimeFailure::PluginFailure {
                detail: format!("command Adapter returned invalid JSON: {error}"),
            }
        })?;
    if response.protocol != COMMAND_PROTOCOL
        || response.result.is_some() == response.error.is_some()
    {
        return Err(RuntimeFailure::PluginFailure {
            detail: "command Adapter returned an invalid response envelope".to_owned(),
        });
    }
    Ok(match (response.result, response.error) {
        (Some(result), None) => CommandOutcome::Result(result),
        (None, Some(error)) if !error.is_empty() && error.len() <= 128 => {
            CommandOutcome::DomainError(error)
        }
        _ => {
            return Err(RuntimeFailure::PluginFailure {
                detail: "command Adapter returned an invalid domain error".to_owned(),
            });
        }
    })
}

fn command_failure(error: impl std::fmt::Display) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: format!("command Adapter failed: {error}"),
    }
}

/// Finite server-output stream used by deterministic and fully computed turns.
#[derive(Debug)]
pub struct FiniteOutputStream {
    capability: &'static str,
    events: RefCell<VecDeque<NativeStreamItem>>,
    cancelled: Cell<bool>,
    send_closed: Cell<bool>,
}

impl FiniteOutputStream {
    /// Builds an ordered stream and appends peer-half-close plus terminal success.
    pub fn successful<M: Any>(capability: &'static str, messages: Vec<M>) -> Self {
        let mut events = messages
            .into_iter()
            .map(|message| NativeStreamItem::Message(Box::new(message) as Box<dyn Any>))
            .collect::<VecDeque<_>>();
        events.push_back(NativeStreamItem::PeerHalfClosed);
        events.push_back(NativeStreamItem::Terminal(Ok(())));
        Self {
            capability,
            events: RefCell::new(events),
            cancelled: Cell::new(false),
            send_closed: Cell::new(false),
        }
    }
}

impl NativeStreamSession for FiniteOutputStream {
    fn send(&self, _message: Box<dyn Any>) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        Box::pin(ready(Err(RuntimeFailure::ProtocolViolation {
            capability: self.capability,
        })))
    }

    fn receive(&self) -> LocalBoxFuture<'static, Result<NativeStreamItem, RuntimeFailure>> {
        let result = if self.cancelled.get() {
            Err(RuntimeFailure::AdmissionClosed)
        } else {
            self.events
                .borrow_mut()
                .pop_front()
                .ok_or(RuntimeFailure::ProtocolViolation {
                    capability: self.capability,
                })
        };
        Box::pin(ready(result))
    }

    fn close_send(&self) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        let result = if self.send_closed.replace(true) {
            Err(RuntimeFailure::ProtocolViolation {
                capability: self.capability,
            })
        } else {
            Ok(())
        };
        Box::pin(ready(result))
    }

    fn cancel(&self) {
        self.cancelled.set(true);
        self.events.borrow_mut().clear();
    }
}

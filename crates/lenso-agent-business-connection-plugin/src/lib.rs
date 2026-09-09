//! Owns business grants; no credential crosses the `AuthConnection` surface.
mod state;
use lenso::ManagedTasks;
use lenso_capability_agent_auth_connection as auth;
use lenso_capability_agent_tool_provider as tools;
use lenso_capability_agent_turn_binding as binding;
use lenso_kernel::{InvocationContext, RuntimeFailure};
use serde::{Deserialize, Serialize};
use state::{Attempt, Grant, State};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Serialize, Deserialize, lenso::PluginConfig)]
#[serde(deny_unknown_fields)]
pub struct BusinessConnectionConfig {
    /// Clean HTTPS or loopback origin, selected by the App owner, never a Tool argument.
    pub origin: String,
    pub label: String,
}
fn validate(config: &BusinessConnectionConfig) -> Result<(), RuntimeFailure> {
    let url = reqwest::Url::parse(&config.origin).map_err(|_| failure())?;
    if url.origin().ascii_serialization() != config.origin
        || !(url.scheme() == "https"
            || url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
        || config.label.trim().is_empty()
        || config.label.len() > 100
    {
        return Err(failure());
    }
    Ok(())
}
#[lenso::plugin(validate = validate)]
#[derive(Clone, Debug)]
struct BusinessConnection {
    #[config]
    config: BusinessConnectionConfig,
    state: Arc<Mutex<State>>,
    #[tasks]
    tasks: ManagedTasks,
}
fn failure() -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: "Business App connection unavailable".into(),
    }
}
fn client() -> Result<reqwest::Client, RuntimeFailure> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| failure())
}
/// Never surface remote bodies or HTTP errors that may carry credentials.
async fn response<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
    secret: Option<&str>,
) -> Result<T, RuntimeFailure> {
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|size| size > 2_097_152)
    {
        return Err(failure());
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await.map_err(|_| failure())? {
        if bytes.len().saturating_add(chunk.len()) > 2_097_152 {
            return Err(failure());
        }
        bytes.extend_from_slice(&chunk);
    }
    if secret.is_some_and(|secret| {
        !secret.is_empty()
            && bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
    }) {
        return Err(failure());
    }
    serde_json::from_slice(&bytes).map_err(|_| failure())
}
#[lenso::provides(auth::AuthConnection, binding::TurnBinding, tools::ToolProvider)]
impl BusinessConnection {
    async fn status(
        &self,
        _context: InvocationContext,
        _request: auth::StatusRequest,
    ) -> Result<Result<auth::StatusResponse, auth::StatusError>, RuntimeFailure> {
        Ok(Ok(auth::StatusResponse {
            label: self.config.label.clone(),
            connected: self
                .state
                .lock()
                .await
                .grant
                .as_ref()
                .is_some_and(|grant| grant.valid()),
            methods: vec![auth::LoginMethod::BrowserConsent],
        }))
    }
    async fn begin(
        &self,
        context: InvocationContext,
        request: auth::BeginRequest,
    ) -> Result<Result<auth::BeginResponse, auth::BeginError>, RuntimeFailure> {
        if !matches!(request.method, auth::LoginMethod::BrowserConsent) {
            return Ok(Err(auth::BeginError::UnsupportedMethod));
        }
        let mut state = self.state.lock().await;
        if state.attempt.as_ref().is_some_and(Attempt::valid) {
            return Ok(Err(auth::BeginError::AttemptInProgress));
        }
        let cancellation = context.cancellation();
        let remote = tokio::select! {
            () = cancellation.cancelled() => return Err(failure()),
            result = async { response::<state::Begin>(client()?.post(format!("{}/auth/agent/connection/begin", self.config.origin)).send().await.map_err(|_| failure())?, None).await } => result?,
        };
        let attempt = Attempt::new(remote, &self.config.origin)?;
        let presentation = attempt.presentation();
        state.attempt = Some(attempt);
        Ok(Ok(presentation))
    }
    async fn poll(
        &self,
        context: InvocationContext,
        request: auth::AttemptRequest,
    ) -> Result<Result<auth::PollResponse, auth::PollError>, RuntimeFailure> {
        let mut state = self.state.lock().await;
        let Some(attempt) = state
            .attempt
            .as_ref()
            .filter(|a| a.id == request.attempt_id && a.valid())
        else {
            return Ok(Err(auth::PollError::UnknownAttempt));
        };
        if attempt.connected {
            return Ok(Ok(auth::PollResponse {
                state: auth::LoginState::Connected,
            }));
        }
        let cancellation = context.cancellation();
        let remote: state::Poll = tokio::select! {
            () = cancellation.cancelled() => return Err(failure()),
            result = async { response(client()?.post(format!("{}/auth/agent/connection/poll", self.config.origin))
                .json(&serde_json::json!({"attempt_id":attempt.remote_id,"polling_secret":attempt.secret.as_str()})).send().await.map_err(|_| failure())?, None).await } => result?,
        };
        let status = match remote.state.as_str() {
            "pending" => auth::LoginState::Pending,
            "connected" => {
                let grant = Grant::new(remote.grant.ok_or_else(failure)?)?;
                state.grant = Some(Arc::new(grant));
                if let Some(attempt) = state.attempt.as_mut() {
                    attempt.connected = true;
                }
                auth::LoginState::Connected
            }
            "failed" => {
                state.attempt = None;
                auth::LoginState::Failed
            }
            _ => return Err(failure()),
        };
        Ok(Ok(auth::PollResponse { state: status }))
    }
    async fn cancel(
        &self,
        _context: InvocationContext,
        request: auth::AttemptRequest,
    ) -> Result<Result<auth::CancelResponse, auth::CancelError>, RuntimeFailure> {
        let mut state = self.state.lock().await;
        let matches = state
            .attempt
            .as_ref()
            .is_some_and(|a| a.id == request.attempt_id && !a.connected);
        if matches {
            state.attempt = None;
        }
        Ok(Ok(auth::CancelResponse { cancelled: matches }))
    }
    async fn disconnect(
        &self,
        _context: InvocationContext,
        _request: auth::DisconnectRequest,
    ) -> Result<Result<auth::DisconnectResponse, auth::DisconnectError>, RuntimeFailure> {
        let mut state = self.state.lock().await;
        state.attempt = None;
        state.grant = None;
        Ok(Ok(auth::DisconnectResponse { disconnected: true }))
    }
    async fn capture(
        &self,
        context: InvocationContext,
        request: binding::CaptureRequest,
    ) -> Result<Result<binding::CaptureResponse, binding::CaptureError>, RuntimeFailure> {
        let id = uuid::Uuid::parse_str(&request.scope_id).map_err(|_| failure())?;
        let mut state = self.state.lock().await;
        state.prune();
        if state.turns.len() >= 1024 || state.turns.contains_key(&id) {
            return Ok(Err(binding::CaptureError::CapacityExceeded));
        }
        let grant = state.grant.clone().filter(|grant| grant.valid());
        state.turns.insert(
            id,
            state::Turn {
                grant,
                cancellation: context.cancellation(),
                created: std::time::Instant::now(),
            },
        );
        let weak = Arc::downgrade(&self.state);
        let cancellation = context.cancellation();
        let shutdown = self.tasks.cancellation().map_err(|_| failure())?;
        if self
            .tasks
            .spawn_local(async move {
                tokio::select! {
                    () = cancellation.cancelled() => {},
                    () = shutdown.cancelled() => {},
                    () = tokio::time::sleep(Duration::from_secs(3600)) => {},
                }
                if let Some(state) = weak.upgrade() {
                    state.lock().await.turns.remove(&id);
                }
            })
            .is_err()
        {
            state.turns.remove(&id);
            return Err(failure());
        }
        Ok(Ok(binding::CaptureResponse {}))
    }
    async fn catalog(
        &self,
        context: InvocationContext,
        _request: tools::CatalogRequest,
    ) -> Result<Result<tools::CatalogResponse, tools::CatalogError>, RuntimeFailure> {
        let cancellation = context.cancellation();
        let catalog = tokio::select! {
            () = cancellation.cancelled() => return Err(failure()),
            result = async { response(client()?.get(format!("{}/projects/agent/manifest", self.config.origin)).send().await.map_err(|_| failure())?, None).await } => result?,
        };
        Ok(Ok(catalog))
    }
    async fn execute(
        &self,
        context: InvocationContext,
        request: tools::ExecuteRequest,
    ) -> Result<Result<tools::ExecuteResponse, tools::ExecuteError>, RuntimeFailure> {
        let Some(grant) = self.state.lock().await.for_context(&context, true)? else {
            return Ok(Err(tools::ExecuteError::PermissionDenied));
        };
        if !request.name.starts_with("projects_") {
            return Ok(Err(tools::ExecuteError::NotFound));
        }
        let cancellation = context.cancellation();
        let result = tokio::select! {
            () = cancellation.cancelled() => return Err(failure()),
            result = client()?.post(format!("{}/projects/agent/tools/execute", self.config.origin))
                .bearer_auth(grant.credential.as_str()).json(&request).send() => result.map_err(|_| failure())?,
        };
        match result.status().as_u16() {
            401 | 403 => Ok(Err(tools::ExecuteError::PermissionDenied)),
            404 => Ok(Err(tools::ExecuteError::NotFound)),
            400 => Ok(Err(tools::ExecuteError::InvalidArguments)),
            413 => Ok(Err(tools::ExecuteError::OutputLimitExceeded)),
            422 => {
                // Decode the explicit domain error, never stringify transport failures.
                let mut result = result;
                let mut bytes = zeroize::Zeroizing::new(Vec::new());
                while let Some(chunk) = result.chunk().await.map_err(|_| failure())? {
                    if bytes.len().saturating_add(chunk.len()) > 131_072 {
                        return Err(failure());
                    }
                    bytes.extend_from_slice(&chunk);
                }
                if bytes
                    .windows(grant.credential.len())
                    .any(|w| w == grant.credential.as_bytes())
                {
                    return Err(failure());
                }
                let error: tools::ExecuteError =
                    serde_json::from_slice(&bytes).map_err(|_| failure())?;
                if matches!(error, tools::ExecuteError::ExecutionFailed { .. }) {
                    Ok(Err(error))
                } else {
                    Err(failure())
                }
            }
            _ => Ok(Ok(response(result, Some(&grant.credential)).await?)),
        }
    }
}

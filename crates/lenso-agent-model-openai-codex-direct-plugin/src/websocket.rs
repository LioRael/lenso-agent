//! Private, generation-local Responses transport. A lease owns one in-flight response.

mod continuation;
use continuation::Checkpoint;

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::VecDeque,
    fmt,
    rc::Rc,
    time::{Duration, Instant},
};

use futures::{
    FutureExt, SinkExt, StreamExt,
    future::{LocalBoxFuture, ready},
    lock::Mutex,
};
use lenso_capability_agent_auth_openai_codex::AccessResponse;
use lenso_kernel::{CancellationToken, NativeStreamItem, NativeStreamSession, RuntimeFailure};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{self, Message, client::IntoClientRequest, protocol::WebSocketConfig},
};

use super::{
    CAPABILITY_ID, CompleteError, DirectModelConfig, ModelCompleteInvocationError,
    ResponsesDecoder, ResponsesRequest, map_status, protocol_failure, provider_failure,
};

const MAX_CONNECTIONS: usize = 4;
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECTION_LIFETIME: Duration = Duration::from_mins(50);
const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
const READ_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Transport {
    Sse,
    #[default]
    Websocket,
    Auto,
}

#[derive(Clone, Default)]
pub(super) struct Pool(Rc<RefCell<PoolState>>);

#[derive(Default)]
struct PoolState {
    idle: Vec<Connection>,
    active: usize,
    identity: Option<[u8; 32]>,
    closed: bool,
}

impl fmt::Debug for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.0.borrow();
        f.debug_struct("WebSocketPool")
            .field("active", &state.active)
            .field("idle", &state.idle.len())
            .field("closed", &state.closed)
            .finish()
    }
}

struct Connection {
    socket: Socket,
    identity: [u8; 32],
    created: Instant,
    idle_since: Instant,
    checkpoint: Option<Checkpoint>,
}

struct Lease {
    pool: Pool,
    connection: Option<Connection>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.pool.0.borrow_mut().active -= 1;
    }
}

impl Lease {
    fn recycle(&mut self) {
        if let Some(mut connection) = self.connection.take() {
            let mut state = self.pool.0.borrow_mut();
            if !state.closed
                && state.identity == Some(connection.identity)
                && connection.created.elapsed() < CONNECTION_LIFETIME
            {
                connection.idle_since = Instant::now();
                state.idle.push(connection);
            }
        }
    }
}

#[derive(Debug)]
pub(super) enum OpenError {
    Unsupported,
    Model(ModelCompleteInvocationError),
}

impl OpenError {
    pub(super) fn into_model_error(self) -> ModelCompleteInvocationError {
        match self {
            Self::Unsupported => provider_failure(
                "websocket_unsupported",
                "direct Codex WebSocket transport is unavailable",
                false,
            ),
            Self::Model(error) => error,
        }
    }
}

impl Pool {
    pub(super) fn prune_idle(&self) {
        self.0.borrow_mut().idle.retain(|connection| {
            connection.idle_since.elapsed() < IDLE_TIMEOUT
                && connection.created.elapsed() < CONNECTION_LIFETIME
        });
    }

    pub(super) fn shutdown(&self) {
        let mut state = self.0.borrow_mut();
        state.closed = true;
        state.idle.clear();
    }

    fn reserve(&self, identity: [u8; 32], scope: Option<&str>) -> Result<Lease, OpenError> {
        let mut state = self.0.borrow_mut();
        if state.closed {
            return Err(OpenError::Model(ModelCompleteInvocationError::Runtime(
                RuntimeFailure::AdmissionClosed,
            )));
        }
        if state.active >= MAX_CONNECTIONS {
            return Err(OpenError::Model(ModelCompleteInvocationError::Domain(
                CompleteError::Overloaded,
            )));
        }
        if state.identity != Some(identity) {
            state.idle.clear();
            state.identity = Some(identity);
        }
        state.idle.retain(|connection| {
            connection.idle_since.elapsed() < IDLE_TIMEOUT
                && connection.created.elapsed() < CONNECTION_LIFETIME
        });
        state.active += 1;
        let preferred = state.idle.iter().position(|connection| {
            connection
                .checkpoint
                .as_ref()
                .is_some_and(|checkpoint| Some(checkpoint.scope.as_str()) == scope)
        });
        let connection = match preferred {
            Some(index) => Some(state.idle.swap_remove(index)),
            None => state.idle.pop(),
        };
        Ok(Lease {
            pool: self.clone(),
            connection,
        })
    }

    pub(super) async fn open(
        &self,
        config: &DirectModelConfig,
        credential: &AccessResponse,
        request: &ResponsesRequest,
    ) -> Result<ResponseStream, OpenError> {
        let identity: [u8; 32] = Sha256::digest(format!(
            "{}\0{}\0{}",
            config.base_url, credential.account_id, credential.access_token
        ))
        .into();
        let full_message = create_message(request)?;
        let mut lease = self.reserve(identity, request.continuation_scope.as_deref())?;
        // An idle server may have closed or sent control frames. Never let a late
        // response frame become part of the next logical completion.
        if let Some(connection) = lease.connection.as_mut()
            && !idle_is_clean(&mut connection.socket).await
        {
            lease.connection = None;
        }
        if lease.connection.is_none() {
            let socket = connect(config, credential).await?;
            lease.connection = Some(Connection {
                socket,
                identity,
                created: Instant::now(),
                idle_since: Instant::now(),
                checkpoint: None,
            });
        }
        let Some(connection) = lease.connection.as_mut() else {
            return Err(open_failure("websocket_closed", false));
        };
        let incremental = connection
            .checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.incremental_body(request));
        let recovery = incremental.as_ref().map(|_| full_message.clone());
        let message = match incremental {
            Some(body) => encode_message(body)?,
            None => full_message,
        };
        connection.checkpoint = None;
        // Once sending starts, failure has an unknown acceptance outcome. It is
        // not a retryable open failure, even if no output reached the caller.
        tokio::time::timeout(OPEN_TIMEOUT, connection.socket.send(message))
            .await
            .map_err(|_| open_failure("websocket_send_timeout", false))?
            .map_err(|_| open_failure("websocket_send_failed", false))?;
        Ok(ResponseStream {
            lease: Rc::new(Mutex::new(Some(lease))),
            decoder: Rc::new(RefCell::new(ResponsesDecoder::new(
                config.max_event_bytes,
                request.provider_to_lenso_tool_names.clone(),
            ))),
            events: Rc::new(RefCell::new(VecDeque::new())),
            cancellation: CancellationToken::new(),
            send_closed: Cell::new(false),
            max_event_bytes: config.max_event_bytes,
            checkpoint: Rc::new(RefCell::new(Checkpoint::start(request))),
            recovery: Rc::new(RefCell::new(recovery)),
        })
    }
}

fn create_message(request: &ResponsesRequest) -> Result<Message, OpenError> {
    encode_message(request.body.clone())
}

fn encode_message(mut body: serde_json::Value) -> Result<Message, OpenError> {
    let Some(object) = body.as_object_mut() else {
        return Err(open_failure("websocket_invalid_request", false));
    };
    object.remove("stream");
    object.remove("background");
    object.insert("type".into(), "response.create".into());
    let encoded = body.to_string();
    if encoded.len() > MAX_REQUEST_BYTES {
        return Err(OpenError::Model(ModelCompleteInvocationError::Domain(
            CompleteError::InvalidRequest,
        )));
    }
    Ok(Message::Text(encoded.into()))
}

async fn idle_is_clean(socket: &mut Socket) -> bool {
    for _ in 0..16 {
        match socket.next().now_or_never() {
            None => return true,
            Some(Some(Ok(Message::Ping(_)))) => {
                if !matches!(
                    tokio::time::timeout(OPEN_TIMEOUT, socket.flush()).await,
                    Ok(Ok(()))
                ) {
                    return false;
                }
            }
            Some(Some(Ok(Message::Pong(_)))) => {}
            _ => return false,
        }
    }
    false
}

async fn connect(
    config: &DirectModelConfig,
    credential: &AccessResponse,
) -> Result<Socket, OpenError> {
    let mut url = config
        .endpoint()
        .map_err(|error| OpenError::Model(ModelCompleteInvocationError::Runtime(error)))?;
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme)
        .map_err(|()| open_failure("websocket_invalid_url", false))?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| open_failure("websocket_invalid_url", false))?;
    for (name, value) in [
        (
            "authorization",
            format!("Bearer {}", credential.access_token),
        ),
        ("chatgpt-account-id", credential.account_id.clone()),
        ("originator", "lenso".to_owned()),
        ("user-agent", "lenso-agent/0.1.0".to_owned()),
        ("openai-beta", "responses_websockets=2026-02-06".to_owned()),
    ] {
        let mut value = tungstenite::http::HeaderValue::from_str(&value)
            .map_err(|_| open_failure("websocket_invalid_header", false))?;
        value.set_sensitive(true);
        request.headers_mut().insert(name, value);
    }
    let websocket_config = WebSocketConfig::default()
        .max_message_size(Some(config.max_event_bytes))
        .max_frame_size(Some(config.max_event_bytes))
        .max_write_buffer_size(MAX_REQUEST_BYTES + 131_072);
    let result = tokio::time::timeout(
        OPEN_TIMEOUT,
        connect_async_tls_with_config(
            request,
            Some(websocket_config),
            true,
            Some(tls_connector()?),
        ),
    )
    .await
    .map_err(|_| open_failure("websocket_connect_timeout", true))?;
    match result {
        Ok((socket, _)) => Ok(socket),
        Err(tungstenite::Error::Http(response)) if response.status().as_u16() == 426 => {
            Err(OpenError::Unsupported)
        }
        Err(tungstenite::Error::Http(response)) => {
            Err(OpenError::Model(map_status(response.status())))
        }
        Err(_) => Err(open_failure("websocket_connect_failed", true)),
    }
}

fn open_failure(code: &str, retryable: bool) -> OpenError {
    OpenError::Model(provider_failure(
        code,
        "direct Codex WebSocket request failed",
        retryable,
    ))
}

fn tls_connector() -> Result<Connector, OpenError> {
    // Reqwest and other Host plugins can enable a different Rustls provider.
    // Select locally; never install or replace the process-wide default.
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| open_failure("websocket_tls_configuration", false))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Connector::Rustls(std::sync::Arc::new(config)))
}

pub(super) struct ResponseStream {
    lease: Rc<Mutex<Option<Lease>>>,
    decoder: Rc<RefCell<ResponsesDecoder>>,
    events: Rc<RefCell<VecDeque<NativeStreamItem>>>,
    cancellation: CancellationToken,
    send_closed: Cell<bool>,
    max_event_bytes: usize,
    checkpoint: Rc<RefCell<Option<Checkpoint>>>,
    recovery: Rc<RefCell<Option<Message>>>,
}

impl fmt::Debug for ResponseStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebSocketResponseStream")
            .finish_non_exhaustive()
    }
}

impl Drop for ResponseStream {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl NativeStreamSession for ResponseStream {
    fn send(&self, _message: Box<dyn Any>) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        Box::pin(ready(Err(RuntimeFailure::ProtocolViolation {
            capability: CAPABILITY_ID,
        })))
    }

    fn close_send(&self) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        Box::pin(ready(if self.send_closed.replace(true) {
            Err(RuntimeFailure::ProtocolViolation {
                capability: CAPABILITY_ID,
            })
        } else {
            Ok(())
        }))
    }

    fn cancel(&self) {
        self.cancellation.cancel();
        self.events.borrow_mut().clear();
        if let Some(mut lease) = self.lease.try_lock() {
            lease.take();
        }
    }

    fn receive(&self) -> LocalBoxFuture<'static, Result<NativeStreamItem, RuntimeFailure>> {
        let lease = self.lease.clone();
        let decoder = self.decoder.clone();
        let events = self.events.clone();
        let cancellation = self.cancellation.clone();
        let max_event_bytes = self.max_event_bytes;
        let checkpoint = self.checkpoint.clone();
        let recovery = self.recovery.clone();
        Box::pin(async move {
            let mut lease = lease.lock().await;
            loop {
                if cancellation.is_cancelled() {
                    lease.take();
                    return Err(RuntimeFailure::AdmissionClosed);
                }
                if let Some(event) = events.borrow_mut().pop_front() {
                    return Ok(event);
                }
                let Some(connection) = lease.as_mut().and_then(|lease| lease.connection.as_mut())
                else {
                    return Err(RuntimeFailure::ProtocolViolation {
                        capability: CAPABILITY_ID,
                    });
                };
                let next = tokio::select! {
                    () = cancellation.cancelled() => { lease.take(); return Err(RuntimeFailure::AdmissionClosed); }
                    next = tokio::time::timeout(READ_TIMEOUT, read_event(&mut connection.socket, max_event_bytes)) => next,
                };
                // An explicit cache miss rejects this create before generation.
                // Recover once with full input, never on an ambiguous disconnect.
                if let Ok(Ok(event)) = &next {
                    let cache_miss = event.get("type").and_then(serde_json::Value::as_str)
                        == Some("error")
                        && event
                            .get("error")
                            .and_then(|error| error.get("code"))
                            .and_then(serde_json::Value::as_str)
                            == Some("previous_response_not_found");
                    let full = recovery.borrow_mut().take();
                    if cache_miss && let Some(full) = full {
                        let sent = tokio::select! {
                            () = cancellation.cancelled() => { lease.take(); return Err(RuntimeFailure::AdmissionClosed); }
                            sent = tokio::time::timeout(OPEN_TIMEOUT, connection.socket.send(full)) => sent,
                        };
                        if !matches!(sent, Ok(Ok(()))) {
                            lease.take();
                            return Err(protocol_failure(
                                "direct Codex continuation recovery send failed",
                            ));
                        }
                        continue;
                    }
                }
                let mut output = Vec::new();
                let result = match next {
                    Ok(Ok(event)) => {
                        let result = decoder.borrow_mut().decode_event(&event, &mut output);
                        let mut candidate = checkpoint.borrow_mut();
                        if candidate
                            .as_mut()
                            .is_some_and(|candidate| !candidate.observe(&event))
                        {
                            candidate.take();
                        }
                        result
                    }
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(protocol_failure("direct Codex WebSocket stream timed out")),
                };
                if let Err(error) = result {
                    lease.take();
                    return Err(error);
                }
                if decoder.borrow().terminal
                    && let Some(mut completed) = lease.take()
                {
                    if let Some(connection) = completed.connection.as_mut() {
                        connection.checkpoint = checkpoint.borrow_mut().take();
                    }
                    completed.recycle();
                }
                events.borrow_mut().extend(output);
            }
        })
    }
}

async fn read_event(
    socket: &mut Socket,
    max_event_bytes: usize,
) -> Result<serde_json::Value, RuntimeFailure> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) if text.len() <= max_event_bytes => {
                return serde_json::from_str(&text)
                    .map_err(|_| protocol_failure("direct Codex emitted invalid WebSocket JSON"));
            }
            Some(Ok(Message::Ping(_))) => socket
                .flush()
                .await
                .map_err(|_| protocol_failure("direct Codex WebSocket ping failed"))?,
            Some(Ok(Message::Pong(_))) => {}
            _ => {
                return Err(protocol_failure(
                    "direct Codex WebSocket ended before completion",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests;

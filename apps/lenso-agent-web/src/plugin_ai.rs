//! Optional Host-authenticated, session-free completion transport.
use super::{ApiProblem, RuntimeCommand, WebRuntime};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::HeaderMap,
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use lenso_agent_host::generation::{AgentApp, plugin_ai::CompletionGeneration};
use lenso_auth_sdk::{ActorAssertion, ActorAssertionVerifier, AuthOutcome};
use lenso_capability_agent_model::{
    CAPABILITY_ID, COMPLETE_OPERATION, CompleteMessageKind, CompleteOpen,
};
use lenso_kernel::{CancellationToken, StreamEvent};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::oneshot;

struct VerifiedActor;
impl lenso_auth_sdk::TypedActor for VerifiedActor {
    fn from_assertion(_: &ActorAssertion) -> Result<Self, lenso_auth_sdk::ActorProjectionError> {
        Ok(Self)
    }
}

#[derive(Clone, Debug)]
struct Signal(tokio::sync::watch::Sender<bool>);
impl Signal {
    fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
    fn cancel(&self) {
        self.0.send_replace(true);
    }
    async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct BridgeAuthority {
    verifier: ActorAssertionVerifier,
    active: Arc<Mutex<BTreeMap<String, AdmittedRun>>>,
    terminal: Arc<Mutex<BTreeMap<String, String>>>,
    #[cfg(test)]
    cancel_fault: Option<std::path::PathBuf>,
}

/// Stop-only authority is the exact assertion snapshot verified at admission.
/// Its digest confers no permission to start, recover or inspect another run.
#[derive(Clone, Debug)]
struct AdmittedRun {
    subject: String,
    stop_assertion: [u8; 32],
    cancellation: Signal,
}
fn assertion_snapshot(headers: &HeaderMap) -> Option<[u8; 32]> {
    use sha2::{Digest as _, Sha256};
    let header = headers.get("x-lenso-actor")?.to_str().ok()?;
    (header.len() <= 16384).then(|| Sha256::digest(header.as_bytes()).into())
}

impl BridgeAuthority {
    /// Loads only an existing realm verification authority, never a signing key.
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Authority {
            realm: String,
            issuer: String,
            public_key: String,
        }
        if !path.is_absolute() || std::fs::metadata(path).map_err(|e| e.to_string())?.len() > 16384
        {
            return Err("bounded absolute authority file required".into());
        }
        let authority: Authority =
            serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if authority.realm != "operators" {
            return Err("operators verification authority required".into());
        }
        Self::operators(&authority.issuer, &authority.public_key)
    }
    /// Uses an existing operators issuer/public key; never generates credentials.
    pub fn operators(issuer: &str, public_key: &str) -> Result<Self, String> {
        Ok(Self {
            verifier: ActorAssertionVerifier::from_public_key_base64(issuer, public_key)
                .map_err(|_| "invalid existing operators authority".to_owned())?,
            active: Arc::new(Mutex::new(BTreeMap::new())),
            terminal: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(test)]
            cancel_fault: None,
        })
    }

    fn actor(&self, headers: &HeaderMap) -> Result<ActorAssertion, ApiProblem> {
        self.actor_for(headers, CAPABILITY_ID, COMPLETE_OPERATION)
    }
    fn actor_for(
        &self,
        headers: &HeaderMap,
        capability: &str,
        operation: &str,
    ) -> Result<ActorAssertion, ApiProblem> {
        let encoded = headers
            .get("x-lenso-actor")
            .and_then(|h| h.to_str().ok())
            .filter(|s| s.len() <= 16384)
            .ok_or_else(|| ApiProblem::forbidden("Host actor required"))?;
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| ApiProblem::forbidden("invalid actor"))?;
        let wire =
            serde_json::from_slice(&bytes).map_err(|_| ApiProblem::forbidden("invalid actor"))?;
        let response = lenso_capability_auth::AuthResponse {
            kind: lenso_capability_auth::AuthResponseKind::Authenticated,
            assertion: Some(wire),
        };
        let AuthOutcome::Authenticated(actor) = lenso_auth_sdk::decode_auth_response(response)
            .map_err(|_| ApiProblem::forbidden("invalid actor"))?
        else {
            return Err(ApiProblem::forbidden("Host actor required"));
        };
        let context = actor
            .clone()
            .attach(lenso_kernel::InvocationContext::new(
                1,
                None,
                CancellationToken::new(),
            ))
            .map_err(|_| ApiProblem::forbidden("invalid actor context"))?;
        self.verifier
            .project_context::<VerifiedActor>(
                &context,
                capability,
                operation,
                &lenso_auth_sdk::FixedClock::new(time::OffsetDateTime::now_utc()),
            )
            .map_err(|_| ApiProblem::forbidden("actor authority or audience denied"))?;
        if actor.actor_kind() != "user" {
            return Err(ApiProblem::forbidden("user actor required"));
        }
        let wire = actor.to_wire();
        let issued = time::OffsetDateTime::parse(
            &wire.issued_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| ApiProblem::forbidden("actor validity denied"))?;
        let expires = time::OffsetDateTime::parse(
            &wire.expires_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| ApiProblem::forbidden("actor validity denied"))?;
        if expires - issued > time::Duration::hours(1) {
            return Err(ApiProblem::forbidden("actor TTL denied"));
        }
        Ok(actor)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub run_id: String,
    pub model: String,
    pub prompt: String,
    pub max_output: u64,
    pub generation: Option<String>,
    #[serde(default)]
    pub input_ceiling: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct Quote {
    pub model: String,
    pub provider_instance: String,
    pub generation: String,
    pub input_ceiling: u64,
}

#[derive(Debug, Serialize)]
pub struct Reply {
    pub binding: Quote,
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub(super) struct CompletionCommand {
    actor: ActorAssertion,
    input: Input,
    quote_only: bool,
    cancellation: Signal,
    reply: oneshot::Sender<Result<serde_json::Value, String>>,
    active: ActiveRun,
    task: Option<RunFields>,
    session: Option<SessionInput>,
}

impl std::fmt::Debug for CompletionCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompletionCommand")
            .field("run_id", &self.input.run_id)
            .field("model", &self.input.model)
            .field("quote_only", &self.quote_only)
            .finish_non_exhaustive()
    }
}

pub(super) fn routes() -> Router<WebRuntime> {
    Router::new()
        .route("/api/console/v1/agent/plugin-ai/runs", post(run))
        .route("/api/console/v1/agent/plugin-ai/session", post(session))
        .route("/api/console/v1/agent/plugin-ai/quote", post(quote))
        .route(
            "/api/console/v1/agent/plugin-ai/completions",
            post(complete),
        )
        .route(
            "/api/console/v1/agent/plugin-ai/completions/{run_id}/cancel",
            post(cancel),
        )
        .route(
            "/api/console/v1/agent/plugin-ai/completions/{run_id}/recover",
            post(recover),
        )
}

async fn quote(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Json(input): Json<Input>,
) -> Result<Json<serde_json::Value>, ApiProblem> {
    invoke(runtime, headers, input, true, None, None).await
}
async fn complete(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Json(input): Json<Input>,
) -> Result<Json<serde_json::Value>, ApiProblem> {
    invoke(runtime, headers, input, false, None, None).await
}

struct ActiveRun {
    authority: Arc<BridgeAuthority>,
    id: String,
    cancellation: Signal,
}
struct CancelOnDrop(Signal);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
impl Drop for ActiveRun {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Ok(mut active) = self.authority.active.lock() {
            active.remove(&self.id);
        }
    }
}

async fn invoke(
    runtime: WebRuntime,
    headers: HeaderMap,
    input: Input,
    quote_only: bool,
    task: Option<RunFields>,
    session: Option<SessionInput>,
) -> Result<Json<serde_json::Value>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let authority = runtime
        .plugin_ai
        .as_ref()
        .ok_or_else(|| ApiProblem::not_found("Plugin AI is not enabled"))?
        .clone();
    let actor = authority.actor(&headers)?;
    if uuid::Uuid::parse_str(&input.run_id).is_err()
        || input.prompt.len() > 65536
        || input.max_output == 0
        || input.max_output > 4096
    {
        return Err(ApiProblem::conflict("invalid bounded completion"));
    }
    let cancellation = Signal::new();
    {
        let mut active = authority
            .active
            .lock()
            .map_err(|_| ApiProblem::unavailable("completion state unavailable"))?;
        if active.len() >= 32 || active.contains_key(&input.run_id) {
            return Err(ApiProblem::conflict("completion admission busy"));
        }
        active.insert(
            input.run_id.clone(),
            AdmittedRun {
                subject: actor.subject().to_owned(),
                stop_assertion: assertion_snapshot(&headers)
                    .ok_or_else(|| ApiProblem::forbidden("Host actor required"))?,
                cancellation: cancellation.clone(),
            },
        );
    }
    authority
        .terminal
        .lock()
        .map_err(|_| ApiProblem::unavailable("completion evidence unavailable"))?
        .remove(&input.run_id);
    let active = ActiveRun {
        authority,
        id: input.run_id.clone(),
        cancellation: cancellation.clone(),
    };
    let _cancel_on_drop = CancelOnDrop(cancellation.clone());
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::PluginCompletion(CompletionCommand {
            actor,
            input,
            quote_only,
            cancellation,
            reply,
            active,
            task,
            session,
        }))
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime unavailable"))?;
    tokio::time::timeout(Duration::from_secs(20), response)
        .await
        .map_err(|_| ApiProblem::unavailable("completion deadline exceeded"))?
        .map_err(|_| ApiProblem::unavailable("completion worker stopped"))?
        .map(Json)
        .map_err(ApiProblem::conflict)
}

async fn cancel(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<bool>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let authority = runtime
        .plugin_ai
        .as_ref()
        .ok_or_else(|| ApiProblem::not_found("Plugin AI is not enabled"))?;
    #[cfg(test)]
    if let Some(path) = &authority.cancel_fault {
        let fault = std::fs::read_to_string(path).unwrap_or_default();
        if !fault.is_empty() {
            std::fs::write(path.with_extension("entered"), &fault)
                .map_err(|_| ApiProblem::unavailable("fixture fault trace"))?;
            if fault == "timeout" {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            return Err(ApiProblem::unavailable("synthetic cancellation failure"));
        }
    }
    let active = authority
        .active
        .lock()
        .map_err(|_| ApiProblem::unavailable("completion state unavailable"))?;
    if let Some(run) = active.get(&id) {
        // Admission already verified these exact bytes. Expiry/revocation must
        // never prevent stopping that admitted run; all other access still
        // requires a currently valid assertion and the same verified owner.
        if assertion_snapshot(&headers) != Some(run.stop_assertion)
            && run.subject != authority.actor(&headers)?.subject()
        {
            return Err(ApiProblem::forbidden("completion owner mismatch"));
        }
        run.cancellation.cancel();
        return Ok(Json(true));
    }
    authority.actor(&headers)?;
    Ok(Json(false))
}

/// Explicit recovery never replays a request or refunds unknown usage. Report
/// a successful provider terminal receipt. Worker disappearance alone is not
/// proof: Kernel dispatch may outlive the worker or its waiting Future.
async fn recover(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<bool>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let authority = runtime
        .plugin_ai
        .as_ref()
        .ok_or_else(|| ApiProblem::not_found("Plugin AI is not enabled"))?;
    let actor = authority.actor(&headers)?;
    let active = authority
        .active
        .lock()
        .map_err(|_| ApiProblem::unavailable("completion state unavailable"))?;
    if let Some(run) = active.get(&id) {
        if run.subject != actor.subject() {
            return Err(ApiProblem::forbidden("completion owner mismatch"));
        }
        run.cancellation.cancel();
        return Ok(Json(false));
    }
    let terminal = authority
        .terminal
        .lock()
        .map_err(|_| ApiProblem::unavailable("completion evidence unavailable"))?;
    match terminal.get(&id) {
        Some(owner) if owner == actor.subject() => Ok(Json(true)),
        Some(_) => Err(ApiProblem::forbidden("completion owner mismatch")),
        None => Ok(Json(false)),
    }
}

pub(super) async fn dispatch(app: &AgentApp, command: CompletionCommand) {
    if let Some(input) = &command.session {
        let result = app
            .plugin_session(
                command.actor.clone(),
                input.session_id.clone(),
                input.namespace.clone(),
            )
            .await
            .map(|id| serde_json::json!({"session_id":id}));
        let _ = command.reply.send(result);
        return;
    }
    if let Some(task) = &command.task {
        let cancellation = CancellationToken::new();
        let admitted=async {
            let quote=app.lease_plugin_completion(command.actor.clone()).await?;
            if command.input.generation.as_deref()!=Some(quote.generation_digest())
                || command.input.input_ceiling!=quote.input_ceiling(&command.input.model) {
                return Err("quoted task Model binding changed".into());
            }
            let binding=serde_json::json!({"model":command.input.model,"provider_instance":quote.provider_instance(),"generation":quote.generation_digest(),"input_ceiling":quote.input_ceiling(&command.input.model)});
            let policy=lenso_agent_host::generation::plugin_run::RunPolicy {
                model:command.input.model.clone(),max_calls:task.max_calls,max_output:command.input.max_output,
                input_ceiling:command.input.input_ceiling.ok_or("missing task ceiling")?,allowed_tools:task.allowed_tools.clone(),
                workspace:task.workspace.clone(),session_id:task.session_id.clone(),session_namespace:task.session_namespace.clone(),expected_generation:Some(quote.generation_digest().into()),expected_provider:Some(quote.provider_instance().into()),
            };
            let run=app.lease_plugin_run(command.actor.clone(),policy,cancellation.clone()).await?;
            Ok::<_,String>((run,binding))
        }.await;
        tokio::task::spawn_local(async move {
            // Retain active ownership until execution and shutdown settle.
            let active = command.active;
            let result = match admitted {
                Err(e) => Err(e),
                Ok((run, binding)) => {
                    let future = run.run(command.input.prompt.clone());
                    tokio::pin!(future);
                    let result = tokio::select! { biased;
                        ()=command.cancellation.cancelled()=>{cancellation.cancel();future.await},
                        result=&mut future=>result,
                    };
                    result.map(|r|serde_json::json!({"text":r.text,"input_tokens":r.usage.input_tokens,"output_tokens":r.usage.output_tokens,"calls":r.usage.calls,"binding":binding,"session_id":r.session_id}))
                }
            };
            if result.is_ok()
                && let Ok(mut terminal) = active.authority.terminal.lock()
                && terminal.len() < 1024
            {
                terminal.insert(
                    command.input.run_id.clone(),
                    command.actor.subject().to_owned(),
                );
            }
            let _ = command.reply.send(result);
        });
        return;
    }
    let lease = app.lease_plugin_completion(command.actor.clone()).await;
    tokio::task::spawn_local(async move {
        let result = match lease {
            Ok(lease) => execute(&lease, &command).await,
            Err(error) => Err(error),
        };
        if result.is_ok()
            && !command.quote_only
            && let Ok(mut terminal) = command.active.authority.terminal.lock()
        {
            // Bounded receipts contain only run IDs and verified owners.
            // Exhaustion loses recovery evidence, never grants admission.
            if terminal.len() < 1024 {
                terminal.insert(
                    command.input.run_id.clone(),
                    command.actor.subject().to_owned(),
                );
            }
        }
        let _ = command.reply.send(result);
    });
}

async fn execute(
    lease: &CompletionGeneration,
    command: &CompletionCommand,
) -> Result<serde_json::Value, String> {
    let input = &command.input;
    let binding = Quote {
        model: input.model.clone(),
        provider_instance: lease.provider_instance().to_owned(),
        generation: lease.generation_digest().to_owned(),
        input_ceiling: lease
            .input_ceiling(&input.model)
            .ok_or_else(|| "unknown Model input ceiling".to_owned())?,
    };
    if command.quote_only {
        return serde_json::to_value(binding).map_err(|e| e.to_string());
    }
    if input.generation.as_deref() != Some(binding.generation.as_str()) {
        return Err("quoted Generation changed".into());
    }
    if input.input_ceiling != Some(binding.input_ceiling) {
        return Err("quoted input ceiling changed".into());
    }
    let request: CompleteOpen = serde_json::from_value(serde_json::json!({
        "model":input.model,"messages":[{"role":"user","content":input.prompt}],
        "tools":[],"temperature":0.0,"max_output_tokens":input.max_output
    }))
    .map_err(|e| e.to_string())?;
    let native_cancellation = CancellationToken::new();
    let stream = cancel_open(
        &native_cancellation,
        &command.cancellation,
        lease.complete(request, native_cancellation.clone()),
    )
    .await?;
    let mut text = String::new();
    let mut usage = None;
    loop {
        let event = tokio::select! {
            () = command.cancellation.cancelled() => { stream.cancel(); return Err("completion cancelled".into()); }
            event = stream.receive() => event.map_err(|e| format!("completion failed: {e:?}"))?,
        };
        match event {
            StreamEvent::Message(message) => match message.kind {
                CompleteMessageKind::TextDelta => {
                    text.push_str(&message.text);
                    if text.len() > 1_048_576 {
                        stream.cancel();
                        return Err("completion output too large".into());
                    }
                }
                CompleteMessageKind::Usage => {
                    usage = Some((
                        message
                            .input_tokens
                            .parse::<u64>()
                            .map_err(|_| "unknown input usage")?,
                        message
                            .output_tokens
                            .parse::<u64>()
                            .map_err(|_| "unknown output usage")?,
                    ));
                }
                CompleteMessageKind::ToolCall => {
                    stream.cancel();
                    return Err("lightweight completion cannot call tools".into());
                }
                CompleteMessageKind::ReasoningSummaryDelta => {}
            },
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => break,
            StreamEvent::Terminal(Err(error)) => {
                return Err(format!("completion rejected: {error:?}"));
            }
        }
    }
    let (input_tokens, output_tokens) =
        usage.ok_or_else(|| "missing completion usage".to_owned())?;
    if input_tokens > binding.input_ceiling || output_tokens > input.max_output {
        return Err("usage exceeds admitted bound".into());
    }
    serde_json::to_value(Reply {
        binding,
        text,
        input_tokens,
        output_tokens,
    })
    .map_err(|e| e.to_string())
}

async fn cancel_open<T>(
    native: &CancellationToken,
    signal: &Signal,
    open: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tokio::select! {
        biased;
        () = signal.cancelled() => { native.cancel(); Err("completion cancelled".into()) }
        result = open => result,
    }
}

#[cfg(test)]
mod cancel_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// Separate-process fixture used by the Console native adapter integration test.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires explicit synthetic fixture paths and existing test authority"]
    async fn plugin_ai_fixture_process() {
        let root = std::path::PathBuf::from(std::env::var("LENSO_AI_FIXTURE_ROOT").unwrap());
        let issuer = std::env::var("LENSO_AI_FIXTURE_ISSUER").unwrap();
        let key = std::env::var("LENSO_AI_FIXTURE_PUBLIC_KEY").unwrap();
        tokio::task::LocalSet::new()
            .run_until(Box::pin(async move {
                super::super::configure_test_fixture_model(&root);
                let scoped = std::env::var("LENSO_AI_FIXTURE_SCOPED").is_ok_and(|s| s == "1");
                let database = root.join("scoped-history.sqlite");
                if scoped {
                    let directory = root.join("plugins/lenso.agent.session.sqlite");
                    std::fs::create_dir_all(&directory).unwrap();
                    std::fs::write(
                        directory.join("sessions.toml"),
                        format!(
                            "database = {}\n[authentication]\nissuer = {}\nverification_key = {}\n",
                            serde_json::to_string(&database).unwrap(),
                            serde_json::to_string(&issuer).unwrap(),
                            serde_json::to_string(&key).unwrap()
                        ),
                    )
                    .unwrap();
                    let text = root.join("plugins/lenso.agent.text-tools");
                    std::fs::create_dir_all(&text).unwrap();
                    std::fs::write(text.join("default.toml"), "").unwrap();
                    let hook = root.join("plugins/lenso.agent.interactive-approval-hook");
                    std::fs::create_dir_all(&hook).unwrap();
                    std::fs::write(hook.join("default.toml"), "default_decision = \"ask\"\nallow_tools = [\"uppercase\"]\nmax_preview_bytes = 16384\n").unwrap();
                }
                let mut config =
                    super::super::AgentWebConfig::new(lenso_agent_default_plugins::link);
                config.agent_home = Some(root.clone());
                config.access = super::super::AgentWebAccess::Local;
                config.control =
                    super::super::AgentWebControl::Bearer("synthetic-existing-host-control".into());
                let mut authority = BridgeAuthority::operators(&issuer, &key).unwrap();
                authority.cancel_fault = Some(root.join("fixture-cancel-fault"));
                config.plugin_ai = Some(Arc::new(authority));
                let surface = super::super::AgentWebSurface::start(config).await.unwrap();
                if !scoped {
                    let (reply, response) = oneshot::channel();
                    surface
                        .runtime
                        .commands
                        .send(RuntimeCommand::ListSessions { reply, identity: crate::RequestIdentity::default() })
                        .await
                        .unwrap();
                    assert!(response.await.unwrap().unwrap().sessions.is_empty());
                }
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                std::fs::write(
                    root.join("fixture-origin"),
                    format!("http://{}", listener.local_addr().unwrap()),
                )
                .unwrap();
                axum::serve(listener, surface.router())
                    .with_graceful_shutdown(async move {
                        while !root.join("fixture-stop").exists() {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    })
                    .await
                    .unwrap();
                if !scoped {
                    let (reply, response) = oneshot::channel();
                    surface
                        .runtime
                        .commands
                        .send(RuntimeCommand::ListSessions { reply, identity: crate::RequestIdentity::default() })
                        .await
                        .unwrap();
                    assert!(response.await.unwrap().unwrap().sessions.is_empty());
                }
                surface.shutdown().await.unwrap();
                if scoped {
                    let c = rusqlite::Connection::open(database).unwrap();
                    assert_eq!(
                        c.query_row::<i64, _, _>("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
                            .unwrap(),
                        2
                    );
                }
            }))
            .await;
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunFields {
    max_calls: u32,
    allowed_tools: std::collections::BTreeSet<String>,
    workspace: String,
    session_id: Option<String>,
    session_namespace: Option<lenso_capability_agent_session::SessionNamespace>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunInput {
    completion: Input,
    policy: RunFields,
}
async fn run(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Json(input): Json<RunInput>,
) -> Result<Json<serde_json::Value>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    runtime
        .plugin_ai
        .as_ref()
        .ok_or_else(|| ApiProblem::not_found("Plugin AI is not enabled"))?
        .actor_for(
            &headers,
            lenso_capability_agent::CAPABILITY_ID,
            lenso_capability_agent::RUN_TURN_OPERATION,
        )?;
    invoke(
        runtime,
        headers,
        input.completion,
        false,
        Some(input.policy),
        None,
    )
    .await
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionInput {
    session_id: Option<String>,
    namespace: Option<lenso_capability_agent_session::SessionNamespace>,
}
async fn session(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Json(input): Json<SessionInput>,
) -> Result<Json<serde_json::Value>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    runtime
        .plugin_ai
        .as_ref()
        .ok_or_else(|| ApiProblem::not_found("Plugin AI is not enabled"))?
        .actor_for(
            &headers,
            "lenso.agent.session@1",
            if input.session_id.is_some() {
                "read"
            } else {
                "open"
            },
        )?;
    invoke(
        runtime,
        headers,
        Input {
            run_id: uuid::Uuid::new_v4().to_string(),
            model: String::new(),
            prompt: String::new(),
            max_output: 1,
            generation: None,
            input_ceiling: None,
        },
        false,
        None,
        Some(input),
    )
    .await
}

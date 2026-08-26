use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use reqwest::{Client, header};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::{Instant, Sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{
    channel::{TurnGate, run_agent_turn},
    generation::AgentApp,
};

const DEFAULT_API_BASE: &str = "https://discord.com/api/v10";
const MAX_MESSAGE_CHARS: usize = 2_000;
const MAX_CONVERSATIONS: usize = 10_000;
const GUILDS_INTENT: u64 = 1;
const GUILD_MESSAGES_INTENT: u64 = 1 << 9;
const DIRECT_MESSAGES_INTENT: u64 = 1 << 12;
const MESSAGE_CONTENT_INTENT: u64 = 1 << 15;

#[derive(Clone, Debug)]
pub struct DiscordOptions {
    pub token: String,
    pub api_base: String,
    pub gateway_url: Option<String>,
    pub allowed_channels: ChannelAllowlist,
    pub allowed_tools: Vec<String>,
    pub respond_all_guilds: bool,
    pub message_content_intent: bool,
    pub state_path: PathBuf,
    pub max_messages: Option<u64>,
    pub turn_gate: TurnGate,
}

impl DiscordOptions {
    pub fn new(token: String, allowed_channels: ChannelAllowlist, state_path: PathBuf) -> Self {
        Self {
            token,
            api_base: std::env::var("LENSO_DISCORD_API_BASE")
                .unwrap_or_else(|_| DEFAULT_API_BASE.to_owned()),
            gateway_url: std::env::var("LENSO_DISCORD_GATEWAY_URL").ok(),
            allowed_channels,
            allowed_tools: Vec::new(),
            respond_all_guilds: false,
            message_content_intent: false,
            state_path,
            max_messages: None,
            turn_gate: TurnGate::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelAllowlist {
    allow_all: bool,
    channel_ids: BTreeSet<String>,
}

impl ChannelAllowlist {
    pub fn parse(values: &[String]) -> Result<Self, String> {
        if values.is_empty() {
            return Err("at least one --allow-channel value is required".to_owned());
        }
        let allow_all = values.iter().any(|value| value == "*");
        if allow_all && values.len() != 1 {
            return Err(
                "--allow-channel '*' cannot be combined with explicit channel IDs".to_owned(),
            );
        }
        let channel_ids = values
            .iter()
            .filter(|value| value.as_str() != "*")
            .map(|value| {
                validate_snowflake("Discord channel ID", value)?;
                Ok(value.clone())
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            allow_all,
            channel_ids,
        })
    }

    fn contains(&self, channel_id: &str) -> bool {
        self.allow_all || self.channel_ids.contains(channel_id)
    }
}

#[derive(Clone)]
struct DiscordClient {
    http: Client,
    api_base: String,
}

impl std::fmt::Debug for DiscordClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DiscordClient")
            .field("api_base", &self.api_base)
            .field("authorization", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl DiscordClient {
    fn new(api_base: &str, token: &str) -> Result<Self, String> {
        if token.trim().is_empty() {
            return Err("Discord Bot token is empty".to_owned());
        }
        let authorization = header::HeaderValue::from_str(&format!("Bot {token}"))
            .map_err(|_| "Discord Bot token contains invalid header characters".to_owned())?;
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static("Lenso-Agent-Harness/0.1"),
        );
        let http = Client::builder()
            .default_headers(headers)
            .build()
            .map_err(|error| format!("failed to build Discord HTTP client: {error}"))?;
        Ok(Self {
            http,
            api_base: api_base.trim_end_matches('/').to_owned(),
        })
    }

    async fn gateway_url(&self) -> Result<String, String> {
        let response = self
            .http
            .get(format!("{}/gateway/bot", self.api_base))
            .send()
            .await
            .map_err(|error| format!("Discord gateway request failed: {}", error.without_url()))?
            .error_for_status()
            .map_err(|error| {
                format!(
                    "Discord gateway request returned an HTTP error: {}",
                    error.without_url()
                )
            })?
            .json::<GatewayBotResponse>()
            .await
            .map_err(|error| {
                format!(
                    "Discord gateway response was invalid: {}",
                    error.without_url()
                )
            })?;
        Ok(response.url)
    }

    async fn send_typing(&self, channel_id: &str) -> Result<(), String> {
        self.http
            .post(format!("{}/channels/{channel_id}/typing", self.api_base))
            .send()
            .await
            .map_err(|error| format!("Discord typing request failed: {}", error.without_url()))?
            .error_for_status()
            .map_err(|error| {
                format!(
                    "Discord typing request returned an HTTP error: {}",
                    error.without_url()
                )
            })?;
        Ok(())
    }

    async fn send_text(&self, message: &DiscordMessage, text: &str) -> Result<(), String> {
        self.http
            .post(format!(
                "{}/channels/{}/messages",
                self.api_base, message.channel_id
            ))
            .json(&serde_json::json!({
                "content": text,
                "message_reference": {
                    "message_id": message.id,
                    "channel_id": message.channel_id,
                    "fail_if_not_exists": false
                },
                "allowed_mentions": {"parse": [], "replied_user": false}
            }))
            .send()
            .await
            .map_err(|error| format!("Discord message request failed: {}", error.without_url()))?
            .error_for_status()
            .map_err(|error| {
                format!(
                    "Discord message request returned an HTTP error: {}",
                    error.without_url()
                )
            })?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct GatewayBotResponse {
    url: String,
}

#[derive(Clone, Debug, Deserialize)]
struct DiscordMessage {
    id: String,
    channel_id: String,
    #[serde(default)]
    guild_id: Option<String>,
    author: DiscordUser,
    #[serde(default)]
    content: String,
    #[serde(default)]
    mentions: Vec<DiscordUser>,
    #[serde(default)]
    referenced_message: Option<Box<DiscordMessage>>,
}

#[derive(Clone, Debug, Deserialize)]
struct DiscordUser {
    id: String,
    #[serde(default)]
    bot: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DiscordState {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gateway: Option<GatewaySession>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    sessions: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GatewaySession {
    session_id: String,
    resume_gateway_url: String,
    sequence: u64,
    bot_user_id: String,
}

pub async fn run(app: &AgentApp, options: &DiscordOptions) -> Result<(), String> {
    if options.respond_all_guilds && !options.message_content_intent {
        return Err("--respond-all-guilds requires --message-content-intent".to_owned());
    }
    let client = DiscordClient::new(&options.api_base, &options.token)?;
    let mut state = load_state(&options.state_path)?;
    let mut observed_messages = 0_u64;
    loop {
        let gateway_url = match (&options.gateway_url, &state.gateway) {
            (Some(url), _) => url.clone(),
            (None, Some(session)) => session.resume_gateway_url.clone(),
            (None, None) => client.gateway_url().await?,
        };
        match run_connection(
            app,
            &client,
            options,
            &gateway_url,
            &mut state,
            &mut observed_messages,
        )
        .await?
        {
            ConnectionOutcome::Completed => return Ok(()),
            ConnectionOutcome::Reconnect => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionOutcome {
    Completed,
    Reconnect,
}

async fn run_connection(
    app: &AgentApp,
    client: &DiscordClient,
    options: &DiscordOptions,
    gateway_url: &str,
    state: &mut DiscordState,
    observed_messages: &mut u64,
) -> Result<ConnectionOutcome, String> {
    let url = gateway_endpoint(state, gateway_url, options.gateway_url.is_some())?;
    let (mut gateway, _) = connect_async(&url)
        .await
        .map_err(|error| format!("Discord Gateway connection failed: {error}"))?;
    let hello = gateway
        .next()
        .await
        .ok_or_else(|| "Discord Gateway closed before Hello".to_owned())?
        .map_err(|error| format!("Discord Gateway failed before Hello: {error}"))?;
    let hello = parse_gateway_message(hello)?;
    if hello.op != 10 {
        return Err(format!(
            "Discord Gateway sent opcode {} before Hello",
            hello.op
        ));
    }
    let heartbeat_interval = hello
        .d
        .get("heartbeat_interval")
        .and_then(Value::as_u64)
        .filter(|interval| *interval > 0)
        .ok_or_else(|| "Discord Gateway Hello omitted its heartbeat interval".to_owned())?;

    let auth = state.gateway.as_ref().map_or_else(
        || identify_payload(&options.token, options.message_content_intent),
        |session| resume_payload(&options.token, session),
    );
    send_gateway_json(&mut gateway, &auth).await?;

    let heartbeat_delay = Duration::from_millis(heartbeat_interval / 2);
    let mut heartbeat = Box::pin(tokio::time::sleep(heartbeat_delay));
    let mut heartbeat_acknowledged = true;

    loop {
        tokio::select! {
            () = &mut heartbeat => {
                if !heartbeat_acknowledged {
                    return Ok(ConnectionOutcome::Reconnect);
                }
                send_heartbeat(&mut gateway, state.gateway.as_ref().map(|session| session.sequence)).await?;
                heartbeat_acknowledged = false;
                reset_heartbeat(&mut heartbeat, heartbeat_interval);
            }
            incoming = gateway.next() => {
                let Some(incoming) = incoming else {
                    return Ok(ConnectionOutcome::Reconnect);
                };
                let incoming = incoming
                    .map_err(|error| format!("Discord Gateway receive failed: {error}"))?;
                if matches!(incoming, Message::Close(_)) {
                    return Ok(ConnectionOutcome::Reconnect);
                }
                if incoming.is_ping() || incoming.is_pong() {
                    continue;
                }
                let payload = parse_gateway_message(incoming)?;
                match payload.op {
                    0 => {
                        let sequence = payload.s.ok_or_else(|| {
                            "Discord dispatch omitted its sequence".to_owned()
                        })?;
                        process_dispatch(app, client, options, state, &payload, sequence).await?;
                        persist_state(&options.state_path, state)?;
                        if payload.t.as_deref() == Some("MESSAGE_CREATE") {
                            *observed_messages = observed_messages
                                .checked_add(1)
                                .ok_or_else(|| "Discord message counter overflowed".to_owned())?;
                            if options.max_messages.is_some_and(|limit| *observed_messages >= limit) {
                                gateway.close(None).await.map_err(|error| {
                                    format!("failed to close Discord Gateway: {error}")
                                })?;
                                return Ok(ConnectionOutcome::Completed);
                            }
                        }
                    }
                    1 => {
                        send_heartbeat(&mut gateway, state.gateway.as_ref().map(|session| session.sequence)).await?;
                        heartbeat_acknowledged = false;
                        reset_heartbeat(&mut heartbeat, heartbeat_interval);
                    }
                    7 => return Ok(ConnectionOutcome::Reconnect),
                    9 => {
                        if !payload.d.as_bool().unwrap_or(false) {
                            state.gateway = None;
                            persist_state(&options.state_path, state)?;
                        }
                        return Ok(ConnectionOutcome::Reconnect);
                    }
                    11 => heartbeat_acknowledged = true,
                    _ => {}
                }
            }
            signal = tokio::signal::ctrl_c(), if options.max_messages.is_none() => {
                signal.map_err(|error| format!("failed to listen for Ctrl-C: {error}"))?;
                gateway.close(None).await.map_err(|error| {
                    format!("failed to close Discord Gateway: {error}")
                })?;
                return Ok(ConnectionOutcome::Completed);
            }
        }
    }
}

async fn process_dispatch(
    app: &AgentApp,
    client: &DiscordClient,
    options: &DiscordOptions,
    state: &mut DiscordState,
    payload: &GatewayPayload,
    sequence: u64,
) -> Result<(), String> {
    match payload.t.as_deref() {
        Some("READY") => {
            let ready: ReadyEvent = serde_json::from_value(payload.d.clone())
                .map_err(|error| format!("Discord Ready event was invalid: {error}"))?;
            validate_snowflake("Discord Bot user ID", &ready.user.id)?;
            state.gateway = Some(GatewaySession {
                session_id: ready.session_id,
                resume_gateway_url: ready.resume_gateway_url,
                sequence,
                bot_user_id: ready.user.id,
            });
        }
        Some("MESSAGE_CREATE") => {
            let message: DiscordMessage = serde_json::from_value(payload.d.clone())
                .map_err(|error| format!("Discord Message Create event was invalid: {error}"))?;
            if let Some(session) = state.gateway.as_mut() {
                session.sequence = sequence;
            }
            process_message(app, client, options, state, &message).await?;
        }
        _ => {
            if let Some(session) = state.gateway.as_mut() {
                session.sequence = sequence;
            }
        }
    }
    Ok(())
}

async fn process_message(
    app: &AgentApp,
    client: &DiscordClient,
    options: &DiscordOptions,
    state: &mut DiscordState,
    message: &DiscordMessage,
) -> Result<(), String> {
    validate_snowflake("Discord message ID", &message.id)?;
    validate_snowflake("Discord channel ID", &message.channel_id)?;
    if message.author.bot || !options.allowed_channels.contains(&message.channel_id) {
        return Ok(());
    }
    let bot_user_id = state
        .gateway
        .as_ref()
        .map(|session| session.bot_user_id.as_str())
        .ok_or_else(|| {
            "Discord message arrived before Ready established Bot identity".to_owned()
        })?;
    let Some(prompt) = message_prompt(message, bot_user_id, options.respond_all_guilds) else {
        return Ok(());
    };
    if let Err(error) = client.send_typing(&message.channel_id).await {
        eprintln!("warning: {error}");
    }
    let conversation_key = format!("discord_{bot_user_id}_{}", message.channel_id);
    if !state.sessions.contains_key(&conversation_key) && state.sessions.len() >= MAX_CONVERSATIONS
    {
        return Err(format!(
            "Discord conversation mapping reached its {MAX_CONVERSATIONS}-entry limit"
        ));
    }
    let existing_session = state.sessions.get(&conversation_key).cloned();
    let Ok(_turn_permit) = options.turn_gate.enter().await else {
        client
            .send_text(message, "The Agent is busy. Please try again shortly.")
            .await?;
        return Ok(());
    };
    let turn = app.lease_discord_turn().await?;
    let response = match run_agent_turn(
        turn,
        prompt,
        existing_session.as_deref(),
        &options.allowed_tools,
    )
    .await
    {
        Ok(response) => {
            state
                .sessions
                .insert(conversation_key, response.session_id.clone());
            response.text
        }
        Err(error) => {
            eprintln!("Discord Agent Turn failed: {error}");
            "The Agent could not complete this request.".to_owned()
        }
    };
    for chunk in split_message(&response) {
        client.send_text(message, &chunk).await?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ReadyEvent {
    session_id: String,
    resume_gateway_url: String,
    user: DiscordUser,
}

#[derive(Debug, Deserialize)]
struct GatewayPayload {
    op: u64,
    #[serde(default)]
    d: Value,
    #[serde(default)]
    s: Option<u64>,
    #[serde(default)]
    t: Option<String>,
}

fn gateway_endpoint(
    state: &DiscordState,
    initial_url: &str,
    test_override: bool,
) -> Result<String, String> {
    let base = state
        .gateway
        .as_ref()
        .map_or(initial_url, |session| session.resume_gateway_url.as_str());
    validate_gateway_url(base, initial_url, test_override)?;
    let has_path = base
        .find("://")
        .is_some_and(|scheme_end| base[scheme_end + 3..].contains('/'));
    let rooted = if has_path {
        base.to_owned()
    } else {
        format!("{base}/")
    };
    let separator = if rooted.contains('?') { '&' } else { '?' };
    Ok(format!("{rooted}{separator}v=10&encoding=json"))
}

fn validate_gateway_url(
    candidate: &str,
    initial_url: &str,
    test_override: bool,
) -> Result<(), String> {
    let candidate =
        reqwest::Url::parse(candidate).map_err(|_| "Discord Gateway URL is invalid".to_owned())?;
    if !candidate.username().is_empty()
        || candidate.password().is_some()
        || candidate.query().is_some()
        || candidate.fragment().is_some()
    {
        return Err("Discord Gateway URL contains unsupported authority data".to_owned());
    }
    let host = candidate
        .host_str()
        .ok_or_else(|| "Discord Gateway URL omitted its host".to_owned())?;
    if test_override {
        let initial = reqwest::Url::parse(initial_url)
            .map_err(|_| "Discord Gateway override URL is invalid".to_owned())?;
        if !matches!(candidate.scheme(), "ws" | "wss")
            || host != initial.host_str().unwrap_or_default()
            || candidate.port_or_known_default() != initial.port_or_known_default()
        {
            return Err(
                "Discord resume Gateway does not match the explicit test override".to_owned(),
            );
        }
    } else if candidate.scheme() != "wss"
        || (host != "discord.gg" && !host.ends_with(".discord.gg"))
    {
        return Err("Discord Gateway must use wss on a discord.gg host".to_owned());
    }
    Ok(())
}

fn identify_payload(token: &str, message_content_intent: bool) -> Value {
    let mut intents = GUILDS_INTENT | GUILD_MESSAGES_INTENT | DIRECT_MESSAGES_INTENT;
    if message_content_intent {
        intents |= MESSAGE_CONTENT_INTENT;
    }
    serde_json::json!({
        "op": 2,
        "d": {
            "token": token,
            "intents": intents,
            "properties": {
                "os": std::env::consts::OS,
                "browser": "lenso-agent-harness",
                "device": "lenso-agent-harness"
            }
        }
    })
}

fn resume_payload(token: &str, session: &GatewaySession) -> Value {
    serde_json::json!({
        "op": 6,
        "d": {
            "token": token,
            "session_id": session.session_id,
            "seq": session.sequence
        }
    })
}

async fn send_heartbeat<S>(gateway: &mut S, sequence: Option<u64>) -> Result<(), String>
where
    S: futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    send_gateway_json(gateway, &serde_json::json!({"op": 1, "d": sequence})).await
}

async fn send_gateway_json<S>(gateway: &mut S, value: &Value) -> Result<(), String>
where
    S: futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    gateway
        .send(Message::Text(value.to_string().into()))
        .await
        .map_err(|error| format!("Discord Gateway send failed: {error}"))
}

fn parse_gateway_message(message: Message) -> Result<GatewayPayload, String> {
    let text = message
        .into_text()
        .map_err(|_| "Discord Gateway sent a non-text payload".to_owned())?;
    serde_json::from_str(text.as_ref())
        .map_err(|error| format!("Discord Gateway payload was invalid: {error}"))
}

fn reset_heartbeat(heartbeat: &mut std::pin::Pin<Box<Sleep>>, interval_ms: u64) {
    heartbeat
        .as_mut()
        .reset(Instant::now() + Duration::from_millis(interval_ms));
}

fn message_prompt(
    message: &DiscordMessage,
    bot_user_id: &str,
    respond_all_guilds: bool,
) -> Option<String> {
    let text = message.content.trim();
    if text.is_empty() {
        return None;
    }
    if message.guild_id.is_none() || respond_all_guilds {
        return Some(text.to_owned());
    }
    let mentions_bot = message.mentions.iter().any(|user| user.id == bot_user_id);
    let replies_to_bot = message
        .referenced_message
        .as_ref()
        .is_some_and(|reply| reply.author.id == bot_user_id);
    if !mentions_bot && !replies_to_bot {
        return None;
    }
    let prompt = text
        .replace(&format!("<@{bot_user_id}>"), "")
        .replace(&format!("<@!{bot_user_id}>"), "");
    let prompt = prompt.trim();
    (!prompt.is_empty()).then(|| prompt.to_owned())
}

fn split_message(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if current.chars().count() == MAX_MESSAGE_CHARS {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(character);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push("The Agent completed without a text response.".to_owned());
    }
    chunks
}

fn validate_snowflake(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("invalid {label} `{value}`"));
    }
    Ok(())
}

fn load_state(path: &Path) -> Result<DiscordState, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DiscordState {
                schema_version: 1,
                ..DiscordState::default()
            });
        }
        Err(error) => {
            return Err(format!(
                "failed to inspect Discord state {}: {error}",
                path.display()
            ));
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Discord state path is not a regular file".to_owned());
    }
    let bytes = fs::read(path)
        .map_err(|error| format!("failed to read Discord state {}: {error}", path.display()))?;
    let state: DiscordState = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Discord state is invalid: {error}"))?;
    if state.schema_version != 1
        || state.sessions.len() > MAX_CONVERSATIONS
        || state.sessions.iter().any(|(key, session_id)| {
            key.is_empty() || key.len() > 128 || session_id.is_empty() || session_id.len() > 128
        })
        || state.gateway.as_ref().is_some_and(|gateway| {
            gateway.session_id.is_empty()
                || gateway.session_id.len() > 256
                || gateway.resume_gateway_url.is_empty()
                || gateway.resume_gateway_url.len() > 2_048
                || validate_snowflake("Discord Bot user ID", &gateway.bot_user_id).is_err()
        })
    {
        return Err("Discord state version or contents are invalid".to_owned());
    }
    Ok(state)
}

fn persist_state(path: &Path, state: &DiscordState) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Discord state path {} has no parent", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create Discord state directory {}: {error}",
            parent.display()
        )
    })?;
    let parent_metadata = fs::symlink_metadata(parent)
        .map_err(|error| format!("failed to inspect Discord state directory: {error}"))?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        return Err("Discord state directory is not a regular directory".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("failed to secure Discord state directory: {error}"))?;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err("Discord state path is not a regular file".to_owned());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "failed to inspect Discord state {}: {error}",
                path.display()
            ));
        }
    }
    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("failed to encode Discord state: {error}"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Discord state filename is invalid".to_owned())?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let write_result = (|| {
        let mut open = OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
        }
        let mut file = open
            .open(&temporary)
            .map_err(|error| format!("failed to create Discord state: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("failed to write Discord state: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("failed to sync Discord state: {error}"))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("failed to replace Discord state: {error}"))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str, bot: bool) -> DiscordUser {
        DiscordUser {
            id: id.to_owned(),
            bot,
        }
    }

    fn message(content: &str, guild: bool) -> DiscordMessage {
        DiscordMessage {
            id: "100".to_owned(),
            channel_id: "200".to_owned(),
            guild_id: guild.then(|| "300".to_owned()),
            author: user("400", false),
            content: content.to_owned(),
            mentions: Vec::new(),
            referenced_message: None,
        }
    }

    #[test]
    fn allowlist_is_explicit_and_validates_snowflakes() {
        assert!(ChannelAllowlist::parse(&[]).is_err());
        assert!(ChannelAllowlist::parse(&["not-an-id".to_owned()]).is_err());
        assert!(ChannelAllowlist::parse(&["*".to_owned(), "200".to_owned()]).is_err());
        assert!(
            ChannelAllowlist::parse(&["200".to_owned()])
                .unwrap()
                .contains("200")
        );
    }

    #[test]
    fn guild_messages_require_a_mention_or_reply_by_default() {
        assert_eq!(
            message_prompt(&message("hello", false), "999", false).as_deref(),
            Some("hello")
        );
        assert!(message_prompt(&message("hello", true), "999", false).is_none());
        let mut mentioned = message("<@999> hello", true);
        mentioned.mentions.push(user("999", true));
        assert_eq!(
            message_prompt(&mentioned, "999", false).as_deref(),
            Some("hello")
        );
        let mut replied = message("hello", true);
        let mut reference = message("prior", true);
        reference.author = user("999", true);
        replied.referenced_message = Some(Box::new(reference));
        assert_eq!(
            message_prompt(&replied, "999", false).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn identify_requests_message_content_only_when_selected() {
        assert_eq!(
            identify_payload("secret", false)["d"]["intents"],
            serde_json::json!(4_609)
        );
        assert_eq!(
            identify_payload("secret", true)["d"]["intents"],
            serde_json::json!(37_377)
        );
    }

    #[test]
    fn production_resume_urls_cannot_exfiltrate_the_bot_token() {
        assert!(
            validate_gateway_url(
                "wss://gateway.discord.gg",
                "wss://gateway.discord.gg",
                false
            )
            .is_ok()
        );
        assert!(
            validate_gateway_url(
                "wss://gateway-us-east1-b.discord.gg",
                "wss://gateway.discord.gg",
                false
            )
            .is_ok()
        );
        assert!(
            validate_gateway_url("wss://example.com", "wss://gateway.discord.gg", false).is_err()
        );
        assert!(
            validate_gateway_url("ws://gateway.discord.gg", "wss://gateway.discord.gg", false)
                .is_err()
        );
        assert!(validate_gateway_url("ws://127.0.0.1:9001", "ws://127.0.0.1:9002", true).is_err());
    }

    #[test]
    fn replies_split_on_unicode_boundaries() {
        let text = "界".repeat(MAX_MESSAGE_CHARS + 1);
        let chunks = split_message(&text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), MAX_MESSAGE_CHARS);
        assert_eq!(chunks[1], "界");
    }
}

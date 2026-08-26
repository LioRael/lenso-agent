use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use reqwest::Client;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    channel::{TurnGate, run_agent_turn},
    generation::AgentApp,
};

const DEFAULT_API_BASE: &str = "https://api.telegram.org";
const MAX_MESSAGE_CHARS: usize = 4_000;

#[derive(Clone, Debug)]
pub struct TelegramOptions {
    pub token: String,
    pub api_base: String,
    pub allowed_chats: ChatAllowlist,
    pub allowed_tools: Vec<String>,
    pub respond_all_groups: bool,
    pub state_path: PathBuf,
    pub poll_timeout_seconds: u64,
    pub max_updates: Option<u64>,
    pub turn_gate: TurnGate,
}

impl TelegramOptions {
    pub fn new(token: String, allowed_chats: ChatAllowlist, state_path: PathBuf) -> Self {
        Self {
            token,
            api_base: std::env::var("LENSO_TELEGRAM_API_BASE")
                .unwrap_or_else(|_| DEFAULT_API_BASE.to_owned()),
            allowed_chats,
            allowed_tools: Vec::new(),
            respond_all_groups: false,
            state_path,
            poll_timeout_seconds: 30,
            max_updates: None,
            turn_gate: TurnGate::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatAllowlist {
    allow_all: bool,
    chat_ids: BTreeSet<i64>,
}

impl ChatAllowlist {
    pub fn parse(values: &[String]) -> Result<Self, String> {
        if values.is_empty() {
            return Err("at least one --allow-chat value is required".to_owned());
        }
        let allow_all = values.iter().any(|value| value == "*");
        if allow_all && values.len() != 1 {
            return Err("--allow-chat '*' cannot be combined with explicit chat IDs".to_owned());
        }
        let chat_ids = values
            .iter()
            .filter(|value| value.as_str() != "*")
            .map(|value| {
                value
                    .parse::<i64>()
                    .map_err(|_| format!("invalid Telegram chat ID `{value}`"))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            allow_all,
            chat_ids,
        })
    }

    fn contains(&self, chat_id: i64) -> bool {
        self.allow_all || self.chat_ids.contains(&chat_id)
    }
}

#[derive(Clone)]
struct TelegramClient {
    http: Client,
    api_base: String,
    token: String,
}

impl std::fmt::Debug for TelegramClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelegramClient")
            .field("api_base", &self.api_base)
            .field("token", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl TelegramClient {
    fn new(api_base: &str, token: String) -> Result<Self, String> {
        if token.trim().is_empty() {
            return Err("Telegram Bot token is empty".to_owned());
        }
        let http = Client::builder()
            .build()
            .map_err(|error| format!("failed to build Telegram HTTP client: {error}"))?;
        Ok(Self {
            http,
            api_base: api_base.trim_end_matches('/').to_owned(),
            token,
        })
    }

    async fn get_me(&self) -> Result<TelegramUser, String> {
        self.call("getMe", &serde_json::json!({})).await
    }

    async fn get_updates(
        &self,
        offset: Option<i64>,
        timeout_seconds: u64,
    ) -> Result<Vec<TelegramUpdate>, String> {
        self.call(
            "getUpdates",
            &serde_json::json!({
                "offset": offset,
                "timeout": timeout_seconds,
                "allowed_updates": ["message"]
            }),
        )
        .await
    }

    async fn send_typing(&self, message: &TelegramMessage) -> Result<(), String> {
        let _: serde_json::Value = self
            .call(
                "sendChatAction",
                &serde_json::json!({
                    "chat_id": message.chat.id,
                    "message_thread_id": message.message_thread_id,
                    "action": "typing"
                }),
            )
            .await?;
        Ok(())
    }

    async fn send_text(&self, message: &TelegramMessage, text: &str) -> Result<(), String> {
        let _: serde_json::Value = self
            .call(
                "sendMessage",
                &serde_json::json!({
                    "chat_id": message.chat.id,
                    "message_thread_id": message.message_thread_id,
                    "reply_parameters": {"message_id": message.message_id},
                    "text": text
                }),
            )
            .await?;
        Ok(())
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        body: &serde_json::Value,
    ) -> Result<T, String> {
        let url = format!("{}/bot{}/{}", self.api_base, self.token, method);
        let response = self
            .http
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|error| {
                format!(
                    "Telegram API request `{method}` failed: {}",
                    error.without_url()
                )
            })?;
        let response = response
            .error_for_status()
            .map_err(|error| {
                format!(
                    "Telegram API request `{method}` returned an HTTP error: {}",
                    error.without_url()
                )
            })?
            .json::<TelegramResponse<T>>()
            .await
            .map_err(|error| {
                format!(
                    "Telegram API response `{method}` was invalid: {}",
                    error.without_url()
                )
            })?;
        if response.ok {
            response
                .result
                .ok_or_else(|| format!("Telegram API response `{method}` omitted its result"))
        } else {
            Err(format!(
                "Telegram API rejected `{method}`: {}",
                response
                    .description
                    .unwrap_or_else(|| "unknown error".to_owned())
            ))
        }
    }
}

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    #[serde(default)]
    message: Option<TelegramMessage>,
}

#[derive(Clone, Debug, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    #[serde(default)]
    message_thread_id: Option<i64>,
    #[serde(default)]
    from: Option<TelegramUser>,
    chat: TelegramChat,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    reply_to_message: Option<Box<TelegramMessage>>,
}

#[derive(Clone, Debug, Deserialize)]
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Clone, Debug, Deserialize)]
struct TelegramUser {
    id: i64,
    #[serde(default)]
    is_bot: bool,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TelegramState {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_update_id: Option<i64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    sessions: BTreeMap<String, String>,
}

pub async fn run(app: &AgentApp, options: &TelegramOptions) -> Result<(), String> {
    if !(1..=50).contains(&options.poll_timeout_seconds) {
        return Err("Telegram poll timeout must be between 1 and 50 seconds".to_owned());
    }
    let client = TelegramClient::new(&options.api_base, options.token.clone())?;
    let bot = client.get_me().await?;
    if !bot.is_bot {
        return Err("Telegram getMe returned a non-bot identity".to_owned());
    }
    let mut state = load_state(&options.state_path)?;
    let mut observed_updates = 0_u64;

    loop {
        let updates = if options.max_updates.is_some() {
            client
                .get_updates(state.next_update_id, options.poll_timeout_seconds)
                .await?
        } else {
            tokio::select! {
                result = client.get_updates(state.next_update_id, options.poll_timeout_seconds) => result?,
                signal = tokio::signal::ctrl_c() => {
                    signal.map_err(|error| format!("failed to listen for Ctrl-C: {error}"))?;
                    return Ok(());
                }
            }
        };

        for update in updates {
            if state
                .next_update_id
                .is_some_and(|offset| update.update_id < offset)
            {
                continue;
            }
            process_update(app, &client, &bot, &update, options, &mut state).await?;
            state.next_update_id = Some(
                update
                    .update_id
                    .checked_add(1)
                    .ok_or_else(|| "Telegram update identity space is exhausted".to_owned())?,
            );
            persist_state(&options.state_path, &state)?;
            observed_updates = observed_updates
                .checked_add(1)
                .ok_or_else(|| "Telegram update counter overflowed".to_owned())?;
            if options
                .max_updates
                .is_some_and(|limit| observed_updates >= limit)
            {
                return Ok(());
            }
        }
    }
}

async fn process_update(
    app: &AgentApp,
    client: &TelegramClient,
    bot: &TelegramUser,
    update: &TelegramUpdate,
    options: &TelegramOptions,
    state: &mut TelegramState,
) -> Result<(), String> {
    let Some(message) = update.message.as_ref() else {
        return Ok(());
    };
    if !options.allowed_chats.contains(message.chat.id)
        || message.from.as_ref().is_some_and(|sender| sender.is_bot)
    {
        return Ok(());
    }
    let Some(prompt) = message_prompt(message, bot, options.respond_all_groups) else {
        return Ok(());
    };

    if let Err(error) = client.send_typing(message).await {
        eprintln!("warning: {error}");
    }
    let conversation_key = conversation_key(bot.id, message);
    let existing_session = state.sessions.get(&conversation_key).cloned();
    let Ok(_turn_permit) = options.turn_gate.enter().await else {
        client
            .send_text(message, "The Agent is busy. Please try again shortly.")
            .await?;
        return Ok(());
    };
    let turn = app.lease_telegram_turn().await?;
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
            eprintln!("Telegram Agent Turn failed: {error}");
            "The Agent could not complete this request.".to_owned()
        }
    };
    for chunk in split_message(&response) {
        client.send_text(message, &chunk).await?;
    }
    Ok(())
}

fn message_prompt(
    message: &TelegramMessage,
    bot: &TelegramUser,
    respond_all_groups: bool,
) -> Option<String> {
    let text = message.text.as_deref()?.trim();
    if text.is_empty() {
        return None;
    }
    if message.chat.kind == "private" || respond_all_groups {
        return Some(text.to_owned());
    }
    let replies_to_bot = message
        .reply_to_message
        .as_ref()
        .and_then(|reply| reply.from.as_ref())
        .is_some_and(|user| user.id == bot.id);
    let username = bot.username.as_deref()?;
    let mention = format!("@{username}");
    let Some(start) = text
        .to_ascii_lowercase()
        .find(&mention.to_ascii_lowercase())
    else {
        return replies_to_bot.then(|| text.to_owned());
    };
    let end = start + mention.len();
    let mut prompt = text.to_owned();
    prompt.replace_range(start..end, "");
    let prompt = prompt.trim();
    (!prompt.is_empty()).then(|| prompt.to_owned())
}

fn conversation_key(bot_id: i64, message: &TelegramMessage) -> String {
    message.message_thread_id.map_or_else(
        || format!("telegram_{bot_id}_{}", message.chat.id),
        |thread_id| format!("telegram_{bot_id}_{}_thread_{thread_id}", message.chat.id),
    )
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

fn load_state(path: &Path) -> Result<TelegramState, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TelegramState {
                schema_version: 1,
                ..TelegramState::default()
            });
        }
        Err(error) => {
            return Err(format!(
                "failed to inspect Telegram state {}: {error}",
                path.display()
            ));
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Telegram state path is not a regular file".to_owned());
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(format!(
                "failed to read Telegram state {}: {error}",
                path.display()
            ));
        }
    };
    let state: TelegramState = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Telegram state is invalid: {error}"))?;
    if state.schema_version != 1
        || state.next_update_id.is_some_and(|offset| offset < 0)
        || state.sessions.iter().any(|(key, session_id)| {
            key.is_empty() || key.len() > 128 || session_id.is_empty() || session_id.len() > 128
        })
    {
        return Err("Telegram state version or update offset is invalid".to_owned());
    }
    Ok(state)
}

fn persist_state(path: &Path, state: &TelegramState) -> Result<(), String> {
    if state.next_update_id.is_some_and(|offset| offset < 0) {
        return Err("Telegram update offset cannot be negative".to_owned());
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("Telegram state path {} has no parent", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create Telegram state directory {}: {error}",
            parent.display()
        )
    })?;
    let parent_metadata = fs::symlink_metadata(parent)
        .map_err(|error| format!("failed to inspect Telegram state directory: {error}"))?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        return Err("Telegram state directory is not a regular directory".to_owned());
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err("Telegram state path is not a regular file".to_owned());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "failed to inspect Telegram state {}: {error}",
                path.display()
            ));
        }
    }

    let bytes = serde_json::to_vec(state)
        .map_err(|error| format!("failed to encode Telegram state: {error}"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Telegram state filename is invalid".to_owned())?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("failed to create Telegram state: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("failed to write Telegram state: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("failed to sync Telegram state: {error}"))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("failed to commit Telegram state: {error}"))?;
        OpenOptions::new()
            .read(true)
            .open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("failed to sync Telegram state directory: {error}"))
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bot() -> TelegramUser {
        TelegramUser {
            id: 42,
            is_bot: true,
            username: Some("lenso_bot".to_owned()),
        }
    }

    fn message(kind: &str, text: &str) -> TelegramMessage {
        TelegramMessage {
            message_id: 7,
            message_thread_id: None,
            from: Some(TelegramUser {
                id: 9,
                is_bot: false,
                username: None,
            }),
            chat: TelegramChat {
                id: -100,
                kind: kind.to_owned(),
            },
            text: Some(text.to_owned()),
            reply_to_message: None,
        }
    }

    #[test]
    fn allowlist_is_explicit_and_supports_one_intentional_wildcard() {
        assert!(ChatAllowlist::parse(&[]).is_err());
        assert!(ChatAllowlist::parse(&["*".to_owned(), "1".to_owned()]).is_err());
        let explicit = ChatAllowlist::parse(&["-100".to_owned()]).unwrap();
        assert!(explicit.contains(-100));
        assert!(!explicit.contains(1));
        assert!(ChatAllowlist::parse(&["*".to_owned()]).unwrap().contains(1));
    }

    #[test]
    fn private_messages_pass_and_groups_require_a_mention_by_default() {
        assert_eq!(
            message_prompt(&message("private", "hello"), &bot(), false).as_deref(),
            Some("hello")
        );
        assert!(message_prompt(&message("group", "hello"), &bot(), false).is_none());
        assert_eq!(
            message_prompt(
                &message("group", "@Lenso_Bot summarize this"),
                &bot(),
                false
            )
            .as_deref(),
            Some("summarize this")
        );
    }

    #[test]
    fn conversation_identity_is_stable_per_bot_chat_and_topic() {
        let mut value = message("supergroup", "hello");
        assert_eq!(conversation_key(42, &value), "telegram_42_-100");
        value.message_thread_id = Some(8);
        assert_eq!(conversation_key(42, &value), "telegram_42_-100_thread_8");
    }

    #[test]
    fn replies_are_split_on_unicode_boundaries() {
        let text = "界".repeat(MAX_MESSAGE_CHARS + 1);
        let chunks = split_message(&text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), MAX_MESSAGE_CHARS);
        assert_eq!(chunks[1], "界");
    }

    #[test]
    fn update_offset_round_trips_and_rejects_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("telegram/state.json");
        let mut state = load_state(&path).unwrap();
        assert_eq!(state.next_update_id, None);
        state.next_update_id = Some(12);
        state
            .sessions
            .insert("telegram_42_100".to_owned(), "session-1".to_owned());
        persist_state(&path, &state).unwrap();
        let restored = load_state(&path).unwrap();
        assert_eq!(restored.next_update_id, Some(12));
        assert_eq!(restored.sessions["telegram_42_100"], "session-1");
        fs::write(&path, b"{}").unwrap();
        assert!(load_state(&path).is_err());
    }
}

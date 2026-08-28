use std::{
    fs,
    path::{Path, PathBuf},
};

use lenso_agent_host::AgentDirectories;
use serde::Deserialize;

use crate::{
    channel::TurnGate,
    discord::{ChannelAllowlist, DiscordOptions},
    telegram::{ChatAllowlist, TelegramOptions},
};

const CONFIG_SCHEMA_VERSION: u32 = 1;
const DEFAULT_QUEUE_CAPACITY: usize = 16;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelHostConfig {
    schema_version: u32,
    #[serde(default = "default_queue_capacity")]
    queue_capacity: usize,
    #[serde(default)]
    telegram: Option<TelegramConfig>,
    #[serde(default)]
    discord: Option<DiscordConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TelegramConfig {
    #[serde(default = "default_telegram_token_env")]
    token_env: String,
    allowed_chats: Vec<String>,
    #[serde(default)]
    allowed_tools: Vec<String>,
    #[serde(default)]
    respond_all_groups: bool,
    #[serde(default)]
    state: Option<PathBuf>,
    #[serde(default = "default_poll_timeout")]
    poll_timeout_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscordConfig {
    #[serde(default = "default_discord_token_env")]
    token_env: String,
    allowed_channels: Vec<String>,
    #[serde(default)]
    allowed_tools: Vec<String>,
    #[serde(default)]
    respond_all_guilds: bool,
    #[serde(default)]
    message_content_intent: bool,
    #[serde(default)]
    state: Option<PathBuf>,
}

#[derive(Debug)]
pub struct ChannelOptions {
    pub telegram: Option<TelegramOptions>,
    pub discord: Option<DiscordOptions>,
}

impl ChannelHostConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let source = fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        toml::from_str(&source)
            .map_err(|error| format!("invalid Channel configuration {}: {error}", path.display()))
    }

    pub fn resolve(self) -> Result<ChannelOptions, String> {
        let directories = AgentDirectories::resolve()?;
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(format!(
                "unsupported Channel configuration schema version {}; expected {CONFIG_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if self.telegram.is_none() && self.discord.is_none() {
            return Err("Channel configuration must select Telegram, Discord, or both".to_owned());
        }
        let turn_gate = TurnGate::new(self.queue_capacity)?;
        let telegram = self
            .telegram
            .map(|config| config.resolve(turn_gate.clone(), &directories))
            .transpose()?;
        let discord = self
            .discord
            .map(|config| config.resolve(turn_gate, &directories))
            .transpose()?;
        Ok(ChannelOptions { telegram, discord })
    }
}

impl TelegramConfig {
    fn resolve(
        self,
        turn_gate: TurnGate,
        directories: &AgentDirectories,
    ) -> Result<TelegramOptions, String> {
        let token = read_token("Telegram", &self.token_env)?;
        let allowed_chats = ChatAllowlist::parse(&self.allowed_chats)?;
        let mut options = TelegramOptions::new(
            token,
            allowed_chats,
            self.state.unwrap_or_else(|| directories.telegram_state()),
        );
        options.allowed_tools = self.allowed_tools;
        options.respond_all_groups = self.respond_all_groups;
        options.poll_timeout_seconds = self.poll_timeout_seconds;
        options.turn_gate = turn_gate;
        Ok(options)
    }
}

impl DiscordConfig {
    fn resolve(
        self,
        turn_gate: TurnGate,
        directories: &AgentDirectories,
    ) -> Result<DiscordOptions, String> {
        if self.respond_all_guilds && !self.message_content_intent {
            return Err(
                "Discord `respond_all_guilds` requires `message_content_intent = true`".to_owned(),
            );
        }
        let token = read_token("Discord", &self.token_env)?;
        let allowed_channels = ChannelAllowlist::parse(&self.allowed_channels)?;
        let mut options = DiscordOptions::new(
            token,
            allowed_channels,
            self.state.unwrap_or_else(|| directories.discord_state()),
        );
        options.allowed_tools = self.allowed_tools;
        options.respond_all_guilds = self.respond_all_guilds;
        options.message_content_intent = self.message_content_intent;
        options.turn_gate = turn_gate;
        Ok(options)
    }
}

fn read_token(channel: &str, variable: &str) -> Result<String, String> {
    if variable.trim().is_empty() || variable.contains('=') {
        return Err(format!(
            "{channel} token environment variable name is invalid"
        ));
    }
    std::env::var(variable)
        .map_err(|_| format!("{channel} Bot token environment variable `{variable}` is missing"))
}

const fn default_queue_capacity() -> usize {
    DEFAULT_QUEUE_CAPACITY
}

fn default_telegram_token_env() -> String {
    "TELEGRAM_BOT_TOKEN".to_owned()
}

fn default_discord_token_env() -> String {
    "DISCORD_BOT_TOKEN".to_owned()
}

const fn default_poll_timeout() -> u64 {
    30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_channels_without_secret_values() {
        let config: ChannelHostConfig = toml::from_str(
            r#"
schema_version = 1
queue_capacity = 8

[telegram]
allowed_chats = ["-100123"]

[discord]
allowed_channels = ["1234567890"]
"#,
        )
        .unwrap();

        assert_eq!(config.queue_capacity, 8);
        assert_eq!(config.telegram.unwrap().token_env, "TELEGRAM_BOT_TOKEN");
        assert_eq!(config.discord.unwrap().token_env, "DISCORD_BOT_TOKEN");
    }

    #[test]
    fn rejects_unknown_configuration() {
        let error = toml::from_str::<ChannelHostConfig>(
            r#"
schema_version = 1
surprise = true
[telegram]
allowed_chats = ["*"]
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown field `surprise`"));
    }

    #[test]
    fn rejects_an_empty_channel_selection_before_reading_tokens() {
        let config: ChannelHostConfig = toml::from_str("schema_version = 1").unwrap();
        let error = config.resolve().unwrap_err();
        assert!(error.contains("must select Telegram, Discord, or both"));
    }
}

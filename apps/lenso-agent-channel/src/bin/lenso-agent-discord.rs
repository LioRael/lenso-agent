use std::{path::PathBuf, process::ExitCode};

use clap::{ArgAction, Parser};
use lenso_agent_channel::discord::{self, ChannelAllowlist, DiscordOptions};
use lenso_agent_discord_plugin as _;
use lenso_agent_host::{AgentDirectories, AgentHost, DiscordSurface, Profile};

/// Run the composed Lenso Agent as a Discord Bot.
#[derive(Debug, Parser)]
#[command(
    name = "lenso-agent-discord",
    version,
    about = "Run the composed Lenso Agent through Discord"
)]
struct Args {
    /// Exact immutable Resolved App Plan used by the Discord surface.
    #[arg(long, value_name = "PATH")]
    plan: Option<PathBuf>,

    /// Environment variable containing the Discord Bot token.
    #[arg(long, default_value = "DISCORD_BOT_TOKEN", value_name = "NAME")]
    token_env: String,

    /// Discord channel ID allowed to invoke the Agent. Repeat or use '*' intentionally.
    #[arg(
        long = "allow-channel",
        value_name = "ID",
        action = ArgAction::Append,
        required = true
    )]
    allowed_channels: Vec<String>,

    /// Model-visible Tool allowed for Discord Turns. No Tools are allowed by default.
    #[arg(long = "allow-tool", value_name = "NAME", action = ArgAction::Append)]
    allowed_tools: Vec<String>,

    /// Respond to every guild message instead of requiring an @mention or reply.
    #[arg(long, requires = "message_content_intent")]
    respond_all_guilds: bool,

    /// Request Discord's privileged Message Content Gateway Intent.
    #[arg(long)]
    message_content_intent: bool,

    /// Durable Discord Gateway state. Defaults below the Agent Home.
    #[arg(long, value_name = "PATH")]
    state: Option<PathBuf>,

    /// Stop after observing this many Message Create events. Useful for supervised smoke runs.
    #[arg(long, value_name = "COUNT")]
    max_messages: Option<u64>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let local = tokio::task::LocalSet::new();
    match Box::pin(local.run_until(run(Args::parse()))).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    if args.max_messages == Some(0) {
        return Err("--max-messages must be greater than zero".to_owned());
    }
    let token = std::env::var(&args.token_env).map_err(|_| {
        format!(
            "Discord Bot token environment variable `{}` is missing",
            args.token_env
        )
    })?;
    let allowed_channels = ChannelAllowlist::parse(&args.allowed_channels)?;
    let profile = args.plan.map_or(Profile::Default, Profile::resolved_plan);
    let host = AgentHost::builder()
        .plugins(lenso_agent_default_plugins::link)
        .surface(DiscordSurface::messaging())
        .build()?;
    let mut app = host.run(profile).await?;
    let state = args
        .state
        .unwrap_or(AgentDirectories::resolve()?.discord_state());
    let mut options = DiscordOptions::new(token, allowed_channels, state);
    options.allowed_tools = args.allowed_tools;
    options.respond_all_guilds = args.respond_all_guilds;
    options.message_content_intent = args.message_content_intent;
    options.max_messages = args.max_messages;
    let result = discord::run(&app, &options).await;
    let shutdown = app.shutdown().await;
    result.and(shutdown)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn discord_surface_requires_an_explicit_channel_allowlist() {
        let command = Args::command();
        command.clone().debug_assert();
        assert!(Args::try_parse_from(["lenso-agent-discord"]).is_err());
        let args =
            Args::try_parse_from(["lenso-agent-discord", "--allow-channel", "1234567890"]).unwrap();
        assert_eq!(args.allowed_channels, ["1234567890"]);
        assert!(args.allowed_tools.is_empty());
    }

    #[test]
    fn responding_to_all_guild_messages_requires_the_privileged_intent() {
        assert!(
            Args::try_parse_from([
                "lenso-agent-discord",
                "--allow-channel",
                "1234567890",
                "--respond-all-guilds",
            ])
            .is_err()
        );
    }
}

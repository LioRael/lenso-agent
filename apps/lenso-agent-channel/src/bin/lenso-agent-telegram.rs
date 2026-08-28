use std::{path::PathBuf, process::ExitCode};

use clap::{ArgAction, Parser};
use lenso_agent_channel::telegram::{self, ChatAllowlist, TelegramOptions};
use lenso_agent_host::{generation::AgentApp, plan_bytes};
use lenso_agent_telegram_plugin as _;

/// Run the composed Lenso Agent as a Telegram Bot.
#[derive(Debug, Parser)]
#[command(
    name = "lenso-agent-telegram",
    version,
    about = "Run the composed Lenso Agent through Telegram"
)]
struct Args {
    /// Exact immutable Resolved App Plan used by the Telegram surface.
    #[arg(long, value_name = "PATH")]
    plan: Option<PathBuf>,

    /// Environment variable containing the Telegram Bot token.
    #[arg(long, default_value = "TELEGRAM_BOT_TOKEN", value_name = "NAME")]
    token_env: String,

    /// Telegram chat ID allowed to invoke the Agent. Repeat or use '*' intentionally.
    #[arg(
        long = "allow-chat",
        value_name = "ID",
        action = ArgAction::Append,
        required = true,
        allow_hyphen_values = true
    )]
    allowed_chats: Vec<String>,

    /// Model-visible Tool allowed for Telegram Turns. No Tools are allowed by default.
    #[arg(long = "allow-tool", value_name = "NAME", action = ArgAction::Append)]
    allowed_tools: Vec<String>,

    /// Respond to every group message instead of requiring an @mention or reply.
    #[arg(long)]
    respond_all_groups: bool,

    /// Durable Telegram update cursor.
    #[arg(
        long,
        default_value = ".lenso/telegram/state.json",
        value_name = "PATH"
    )]
    state: PathBuf,

    /// Telegram long-poll timeout.
    #[arg(long, default_value_t = 30, value_name = "SECONDS")]
    poll_timeout_seconds: u64,

    /// Stop after observing this many updates. Useful for supervised smoke runs.
    #[arg(long, value_name = "COUNT")]
    max_updates: Option<u64>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let local = tokio::task::LocalSet::new();
    match local.run_until(run(Args::parse())).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    lenso_agent_default_plugins::link();
    if args.max_updates == Some(0) {
        return Err("--max-updates must be greater than zero".to_owned());
    }
    let token = std::env::var(&args.token_env).map_err(|_| {
        format!(
            "Telegram Bot token environment variable `{}` is missing",
            args.token_env
        )
    })?;
    let allowed_chats = ChatAllowlist::parse(&args.allowed_chats)?;
    let bytes = plan_bytes(args.plan.as_deref())?;
    let mut app = AgentApp::start_telegram(&bytes)
        .await
        .map_err(|error| format!("App startup failed: {error}"))?;
    let mut options = TelegramOptions::new(token, allowed_chats, args.state);
    options.allowed_tools = args.allowed_tools;
    options.respond_all_groups = args.respond_all_groups;
    options.poll_timeout_seconds = args.poll_timeout_seconds;
    options.max_updates = args.max_updates;
    let result = telegram::run(&app, &options).await;
    let shutdown = app.shutdown().await;
    result.and(shutdown)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn telegram_surface_requires_an_explicit_chat_allowlist() {
        let command = Args::command();
        command.clone().debug_assert();
        assert!(Args::try_parse_from(["lenso-agent-telegram"]).is_err());
        let args =
            Args::try_parse_from(["lenso-agent-telegram", "--allow-chat", "-100123"]).unwrap();
        assert_eq!(args.allowed_chats, ["-100123"]);
        assert!(args.allowed_tools.is_empty());
    }
}

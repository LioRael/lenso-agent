use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use lenso_agent_channel::{channel_host::ChannelHostConfig, discord, telegram};
use lenso_agent_discord_plugin as _;
use lenso_agent_host::{generation::AgentApp, plan_bytes};
use lenso_agent_telegram_plugin as _;

/// Run the composed Lenso Agent through every configured messaging Channel.
#[derive(Debug, Parser)]
#[command(
    name = "lenso-agent-channel",
    version,
    about = "Run Telegram and Discord through one Lenso Agent Host"
)]
struct Args {
    /// Human-authored Channel configuration. Tokens remain in environment variables.
    #[arg(long, default_value = "lenso.channels.toml", value_name = "PATH")]
    config: PathBuf,

    /// Exact immutable Resolved App Plan. This is an advanced Host override.
    #[arg(long, value_name = "PATH", hide = true)]
    plan: Option<PathBuf>,
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
    lenso_agent_default_plugins::link();
    let options = ChannelHostConfig::load(&args.config)?.resolve()?;
    let bytes = plan_bytes(args.plan.as_deref())?;
    let mut app = AgentApp::start_channels(&bytes)
        .await
        .map_err(|error| format!("App startup failed: {error}"))?;
    let route_validation = validate_routes(&app, &options).await;
    if let Err(error) = route_validation {
        return match app.shutdown().await {
            Ok(()) => Err(error),
            Err(shutdown) => Err(format!("{error}; App shutdown also failed: {shutdown}")),
        };
    }

    let surfaces = match (&options.telegram, &options.discord) {
        (Some(telegram_options), Some(discord_options)) => {
            eprintln!("Channel Host ready: Telegram + Discord");
            tokio::select! {
                result = telegram::run(&app, telegram_options) => result,
                result = discord::run(&app, discord_options) => result,
            }
        }
        (Some(telegram_options), None) => {
            eprintln!("Channel Host ready: Telegram");
            telegram::run(&app, telegram_options).await
        }
        (None, Some(discord_options)) => {
            eprintln!("Channel Host ready: Discord");
            discord::run(&app, discord_options).await
        }
        (None, None) => unreachable!("Channel configuration validation requires a surface"),
    };
    let shutdown = app.shutdown().await;
    surfaces.and(shutdown)
}

async fn validate_routes(
    app: &AgentApp,
    options: &lenso_agent_channel::channel_host::ChannelOptions,
) -> Result<(), String> {
    if options.telegram.is_some() {
        drop(
            app.lease_telegram_turn()
                .await
                .map_err(|error| format!("Telegram route validation failed: {error}"))?,
        );
    }
    if options.discord.is_some() {
        drop(
            app.lease_discord_turn()
                .await
                .map_err(|error| format!("Discord route validation failed: {error}"))?,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn channel_distribution_links_only_messaging_surfaces() {
        lenso_agent_default_plugins::link();
        let catalog =
            serde_json::to_value(lenso_agent_host::generation::linked_host_catalog()).unwrap();
        let catalog = catalog.to_string();
        assert!(catalog.contains(r#""plugin_id":"lenso.agent.telegram""#));
        assert!(catalog.contains(r#""plugin_id":"lenso.agent.discord""#));
        assert!(!catalog.contains(r#""plugin_id":"lenso.agent.cli""#));
        assert!(!catalog.contains(r#""plugin_id":"lenso.agent.tui""#));
    }

    #[test]
    fn channel_host_has_a_direct_no_subcommand_entrypoint() {
        let command = Args::command();
        command.clone().debug_assert();
        let args = Args::try_parse_from(["lenso-agent-channel"]).unwrap();
        assert_eq!(args.config, PathBuf::from("lenso.channels.toml"));
        assert!(args.plan.is_none());
    }
}

use std::{path::PathBuf, process::ExitCode};

use clap::{ArgAction, Parser};
use lenso_agent_host::{AgentHost, Profile, TuiSurface};
use lenso_agent_session_terminal_plugin as _;
use lenso_agent_tui_plugin as _;
use lenso_agent_tui_static_plugin as _;
use lenso_agent_tui_workspace_suggestions_plugin as _;
use lenso_terminal_command_plugin as _;
use lenso_terminal_tui_plugin as _;

mod dispatch;
mod tui;
use tui::TuiOptions;

/// Interactive Lenso Agent. Running without arguments opens the TUI.
#[derive(Debug, Parser)]
#[command(
    name = "lenso-agent",
    version,
    about = "Lenso Agent: interactive coding, headless tasks, and workspace management",
    after_help = dispatch::COMMAND_HELP
)]
struct Args {
    /// Exact immutable Resolved App Plan used by the TUI.
    #[arg(long, value_name = "PATH")]
    plan: Option<PathBuf>,

    /// Resume an existing Session.
    #[arg(long, value_name = "ID")]
    session: Option<String>,

    /// Select `<agent-home>/profiles/<name>.toml` for this Session.
    #[arg(long, value_name = "NAME", conflicts_with = "plan")]
    profile: Option<String>,

    /// Narrow the selected App's Tool set for every submitted Turn.
    #[arg(long = "allow-tool", value_name = "NAME", action = ArgAction::Append, conflicts_with = "no_tools")]
    allowed_tools: Vec<String>,

    /// Disable every Tool for submitted Turns.
    #[arg(long, conflicts_with = "allowed_tools")]
    no_tools: bool,
}

fn main() -> ExitCode {
    let mut raw = std::env::args_os().skip(1).collect::<Vec<_>>();
    if let Some((executable, args)) = dispatch::route(&raw) {
        return dispatch::launch(executable, &args);
    }
    if raw.first().is_some_and(|arg| arg == "tui") {
        raw.remove(0);
    }
    let args =
        Args::parse_from(std::iter::once(std::ffi::OsString::from("lenso-agent")).chain(raw));
    run_tui(args)
}

#[tokio::main(flavor = "current_thread")]
async fn run_tui(args: Args) -> ExitCode {
    let local = tokio::task::LocalSet::new();
    match Box::pin(local.run_until(run(args))).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    let profile = match (&args.plan, &args.profile) {
        (Some(plan), None) => Profile::resolved_plan(plan),
        (None, Some(profile)) => Profile::named(profile),
        (None, None) => Profile::Default,
        (Some(_), Some(_)) => unreachable!("clap rejects Plan/Profile conflicts"),
    };
    let host = AgentHost::builder()
        .plugins(lenso_agent_default_plugins::link)
        .surface(TuiSurface::terminal())
        .build()?;
    let mut app = host.run(profile).await?;
    let allowed_tools = if args.no_tools {
        Some(Vec::new())
    } else if args.allowed_tools.is_empty() {
        None
    } else {
        Some(args.allowed_tools)
    };
    let result = tui::run(
        &app,
        TuiOptions {
            allowed_tools,
            profile: args.profile,
            session_id: args.session,
        },
    )
    .await;
    let shutdown = app.shutdown().await;
    result.and(shutdown)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn tui_distribution_links_only_the_tui_surface() {
        lenso_agent_default_plugins::link();
        let catalog =
            serde_json::to_value(lenso_agent_host::generation::linked_host_catalog()).unwrap();
        let catalog = catalog.to_string();
        assert!(catalog.contains(r#""plugin_id":"lenso.agent.tui""#));
        assert!(catalog.contains(r#""plugin_id":"lenso.terminal.tui""#));
        assert!(catalog.contains(r#""plugin_id":"lenso.terminal.command""#));
        assert!(catalog.contains(r#""plugin_id":"lenso.agent.session-terminal""#));
        assert!(!catalog.contains(r#""plugin_id":"lenso.agent.cli""#));
        assert!(!catalog.contains(r#""plugin_id":"lenso.terminal.cli""#));
        assert!(!catalog.contains(r#""plugin_id":"lenso.agent.telegram""#));
        assert!(!catalog.contains(r#""plugin_id":"lenso.agent.discord""#));
    }

    #[test]
    fn product_entrypoint_has_no_subcommands() {
        let command = Args::command();
        command.clone().debug_assert();
        assert_eq!(command.get_subcommands().count(), 0);
    }

    #[test]
    fn empty_arguments_select_the_tui() {
        let args = Args::try_parse_from(["lenso-agent"]).unwrap();
        assert!(args.plan.is_none());
        assert!(args.session.is_none());
        assert!(args.profile.is_none());
        assert!(args.allowed_tools.is_empty());
        assert!(!args.no_tools);
    }
}

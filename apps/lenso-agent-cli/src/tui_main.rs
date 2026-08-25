use std::{fs, path::PathBuf, process::ExitCode};

use clap::{ArgAction, Parser};
use lenso_agent_cli::{
    default_plan,
    generation::AgentApp,
    tui::{self, TuiOptions},
};

/// Interactive Lenso Agent. Running without arguments opens the TUI.
#[derive(Debug, Parser)]
#[command(
    name = "lenso-agent",
    version,
    about = "Run the composed Lenso Agent terminal interface"
)]
struct Args {
    /// Exact immutable Resolved App Plan used by the TUI.
    #[arg(long, value_name = "PATH")]
    plan: Option<PathBuf>,

    /// Resume an existing Session.
    #[arg(long, value_name = "ID")]
    session: Option<String>,

    /// Narrow the selected App's Tool set for every submitted Turn.
    #[arg(long = "allow-tool", value_name = "NAME", action = ArgAction::Append, conflicts_with = "no_tools")]
    allowed_tools: Vec<String>,

    /// Disable every Tool for submitted Turns.
    #[arg(long, conflicts_with = "allowed_tools")]
    no_tools: bool,
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
    let plan = match args.plan {
        Some(plan) => plan,
        None => default_plan()?,
    };
    let bytes =
        fs::read(&plan).map_err(|error| format!("failed to read {}: {error}", plan.display()))?;
    let mut app = AgentApp::start_tui(&bytes)
        .await
        .map_err(|error| format!("App startup failed: {error}"))?;
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
        assert!(args.allowed_tools.is_empty());
        assert!(!args.no_tools);
    }
}

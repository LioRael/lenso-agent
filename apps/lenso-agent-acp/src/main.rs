use std::{path::PathBuf, process::ExitCode};

use clap::{ArgAction, Parser};
use lenso_agent_acp::{AgentAcpConfig, run_stdio};

#[derive(Debug, Parser)]
#[command(
    name = "lenso-agent-acp",
    version,
    about = "Run the Lenso Agent Harness over ACP stdio"
)]
struct Args {
    /// Exact immutable Resolved App Plan used by the ACP surface.
    #[arg(long, value_name = "PATH")]
    plan: Option<PathBuf>,

    /// Select `<agent-home>/profiles/<name>.toml` for this ACP process.
    #[arg(long, value_name = "NAME", conflicts_with = "plan")]
    profile: Option<String>,

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
    let allowed_tools = if args.no_tools {
        Some(Vec::new())
    } else if args.allowed_tools.is_empty() {
        None
    } else {
        Some(args.allowed_tools)
    };
    run_stdio(AgentAcpConfig {
        agent_home: None,
        allowed_tools,
        plan: args.plan,
        plugins: lenso_agent_default_plugins::link,
        profile: args.profile,
    })
    .await
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
    fn empty_arguments_preserve_composed_tools() {
        let args = Args::try_parse_from(["lenso-agent-acp"]).unwrap();
        assert!(args.plan.is_none());
        assert!(args.profile.is_none());
        assert!(args.allowed_tools.is_empty());
        assert!(!args.no_tools);
    }

    #[test]
    fn acp_distribution_links_only_its_surface_consumer() {
        lenso_agent_default_plugins::link();
        let catalog =
            serde_json::to_value(lenso_agent_host::generation::linked_host_catalog()).unwrap();
        let catalog = catalog.to_string();
        assert!(catalog.contains(r#""plugin_id":"lenso.agent.acp""#));
        assert!(!catalog.contains(r#""plugin_id":"lenso.agent.cli""#));
        assert!(!catalog.contains(r#""plugin_id":"lenso.agent.tui""#));
        assert!(!catalog.contains(r#""plugin_id":"lenso.agent.web""#));
    }
}

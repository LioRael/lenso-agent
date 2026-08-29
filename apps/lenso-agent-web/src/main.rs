use std::{net::SocketAddr, path::PathBuf, process::ExitCode};

use clap::{ArgAction, Parser};
use lenso_agent_web::{AgentWebConfig, AgentWebControl, AgentWebSurface, CONTROL_TOKEN_ENV};

#[derive(Debug, Parser)]
#[command(
    name = "lenso-agent-web",
    version,
    about = "Run the Lenso Agent Harness Web API"
)]
struct Args {
    /// Address used by the Agent Web API.
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,

    /// Exact immutable Resolved App Plan used by the Web surface.
    #[arg(long, value_name = "PATH")]
    plan: Option<PathBuf>,

    /// Select `<agent-home>/profiles/<name>.toml` for this Web process.
    #[arg(long, value_name = "NAME", conflicts_with = "plan")]
    profile: Option<String>,

    /// Admit one Plan-bound Tool to every Console Agent Turn. Repeat to admit more.
    #[arg(long = "allow-tool", value_name = "NAME", action = ArgAction::Append)]
    allowed_tools: Vec<String>,

    /// Durable Tool policy file. Enabling mutation also requires `LENSO_AGENT_CONTROL_TOKEN`.
    #[arg(long, value_name = "PATH")]
    tool_policy: Option<PathBuf>,

    /// Allow the authorized Console Host to mutate this Agent Home's Plugin Root.
    #[arg(long)]
    plugin_control: bool,
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
    let mut config = AgentWebConfig::new(lenso_agent_default_plugins::link);
    config.plan = args.plan;
    config.profile = args.profile;
    config.allowed_tools = args.allowed_tools;
    config.tool_policy = args.tool_policy;
    config.plugin_control = args.plugin_control;
    config.control = std::env::var(CONTROL_TOKEN_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .map_or(AgentWebControl::Disabled, AgentWebControl::Bearer);
    let surface = AgentWebSurface::start(config).await?;
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .map_err(|error| format!("failed to bind {}: {error}", args.listen))?;
    println!("Lenso Agent Web listening on http://{}", args.listen);
    axum::serve(listener, surface.router())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("Agent Web server failed: {error}"))?;
    surface.shutdown().await
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

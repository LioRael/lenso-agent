use std::{net::SocketAddr, path::PathBuf, process::ExitCode};

use clap::{ArgAction, Parser};
use lenso_agent_web::{
    AgentWebAccess, AgentWebConfig, AgentWebControl, AgentWebSurface, CONTROL_TOKEN_ENV,
    DATA_PLANE_TOKEN_ENV, PluginConfigurationStoreConfig, RemotePluginConfigurationConfig,
    RemotePluginConfigurationResource,
};

const REMOTE_CONFIGURATION_TOKEN_ENV: &str = "LENSO_PLUGIN_CONFIGURATION_REMOTE_TOKEN";

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

    /// SQLite store for managed Plugin configuration proposals and publications.
    #[arg(long, value_name = "PATH", requires = "plugin_control")]
    plugin_configuration_store: Option<PathBuf>,

    /// Remote Plugin configuration service base URL.
    #[arg(
        long,
        value_name = "URL",
        requires_all = ["plugin_control", "plugin_configuration_app", "plugin_configuration_environment"],
        conflicts_with = "plugin_configuration_store"
    )]
    plugin_configuration_remote: Option<String>,

    /// Stable App identity in the remote configuration service.
    #[arg(long, value_name = "APP", requires = "plugin_configuration_remote")]
    plugin_configuration_app: Option<String>,

    /// Stable environment identity in the remote configuration service.
    #[arg(
        long,
        value_name = "ENVIRONMENT",
        requires = "plugin_configuration_remote"
    )]
    plugin_configuration_environment: Option<String>,
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
    let data_plane_token = std::env::var(DATA_PLANE_TOKEN_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let access = access_for_listener(args.listen, data_plane_token)?;
    let mut config = AgentWebConfig::new(lenso_agent_default_plugins::link);
    config.plan = args.plan;
    config.profile = args.profile;
    config.allowed_tools = args.allowed_tools;
    config.tool_policy = args.tool_policy;
    config.plugin_control = args.plugin_control;
    config.plugin_configuration_store = args
        .plugin_configuration_store
        .map(|database| PluginConfigurationStoreConfig::new(database, "agent"));
    config.plugin_configuration_remote = match (
        args.plugin_configuration_remote,
        args.plugin_configuration_app,
        args.plugin_configuration_environment,
    ) {
        (Some(service_url), Some(app), Some(environment)) => {
            let token = std::env::var(REMOTE_CONFIGURATION_TOKEN_ENV)
                .map_err(|_| format!("{REMOTE_CONFIGURATION_TOKEN_ENV} is required for a remote Plugin configuration authority"))?;
            let resource = RemotePluginConfigurationResource::new(service_url, app, environment)
                .map_err(|error| error.to_string())?;
            Some(
                RemotePluginConfigurationConfig::new(resource, token)
                    .map_err(|error| error.to_string())?,
            )
        }
        (None, None, None) => None,
        _ => {
            return Err(
                "remote Plugin configuration requires URL, App, and environment".to_owned(),
            );
        }
    };
    config.access = access;
    config.control = std::env::var(CONTROL_TOKEN_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
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

fn access_for_listener(
    listen: SocketAddr,
    token: Option<String>,
) -> Result<AgentWebAccess, String> {
    match token {
        Some(token) if !token.trim().is_empty() => Ok(AgentWebAccess::Bearer(token)),
        None | Some(_) if listen.ip().is_loopback() => Ok(AgentWebAccess::Local),
        None | Some(_) => Err(format!(
            "{DATA_PLANE_TOKEN_ENV} is required when Agent Web listens beyond loopback"
        )),
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_listener_preserves_token_free_local_startup() {
        let access = access_for_listener("127.0.0.1:8787".parse().unwrap(), None).unwrap();
        assert!(matches!(access, AgentWebAccess::Local));
    }

    #[test]
    fn non_loopback_listener_requires_a_token() {
        let error = access_for_listener("0.0.0.0:8787".parse().unwrap(), None).unwrap_err();
        assert!(error.contains(DATA_PLANE_TOKEN_ENV));
        let whitespace =
            access_for_listener("0.0.0.0:8787".parse().unwrap(), Some(" \t".to_owned()))
                .unwrap_err();
        assert!(whitespace.contains(DATA_PLANE_TOKEN_ENV));
    }

    #[test]
    fn non_loopback_listener_uses_bearer_authorization() {
        let access = access_for_listener(
            "0.0.0.0:8787".parse().unwrap(),
            Some("fixture-token".to_owned()),
        )
        .unwrap();
        assert!(matches!(access, AgentWebAccess::Bearer(token) if token == "fixture-token"));
    }
}

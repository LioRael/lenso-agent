use std::{net::SocketAddr, path::PathBuf, process::ExitCode, sync::Arc};

use clap::{ArgAction, Parser};
use lenso_agent_web::{
    AgentWebAccess, AgentWebConfig, AgentWebControl, AgentWebSurface, CONTROL_TOKEN_ENV,
    DATA_PLANE_TOKEN_ENV, PluginConfigurationStoreConfig, RemotePluginConfigurationConfig,
    RemotePluginConfigurationResource, TrustedPluginBundle,
};

use crate::app_agent_management::{
    AppAgentAdapter, AppAgentPluginManagementTarget, AppAgentToolTarget,
};

const REMOTE_CONFIGURATION_TOKEN_ENV: &str = "LENSO_PLUGIN_CONFIGURATION_REMOTE_TOKEN";

#[derive(Debug, Parser)]
#[command(name = "lenso-agent-web", version, about = "Run Lenso Agent Web API")]
struct Args {
    /// Enable Plugin completion with an existing operators issuer/public-key JSON file.
    #[arg(long, value_name = "ABSOLUTE_PATH")]
    plugin_ai_authority: Option<PathBuf>,
    /// Require signed Console identities using an issuer/public-key JSON verification file.
    #[arg(long, value_name = "ABSOLUTE_PATH")]
    assistant_authority: Option<PathBuf>,
    /// JSON scheduling limits; omitted preserves the original single-user capacity.
    #[arg(long, value_name = "PATH")]
    assistant_scheduling: Option<PathBuf>,
    /// Host-owned platform/member provider assignment and optional BYOK policy JSON.
    #[arg(long, value_name = "PATH")]
    assistant_providers: Option<PathBuf>,
    /// Address used by the Agent Web API.
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,

    /// Grant this Console Agent management access to one loopback App Agent as `ID=URL`.
    #[arg(long = "managed-agent", value_name = "ID=URL", action = ArgAction::Append, requires = "plugin_control")]
    managed_agents: Vec<String>,

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

    /// Trust one local Bundle for model-visible installation as `ID=ABSOLUTE_PATH`.
    #[arg(long = "trusted-plugin-bundle", value_name = "ID=PATH", action = ArgAction::Append, requires = "plugin_control")]
    trusted_plugin_bundles: Vec<String>,

    /// `SQLite` store for managed Plugin configuration proposals and publications.
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

pub(crate) fn launch(linked_plugins: fn(), console: bool) -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Agent Web runtime must initialize");
    let local = tokio::task::LocalSet::new();
    match runtime.block_on(local.run_until(run(Args::parse(), linked_plugins, console))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args, linked_plugins: fn(), console: bool) -> Result<(), String> {
    let data_plane_token = std::env::var(DATA_PLANE_TOKEN_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let access = assistant_access(args.listen, args.assistant_authority, data_plane_token)?;
    let mut config = AgentWebConfig::new(linked_plugins);
    if let Some(path) = args.assistant_providers {
        let policy = read_assistant_configuration(path, "providers")?;
        config.assistant_providers = Some(Arc::new(
            lenso_agent_web::assistant_provider::AssistantProviderService::open(policy)
                .map_err(|e| e.to_string())?,
        ));
    }
    if let Some(path) = args.assistant_scheduling {
        config.scheduling = read_assistant_configuration(path, "scheduling")?;
    }
    if let Some(path) = &args.plugin_ai_authority {
        if !args.listen.ip().is_loopback() {
            return Err("Plugin AI bridge requires a loopback listener".into());
        }
        config.plugin_ai = Some(Arc::new(
            lenso_agent_web::plugin_ai::BridgeAuthority::from_file(path)?,
        ));
    }
    config.plan = args.plan;
    config.profile = args.profile;
    config.allowed_tools = args.allowed_tools;
    config.tool_policy = args.tool_policy;
    config.plugin_control = args.plugin_control;
    let managed_agents = args
        .managed_agents
        .into_iter()
        .map(|value| AppAgentAdapter::parse(&value))
        .collect::<Result<Vec<_>, _>>()?;
    if !managed_agents.is_empty() {
        if !console {
            return Err("--managed-agent requires lenso-agent-console-web".to_owned());
        }
        let tool_target = Arc::new(AppAgentToolTarget::new(managed_agents.clone()));
        config.allowed_tools.extend(
            tool_target
                .prepare()
                .await
                .map_err(|error| format!("managed Agent Tool catalog failed: {error:?}"))?,
        );
        config.agent_tool_target = Some(tool_target);
        config.plugin_management_target = Some(Arc::new(AppAgentPluginManagementTarget::new(
            managed_agents,
        )));
    }
    config.trusted_plugin_bundles = args
        .trusted_plugin_bundles
        .into_iter()
        .map(|value| parse_trusted_plugin_bundle(&value))
        .collect::<Result<_, _>>()?;
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
    if config.plugin_ai.is_some() && matches!(config.control, AgentWebControl::Disabled) {
        return Err("Plugin AI requires the existing Host control authorization seam".into());
    }
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

fn read_assistant_configuration<T: serde::de::DeserializeOwned>(
    path: PathBuf,
    kind: &str,
) -> Result<T, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| format!("invalid assistant {kind}: {e}"))
}

fn assistant_access(
    listen: SocketAddr,
    path: Option<PathBuf>,
    data_plane_token: Option<String>,
) -> Result<AgentWebAccess, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Authority {
        issuer: String,
        public_key: String,
    }
    let Some(path) = path else {
        return access_for_listener(listen, data_plane_token);
    };
    if !path.is_absolute() || std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 16384 {
        return Err("bounded absolute assistant authority file required".into());
    }
    let authority: Authority =
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("invalid assistant authority: {e}"))?;
    let bearer = data_plane_token
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{DATA_PLANE_TOKEN_ENV} is required for assistant proxy ingress"))?;
    Ok(AgentWebAccess::AuthenticatedBearer {
        bearer,
        issuer: authority.issuer,
        public_key: authority.public_key,
    })
}

fn parse_trusted_plugin_bundle(value: &str) -> Result<TrustedPluginBundle, String> {
    let (id, path) = value
        .split_once('=')
        .ok_or_else(|| "trusted Plugin Bundle must use ID=ABSOLUTE_PATH".to_owned())?;
    TrustedPluginBundle::new(id, PathBuf::from(path))
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

    #[test]
    fn trusted_bundle_argument_keeps_the_path_inside_host_configuration() {
        let bundle =
            parse_trusted_plugin_bundle("reviewed.tools=/tmp/reviewed.lenso-plugin").unwrap();

        assert_eq!(bundle.id, "reviewed.tools");
        assert_eq!(bundle.path, PathBuf::from("/tmp/reviewed.lenso-plugin"));
    }

    #[test]
    fn trusted_bundle_argument_rejects_paths_without_host_identity() {
        assert!(parse_trusted_plugin_bundle("/tmp/reviewed.lenso-plugin").is_err());
        assert!(parse_trusted_plugin_bundle("reviewed=relative.lenso-plugin").is_err());
    }
}

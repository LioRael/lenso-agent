use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::Parser;
use lenso_agent_web::{
    CONFIGURATION_SERVICE_READ_TOKEN_ENV, CONFIGURATION_SERVICE_WRITE_TOKEN_ENV,
    PluginConfigurationService, PluginConfigurationServiceAccess,
    PluginConfigurationServiceResource, PluginConfigurationStoreConfig,
    SqlitePluginConfigurationAuthority,
};
use lenso_app_plan::authoring::HostCatalog;

#[derive(Debug, Parser)]
#[command(
    name = "lenso-plugin-configuration-service",
    about = "Serve one durable Lenso Plugin configuration resource."
)]
struct Args {
    /// Address for the HTTP listener. Put TLS in front of non-loopback deployments.
    #[arg(long, default_value = "127.0.0.1:8790")]
    listen: SocketAddr,

    /// Absolute root containing .lenso/host-catalog.json and the managed plugins/ root.
    #[arg(long, value_name = "PATH")]
    root: PathBuf,

    /// Absolute SQLite database path for CAS, proposal, and publication evidence.
    #[arg(long, value_name = "PATH")]
    database: PathBuf,

    /// Stable App identity exposed by this service.
    #[arg(long, value_name = "APP")]
    app: String,

    /// Stable environment identity exposed by this service.
    #[arg(long, value_name = "ENVIRONMENT")]
    environment: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    validate_root(&args.root)?;
    let resource = PluginConfigurationServiceResource::new(args.app, args.environment)
        .map_err(|error| error.to_string())?;
    let access = PluginConfigurationServiceAccess::new(
        required_token(CONFIGURATION_SERVICE_READ_TOKEN_ENV)?,
        required_token(CONFIGURATION_SERVICE_WRITE_TOKEN_ENV)?,
    )
    .map_err(|error| error.to_string())?;
    let authority = SqlitePluginConfigurationAuthority::open(
        &args.root,
        PluginConfigurationStoreConfig::new(&args.database, resource.reference()),
    )
    .map_err(|error| error.to_string())?;
    let service = PluginConfigurationService::new(authority, resource, access);
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .map_err(|error| format!("failed to bind {}: {error}", args.listen))?;
    axum::serve(listener, service.router())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("Plugin configuration service failed: {error}"))
}

fn validate_root(root: &Path) -> Result<(), String> {
    if !root.is_absolute() || root.to_str().is_none() {
        return Err(format!(
            "Plugin configuration service root must be an absolute UTF-8 path: {}",
            root.display()
        ));
    }
    let catalog = root.join(".lenso/host-catalog.json");
    let bytes = fs::read(&catalog)
        .map_err(|error| format!("failed to read {}: {error}", catalog.display()))?;
    serde_json::from_slice::<HostCatalog>(&bytes)
        .map_err(|error| format!("invalid Host Catalog {}: {error}", catalog.display()))?;
    Ok(())
}

fn required_token(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is required"))
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use lenso_agent_foundation::{FoundationCheck, check, foundation_host_catalog, initialize};

#[derive(Debug, Parser)]
#[command(
    name = "lenso-agent-foundation",
    about = "Create and inspect a blank, explicitly composed Lenso Agent Foundation"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize a new blank Foundation root without replacing another Host's authority.
    Init {
        /// Project root that will own `.lenso/host-catalog.json` and `plugins/`.
        #[arg(long, default_value = ".")]
        root: PathBuf,
    },
    /// Resolve the current explicit Plugin Root without starting any runtime behavior.
    Check {
        /// Project root initialized by `lenso-agent-foundation init`.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Emit the versioned diagnostic result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the product-neutral Host Catalog used by a blank Foundation root.
    Catalog {
        /// Emit compact JSON instead of indented JSON.
        #[arg(long)]
        compact: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { root } => {
            let catalog = initialize(&root).map_err(anyhow::Error::msg)?;
            println!("Initialized blank Agent Foundation at {}", root.display());
            println!(
                "Published product-neutral Host Catalog at {}",
                catalog.display()
            );
        }
        Command::Check { root, json } => {
            let result = check(&root).map_err(anyhow::Error::msg)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                print_check(&result);
            }
        }
        Command::Catalog { compact } => {
            let catalog = foundation_host_catalog();
            let encoded = if compact {
                serde_json::to_string(&catalog)?
            } else {
                serde_json::to_string_pretty(&catalog)?
            };
            println!("{encoded}");
        }
    }
    Ok(())
}

fn print_check(result: &FoundationCheck) {
    println!("Foundation status: {:?}", result.status);
    if result.plugin_instances.is_empty() {
        println!("Selected Plugin instances: none");
    } else {
        println!("Selected Plugin instances:");
        for instance in &result.plugin_instances {
            println!("  {instance}");
        }
    }
    if result.capability_bindings.is_empty() {
        println!("Capability bindings: none");
    } else {
        println!("Capability bindings:");
        for binding in &result.capability_bindings {
            println!("  {binding}");
        }
    }
    if !result.plugin_sources.is_empty() {
        println!("Plugin source diagnostics:");
        for source in &result.plugin_sources {
            let revision = if source.package_revision.is_empty() {
                "<unlocked>"
            } else {
                source.package_revision.as_str()
            };
            println!(
                "  {}: {:?}; package {}@{}; entrypoint {}; profile {}; class {}",
                source.instance,
                source.selection_source,
                source.package_id,
                revision,
                source.entrypoint,
                source.runtime_profile,
                source.execution_class,
            );
            println!("    Why: {}", source.selection_reason);
        }
    }
    if !result.capability_selections.is_empty() {
        println!("Capability selection diagnostics:");
        for selection in &result.capability_selections {
            println!(
                "  {}: {} --{}@{}--> {} (order {})",
                selection.requirement_id,
                selection.consumer_instance,
                selection.capability_id,
                selection.descriptor_version,
                selection.provider_instance,
                selection.provider_order,
            );
            println!("    Why: {}", selection.selection_reason);
        }
    }
    println!(
        "Authorization boundary: {}. {}",
        result.authorization_boundary.policy_owner, result.authorization_boundary.message
    );
    for diagnostic in &result.diagnostics {
        println!("{}: {}", diagnostic.code, diagnostic.message);
        println!("Next: {}", diagnostic.next_action);
    }
}

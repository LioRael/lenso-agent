use std::{env, path::PathBuf};

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "lenso-agent-dialogue",
    about = "Run the explicit deterministic non-Coding Lenso Agent dialogue starter"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Display the exact non-Coding Plan selected for this home.
    Plan {
        /// Agent state directory. Defaults to `.lenso-agent-dialogue` in the current directory.
        #[arg(long, default_value = ".lenso-agent-dialogue")]
        home: PathBuf,
    },
    /// Run one fixture-backed dialogue Turn with no Tool Provider or workspace access.
    Run {
        /// Agent state directory. Defaults to `.lenso-agent-dialogue` in the current directory.
        #[arg(long, default_value = ".lenso-agent-dialogue")]
        home: PathBuf,
        /// Input sent to the selected fixture Model.
        input: String,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(Box::pin(async move {
            match cli.command {
                Command::Plan { home } => {
                    let home = absolute(home)?;
                    let path = lenso_agent_dialogue::write_starter_plan(&home)
                        .map_err(anyhow::Error::msg)?;
                    let plan = std::fs::read_to_string(path)?;
                    println!("{plan}");
                }
                Command::Run { home, input } => {
                    let home = absolute(home)?;
                    let result = lenso_agent_dialogue::run_dialogue(&home, input)
                        .await
                        .map_err(anyhow::Error::msg)?;
                    print!("{}", result.text);
                    if !result.text.ends_with('\n') {
                        println!();
                    }
                    if let Some(session_id) = result.session_id {
                        eprintln!("session: {session_id}");
                    }
                }
            }
            Ok(())
        }))
        .await
}

fn absolute(path: PathBuf) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(env::current_dir()?.join(path))
}

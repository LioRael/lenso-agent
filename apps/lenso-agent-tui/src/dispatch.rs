//! Select a process-owning surface before constructing a Host or Tokio runtime.
use std::{
    env,
    ffi::OsString,
    process::{Command, ExitCode},
};

pub(super) const COMMAND_HELP: &str = "Commands:
  run <prompt>       Run a task without the terminal UI
  auth               Log in, inspect authentication, or log out
  profiles           Install or import coding Profiles
  sessions           List, export, or inspect Sessions
  models             List available models
  contexts           List available context sources
  approvals          Inspect or resolve pending approvals
  doctor             Check the installation and Agent Home
  generations        Inspect Generation provenance
  runtime            Inspect runtime status
  acp                Start the editor protocol (requires the ACP component)
  tui                Explicitly start the terminal UI
  cli                Compatibility entrypoint for the legacy CLI

Examples:
  lenso-agent auth login
  lenso-agent profiles install coding
  lenso-agent --profile code
  lenso-agent run --profile plan \"Summarize this workspace\"

Put terminal options after the command when using a subcommand.
Use lenso-agent run --help for headless options.";

/// Only reserved command names select another surface. Arguments are never
/// interpreted as shell syntax and executable lookup never searches PATH.
pub(super) fn route(args: &[OsString]) -> Option<(&'static str, &[OsString])> {
    match args.first()?.to_str()? {
        "cli" => Some(("lenso-agent-cli", &args[1..])),
        "acp" => Some(("lenso-agent-acp", &args[1..])),
        "run" | "auth" | "profiles" | "sessions" | "models" | "contexts" | "approvals"
        | "doctor" | "generations" | "runtime" | "plugins" => Some(("lenso-agent-cli", args)),
        _ => None,
    }
}

pub(super) fn launch(executable: &str, args: &[OsString]) -> ExitCode {
    match launch_inner(executable, args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!(
                "error: cannot start {executable}: {error}. Install matching Agent components together in the same binary directory."
            );
            ExitCode::FAILURE
        }
    }
}

fn launch_inner(executable: &str, args: &[OsString]) -> std::io::Result<ExitCode> {
    let current = env::current_exe()?;
    let mut path = current.with_file_name(executable);
    if let Some(extension) = current.extension() {
        path.set_extension(extension);
    }
    let mut command = Command::new(path);
    command.args(args);
    // exec preserves the PID, environment, cwd, stdio, signal handling and exit
    // status without adding a second runtime or sharing Plugin registrations.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec())
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        Ok(ExitCode::from(
            status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(1),
        ))
    }
}

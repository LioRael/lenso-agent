use std::{
    io::{self, Write},
    path::PathBuf,
};

use lenso_agent_host::{AgentHost, HeadlessSurface, generation};
use lenso_capability_terminal_command::{ExecuteOpen, OutputKind};
use lenso_kernel::StreamEvent;
use lenso_terminal_cli_surface::{ParseOutcome, parse_args};

use super::{CliCommand, compose_context, invoke, parse_command, selected_profile};

struct TerminalSurfaceSelection {
    plan: Option<PathBuf>,
    profile: Option<String>,
    command: Vec<String>,
}

pub(super) async fn run_composed_surface(raw: Vec<String>) -> Result<(), String> {
    let selection = terminal_surface_selection(&raw)?;
    let host = AgentHost::builder()
        .plugins(lenso_agent_default_plugins::link)
        .surface(HeadlessSurface::stdio())
        .build()
        .map_err(|error| format!("Host composition failed: {error}"))?;
    let mut app = host
        .run(selected_profile(selection.plan, selection.profile))
        .await?;
    let Some(terminal) = app.try_lease_cli_terminal().await? else {
        return run_agent_fallback(app, raw).await;
    };
    let catalog = terminal.catalog().await?;
    let outcome = parse_args(&catalog.commands, "lenso-agent-cli", &selection.command)
        .map_err(|error| error.to_string())?;

    match outcome {
        ParseOutcome::Command(command) => {
            let result = execute_terminal_command(&terminal, command).await;
            drop(terminal);
            let shutdown = app.shutdown().await;
            result.and(shutdown)
        }
        ParseOutcome::Help(help) => {
            print!("{help}");
            io::stdout()
                .flush()
                .map_err(|error| format!("failed to flush command help: {error}"))?;
            drop(terminal);
            app.shutdown().await
        }
        ParseOutcome::NoMatch => {
            drop(terminal);
            run_agent_fallback(app, raw).await
        }
    }
}

async fn run_agent_fallback(mut app: generation::AgentApp, raw: Vec<String>) -> Result<(), String> {
    let CliCommand::Run(mut args) = parse_command(raw)? else {
        return Err("reserved maintenance command bypassed its parser".to_owned());
    };
    args.prompt = compose_context(&app, &args).await?;
    let turn = app.lease_turn().await?;
    let result = invoke(&turn, args).await;
    drop(turn);
    let shutdown = app.shutdown().await;
    result.and(shutdown)
}

async fn execute_terminal_command(
    terminal: &generation::TerminalGeneration,
    command: lenso_terminal_cli_surface::ParsedCommand,
) -> Result<(), String> {
    let stream = terminal
        .execute(ExecuteOpen {
            id: command.id,
            arguments_json: command
                .arguments_json
                .try_into()
                .map_err(|_| "terminal command arguments are not valid JSON".to_owned())?,
            output_format: command.output_format,
        })
        .await?;
    stream
        .close_send()
        .await
        .map_err(|error| format!("failed to half-close terminal command input: {error:?}"))?;
    loop {
        match stream
            .receive()
            .await
            .map_err(|error| format!("terminal command stream failed: {error:?}"))?
        {
            StreamEvent::Message(message) => {
                let stderr = matches!(message.kind, OutputKind::Stderr | OutputKind::Progress);
                if stderr {
                    eprint!("{}", message.content);
                    io::stderr().flush().map_err(|error| {
                        format!("failed to flush terminal command stderr: {error}")
                    })?;
                } else {
                    print!("{}", message.content);
                    io::stdout().flush().map_err(|error| {
                        format!("failed to flush terminal command stdout: {error}")
                    })?;
                }
            }
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => return Ok(()),
            StreamEvent::Terminal(Err(error)) => {
                return Err(format!("terminal command failed: {error:?}"));
            }
        }
    }
}

pub(super) fn should_try_composed_surface(raw: &[String]) -> bool {
    let Some(first) = raw.first().map(String::as_str) else {
        return false;
    };
    if matches!(
        first,
        "--help"
            | "-h"
            | "auth"
            | "plugins"
            | "generations"
            | "runtime"
            | "approvals"
            | "profiles"
            | "contexts"
            | "models"
    ) {
        return false;
    }
    if first == "sessions" {
        return !raw.get(1).is_some_and(|command| {
            matches!(
                command.as_str(),
                "provenance" | "export" | "import" | "migrate" | "replay" | "evaluate" | "otlp"
            )
        });
    }
    true
}

fn terminal_surface_selection(raw: &[String]) -> Result<TerminalSurfaceSelection, String> {
    let mut plan = None;
    let mut profile = None;
    let mut command = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        match raw[index].as_str() {
            "--plan" => {
                if plan.is_some() || profile.is_some() {
                    return Err("--plan may appear once and conflicts with --profile".to_owned());
                }
                index += 1;
                plan = Some(PathBuf::from(
                    raw.get(index)
                        .ok_or_else(|| "--plan requires a path".to_owned())?,
                ));
            }
            "--profile" => {
                if profile.is_some() || plan.is_some() {
                    return Err("--profile may appear once and conflicts with --plan".to_owned());
                }
                index += 1;
                profile = Some(
                    raw.get(index)
                        .ok_or_else(|| "--profile requires a name".to_owned())?
                        .clone(),
                );
            }
            _ => command.push(raw[index].clone()),
        }
        index += 1;
    }
    Ok(TerminalSurfaceSelection {
        plan,
        profile,
        command,
    })
}

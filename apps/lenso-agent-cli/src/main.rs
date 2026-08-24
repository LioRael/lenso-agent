use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
    process::{Command, ExitCode},
};

mod generation;
mod plugin_profiles;
mod plugins;

use lenso_agent_auth_openai_codex_module::{
    DirectAuthOptions, begin_browser_login, begin_device_login, complete_browser_login,
    complete_device_login, direct_auth_status, direct_logout,
};
use lenso_capability_agent::{Agent, RUN_TURN_OPERATION, RunTurnRequest};
use lenso_kernel::{NativeStreamHandle, StreamEvent};

#[derive(Debug)]
struct Args {
    plan: PathBuf,
    prompt: String,
    session: Option<String>,
}

#[derive(Debug)]
enum CliCommand {
    Run(Args),
    Auth(AuthCommand),
    Plugins(plugins::PluginCommand),
}

#[derive(Debug)]
enum AuthCommand {
    Login { device_auth: bool },
    Status,
    Logout,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let local = tokio::task::LocalSet::new();
    match local.run_until(run()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let args = match parse_args()? {
        CliCommand::Run(args) => args,
        CliCommand::Auth(command) => return run_auth(&command).await,
        CliCommand::Plugins(command) => return plugins::run(command),
    };
    let bytes = fs::read(&args.plan)
        .map_err(|error| format!("failed to read {}: {error}", args.plan.display()))?;
    let mut app = generation::AgentApp::start(&bytes)
        .await
        .map_err(|error| format!("App startup failed: {error}"))?;
    let turn = app.lease_turn()?;
    let result = invoke(turn.handle(), args).await;
    drop(turn);
    let shutdown = app.shutdown().await;
    result.and(shutdown)
}

async fn invoke(handle: &NativeStreamHandle<Agent>, args: Args) -> Result<(), String> {
    let stream = handle
        .open(
            RUN_TURN_OPERATION,
            RunTurnRequest {
                input: args.prompt,
                session_id: args.session,
            },
        )
        .await
        .map_err(|error| format!("Agent stream failed to open: {error:?}"))?
        .map_err(|error| format!("Agent rejected the turn: {error:?}"))?;
    stream
        .close_send()
        .await
        .map_err(|error| format!("failed to half-close Agent input: {error:?}"))?;
    let mut session_id = None;
    loop {
        match stream
            .receive()
            .await
            .map_err(|error| format!("Agent stream failed: {error:?}"))?
        {
            StreamEvent::Message(message) => {
                session_id = message.session_id.or(session_id);
                print!("{}", message.text);
                io::stdout()
                    .flush()
                    .map_err(|error| format!("failed to flush Agent output: {error}"))?;
            }
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => {
                println!();
                io::stdout()
                    .flush()
                    .map_err(|error| format!("failed to flush Agent output: {error}"))?;
                if let Some(session_id) = session_id {
                    eprintln!("session: {session_id}");
                }
                return Ok(());
            }
            StreamEvent::Terminal(Err(error)) => {
                return Err(format!("Agent turn failed: {error:?}"));
            }
        }
    }
}

fn parse_args() -> Result<CliCommand, String> {
    let raw = env::args().skip(1).collect::<Vec<_>>();
    if raw.first().is_some_and(|value| value == "auth") {
        return parse_auth(&raw[1..]).map(CliCommand::Auth);
    }
    if raw.first().is_some_and(|value| value == "plugins") {
        return plugins::parse_command(&raw[1..]).map(CliCommand::Plugins);
    }
    let mut plan = PathBuf::from("composition/headless-readonly/resolved-plan.json");
    let mut prompt = None;
    let mut session = None;
    let mut arguments = raw.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--plan" => {
                plan = PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| "--plan requires a path".to_owned())?,
                );
            }
            "--prompt" => {
                prompt = Some(
                    arguments
                        .next()
                        .ok_or_else(|| "--prompt requires text".to_owned())?,
                );
            }
            "--session" => {
                session = Some(
                    arguments
                        .next()
                        .ok_or_else(|| "--session requires an ID".to_owned())?,
                );
            }
            "--help" | "-h" => {
                return Err(
                    "usage: lenso-agent-cli <plugins|auth> ... | --prompt <text> [--session <id>] [--plan <path>]".to_owned(),
                );
            }
            unknown => return Err(format!("unknown argument `{unknown}`")),
        }
    }
    Ok(CliCommand::Run(Args {
        plan,
        prompt: prompt.ok_or_else(|| "--prompt is required".to_owned())?,
        session,
    }))
}

fn parse_auth(arguments: &[String]) -> Result<AuthCommand, String> {
    match arguments {
        [command] if command == "login" => Ok(AuthCommand::Login { device_auth: false }),
        [command, flag] if command == "login" && flag == "--device-auth" => {
            Ok(AuthCommand::Login { device_auth: true })
        }
        [command] if command == "status" => Ok(AuthCommand::Status),
        [command] if command == "logout" => Ok(AuthCommand::Logout),
        _ => Err("usage: lenso-agent-cli auth <login [--device-auth]|status|logout>".to_owned()),
    }
}

async fn run_auth(command: &AuthCommand) -> Result<(), String> {
    match command {
        AuthCommand::Login { device_auth: true } => {
            let options = DirectAuthOptions::default();
            let pending = begin_device_login(options.clone()).await?;
            eprintln!("Open this URL in a browser: {}", pending.verification_url);
            eprintln!("Enter this one-time code: {}", pending.user_code);
            let status = complete_device_login(options, pending).await?;
            println!(
                "Direct ChatGPT authentication succeeded; token expires at {}.",
                status.expires_at.unwrap_or_default()
            );
            Ok(())
        }
        AuthCommand::Login { device_auth: false } => {
            let options = DirectAuthOptions::default();
            let pending = begin_browser_login(options.clone()).await?;
            eprintln!("Open this URL in a browser: {}", pending.authorization_url);
            if let Err(error) = open_browser(&pending.authorization_url) {
                eprintln!("Browser did not open automatically: {error}");
            }
            let status = complete_browser_login(options, pending).await?;
            println!(
                "Direct ChatGPT authentication succeeded; token expires at {}.",
                status.expires_at.unwrap_or_default()
            );
            Ok(())
        }
        AuthCommand::Status => {
            let status = direct_auth_status(DirectAuthOptions::default())?;
            if status.authenticated {
                println!(
                    "Direct ChatGPT authentication is active; token expires at {}.",
                    status.expires_at.unwrap_or_default()
                );
                Ok(())
            } else {
                Err("Direct ChatGPT authentication is not configured.".to_owned())
            }
        }
        AuthCommand::Logout => {
            direct_logout(DirectAuthOptions::default())?;
            println!("Direct ChatGPT credentials were removed.");
            Ok(())
        }
    }
}

fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(url).status();
    #[cfg(target_os = "linux")]
    let status = Command::new("xdg-open").arg(url).status();
    #[cfg(target_os = "windows")]
    let status = Command::new("cmd").args(["/C", "start", "", url]).status();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err("automatic browser opening is unsupported on this platform".to_owned());
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("browser command exited with {status}")),
        Err(error) => Err(error.to_string()),
    }
}

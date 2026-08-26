use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
    process::{Command, ExitCode},
};

use lenso_agent_approval_hook_module::{ApprovalDecision, decide_approval, list_approvals};
use lenso_agent_auth_openai_codex_module::{
    DirectAuthOptions, begin_browser_login, begin_device_login, complete_browser_login,
    complete_device_login, direct_auth_status, direct_logout,
};
use lenso_agent_cli::{default_plan, generation, plugins, provenance};
use lenso_agent_loop_module::RunScope;
use lenso_capability_agent::{RUN_TURN_OPERATION, RunTurnRequest};
use lenso_kernel::StreamEvent;

#[derive(Debug)]
struct Args {
    allowed_tools: Option<Vec<String>>,
    plan: PathBuf,
    prompt: String,
    session: Option<String>,
}

#[derive(Debug)]
enum CliCommand {
    Run(Args),
    Help,
    Auth(AuthCommand),
    Plugins(plugins::PluginCommand),
    Generations(provenance::GenerationCommand),
    Sessions(provenance::SessionCommand),
    Approvals(ApprovalCommand),
}

#[derive(Debug)]
enum ApprovalCommand {
    List { root: PathBuf },
    Approve { approval_id: String, root: PathBuf },
    Reject { approval_id: String, root: PathBuf },
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
        CliCommand::Help => {
            println!("{}", run_usage());
            return Ok(());
        }
        CliCommand::Auth(command) => return run_auth(&command).await,
        CliCommand::Plugins(command) => return plugins::run(command).await,
        CliCommand::Generations(command) => return provenance::run_generation(command),
        CliCommand::Sessions(command) => return provenance::run_session(command),
        CliCommand::Approvals(command) => return run_approval(command),
    };
    let bytes = fs::read(&args.plan)
        .map_err(|error| format!("failed to read {}: {error}", args.plan.display()))?;
    let mut app = generation::AgentApp::start(&bytes)
        .await
        .map_err(|error| format!("App startup failed: {error}"))?;
    let turn = app.lease_turn().await?;
    let result = invoke(&turn, args).await;
    drop(turn);
    let shutdown = app.shutdown().await;
    result.and(shutdown)
}

async fn invoke(turn: &generation::TurnGeneration, args: Args) -> Result<(), String> {
    let mut context = turn.invocation_context()?;
    if let Some(allowed_tools) = args.allowed_tools.clone() {
        context = RunScope::new(allowed_tools)?.attach(context)?;
    }
    let stream = turn
        .handle()
        .open_with_context(
            RUN_TURN_OPERATION,
            context,
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
                session_id = message.session_id.clone().or(session_id);
                if message.is_text_delta() {
                    print!("{}", message.text);
                    io::stdout()
                        .flush()
                        .map_err(|error| format!("failed to flush Agent output: {error}"))?;
                }
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
    if raw.first().is_some_and(|value| value == "generations") {
        return provenance::parse_generation_command(&raw[1..]).map(CliCommand::Generations);
    }
    if raw.first().is_some_and(|value| value == "sessions") {
        return provenance::parse_session_command(&raw[1..]).map(CliCommand::Sessions);
    }
    if raw.first().is_some_and(|value| value == "approvals") {
        return parse_approval(&raw[1..]).map(CliCommand::Approvals);
    }
    let mut plan = None;
    let mut plan_source = None;
    let mut prompt = None;
    let mut session = None;
    let mut allowed_tools = None::<Vec<String>>;
    let mut no_tools = false;
    let mut arguments = raw.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--plan" => {
                if let Some(source) = plan_source {
                    return Err(format!("--plan conflicts with {source}"));
                }
                plan = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| "--plan requires a path".to_owned())?,
                ));
                plan_source = Some("--plan");
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
            "--allow-tool" => {
                if no_tools {
                    return Err("--allow-tool conflicts with --no-tools".to_owned());
                }
                allowed_tools.get_or_insert_with(Vec::new).push(
                    arguments
                        .next()
                        .ok_or_else(|| "--allow-tool requires a Tool name".to_owned())?,
                );
            }
            "--no-tools" => {
                if allowed_tools.is_some() {
                    return Err("--no-tools conflicts with --allow-tool".to_owned());
                }
                no_tools = true;
                allowed_tools = Some(Vec::new());
            }
            "--help" | "-h" => {
                return Ok(CliCommand::Help);
            }
            unknown if unknown.starts_with('-') => {
                return Err(format!("unknown argument `{unknown}`"));
            }
            positional_prompt => {
                if prompt.is_some() {
                    return Err(
                        "only one prompt is accepted; quote multi-word prompts as one argument"
                            .to_owned(),
                    );
                }
                prompt = Some(positional_prompt.to_owned());
            }
        }
    }
    Ok(CliCommand::Run(Args {
        allowed_tools,
        plan: match plan {
            Some(plan) => plan,
            None => default_plan()?,
        },
        prompt: prompt.ok_or_else(|| "a prompt is required".to_owned())?,
        session,
    }))
}

fn run_usage() -> String {
    "usage: lenso-agent-cli <prompt> [--session <id>] [--allow-tool <name> ... | --no-tools]\n       lenso-agent-cli <plugins|generations|sessions|approvals|auth> ...\n\nEnable optional capabilities with `plugins enable`; the base App is resolved from lenso.app.json.\n\nAdvanced: --prompt <text> and --plan <path> remain available for automation and exact Plan replay.".to_owned()
}

fn parse_approval(arguments: &[String]) -> Result<ApprovalCommand, String> {
    let mut root = PathBuf::from(".");
    let mut positional = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == "--root" {
            index += 1;
            root = PathBuf::from(
                arguments
                    .get(index)
                    .ok_or_else(|| "--root requires a directory".to_owned())?,
            );
        } else {
            positional.push(arguments[index].clone());
        }
        index += 1;
    }
    match positional.as_slice() {
        [command] if command == "list" => Ok(ApprovalCommand::List { root }),
        [command, approval_id] if command == "approve" => Ok(ApprovalCommand::Approve {
            approval_id: approval_id.clone(),
            root,
        }),
        [command, approval_id] if command == "reject" => Ok(ApprovalCommand::Reject {
            approval_id: approval_id.clone(),
            root,
        }),
        _ => Err(
            "usage: lenso-agent-cli approvals <list|approve <id>|reject <id>> [--root <directory>]"
                .to_owned(),
        ),
    }
}

fn run_approval(command: ApprovalCommand) -> Result<(), String> {
    let (root, action) = match command {
        ApprovalCommand::List { root } => {
            let records = list_approvals(&root.join(".lenso/approvals"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&records)
                    .map_err(|error| format!("failed to encode approvals: {error}"))?
            );
            return Ok(());
        }
        ApprovalCommand::Approve { approval_id, root } => {
            (root, (approval_id, ApprovalDecision::Approve))
        }
        ApprovalCommand::Reject { approval_id, root } => {
            (root, (approval_id, ApprovalDecision::Reject))
        }
    };
    let record = decide_approval(&root.join(".lenso/approvals"), &action.0, action.1)?;
    println!("{}: {:?}", record.approval_id, record.status);
    Ok(())
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

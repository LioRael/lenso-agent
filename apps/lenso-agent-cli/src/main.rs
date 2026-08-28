use std::{
    env,
    io::{self, Write},
    path::PathBuf,
    process::{Command, ExitCode},
};

use lenso_agent_approval_hook_plugin::{ApprovalDecision, decide_approval, list_approvals};
use lenso_agent_auth_openai_codex_plugin::{
    DirectAuthOptions, begin_browser_login, begin_device_login, complete_browser_login,
    complete_device_login, direct_auth_status, direct_logout,
};
use lenso_agent_cli_plugin as _;
use lenso_agent_host::{AgentHost, HeadlessSurface, Profile, generation, provenance};
use lenso_agent_loop_plugin::RunScope;
use lenso_capability_agent::{RUN_TURN_OPERATION, RunTurnRequest};
use lenso_capability_agent_context_source::{
    ContextRole, ReadResourceRequest, RenderPromptRequest,
};
use lenso_kernel::StreamEvent;

#[derive(Debug)]
struct Args {
    allowed_tools: Option<Vec<String>>,
    plan: Option<PathBuf>,
    profile: Option<String>,
    prompt: String,
    session: Option<String>,
    context_prompt: Option<ContextPromptSelection>,
    context_resources: Vec<ContextResourceSelection>,
}

#[derive(Debug)]
struct ContextPromptSelection {
    source: String,
    name: String,
    arguments_json: String,
}

#[derive(Debug)]
struct ContextResourceSelection {
    source: String,
    uri: String,
}

#[derive(Debug)]
enum CliCommand {
    Run(Args),
    Help,
    Auth(AuthCommand),
    Generations(provenance::GenerationCommand),
    Sessions(provenance::SessionCommand),
    Approvals(ApprovalCommand),
    Contexts { profile: Option<String> },
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
        CliCommand::Generations(command) => return provenance::run_generation(command),
        CliCommand::Sessions(command) => return provenance::run_session(command),
        CliCommand::Approvals(command) => return run_approval(command),
        CliCommand::Contexts { profile } => return run_contexts(profile).await,
    };
    let profile = selected_profile(args.plan.clone(), args.profile.clone());
    let host = AgentHost::builder()
        .plugins(lenso_agent_default_plugins::link)
        .surface(HeadlessSurface::stdio())
        .build()
        .map_err(|error| format!("Host composition failed: {error}"))?;
    let mut app = host.run(profile).await?;
    let mut args = args;
    args.prompt = compose_context(&app, &args).await?;
    let turn = app.lease_turn().await?;
    let result = invoke(&turn, args).await;
    drop(turn);
    let shutdown = app.shutdown().await;
    result.and(shutdown)
}

async fn run_contexts(profile: Option<String>) -> Result<(), String> {
    let host = AgentHost::builder()
        .plugins(lenso_agent_default_plugins::link)
        .surface(HeadlessSurface::stdio())
        .build()
        .map_err(|error| format!("Host composition failed: {error}"))?;
    let mut app = host
        .run(profile.map_or(Profile::Default, Profile::named))
        .await?;
    let snapshot = app.cli_context_sources().await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&snapshot)
            .map_err(|error| format!("failed to encode Context Sources: {error}"))?
    );
    app.shutdown().await
}

async fn compose_context(app: &generation::AgentApp, args: &Args) -> Result<String, String> {
    if args.context_prompt.is_none() && args.context_resources.is_empty() {
        return Ok(args.prompt.clone());
    }
    let mut sections = Vec::new();
    if let Some(prompt) = &args.context_prompt {
        let rendered = app
            .render_cli_context_prompt(RenderPromptRequest {
                source: prompt.source.clone(),
                name: prompt.name.clone(),
                arguments_json: prompt
                    .arguments_json
                    .clone()
                    .try_into()
                    .map_err(|error| format!("invalid Context Prompt arguments JSON: {error}"))?,
            })
            .await?;
        let messages = rendered
            .messages
            .into_iter()
            .map(|message| {
                let role = match message.role {
                    ContextRole::User => "user",
                    ContextRole::Assistant => "assistant",
                };
                format!("[{role}]\n{}", message.text)
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        sections.push(format!(
            "Selected Context Prompt: {}/{}\n{}",
            prompt.source, prompt.name, messages
        ));
    }
    for resource in &args.context_resources {
        let response = app
            .read_cli_context_resource(ReadResourceRequest {
                source: resource.source.clone(),
                uri: resource.uri.clone(),
            })
            .await?;
        let contents = response
            .contents
            .into_iter()
            .map(|content| {
                format!(
                    "URI: {}\nMIME: {}\n{}",
                    content.uri, content.mime_type, content.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        sections.push(format!(
            "Selected Context Resource: {}/{}\n{}",
            resource.source, resource.uri, contents
        ));
    }
    sections.push(format!("User task:\n{}", args.prompt));
    Ok(sections.join("\n\n---\n\n"))
}

fn selected_profile(plan: Option<PathBuf>, profile: Option<String>) -> Profile {
    match (plan, profile) {
        (Some(plan), None) => Profile::resolved_plan(plan),
        (None, Some(profile)) => Profile::named(profile),
        (None, None) => Profile::Default,
        (Some(_), Some(_)) => unreachable!("argument parser rejects Plan/Profile conflicts"),
    }
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
    parse_command(raw)
}

fn parse_command(raw: Vec<String>) -> Result<CliCommand, String> {
    if raw.first().is_some_and(|value| value == "auth") {
        return parse_auth(&raw[1..]).map(CliCommand::Auth);
    }
    if raw.first().is_some_and(|value| value == "plugins") {
        return Err(
            "Plugin management moved to `lenso plugins`; the App is derived from `plugins/`"
                .to_owned(),
        );
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
    if raw.first().is_some_and(|value| value == "contexts") {
        return parse_contexts(&raw[1..]);
    }
    parse_run_args(raw)
}

fn parse_contexts(arguments: &[String]) -> Result<CliCommand, String> {
    match arguments {
        [] => Ok(CliCommand::Contexts { profile: None }),
        [flag, profile] if flag == "--profile" && !profile.is_empty() => Ok(CliCommand::Contexts {
            profile: Some(profile.clone()),
        }),
        _ => Err("usage: lenso-agent-cli contexts [--profile <name>]".to_owned()),
    }
}

fn parse_run_args(raw: Vec<String>) -> Result<CliCommand, String> {
    let mut plan = None;
    let mut plan_source = None;
    let mut prompt = None;
    let mut profile = None;
    let mut session = None;
    let mut allowed_tools = None::<Vec<String>>;
    let mut no_tools = false;
    let mut context_prompt = None;
    let mut context_arguments = None;
    let mut context_resources = Vec::new();
    let mut arguments = raw.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--plan" => {
                if let Some(source) = plan_source {
                    return Err(format!("--plan conflicts with {source}"));
                }
                plan = Some(PathBuf::from(required_value(
                    &mut arguments,
                    "--plan",
                    "a path",
                )?));
                plan_source = Some("--plan");
            }
            "--prompt" => prompt = Some(required_value(&mut arguments, "--prompt", "text")?),
            "--profile" => {
                profile = Some(required_value(&mut arguments, "--profile", "a name")?);
            }
            "--session" => session = Some(required_value(&mut arguments, "--session", "an ID")?),
            "--context-prompt" => {
                context_prompt = Some(parse_context_prompt(
                    &mut arguments,
                    context_prompt.as_ref(),
                )?);
            }
            "--context-arguments" => {
                context_arguments = Some(required_value(
                    &mut arguments,
                    "--context-arguments",
                    "a JSON object",
                )?);
            }
            "--context-resource" => {
                context_resources.push(parse_context_resource(&mut arguments)?);
            }
            "--allow-tool" => {
                if no_tools {
                    return Err("--allow-tool conflicts with --no-tools".to_owned());
                }
                allowed_tools
                    .get_or_insert_with(Vec::new)
                    .push(required_value(
                        &mut arguments,
                        "--allow-tool",
                        "a Tool name",
                    )?);
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
    if context_arguments.is_some() && context_prompt.is_none() {
        return Err("--context-arguments requires --context-prompt".to_owned());
    }
    let context_prompt = context_prompt.map(|(source, name)| ContextPromptSelection {
        source,
        name,
        arguments_json: context_arguments.unwrap_or_else(|| "{}".to_owned()),
    });
    Ok(CliCommand::Run(Args {
        allowed_tools,
        plan,
        profile,
        prompt: prompt.ok_or_else(|| "a prompt is required".to_owned())?,
        session,
        context_prompt,
        context_resources,
    }))
}

fn parse_context_prompt(
    arguments: &mut impl Iterator<Item = String>,
    current: Option<&(String, String)>,
) -> Result<(String, String), String> {
    if current.is_some() {
        return Err("only one --context-prompt is accepted".to_owned());
    }
    let value = required_value(arguments, "--context-prompt", "source/name")?;
    split_context_identity(&value, "--context-prompt")
}

fn parse_context_resource(
    arguments: &mut impl Iterator<Item = String>,
) -> Result<ContextResourceSelection, String> {
    let value = required_value(arguments, "--context-resource", "source=URI")?;
    let (source, uri) = value
        .split_once('=')
        .ok_or_else(|| "--context-resource requires source=URI".to_owned())?;
    if source.is_empty() || uri.is_empty() {
        return Err("--context-resource requires non-empty source=URI".to_owned());
    }
    Ok(ContextResourceSelection {
        source: source.to_owned(),
        uri: uri.to_owned(),
    })
}

fn split_context_identity(value: &str, option: &str) -> Result<(String, String), String> {
    let (source, name) = value
        .split_once('/')
        .ok_or_else(|| format!("{option} requires source/name"))?;
    if source.is_empty() || name.is_empty() {
        return Err(format!("{option} requires non-empty source/name"));
    }
    Ok((source.to_owned(), name.to_owned()))
}

fn required_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
    value: &str,
) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} requires {value}"))
}

fn run_usage() -> String {
    "usage: lenso-agent-cli <prompt> [--profile <name>] [--session <id>] [--allow-tool <name> ... | --no-tools]\n       [--context-prompt <source/name> [--context-arguments <json>]]\n       [--context-resource <source=URI> ...]\n       lenso-agent-cli contexts [--profile <name>]\n       lenso-agent-cli <generations|sessions|approvals|auth> ...\n\nThe Host defaults boot with an empty `plugins/` directory. Manage App differences with `lenso plugins`; select `profiles/<name>.toml` with `--profile`.\n\nAdvanced: --prompt <text> and --plan <path> remain available for automation and exact Plan replay.".to_owned()
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

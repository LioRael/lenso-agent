use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use lenso_agent_approval_hook_plugin::{ApprovalDecision, decide_approval, list_approvals};
use lenso_agent_auth_openai_codex_plugin::{
    DirectAuthOptions, begin_browser_login, begin_device_login, complete_browser_login,
    complete_device_login, direct_auth_status, direct_logout,
};
use lenso_agent_cli_plugin as _;
use lenso_agent_host::{
    AgentDirectories, AgentHost, HeadlessSurface, Profile, generation, provenance,
};
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
    Version,
    Doctor { json: bool },
    Auth(AuthCommand),
    Generations(provenance::GenerationCommand),
    RuntimeStatus { root: PathBuf },
    Sessions(provenance::SessionCommand),
    Approvals(ApprovalCommand),
    Profiles(ProfileCommand),
    Contexts { profile: Option<String> },
    Models { profile: Option<String> },
}

#[derive(Debug)]
enum ProfileCommand {
    InstallCoding,
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
        CliCommand::Version => {
            println!("lenso-agent-cli {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        CliCommand::Doctor { json } => return run_doctor(json),
        CliCommand::Auth(command) => return run_auth(&command).await,
        CliCommand::Generations(command) => return provenance::run_generation(command),
        CliCommand::RuntimeStatus { root } => return provenance::run_runtime_status(&root),
        CliCommand::Sessions(command) => return provenance::run_session(command).await,
        CliCommand::Approvals(command) => return run_approval(command),
        CliCommand::Profiles(command) => return run_profile(&command),
        CliCommand::Contexts { profile } => return run_contexts(profile).await,
        CliCommand::Models { profile } => return run_models(profile).await,
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

async fn run_models(profile: Option<String>) -> Result<(), String> {
    let host = AgentHost::builder()
        .plugins(lenso_agent_default_plugins::link)
        .surface(HeadlessSurface::stdio())
        .build()
        .map_err(|error| format!("Host composition failed: {error}"))?;
    let mut app = host
        .run(profile.map_or(Profile::Default, Profile::named))
        .await?;
    let catalog = app.provider_model_catalog().await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&catalog)
            .map_err(|error| format!("failed to encode Provider/Model Catalog: {error}"))?
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
                let body = content.text.flatten().unwrap_or_else(|| {
                    format!(
                        "[binary resource available: {} bytes base64]",
                        content.data_base64.flatten().map_or(0, |data| data.len())
                    )
                });
                format!(
                    "URI: {}\nMIME: {}\n{}",
                    content.uri, content.mime_type, body
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
    if matches!(raw.as_slice(), [argument] if argument == "--version" || argument == "-V") {
        return Ok(CliCommand::Version);
    }
    if raw.first().is_some_and(|value| value == "doctor") {
        return parse_doctor(&raw[1..]);
    }
    if raw.first().is_some_and(|value| value == "auth") {
        return parse_auth(&raw[1..]).map(CliCommand::Auth);
    }
    if raw.first().is_some_and(|value| value == "plugins") {
        return Err(
            "Plugin management moved to `lenso plugins`; run it from the Agent Home".to_owned(),
        );
    }
    if raw.first().is_some_and(|value| value == "generations") {
        return provenance::parse_generation_command(&raw[1..]).map(CliCommand::Generations);
    }
    if raw.first().is_some_and(|value| value == "runtime") {
        return parse_runtime(&raw[1..]);
    }
    if raw.first().is_some_and(|value| value == "sessions") {
        return provenance::parse_session_command(&raw[1..]).map(CliCommand::Sessions);
    }
    if raw.first().is_some_and(|value| value == "approvals") {
        return parse_approval(&raw[1..]).map(CliCommand::Approvals);
    }
    if raw.first().is_some_and(|value| value == "profiles") {
        return parse_profile(&raw[1..]).map(CliCommand::Profiles);
    }
    if raw.first().is_some_and(|value| value == "contexts") {
        return parse_contexts(&raw[1..]);
    }
    if raw.first().is_some_and(|value| value == "models") {
        return parse_models(&raw[1..]);
    }
    parse_run_args(raw)
}

fn parse_doctor(arguments: &[String]) -> Result<CliCommand, String> {
    match arguments {
        [] => Ok(CliCommand::Doctor { json: false }),
        [flag] if flag == "--json" => Ok(CliCommand::Doctor { json: true }),
        _ => Err("usage: lenso-agent-cli doctor [--json]".to_owned()),
    }
}

fn parse_runtime(arguments: &[String]) -> Result<CliCommand, String> {
    let directories = AgentDirectories::resolve()?;
    match arguments {
        [command] if command == "status" => Ok(CliCommand::RuntimeStatus {
            root: directories.runtime(),
        }),
        [command, flag, root] if command == "status" && flag == "--root" => {
            Ok(CliCommand::RuntimeStatus {
                root: PathBuf::from(root),
            })
        }
        _ => Err("usage: lenso-agent-cli runtime status [--root <runtime-root>]".to_owned()),
    }
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

fn parse_models(arguments: &[String]) -> Result<CliCommand, String> {
    match arguments {
        [] => Ok(CliCommand::Models { profile: None }),
        [flag, profile] if flag == "--profile" && !profile.is_empty() => Ok(CliCommand::Models {
            profile: Some(profile.clone()),
        }),
        _ => Err("usage: lenso-agent-cli models [--profile <name>]".to_owned()),
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
    "usage: lenso-agent-cli <prompt> [--profile <name>] [--session <id>] [--allow-tool <name> ... | --no-tools]\n       [--context-prompt <source/name> [--context-arguments <json>]]\n       [--context-resource <source=URI> ...]\n       lenso-agent-cli <contexts|models> [--profile <name>]\n       lenso-agent-cli doctor [--json]\n       lenso-agent-cli runtime status [--root <runtime-root>]\n       lenso-agent-cli <generations|sessions|approvals|profiles|auth> ...\n\nInstall the official coding and read-only planning Profiles with `lenso-agent-cli profiles install coding`.\nThe Host reads Plugin configuration and Profiles from `LENSO_AGENT_HOME`, defaulting to `~/.lenso/agent`; the current directory remains the Workspace. Run `lenso plugins` from the Agent Home.\n\nAdvanced: --prompt <text> and --plan <path> remain available for automation and exact Plan replay.".to_owned()
}

#[derive(Debug, serde::Serialize)]
struct DoctorReport {
    version: &'static str,
    platform: String,
    agent_home: DoctorAgentHome,
    authentication: DoctorAuthentication,
    components: Vec<DoctorCheck>,
    dependencies: Vec<DoctorCheck>,
    ready: bool,
}

#[derive(Debug, serde::Serialize)]
struct DoctorAgentHome {
    path: String,
    exists: bool,
    parent_writable: bool,
}

#[derive(Debug, serde::Serialize)]
struct DoctorAuthentication {
    authenticated: bool,
    expires_at: Option<u64>,
}

#[derive(Debug, serde::Serialize)]
struct DoctorCheck {
    name: String,
    available: bool,
    required: bool,
}

fn run_doctor(json: bool) -> Result<(), String> {
    let directories = AgentDirectories::resolve()?;
    let home = directories.home();
    let auth = direct_auth_status(direct_auth_options()?)?;
    let executable_directory = env::current_exe()
        .map_err(|error| format!("failed to locate lenso-agent-cli: {error}"))?
        .parent()
        .ok_or_else(|| "lenso-agent-cli has no executable directory".to_owned())?
        .to_path_buf();
    let components = ["lenso-agent", "lenso-agent-cli", "lenso-agent-acp"]
        .into_iter()
        .map(|name| DoctorCheck {
            name: name.to_owned(),
            available: executable_exists(&executable_directory, name),
            required: name != "lenso-agent-acp",
        })
        .collect::<Vec<_>>();
    let dependencies = [
        ("git", true),
        ("rg", true),
        ("cargo", false),
        (sandbox_backend(), false),
    ]
    .into_iter()
    .map(|(name, required)| DoctorCheck {
        name: name.to_owned(),
        available: command_available(name),
        required,
    })
    .collect::<Vec<_>>();
    let agent_home_parent_writable = nearest_existing_ancestor(home).is_some_and(|path| {
        !fs::metadata(path).is_ok_and(|metadata| metadata.permissions().readonly())
    });
    let ready = auth.authenticated
        && agent_home_parent_writable
        && components
            .iter()
            .chain(&dependencies)
            .all(|check| !check.required || check.available);
    let report = DoctorReport {
        version: env!("CARGO_PKG_VERSION"),
        platform: format!("{}-{}", env::consts::OS, env::consts::ARCH),
        agent_home: DoctorAgentHome {
            path: home.display().to_string(),
            exists: home.is_dir(),
            parent_writable: agent_home_parent_writable,
        },
        authentication: DoctorAuthentication {
            authenticated: auth.authenticated,
            expires_at: auth.expires_at,
        },
        components,
        dependencies,
        ready,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("failed to encode doctor report: {error}"))?
        );
        return Ok(());
    }
    println!("Lenso Agent doctor {}", report.version);
    println!("platform: {}", report.platform);
    println!("agent-home: {}", report.agent_home.path);
    println!(
        "authentication: {}",
        if report.authentication.authenticated {
            "ready"
        } else {
            "missing; run `lenso-agent-cli auth login`"
        }
    );
    for check in report.components.iter().chain(&report.dependencies) {
        println!(
            "{}: {}{}",
            check.name,
            if check.available {
                "available"
            } else {
                "missing"
            },
            if check.required {
                " (required)"
            } else {
                " (optional)"
            }
        );
    }
    println!(
        "overall: {}",
        if report.ready {
            "ready"
        } else {
            "action required"
        }
    );
    Ok(())
}

fn executable_exists(directory: &Path, name: &str) -> bool {
    let executable = if cfg!(windows) {
        directory.join(format!("{name}.exe"))
    } else {
        directory.join(name)
    };
    executable.is_file()
}

fn command_available(name: &str) -> bool {
    Command::new(name).arg("--version").output().is_ok()
}

fn sandbox_backend() -> &'static str {
    if cfg!(target_os = "macos") {
        "sandbox-exec"
    } else if cfg!(target_os = "linux") {
        "bwrap"
    } else {
        "sandbox-backend"
    }
}

fn nearest_existing_ancestor(path: &Path) -> Option<&Path> {
    path.ancestors().find(|ancestor| ancestor.exists())
}

fn parse_profile(arguments: &[String]) -> Result<ProfileCommand, String> {
    match arguments {
        [install, preset] if install == "install" && preset == "coding" => {
            Ok(ProfileCommand::InstallCoding)
        }
        _ => Err("usage: lenso-agent-cli profiles install coding".to_owned()),
    }
}

fn run_profile(command: &ProfileCommand) -> Result<(), String> {
    match command {
        ProfileCommand::InstallCoding => {
            let home = AgentDirectories::resolve()?.home().to_path_buf();
            install_coding_profiles(&home)?;
            println!("Installed or updated coding Profiles in {}", home.display());
            println!(
                "Run `lenso-agent --profile code`, `lenso-agent --profile code-sandbox`, or `lenso-agent --profile plan`."
            );
            Ok(())
        }
    }
}

fn install_coding_profiles(home: &Path) -> Result<(), String> {
    lenso_agent_host::migrate_legacy_official_files(home)?;
    let files = coding_profile_files();
    for (relative, content) in &files {
        let path = home.join(relative);
        match fs::read_to_string(&path) {
            Ok(existing) if existing == content.as_str() => {}
            Ok(existing) if previous_official_profile(relative, &existing, content) => {}
            Ok(_) => {
                return Err(format!(
                    "refusing to overwrite customized coding Profile file: {}",
                    path.display()
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("failed to inspect {}: {error}", path.display())),
        }
    }
    for (index, (relative, content)) in files.iter().enumerate() {
        let path = home.join(relative);
        if fs::read_to_string(&path).is_ok_and(|existing| existing == content.as_str()) {
            continue;
        }
        let parent = path
            .parent()
            .ok_or_else(|| format!("Profile path has no parent: {}", path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        let temporary = parent.join(format!(
            ".lenso-agent-profile-install-{}-{index}",
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("failed to create {}: {error}", temporary.display()))?;
        file.write_all(content.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
        fs::rename(&temporary, &path)
            .map_err(|error| format!("failed to install {}: {error}", path.display()))?;
    }
    Ok(())
}

fn previous_official_profile(relative: &str, existing: &str, current: &str) -> bool {
    relative.starts_with("profiles/")
        && current.contains("include_enabled = true\n")
        && existing == current.replacen("include_enabled = true\n", "", 1)
}

#[allow(
    clippy::too_many_lines,
    reason = "one reviewed list keeps the official Profile installation auditable"
)]
fn coding_profile_files() -> Vec<(String, String)> {
    let mut files = vec![
        (
            "plugins/lenso.agent.workspace-instructions/default.toml",
            "working_directory = \".\"\nfile_name = \"AGENTS.md\"\nmax_ancestor_depth = 32\nmax_file_bytes = 262144\nmax_total_bytes = 1048576\n",
        ),
        (
            "plugins/lenso.agent.workspace-edit/default.toml",
            "root = \".\"\nmax_file_bytes = 1048576\nmax_edit_bytes = 131072\nrequire_checkpoint = true\n",
        ),
        (
            "plugins/lenso.agent.process.native/default.toml",
            "root = \".\"\nallowed_programs = [\"cargo\", \"git\", \"rg\"]\nprogram_presets = [\"rust\", \"javascript\", \"python\", \"go\", \"build\"]\nenvironment_allowlist = [\"PATH\", \"HOME\", \"CARGO_HOME\", \"RUSTUP_HOME\", \"TMPDIR\", \"LANG\", \"LC_ALL\"]\nmax_timeout_ms = 600000\nmax_output_bytes = 262144\nmax_argument_bytes = 131072\n",
        ),
        (
            "plugins/lenso.agent.process.sandbox/default.toml",
            "root = \".\"\nbackend = \"auto\"\nallow_network = false\nallowed_programs = [\"cargo\", \"git\", \"rg\"]\nprogram_presets = [\"rust\", \"javascript\", \"python\", \"go\", \"build\"]\nenvironment_allowlist = [\"PATH\", \"HOME\", \"CARGO_HOME\", \"RUSTUP_HOME\", \"LANG\", \"LC_ALL\"]\nmax_timeout_ms = 600000\nmax_output_bytes = 262144\nmax_argument_bytes = 131072\n",
        ),
        (
            "plugins/lenso.agent.process-tools/default.toml",
            "default_timeout_ms = 120000\nmax_background_processes = 8\nmax_background_log_bytes = 262144\n",
        ),
        (
            "plugins/lenso.agent.git-tools/default.toml",
            "default_timeout_ms = 30000\nmax_log_entries = 50\nmax_commit_message_bytes = 4096\nenable_branch_management = false\nenable_history_integration = false\nallowed_network_remotes = []\n",
        ),
        (
            "plugins/lenso.agent.code-mode-tools/default.toml",
            "max_code_bytes = 32768\nmax_instructions = 1000000\nmax_memory_bytes = 8388608\nmax_output_bytes = 262144\nmax_parallel_subcalls = 4\nmax_subcalls = 16\n",
        ),
        (
            "plugins/lenso.agent.subagent-tools/worktree.toml",
            "max_output_bytes = 1048576\nmax_task_bytes = 262144\nmax_tasks = 8\nrequire_worktree_provider = true\n",
        ),
        (
            "plugins/lenso.agent.worktree-provider/default.toml",
            "mutation_agents = [\"lenso.agent.loop/worker-a\", \"lenso.agent.loop/worker-b\"]\nmax_worktrees = 8\ntimeout_ms = 120000\nmax_review_bytes = 1048576\n",
        ),
        (
            "plugins/lenso.agent.loop/researcher.toml",
            "# Named read-only child Agent selected by the coding Profiles.\n",
        ),
        (
            "plugins/lenso.agent.loop/reviewer.toml",
            "# Named read-only child Agent selected by the coding Profiles.\n",
        ),
        (
            "plugins/lenso.agent.loop/worker-a.toml",
            "# Named mutation-capable child Agent isolated in its own Git worktree.\n",
        ),
        (
            "plugins/lenso.agent.loop/worker-b.toml",
            "# Second mutation-capable child lane for concurrent isolated work.\n",
        ),
        (
            "plugins/lenso.agent.tools/worker-tools.toml",
            "# Private Tool runtime for mutation-capable child Agents.\n",
        ),
        (
            "plugins/lenso.agent.interactive-approval-hook/default.toml",
            "default_decision = \"ask\"\nallow_tools = [\"read_text\", \"skill_list\", \"skill\", \"skill_resources\", \"skill_resource\", \"ask_user\", \"git_status\", \"git_diff\", \"git_log\", \"list_subagents\", \"list_worktrees\", \"review_worktree\", \"checkpoint_create\", \"checkpoint_review\"]\nask_tools = []\ndeny_tools = []\nmax_preview_bytes = 16384\n",
        ),
        (
            "profiles/code.toml",
            "description = \"Official coding agent with workspace instructions, isolated child worktrees, and inline approval\"\ninclude_enabled = true\ninstances = [\n  \"lenso.agent.workspace-instructions/default\",\n  \"lenso.agent.workspace-edit/default\",\n  \"lenso.agent.process.native/default\",\n  \"lenso.agent.process-tools/default\",\n  \"lenso.agent.git-tools/default\",\n  \"lenso.agent.code-mode-tools/default\",\n  \"lenso.agent.worktree-provider/default\",\n  \"lenso.agent.subagent-tools/worktree\",\n  \"lenso.agent.tools/worker-tools\",\n  \"lenso.agent.loop/researcher\",\n  \"lenso.agent.loop/reviewer\",\n  \"lenso.agent.loop/worker-a\",\n  \"lenso.agent.loop/worker-b\",\n  \"lenso.agent.interactive-approval-hook/default\",\n  \"lenso.agent.prompt.static/coding\",\n]\n",
        ),
        (
            "profiles/code-sandbox.toml",
            "description = \"Official coding agent with OS-isolated process execution and isolated child worktrees\"\ninclude_enabled = true\ninstances = [\n  \"lenso.agent.workspace-instructions/default\",\n  \"lenso.agent.workspace-edit/default\",\n  \"lenso.agent.process.sandbox/default\",\n  \"lenso.agent.process-tools/default\",\n  \"lenso.agent.git-tools/default\",\n  \"lenso.agent.code-mode-tools/default\",\n  \"lenso.agent.worktree-provider/default\",\n  \"lenso.agent.subagent-tools/worktree\",\n  \"lenso.agent.tools/worker-tools\",\n  \"lenso.agent.loop/researcher\",\n  \"lenso.agent.loop/reviewer\",\n  \"lenso.agent.loop/worker-a\",\n  \"lenso.agent.loop/worker-b\",\n  \"lenso.agent.interactive-approval-hook/default\",\n  \"lenso.agent.prompt.static/sandbox-coding\",\n]\n",
        ),
        (
            "profiles/plan.toml",
            "description = \"Official read-only planning agent\"\ninclude_enabled = true\ninstances = [\n  \"lenso.agent.workspace-instructions/default\",\n  \"lenso.agent.prompt.static/plan\",\n]\n",
        ),
    ]
    .into_iter()
    .map(|(path, content)| (path.to_owned(), content.to_owned()))
    .collect::<Vec<_>>();
    files.extend([
        (
            "plugins/lenso.agent.prompt.static/coding.toml".to_owned(),
            String::new(),
        ),
        (
            "plugins/lenso.agent.prompt.static/sandbox-coding.toml".to_owned(),
            String::new(),
        ),
        (
            "plugins/lenso.agent.prompt.static/plan.toml".to_owned(),
            String::new(),
        ),
    ]);
    files.extend(
        [
            "lenso.agent.workspace-instructions/default",
            "lenso.agent.workspace-edit/default",
            "lenso.agent.process.native/default",
            "lenso.agent.process.sandbox/default",
            "lenso.agent.process-tools/default",
            "lenso.agent.git-tools/default",
            "lenso.agent.code-mode-tools/default",
            "lenso.agent.subagent-tools/worktree",
            "lenso.agent.worktree-provider/default",
            "lenso.agent.loop/worker-a",
            "lenso.agent.loop/worker-b",
            "lenso.agent.tools/worker-tools",
            "lenso.agent.interactive-approval-hook/default",
            "lenso.agent.prompt.static/coding",
            "lenso.agent.prompt.static/sandbox-coding",
            "lenso.agent.prompt.static/plan",
        ]
        .into_iter()
        .map(|instance| {
            let (plugin, key) = instance
                .split_once('/')
                .expect("official Profile Instance has a key");
            (format!("plugins/{plugin}/{key}.disabled"), String::new())
        }),
    );
    files
}

fn parse_approval(arguments: &[String]) -> Result<ApprovalCommand, String> {
    let mut root = AgentDirectories::resolve()?.home().to_path_buf();
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
            let records = list_approvals(&root.join("approvals"))?;
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
    let record = decide_approval(&root.join("approvals"), &action.0, action.1)?;
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
    let options = direct_auth_options()?;
    match command {
        AuthCommand::Login { device_auth: true } => {
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
            let status = direct_auth_status(options)?;
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
            direct_logout(options)?;
            println!("Direct ChatGPT credentials were removed.");
            Ok(())
        }
    }
}

fn direct_auth_options() -> Result<DirectAuthOptions, String> {
    Ok(DirectAuthOptions {
        credential_file: Some(AgentDirectories::resolve()?.auth()),
        ..DirectAuthOptions::default()
    })
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

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn coding_profile_install_is_idempotent_and_creates_all_modes() {
        let home = tempfile::tempdir().unwrap();

        install_coding_profiles(home.path()).unwrap();
        install_coding_profiles(home.path()).unwrap();

        let code = fs::read_to_string(home.path().join("profiles/code.toml")).unwrap();
        let sandbox = fs::read_to_string(home.path().join("profiles/code-sandbox.toml")).unwrap();
        let plan = fs::read_to_string(home.path().join("profiles/plan.toml")).unwrap();
        let coding_prompt = fs::read_to_string(
            home.path()
                .join("plugins/lenso.agent.prompt.static/coding.toml"),
        )
        .unwrap();
        let sandbox_prompt = fs::read_to_string(
            home.path()
                .join("plugins/lenso.agent.prompt.static/sandbox-coding.toml"),
        )
        .unwrap();
        let plan_prompt = fs::read_to_string(
            home.path()
                .join("plugins/lenso.agent.prompt.static/plan.toml"),
        )
        .unwrap();
        let native_process = fs::read_to_string(
            home.path()
                .join("plugins/lenso.agent.process.native/default.toml"),
        )
        .unwrap();
        let sandbox_process = fs::read_to_string(
            home.path()
                .join("plugins/lenso.agent.process.sandbox/default.toml"),
        )
        .unwrap();
        assert!(code.contains("lenso.agent.workspace-edit/default"));
        assert!(code.contains("lenso.agent.interactive-approval-hook/default"));
        assert!(code.contains("lenso.agent.loop/researcher"));
        assert!(code.contains("lenso.agent.loop/reviewer"));
        assert!(code.contains("lenso.agent.loop/worker-a"));
        assert!(code.contains("lenso.agent.loop/worker-b"));
        assert!(code.contains("lenso.agent.worktree-provider/default"));
        assert!(code.contains("lenso.agent.subagent-tools/worktree"));
        assert!(sandbox.contains("lenso.agent.process.sandbox/default"));
        assert!(!sandbox.contains("lenso.agent.process.native/default"));
        assert!(!plan.contains("workspace-edit"));
        assert!(plan.contains("lenso.agent.workspace-instructions/default"));
        assert!(coding_prompt.is_empty());
        assert!(sandbox_prompt.is_empty());
        assert!(plan_prompt.is_empty());
        for configuration in [native_process, sandbox_process] {
            assert!(configuration.contains(
                "program_presets = [\"rust\", \"javascript\", \"python\", \"go\", \"build\"]"
            ));
        }
        for profile in [None, Some("code"), Some("code-sandbox"), Some("plan")] {
            lenso_agent_host::validate_desired_plugin_root_for_home(home.path(), profile)
                .unwrap_or_else(|error| {
                    panic!("installed Profile {profile:?} must resolve: {error}")
                });
        }
    }

    #[test]
    fn coding_profile_install_preserves_customized_files() {
        let home = tempfile::tempdir().unwrap();
        install_coding_profiles(home.path()).unwrap();
        let profile = home.path().join("profiles/code.toml");
        fs::write(&profile, "custom = true\n").unwrap();

        let error = install_coding_profiles(home.path()).unwrap_err();

        assert!(error.contains("refusing to overwrite"));
        assert_eq!(fs::read_to_string(profile).unwrap(), "custom = true\n");
    }

    #[test]
    fn coding_profile_install_upgrades_previous_official_profiles() {
        let home = tempfile::tempdir().unwrap();
        let plan = home.path().join("profiles/plan.toml");
        fs::create_dir_all(plan.parent().unwrap()).unwrap();
        let current = coding_profile_files()
            .into_iter()
            .find_map(|(path, content)| (path == "profiles/plan.toml").then_some(content))
            .unwrap();
        fs::write(&plan, current.replacen("include_enabled = true\n", "", 1)).unwrap();

        install_coding_profiles(home.path()).unwrap();

        assert_eq!(fs::read_to_string(plan).unwrap(), current);
    }

    #[test]
    fn profile_command_accepts_only_the_official_installer() {
        assert!(matches!(
            parse_profile(&["install".to_owned(), "coding".to_owned()]),
            Ok(ProfileCommand::InstallCoding)
        ));
        assert!(parse_profile(&["install".to_owned(), "unknown".to_owned()]).is_err());
    }

    #[test]
    fn models_command_accepts_an_optional_profile() {
        assert!(matches!(
            parse_command(vec!["models".to_owned()]),
            Ok(CliCommand::Models { profile: None })
        ));
        assert!(matches!(
            parse_command(vec![
                "models".to_owned(),
                "--profile".to_owned(),
                "code".to_owned(),
            ]),
            Ok(CliCommand::Models {
                profile: Some(profile)
            }) if profile == "code"
        ));
        assert!(parse_command(vec!["models".to_owned(), "--profile".to_owned()]).is_err());
    }

    #[test]
    fn release_commands_are_exact_and_do_not_become_prompts() {
        assert!(matches!(
            parse_command(vec!["--version".to_owned()]),
            Ok(CliCommand::Version)
        ));
        assert!(matches!(
            parse_command(vec!["doctor".to_owned()]),
            Ok(CliCommand::Doctor { json: false })
        ));
        assert!(matches!(
            parse_command(vec!["doctor".to_owned(), "--json".to_owned()]),
            Ok(CliCommand::Doctor { json: true })
        ));
        assert!(parse_command(vec!["doctor".to_owned(), "extra".to_owned()]).is_err());
    }
}

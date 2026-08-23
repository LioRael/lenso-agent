use std::{env, fs, path::PathBuf, process::ExitCode, time::Duration};

use lenso_agent_cli_module::CliModuleFactory;
use lenso_agent_loop_module::AgentLoopFactory;
use lenso_agent_model_fixture_module::FixtureModelFactory;
use lenso_agent_session_file_module::FileSessionFactory;
use lenso_agent_tools_module::ToolsFactory;
use lenso_agent_workspace_read_module::WorkspaceReadFactory;
use lenso_app_plan::ResolvedAppPlan;
use lenso_capability_agent::{Agent, RUN_TURN_OPERATION, RunTurnRequest};
use lenso_kernel::{
    ExecutionAdapterCatalog, Kernel, NativeStreamHandle, ShutdownOutcome, StreamEvent,
};
use lenso_native_adapter::NativeModuleRegistry;
use lenso_runner::TokioDriver;

#[derive(Debug)]
struct Args {
    plan: PathBuf,
    prompt: String,
    session: Option<String>,
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
    let args = parse_args()?;
    let bytes = fs::read(&args.plan)
        .map_err(|error| format!("failed to read {}: {error}", args.plan.display()))?;
    let plan = serde_json::from_slice::<ResolvedAppPlan>(&bytes)
        .map_err(|error| format!("resolved Plan is invalid JSON: {error}"))?;
    plan.validate()
        .map_err(|error| format!("resolved Plan is invalid: {error}"))?;
    let registry = NativeModuleRegistry::new()
        .with_factory(CliModuleFactory)
        .with_factory(AgentLoopFactory)
        .with_factory(FixtureModelFactory)
        .with_factory(ToolsFactory)
        .with_factory(WorkspaceReadFactory)
        .with_factory(FileSessionFactory);
    let app = Kernel::start(
        plan,
        TokioDriver::new(),
        ExecutionAdapterCatalog::single(registry),
    )
    .await
    .map_err(|error| format!("App startup failed: {error:?}"))?;
    let handle = app
        .stream_handle::<Agent>("cli")
        .map_err(|error| format!("Agent binding is unavailable: {error:?}"))?;
    let result = invoke(handle, args).await;
    let shutdown = app.shutdown(Duration::from_secs(2)).await;
    if !matches!(shutdown, ShutdownOutcome::Clean) {
        return Err(format!("App shutdown was not clean: {shutdown:?}"));
    }
    result
}

async fn invoke(handle: NativeStreamHandle<Agent>, args: Args) -> Result<(), String> {
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
            }
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => {
                println!();
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

fn parse_args() -> Result<Args, String> {
    let mut plan = PathBuf::from("composition/headless-readonly/resolved-plan.json");
    let mut prompt = None;
    let mut session = None;
    let mut arguments = env::args().skip(1);
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
                    "usage: lenso-agent-cli --prompt <text> [--session <id>] [--plan <path>]"
                        .to_owned(),
                );
            }
            unknown => return Err(format!("unknown argument `{unknown}`")),
        }
    }
    Ok(Args {
        plan,
        prompt: prompt.ok_or_else(|| "--prompt is required".to_owned())?,
        session,
    })
}

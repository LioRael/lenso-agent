use std::process::ExitCode;

mod app_agent_management;
mod standalone;

fn main() -> ExitCode {
    standalone::launch(lenso_agent_console_plugins::link, true)
}

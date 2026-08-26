use lenso_agent_loop_module::RunScope;
use lenso_capability_agent::{RUN_TURN_OPERATION, RunTurnRequest};
use lenso_kernel::StreamEvent;

use crate::generation::TurnGeneration;

pub(crate) async fn run_agent_turn(
    turn: TurnGeneration,
    prompt: String,
    session_id: Option<&str>,
    allowed_tools: &[String],
) -> Result<AgentTurnResult, String> {
    let context = RunScope::new(allowed_tools.to_vec())?.attach(turn.invocation_context()?)?;
    let stream = turn
        .handle()
        .open_with_context(
            RUN_TURN_OPERATION,
            context,
            RunTurnRequest {
                input: prompt,
                session_id: session_id.map(str::to_owned),
            },
        )
        .await
        .map_err(|error| format!("Agent stream failed to open: {error:?}"))?
        .map_err(|error| format!("Agent rejected the Turn: {error:?}"))?;
    stream
        .close_send()
        .await
        .map_err(|error| format!("failed to half-close Agent input: {error:?}"))?;
    let mut output = String::new();
    let mut returned_session_id = session_id.map(str::to_owned);
    loop {
        match stream
            .receive()
            .await
            .map_err(|error| format!("Agent stream failed: {error:?}"))?
        {
            StreamEvent::Message(message) => {
                returned_session_id = message.session_id.clone().or(returned_session_id);
                if message.is_text_delta() {
                    output.push_str(&message.text);
                }
            }
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => break,
            StreamEvent::Terminal(Err(error)) => {
                return Err(format!("Agent Turn failed: {error:?}"));
            }
        }
    }
    let session_id = returned_session_id
        .ok_or_else(|| "Agent Turn completed without a Session identity".to_owned())?;
    let text = if output.trim().is_empty() {
        "The Agent completed without a text response.".to_owned()
    } else {
        output
    };
    Ok(AgentTurnResult { text, session_id })
}

#[derive(Debug)]
pub(crate) struct AgentTurnResult {
    pub(crate) text: String,
    pub(crate) session_id: String,
}

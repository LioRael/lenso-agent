//! Purely ordered reduction of Agent Turn Stream events into volatile TUI state.

use super::{
    Duration, RunTurnError, RunTurnResponse, RunTurnResponseKind, StreamEvent, ToolStatus,
    TranscriptEntry, TuiState, UiPhase,
};

pub(in crate::tui::shell) fn handle_stream_event(
    event: Result<StreamEvent<RunTurnResponse, RunTurnError>, lenso_kernel::RuntimeFailure>,
    state: &mut TuiState,
) {
    let event = match event {
        Ok(event) => event,
        Err(error) => {
            state.active = None;
            state.pending_interaction = None;
            state.interaction_draft = None;
            state.pending_answers = None;
            state.finish_active_thinking();
            state.transcript.push(TranscriptEntry::Error {
                text: runtime_failure_message(error),
            });
            state.phase = UiPhase::Failed;
            return;
        }
    };
    match event {
        StreamEvent::Message(message) => {
            state.session_id = message
                .session_id
                .clone()
                .or_else(|| state.session_id.clone());
            match message
                .kind
                .clone()
                .unwrap_or(RunTurnResponseKind::TextDelta)
            {
                RunTurnResponseKind::ReasoningDelta => state.append_reasoning(message),
                RunTurnResponseKind::ReasoningCompleted => state.complete_reasoning(message),
                RunTurnResponseKind::TextDelta => state.append_agent_text(&message.text),
                RunTurnResponseKind::ToolStarted => state.start_tool(message),
                RunTurnResponseKind::ToolProgress => state.append_tool_progress(message),
                RunTurnResponseKind::ToolCompleted => {
                    state.finish_tool(message, ToolStatus::Completed);
                }
                RunTurnResponseKind::ToolFailed => {
                    state.finish_tool(message, ToolStatus::Failed);
                }
            }
        }
        StreamEvent::PeerHalfClosed => {}
        StreamEvent::Terminal(Ok(())) => {
            state.finish_active_thinking();
            let elapsed = state
                .active
                .take()
                .map_or(Duration::ZERO, |active| active.started_at.elapsed());
            state
                .transcript
                .push(TranscriptEntry::TurnCompleted { elapsed });
            state.pending_interaction = None;
            state.interaction_draft = None;
            state.pending_answers = None;
            state.phase = UiPhase::Idle;
        }
        StreamEvent::Terminal(Err(error)) => {
            state.finish_active_thinking();
            state.active = None;
            state.pending_interaction = None;
            state.interaction_draft = None;
            state.pending_answers = None;
            state.transcript.push(TranscriptEntry::Error {
                text: format!("Agent turn failed: {error:?}"),
            });
            state.phase = UiPhase::Failed;
        }
    }
}

fn runtime_failure_message(error: lenso_kernel::RuntimeFailure) -> String {
    match error {
        lenso_kernel::RuntimeFailure::PluginFailure { detail } => {
            format!("Turn stopped — {detail}")
        }
        error => format!("Turn stopped — {error:?}"),
    }
}

//! Terminal event-loop orchestration over one volatile state instance.

use super::{
    ACTIVE_TICK, AgentApp, CrosstermBackend, EVENT_TICK, EventStream, SnapshotResponsePanelsItem,
    StreamExt, Suggestion, Terminal, TuiOptions, TuiState, UiPhase, handle_stream_event,
    handle_terminal_event, io, present_online_generation_events, render, submit,
    sync_user_interaction,
};

pub(in crate::tui::shell) async fn run_loop(
    app: &AgentApp,
    options: &TuiOptions,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    events: &mut EventStream,
    panels: Vec<SnapshotResponsePanelsItem>,
    suggestions: Vec<Suggestion>,
) -> Result<(), String> {
    let mut state = TuiState::new(options, panels);
    state.suggestions = suggestions;
    run_loop_inner(app, options, terminal, events, &mut state).await
}

async fn run_loop_inner(
    app: &AgentApp,
    options: &TuiOptions,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    events: &mut EventStream,
    state: &mut TuiState,
) -> Result<(), String> {
    loop {
        present_online_generation_events(app, state).await;
        sync_user_interaction(state).await;
        if state.active.is_none() {
            if state.phase == UiPhase::SubmitRequested {
                submit(app, options, state).await?;
            } else if let Some(input) = state.queued_inputs.pop_front() {
                state.set_input(input);
                state.phase = UiPhase::SubmitRequested;
                submit(app, options, state).await?;
            }
        }
        terminal
            .draw(|frame| render(frame, state))
            .map_err(|error| format!("failed to render TUI: {error}"))?;

        if state.active.is_some() {
            let active = state.active.as_mut().expect("active turn checked");
            tokio::select! {
                event = events.next() => {
                    if handle_terminal_event(event, state)? {
                        return Ok(());
                    }
                }
                stream_event = active.stream.receive() => {
                    handle_stream_event(stream_event, state);
                }
                () = tokio::time::sleep(ACTIVE_TICK) => {
                    state.animation_tick = state.animation_tick.wrapping_add(1);
                }
            }
        } else {
            tokio::select! {
                event = events.next() => {
                    if handle_terminal_event(event, state)? {
                        return Ok(());
                    }
                }
                () = tokio::time::sleep(EVENT_TICK) => {
                    state.animation_tick = state.animation_tick.wrapping_add(1);
                }
            }
        }
    }
}

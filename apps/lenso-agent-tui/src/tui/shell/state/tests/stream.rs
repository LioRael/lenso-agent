use super::*;

#[test]
fn runtime_failure_stays_inline_and_keeps_the_tui_available() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    handle_stream_event(
        Err(lenso_kernel::RuntimeFailure::PluginFailure {
            detail: "fixture failure".to_owned(),
        }),
        &mut state,
    );
    assert_eq!(state.phase, UiPhase::Failed);
    assert!(state.active.is_none());
    assert!(matches!(
        state.transcript.last(),
        Some(TranscriptEntry::Error { text }) if text.contains("fixture failure")
    ));
}

#[test]
fn turn_stream_reducer_accumulates_text_and_preserves_the_session() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    for (sequence, text) in [("1", "hello"), ("2", " world")] {
        handle_stream_event(
            Ok(StreamEvent::Message(RunTurnResponse {
                arguments_json: None,
                content: None,
                duration_ms: None,
                error: None,
                kind: Some(RunTurnResponseKind::TextDelta),
                metadata_json: None,
                progress_channel: None,
                reasoning_id: None,
                sequence: sequence.to_owned(),
                session_id: Some("session-1".to_owned()),
                text: text.to_owned(),
                tool_call_id: None,
                tool_name: None,
            })),
            &mut state,
        );
    }

    assert_eq!(state.session_id.as_deref(), Some("session-1"));
    assert!(matches!(
        state.transcript.as_slice(),
        [TranscriptEntry::Agent { text, .. }] if text == "hello world"
    ));
}

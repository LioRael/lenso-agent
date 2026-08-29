use super::*;

fn choice_interaction() -> PendingInteraction {
    PendingInteraction {
        interaction_id: "question-1".to_owned(),
        questions: vec![
            InteractionQuestion {
                question_id: "mode".to_owned(),
                header: "Mode".to_owned(),
                prompt: "Choose a mode".to_owned(),
                options: vec![
                    InteractionOption {
                        option_id: "safe".to_owned(),
                        label: "Safe".to_owned(),
                        description: "Bounded changes".to_owned(),
                        preview: Some(Some("mode = \"safe\"".to_owned())),
                    },
                    InteractionOption {
                        option_id: "fast".to_owned(),
                        label: "Fast".to_owned(),
                        description: "Faster iteration".to_owned(),
                        preview: Some(Some("mode = \"fast\"".to_owned())),
                    },
                ],
                multi_select: false,
            },
            InteractionQuestion {
                question_id: "checks".to_owned(),
                header: "Checks".to_owned(),
                prompt: "Select checks".to_owned(),
                options: vec![InteractionOption {
                    option_id: "tests".to_owned(),
                    label: "Tests".to_owned(),
                    description: String::new(),
                    preview: Some(None),
                }],
                multi_select: true,
            },
        ],
    }
}

#[test]
fn single_and_multi_select_questions_produce_structured_answers() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    let interaction = choice_interaction();
    state.interaction_draft = Some(InteractionDraft::new(&interaction));
    state.pending_interaction = Some(interaction);

    handle_interaction_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(state.interaction_draft.as_ref().unwrap().question_index, 1);
    handle_interaction_key(
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        &mut state,
    );
    handle_interaction_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    let answers = state.pending_answers.as_ref().unwrap();
    assert_eq!(answers[0].selected_option_ids, ["safe"]);
    assert_eq!(answers[1].selected_option_ids, ["tests"]);
}

#[test]
fn question_card_owns_grok_navigation_keys_without_cancelling_the_turn() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    let interaction = choice_interaction();
    state.interaction_draft = Some(InteractionDraft::new(&interaction));
    state.pending_interaction = Some(interaction);

    handle_interaction_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut state);
    assert_eq!(state.interaction_draft.as_ref().unwrap().option_cursor(), 1);
    handle_interaction_key(
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(state.interaction_draft.as_ref().unwrap().option_cursor(), 0);
    handle_interaction_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut state);
    handle_interaction_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(state.interaction_draft.as_ref().unwrap().question_index, 1);
    handle_interaction_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &mut state);
    assert_eq!(state.interaction_draft.as_ref().unwrap().question_index, 0);
    assert_eq!(state.interaction_draft.as_ref().unwrap().option_cursor(), 1);

    handle_interaction_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert_eq!(state.focus, Focus::Scrollback);
    assert!(state.pending_interaction.is_some());
    assert!(!handle_key(
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        &mut state,
    ));
    assert_eq!(state.focus, Focus::Prompt);
}

#[test]
fn question_option_shortcuts_select_without_becoming_prompt_text() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    let interaction = choice_interaction();
    state.interaction_draft = Some(InteractionDraft::new(&interaction));
    state.pending_interaction = Some(interaction);

    handle_interaction_key(
        KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE),
        &mut state,
    );

    let draft = state.interaction_draft.as_ref().unwrap();
    assert_eq!(draft.question_index, 1);
    assert_eq!(
        draft.selected[0].iter().next().map(String::as_str),
        Some("fast")
    );
    assert!(state.input.is_empty());
}

#[test]
fn question_options_are_focusable_and_selectable_with_the_mouse() {
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    let interaction = choice_interaction();
    state.interaction_draft = Some(InteractionDraft::new(&interaction));
    state.pending_interaction = Some(interaction);
    terminal.draw(|frame| render(frame, &mut state)).unwrap();

    let fast = state.interaction_hit_targets[1].area;
    let position = ratatui::layout::Position::new(fast.x, fast.y);
    handle_mouse_move(position, &mut state);
    assert_eq!(state.interaction_draft.as_ref().unwrap().option_cursor(), 1);
    handle_mouse_click(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: fast.x,
            row: fast.y,
            modifiers: KeyModifiers::NONE,
        },
        position,
        &mut state,
    );

    let draft = state.interaction_draft.as_ref().unwrap();
    assert_eq!(draft.question_index, 1);
    assert_eq!(
        draft.selected[0].iter().next().map(String::as_str),
        Some("fast")
    );

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let tests = state.interaction_hit_targets[0].area;
    let tests_position = ratatui::layout::Position::new(tests.x, tests.y);
    handle_mouse_click(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: tests.x,
            row: tests.y,
            modifiers: KeyModifiers::NONE,
        },
        tests_position,
        &mut state,
    );
    assert!(state.interaction_draft.as_ref().unwrap().selected[1].contains("tests"));

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let other = state.interaction_hit_targets.last().unwrap().area;
    handle_mouse_click(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: other.x,
            row: other.y,
            modifiers: KeyModifiers::NONE,
        },
        ratatui::layout::Position::new(other.x, other.y),
        &mut state,
    );
    assert!(state.interaction_draft.as_ref().unwrap().editing_other);
}

#[test]
fn ask_user_replaces_the_composer_with_a_grok_style_question_card() {
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    let interaction = choice_interaction();
    state.interaction_draft = Some(InteractionDraft::new(&interaction));
    state.pending_interaction = Some(interaction);

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let content = terminal.backend().to_string();
    assert!(content.contains("Choose a mode"));
    assert!(content.contains("┃"));
    assert!(content.contains("1 (○) Safe"));
    assert!(content.contains("z (○) Type your answer here"));
    assert!(content.contains("mode = \"safe\""));
    assert!(!content.contains("╭ Mode"));
}

#[test]
fn accepted_interaction_is_not_recorded_as_a_user_message() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.transcript.push(TranscriptEntry::Agent {
        text: "Before asking".to_owned(),
        created_at: "8:19 AM".to_owned(),
    });
    state.pending_interaction = Some(choice_interaction());
    state.interaction_draft = state
        .pending_interaction
        .as_ref()
        .map(InteractionDraft::new);

    finish_interaction_submission(&mut state, Ok(()));

    assert_eq!(state.transcript.len(), 1);
    assert!(matches!(state.transcript[0], TranscriptEntry::Agent { .. }));
    assert!(state.pending_interaction.is_none());
    assert!(state.interaction_draft.is_none());
}

#[test]
fn every_question_accepts_an_other_answer() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    let mut interaction = choice_interaction();
    interaction.questions.truncate(1);
    let mut draft = InteractionDraft::new(&interaction);
    draft.set_option_cursor(interaction.questions[0].options.len());
    state.interaction_draft = Some(draft);
    state.pending_interaction = Some(interaction);

    handle_interaction_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for character in "balanced".chars() {
        handle_interaction_key(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_interaction_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    let answer = &state.pending_answers.as_ref().unwrap()[0];
    assert!(answer.selected_option_ids.is_empty());
    assert_eq!(answer.other, Some(Some("balanced".to_owned())));
}

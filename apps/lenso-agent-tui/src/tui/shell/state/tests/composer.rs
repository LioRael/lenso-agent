use super::*;

#[test]
fn renders_composed_panel_and_input() {
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(
        &TuiOptions::default(),
        vec![SnapshotResponsePanelsItem {
            id: "agent.help".to_owned(),
            title: "Help".to_owned(),
            body: "Esc quits".to_owned(),
        }],
    );
    state.set_input("hello".to_owned());
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let content = terminal.backend().to_string();
    assert!(!content.contains("Build with Lenso"));
    assert!(content.contains("╭"));
    assert!(content.contains("╰"));
    assert!(!content.contains("Esc quits"));
    assert!(content.contains("hello"));
    assert!(content.contains("enter:send"));
    assert!(!content.contains("Conversation"));

    handle_key(
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        &mut state,
    );
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let content = terminal.backend().to_string();
    assert!(!content.contains("Esc quits"));
    assert!(content.contains("plan…"));
}

#[test]
fn focused_composer_uses_the_canvas_and_hides_its_placeholder() {
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    terminal.draw(|frame| render(frame, &mut state)).unwrap();

    let composer = state.composer_hit.unwrap();
    let cell = terminal
        .backend()
        .buffer()
        .cell(ratatui::layout::Position::new(
            composer.x.saturating_add(2),
            composer.y.saturating_add(1),
        ))
        .unwrap();
    assert_eq!(cell.bg, Palette::BG_BASE);
    assert!(!terminal.backend().to_string().contains("Build anything"));
}

#[test]
fn compact_layout_keeps_the_conversation_and_composer_primary() {
    let backend = TestBackend::new(80, 16);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(
        &TuiOptions::default(),
        vec![SnapshotResponsePanelsItem {
            id: "agent.help".to_owned(),
            title: "Help".to_owned(),
            body: "Esc quits".to_owned(),
        }],
    );
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let content = terminal.backend().to_string();
    assert!(!content.contains("Build with Lenso"));
    assert!(!content.contains("Build anything"));
    assert!(content.contains("Ctrl+.:shortcuts"));
    assert!(!content.contains("Esc quits"));
    assert!(!content.contains("tab panels"));
}

fn composer_suggestions() -> Vec<Suggestion> {
    vec![
        Suggestion {
            id: "agent.command.clear".to_owned(),
            kind: SuggestionKind::Command,
            label: "/clear".to_owned(),
            insert_text: "/clear".to_owned(),
            description: "Clear the visible conversation".to_owned(),
        },
        Suggestion {
            id: "workspace.file.0".to_owned(),
            kind: SuggestionKind::File,
            label: "src/lib.rs".to_owned(),
            insert_text: "@src/lib.rs".to_owned(),
            description: "Workspace file".to_owned(),
        },
        Suggestion {
            id: "agents.skill.rust-review".to_owned(),
            kind: SuggestionKind::Skill,
            label: "/rust-review".to_owned(),
            insert_text: "/rust-review".to_owned(),
            description: "Review Rust code".to_owned(),
        },
    ]
}

#[test]
fn renders_command_suggestions_above_the_composer() {
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.suggestions = composer_suggestions();
    state.append_input("/c");

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let content = terminal.backend().to_string();
    assert!(!content.contains("Commands"));
    assert!(content.contains("/clear"));
    assert!(content.contains("Clear the visible conversation"));
}

#[test]
fn slash_dropdown_uses_grok_separator_chrome() {
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.suggestions = composer_suggestions();
    state.append_input("/");
    terminal.draw(|frame| render(frame, &mut state)).unwrap();

    let first_row = state.suggestion_hit_targets[0].area;
    let top_left = terminal
        .backend()
        .buffer()
        .cell(ratatui::layout::Position::new(
            first_row.x.saturating_sub(2),
            first_row.y.saturating_sub(1),
        ))
        .unwrap();
    assert_eq!(top_left.symbol(), "─");
    assert_eq!(top_left.bg, Palette::BG_BASE);
}

#[test]
fn mouse_click_accepts_a_slash_command_suggestion() {
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.suggestions = composer_suggestions();
    state.append_input("/c");
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let target = state.suggestion_hit_targets[0];

    handle_terminal_event(
        Some(Ok(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: target.area.x,
            row: target.area.y,
            modifiers: KeyModifiers::NONE,
        }))),
        &mut state,
    )
    .unwrap();

    assert_eq!(state.input, "/clear");
    assert_eq!(state.focus, Focus::Prompt);
}

#[test]
fn composer_and_shortcut_hints_are_clickable() {
    let backend = TestBackend::new(100, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.focus = Focus::Scrollback;
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let composer = state.composer_hit.unwrap();

    handle_terminal_event(
        Some(Ok(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: composer.x.saturating_add(1),
            row: composer.y.saturating_add(1),
            modifiers: KeyModifiers::NONE,
        }))),
        &mut state,
    )
    .unwrap();
    assert_eq!(state.focus, Focus::Prompt);

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let target = state
        .shortcut_hit_targets
        .iter()
        .find(|target| matches!(target.action, ShortcutAction::ShowShortcuts))
        .copied()
        .unwrap();
    handle_terminal_event(
        Some(Ok(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: target.area.x,
            row: target.area.y,
            modifiers: KeyModifiers::NONE,
        }))),
        &mut state,
    )
    .unwrap();
    assert!(state.show_shortcuts);
}

#[test]
fn keyboard_accepts_file_suggestion_at_the_active_token() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.suggestions = composer_suggestions();
    state.append_input("Read @src/li");

    assert!(!handle_key(
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        &mut state,
    ));
    assert_eq!(state.input, "Read @src/lib.rs ");
    assert_eq!(state.focus, Focus::Prompt);
}

#[test]
fn enter_executes_the_selected_slash_command_immediately() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.suggestions = composer_suggestions();
    state.append_input("/c");

    assert!(!handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    ));
    assert_eq!(state.input, "/clear");
    assert_eq!(state.phase, UiPhase::SubmitRequested);
}

#[test]
fn enter_selects_a_skill_and_leaves_the_prompt_open() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.suggestions = composer_suggestions();
    state.append_input("/rust");

    assert!(!handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    ));
    assert_eq!(state.input, "/rust-review ");
    assert_eq!(state.phase, UiPhase::Idle);
}

#[test]
fn escape_dismisses_suggestions_before_quitting() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.suggestions = composer_suggestions();
    state.append_input("/");

    assert!(!handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut state,
    ));
    assert_eq!(state.suggestion_visibility, SuggestionVisibility::Dismissed);
    assert!(handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut state,
    ));
}

#[test]
fn tiny_layout_keeps_the_prompt_reachable() {
    let backend = TestBackend::new(40, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.set_input("draft".to_owned());

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let content = terminal.backend().to_string();
    assert!(content.contains("draft"));
    assert!(content.contains("lenso-agent"));
    assert!(content.contains("Ctrl+.:shortcuts"));
}

#[test]
fn escape_quits_when_idle() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    assert!(handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut state
    ));
}

#[test]
fn input_is_bounded_by_the_agent_contract() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.append_input(&"x".repeat(MAX_INPUT_CHARACTERS + 1));
    assert_eq!(state.input_characters, MAX_INPUT_CHARACTERS);
    assert_eq!(state.input.len(), MAX_INPUT_CHARACTERS);
}

#[test]
fn multiline_input_is_preserved_and_shift_enter_adds_a_line() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.append_input("first\r\nsecond\rthird");
    assert_eq!(state.input, "first\nsecond\nthird");

    assert!(!handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        &mut state,
    ));
    assert_eq!(state.input, "first\nsecond\nthird\n");
    assert_eq!(state.phase, UiPhase::Idle);
}

#[test]
fn composer_edits_at_the_unicode_cursor() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.append_input("a界c");
    state.move_cursor(-1);
    state.append_input("b");
    assert_eq!(state.input, "a界bc");
    assert_eq!(state.input_cursor, 3);

    state.pop_input();
    assert_eq!(state.input, "a界c");
    state.delete_input();
    assert_eq!(state.input, "a界");
}

#[test]
fn prompt_history_preserves_the_unsent_draft() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.input_history = vec!["first".to_owned(), "second".to_owned()];
    state.set_input("draft".to_owned());

    state.previous_history();
    assert_eq!(state.input, "second");
    state.previous_history();
    assert_eq!(state.input, "first");
    state.next_history();
    assert_eq!(state.input, "second");
    state.next_history();
    assert_eq!(state.input, "draft");
}

#[test]
fn active_turn_keeps_the_composer_editable_and_queues_enter() {
    let backend = TestBackend::new(80, 18);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.phase = UiPhase::Active;
    state.set_input("follow up while running".to_owned());

    assert!(!handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    ));
    assert!(state.input.is_empty());
    assert_eq!(
        state.queued_inputs.front().map(String::as_str),
        Some("follow up while running")
    );

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("#1 follow up while running"));
    assert!(rendered.contains("[edit][cancel]"));

    let edit = state.queue_hit_targets[0]
        .edit
        .expect("hovered queue row should expose edit");
    handle_terminal_event(
        Some(Ok(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: edit.x,
            row: edit.y,
            modifiers: KeyModifiers::NONE,
        }))),
        &mut state,
    )
    .unwrap();
    assert!(state.queued_inputs.is_empty());
    assert_eq!(state.input, "follow up while running");
}

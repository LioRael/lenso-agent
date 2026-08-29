use super::*;

#[test]
fn conversation_visually_separates_user_and_markdown_agent_content() {
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.transcript = vec![
        TranscriptEntry::User {
            text: "Summarize it".to_owned(),
            created_at: "8:19 AM".to_owned(),
        },
        TranscriptEntry::Agent {
            text: "## Result\n- **Done**".to_owned(),
            created_at: "8:19 AM".to_owned(),
        },
    ];

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let content = terminal.backend().to_string();
    assert!(content.contains("❯ Summarize it"));
    assert!(content.contains("8:19 AM"));
    assert!(content.contains("Result"));
    assert!(content.contains("• Done"));

    let rows = content.lines().collect::<Vec<_>>();
    let user_y = rows
        .iter()
        .position(|line| line.contains("❯ Summarize it"))
        .unwrap();
    let user_target = state.entry_hit_targets[0];
    let user_x = 5;
    let buffer = terminal.backend().buffer();
    assert_eq!(
        buffer
            .cell(ratatui::layout::Position::new(
                user_x,
                user_y.try_into().unwrap()
            ))
            .unwrap()
            .symbol(),
        "❯",
        "Grok entry chrome reserves 1 + 2 columns"
    );
    for y in [user_y - 1, user_y, user_y + 1] {
        let y = y.try_into().unwrap();
        assert_eq!(
            buffer
                .cell(ratatui::layout::Position::new(2, y))
                .unwrap()
                .bg,
            Palette::BG_BASE,
            "the indicator rail stays outside the message surface"
        );
        assert_eq!(
            buffer
                .cell(ratatui::layout::Position::new(3, y))
                .unwrap()
                .bg,
            Palette::USER_SURFACE,
            "the message surface begins immediately after the left rail"
        );
        assert_eq!(
            buffer
                .cell(ratatui::layout::Position::new(4, y))
                .unwrap()
                .bg,
            Palette::USER_SURFACE
        );
        assert_eq!(
            buffer
                .cell(ratatui::layout::Position::new(
                    user_target.area.right().saturating_sub(2),
                    y,
                ))
                .unwrap()
                .bg,
            Palette::USER_SURFACE,
            "the message surface ends immediately before the right rail"
        );
        assert_eq!(
            buffer
                .cell(ratatui::layout::Position::new(
                    user_target.area.right().saturating_sub(1),
                    y,
                ))
                .unwrap()
                .bg,
            Palette::BG_BASE,
            "the right interaction rail stays outside the message surface"
        );
    }
    let agent_y = rows
        .iter()
        .position(|line| line.contains("Result"))
        .unwrap();
    let agent_x = user_x;
    let heading = buffer
        .cell(ratatui::layout::Position::new(
            agent_x,
            agent_y.try_into().unwrap(),
        ))
        .unwrap();
    assert_eq!(heading.fg, Palette::HEADING_H2);
}

#[test]
fn reasoning_stream_becomes_a_clickable_completed_thought() {
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.start_provisional_thinking();

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    assert!(terminal.backend().to_string().contains("Thinking…"));

    handle_stream_event(
        Ok(StreamEvent::Message(RunTurnResponse {
            arguments_json: None,
            content: None,
            duration_ms: None,
            error: None,
            kind: Some(RunTurnResponseKind::ReasoningDelta),
            metadata_json: None,
            progress_channel: None,
            reasoning_id: Some("turn-1:1".to_owned()),
            sequence: "1".to_owned(),
            session_id: Some("session-1".to_owned()),
            text: "Checking the relevant files.".to_owned(),
            tool_call_id: None,
            tool_name: None,
        })),
        &mut state,
    );
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    assert!(
        terminal
            .backend()
            .to_string()
            .contains("Checking the relevant files.")
    );

    handle_stream_event(
        Ok(StreamEvent::Message(RunTurnResponse {
            arguments_json: None,
            content: None,
            duration_ms: Some("1250".to_owned()),
            error: None,
            kind: Some(RunTurnResponseKind::ReasoningCompleted),
            metadata_json: None,
            progress_channel: None,
            reasoning_id: Some("turn-1:1".to_owned()),
            sequence: "2".to_owned(),
            session_id: Some("session-1".to_owned()),
            text: String::new(),
            tool_call_id: None,
            tool_name: None,
        })),
        &mut state,
    );
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let collapsed = terminal.backend().to_string();
    assert!(collapsed.contains("Thought for 1.2s"));
    assert!(!collapsed.contains("Checking the relevant files."));

    let target = state.thinking_hit_targets[0];
    assert!(
        state.toggle_thinking_at(ratatui::layout::Position::new(target.area.x, target.area.y,))
    );
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    assert!(
        terminal
            .backend()
            .to_string()
            .contains("Checking the relevant files.")
    );
}

#[test]
fn collapsed_block_hover_uses_grok_surface_rail_and_chevron() {
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    let mut thought = ThinkingCard::provisional();
    thought.append("turn-1:1".to_owned(), "Inspecting the source.");
    thought.finish(Some(4600));
    state.transcript.push(TranscriptEntry::Thinking(thought));
    state
        .transcript
        .push(TranscriptEntry::Tool(ToolCard::running(
            "call-1".to_owned(),
            "run_process".to_owned(),
            Some(r#"{"program":"cargo","arguments":["test"]}"#.to_owned()),
        )));

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let target = state.entry_hit_targets[0];
    handle_mouse_move(
        ratatui::layout::Position::new(target.area.x.saturating_add(3), target.area.y),
        &mut state,
    );
    terminal.draw(|frame| render(frame, &mut state)).unwrap();

    let buffer = terminal.backend().buffer();
    let rail = buffer
        .cell(ratatui::layout::Position::new(target.area.x, target.area.y))
        .unwrap();
    assert_eq!(rail.symbol(), "│");
    assert_eq!(rail.fg, Palette::HOVER_BORDER);
    let surface = buffer
        .cell(ratatui::layout::Position::new(
            target.area.x.saturating_add(1),
            target.area.y,
        ))
        .unwrap();
    assert_eq!(surface.bg, Palette::HOVER_SURFACE);
    let right_rail = buffer
        .cell(ratatui::layout::Position::new(
            target.area.right().saturating_sub(1),
            target.area.y,
        ))
        .unwrap();
    assert_eq!(right_rail.bg, Palette::BG_BASE);
    let right_surface = buffer
        .cell(ratatui::layout::Position::new(
            target.area.right().saturating_sub(2),
            target.area.y,
        ))
        .unwrap();
    assert_eq!(right_surface.bg, Palette::HOVER_SURFACE);
    let indicator = buffer
        .cell(ratatui::layout::Position::new(
            target.area.x.saturating_add(3),
            target.area.y,
        ))
        .unwrap();
    assert_eq!(indicator.symbol(), "›");

    let tool_target = state.entry_hit_targets[1];
    handle_mouse_move(
        ratatui::layout::Position::new(tool_target.area.x.saturating_add(3), tool_target.area.y),
        &mut state,
    );
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    let tool_surface = buffer
        .cell(ratatui::layout::Position::new(
            tool_target.area.x.saturating_add(2),
            tool_target.area.y,
        ))
        .unwrap();
    assert_eq!(tool_surface.bg, Palette::HOVER_SURFACE);
    let tool_indicator = buffer
        .cell(ratatui::layout::Position::new(
            tool_target.area.x.saturating_add(3),
            tool_target.area.y,
        ))
        .unwrap();
    assert_eq!(tool_indicator.symbol(), "›");
}

#[test]
fn completed_turn_renders_the_grok_session_marker() {
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.transcript.push(TranscriptEntry::TurnCompleted {
        elapsed: Duration::from_millis(4600),
    });

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let content = terminal.backend().to_string();
    assert!(content.contains("Worked for 4.6s"));
    assert_eq!(format_turn_duration(Duration::from_secs(125)), "2m5s");
}

#[test]
fn tool_events_render_a_collapsible_file_change_card() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    handle_stream_event(
        Ok(StreamEvent::Message(RunTurnResponse {
            arguments_json: Some(
                r#"{"path":"src/lib.rs","old_text":"a","new_text":"b"}"#
                    .to_owned()
                    .try_into()
                    .unwrap(),
            ),
            content: None,
            duration_ms: None,
            error: None,
            kind: Some(RunTurnResponseKind::ToolStarted),
            metadata_json: None,
            progress_channel: None,
            reasoning_id: None,
            sequence: "1".to_owned(),
            session_id: Some("session-1".to_owned()),
            text: String::new(),
            tool_call_id: Some("call-1".to_owned()),
            tool_name: Some("edit".to_owned()),
        })),
        &mut state,
    );
    handle_stream_event(
        Ok(StreamEvent::Message(RunTurnResponse {
            arguments_json: None,
            content: Some("edited src/lib.rs".to_owned()),
            duration_ms: Some("12".to_owned()),
            error: None,
            kind: Some(RunTurnResponseKind::ToolCompleted),
            metadata_json: Some(
                r#"{"operation":"edited","path":"src/lib.rs","bytes_written":42}"#
                    .to_owned()
                    .try_into()
                    .unwrap(),
            ),
            progress_channel: None,
            reasoning_id: None,
            sequence: "2".to_owned(),
            session_id: Some("session-1".to_owned()),
            text: String::new(),
            tool_call_id: Some("call-1".to_owned()),
            tool_name: Some("edit".to_owned()),
        })),
        &mut state,
    );

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let collapsed = terminal.backend().to_string();
    assert!(collapsed.contains("Edited src/lib.rs"));
    assert!(collapsed.contains("42 B  12ms"));
    assert!(!collapsed.contains("- a"));

    handle_control_key(KeyCode::Char('o'), &mut state);
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let expanded = terminal.backend().to_string();
    assert!(expanded.contains("Edited src/lib.rs"));
    assert!(expanded.contains("- a"));
    assert!(expanded.contains("+ b"));
    assert!(!expanded.contains("old_text"));
}

#[test]
fn running_command_renders_progress_before_completion() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    handle_stream_event(
        Ok(StreamEvent::Message(RunTurnResponse {
            arguments_json: Some(
                r#"{"program":"cargo","arguments":["test"]}"#.to_owned().try_into().unwrap(),
            ),
            content: None,
            duration_ms: None,
            error: None,
            kind: Some(RunTurnResponseKind::ToolStarted),
            metadata_json: None,
            progress_channel: None,
            reasoning_id: None,
            sequence: "1".to_owned(),
            session_id: Some("session-1".to_owned()),
            text: String::new(),
            tool_call_id: Some("call-1".to_owned()),
            tool_name: Some("run_process".to_owned()),
        })),
        &mut state,
    );
    handle_stream_event(
        Ok(StreamEvent::Message(RunTurnResponse {
            arguments_json: None,
            content: Some("Compiling live-output\n".to_owned()),
            duration_ms: None,
            error: None,
            kind: Some(RunTurnResponseKind::ToolProgress),
            metadata_json: None,
            progress_channel: Some(lenso_capability_agent::RunTurnResponseProgressChannel::Stderr),
            reasoning_id: None,
            sequence: "2".to_owned(),
            session_id: Some("session-1".to_owned()),
            text: String::new(),
            tool_call_id: Some("call-1".to_owned()),
            tool_name: Some("run_process".to_owned()),
        })),
        &mut state,
    );

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let content = terminal.backend().to_string();
    assert!(content.contains("Running cargo test"));
    assert!(!content.contains("Compiling live-output"));

    handle_control_key(KeyCode::Char('o'), &mut state);
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let content = terminal.backend().to_string();
    assert!(content.contains("Compiling live-output"));
}

#[test]
fn markdown_distinguishes_headings_lists_code_and_emphasis() {
    let lines = markdown_lines(
        "## Result\n- **done** with `cargo test`\n```rust\nfn main() {}\n```\nSee [docs](https://example.com), *now*.\n> > nested\n\n| A | B |\n|---|---|\n| 1 | 2 |",
    );
    let text = Text::from(lines);
    assert_eq!(text.lines[0].spans[0].content, "Result");
    assert_eq!(text.lines[1].spans[1].content, "• ");
    assert!(
        text.lines[1]
            .spans
            .iter()
            .any(|span| span.content == "done")
    );
    assert!(
        text.lines[1]
            .spans
            .iter()
            .any(|span| span.content == "cargo test")
    );
    assert!(
        text.lines
            .iter()
            .all(|line| !line.to_string().contains("```") && !line.to_string().contains("rust"))
    );
    let code = text
        .lines
        .iter()
        .find(|line| line.to_string() == "fn main() {}")
        .expect("code line");
    assert_eq!(code.style.bg, Some(Palette::SURFACE));
    let link = text
        .lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == "docs")
        .expect("link span");
    assert_eq!(link.style.fg, Some(Palette::LINK));
    assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));
    assert!(
        text.lines
            .iter()
            .any(|line| line.to_string() == "│ │ nested")
    );
    let table = text.lines.iter().map(Line::to_string).collect::<String>();
    assert!(table.contains('┌'));
    assert!(table.contains('┼'));
    assert!(table.contains('┘'));
}

#[test]
fn markdown_tables_fit_the_message_width_and_links_map_to_screen_cells() {
    let table = markdown_lines_with_width(
        "| command | description |\n|---|---|\n| cargo test --workspace | validate everything |",
        24,
    );
    assert!(table.iter().all(|line| line.width() <= 24));
    assert!(table.iter().any(|line| line.to_string().contains('…')));

    let backend = TestBackend::new(80, 18);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.transcript.push(TranscriptEntry::Agent {
        text: "Read the [official docs](https://example.com/docs).\nOr visit https://x.ai/build."
            .to_owned(),
        created_at: "1:00 PM".to_owned(),
    });
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let link = state.link_hit_targets.first().expect("visible link target");
    assert_eq!(link.url, "https://example.com/docs");
    assert_eq!(link.area.width, 13);
    assert_eq!(state.link_hit_targets[1].url, "https://x.ai/build");
    assert!(safe_link_target(&link.url));
    assert!(!safe_link_target("javascript:alert(1)"));
    assert!(!safe_link_target("https://example.com\nmalicious"));
}

#[test]
fn shortcut_overlay_is_modal_and_escape_closes_it() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    assert!(!handle_key(
        KeyEvent::new(KeyCode::Char('.'), KeyModifiers::CONTROL),
        &mut state,
    ));
    assert!(state.show_shortcuts);
    assert!(!handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut state,
    ));
    assert!(!state.show_shortcuts);
}

#[test]
fn page_navigation_leaves_and_restores_tail_following() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.scroll.viewport_rows = 8;
    state.scroll.max_top = 40;
    state.scroll.top = 40;

    assert!(!handle_key(
        KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        &mut state,
    ));
    assert!(state.scroll.top < state.scroll.max_top);
    assert!(!state.scroll.follow_tail);

    assert!(!handle_key(
        KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
        &mut state,
    ));
    assert_eq!(state.scroll.top, state.scroll.max_top);
    assert!(state.scroll.follow_tail);
}

#[test]
fn mouse_wheel_scrolls_without_leaving_the_prompt() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.input = "draft stays here".to_owned();
    state.input_characters = state.input.chars().count();
    state.scroll.viewport_rows = 8;
    state.scroll.max_top = 40;
    state.scroll.top = 40;

    assert!(
        !handle_terminal_event(
            Some(Ok(Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 12,
                row: 4,
                modifiers: KeyModifiers::NONE,
            }))),
            &mut state,
        )
        .unwrap()
    );
    assert_eq!(state.scroll.top, 40 - WHEEL_SCROLL_LINES);
    assert!(!state.scroll.follow_tail);
    assert_eq!(state.input, "draft stays here");
}

#[test]
fn clicking_a_tool_block_toggles_its_details() {
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state
        .transcript
        .push(TranscriptEntry::Tool(ToolCard::running(
            "call-1".to_owned(),
            "read".to_owned(),
            Some(r#"{"path":"src/lib.rs"}"#.to_owned()),
        )));
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let target = state.tool_hit_targets[0];

    handle_terminal_event(
        Some(Ok(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: target.column_start,
            row: target.row_start,
            modifiers: KeyModifiers::NONE,
        }))),
        &mut state,
    )
    .unwrap();

    assert!(matches!(
        state.transcript.first(),
        Some(TranscriptEntry::Tool(ToolCard { expanded: true, .. }))
    ));
}

#[test]
fn consecutive_completed_tools_collapse_into_one_semantic_group() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    for (call_id, path) in [("call-1", "src/lib.rs"), ("call-2", "src/main.rs")] {
        let mut card = ToolCard::running(
            call_id.to_owned(),
            "read".to_owned(),
            Some(format!(r#"{{"path":"{path}"}}"#)),
        );
        card.status = ToolStatus::Completed;
        state.transcript.push(TranscriptEntry::Tool(card));
    }

    let collapsed = transcript_lines(&state, 100);
    let rows = collapsed.tool_rows;
    let collapsed = Text::from(collapsed.lines).to_string();
    assert!(collapsed.contains("Read 2 files"));
    assert!(!collapsed.contains("src/lib.rs"));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].selection, ToolSelection::Group { start: 0, end: 2 });

    state.selected_block = Some(rows[0].selection);
    state.toggle_tool_details();
    let expanded = transcript_lines(&state, 100);
    let rows = expanded.tool_rows;
    let expanded = Text::from(expanded.lines).to_string();
    assert!(expanded.contains("src/lib.rs"));
    assert!(expanded.contains("src/main.rs"));
    assert_eq!(rows.len(), 3);
}

#[test]
fn prompt_becomes_sticky_only_after_it_scrolls_above_the_viewport() {
    let anchors = vec![
        PromptAnchor {
            start_row: 4,
            end_row: 5,
            text: "first task".to_owned(),
        },
        PromptAnchor {
            start_row: 20,
            end_row: 20,
            text: "second task".to_owned(),
        },
    ];
    assert_eq!(sticky_prompt(&anchors, 5), None);
    assert_eq!(sticky_prompt(&anchors, 8), Some("first task"));
    assert_eq!(sticky_prompt(&anchors, 24), Some("second task"));
}

#[test]
fn rendered_history_exposes_scroll_position_and_follow_control() {
    let backend = TestBackend::new(80, 16);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    for index in 0..24 {
        state.transcript.push(TranscriptEntry::System {
            text: format!("event {index}"),
        });
    }

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    assert!(state.scroll.max_top > 0);
    assert!(state.scroll.follow_tail);

    handle_key(
        KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        &mut state,
    );
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let content = terminal.backend().to_string();
    assert!(content.contains("lines below"));
    assert!(content.contains("End to follow"));
}

#[test]
fn tab_focus_enables_grok_style_scrollback_navigation() {
    let backend = TestBackend::new(80, 16);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.transcript.push(TranscriptEntry::Agent {
        text: "first answer".to_owned(),
        created_at: "1:00 PM".to_owned(),
    });
    state.transcript.push(TranscriptEntry::Agent {
        text: "second answer".to_owned(),
        created_at: "1:01 PM".to_owned(),
    });
    terminal.draw(|frame| render(frame, &mut state)).unwrap();

    handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut state);
    assert_eq!(state.focus, Focus::Scrollback);
    handle_key(
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(state.selected_entry, Some(0));
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let content = terminal.backend().to_string();
    assert!(content.contains('┌'));
    assert!(content.contains('┐'));
    assert!(content.contains('└'));
    assert!(content.contains('┘'));
    handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut state);
    assert_eq!(state.focus, Focus::Prompt);
}

#[test]
fn submitted_prompt_page_flips_then_resumes_tail_following() {
    let backend = TestBackend::new(80, 16);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    for index in 0..18 {
        state.transcript.push(TranscriptEntry::System {
            text: format!("prior event {index}"),
        });
    }
    state.transcript.push(TranscriptEntry::User {
        text: "new turn".to_owned(),
        created_at: "8:19 AM".to_owned(),
    });
    state.scroll.begin_page_flip();

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    assert!(state.scroll.page_flip_anchor.is_some());
    assert!(!state.scroll.follow_tail);

    state.transcript.push(TranscriptEntry::Agent {
        text: (0..30)
            .map(|index| format!("streamed line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
        created_at: "8:19 AM".to_owned(),
    });
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    assert!(state.scroll.follow_tail);
    assert!(state.scroll.page_flip_anchor.is_none());
}

#[test]
fn scrollbar_track_supports_click_and_drag_navigation() {
    let backend = TestBackend::new(80, 16);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    for index in 0..30 {
        state.transcript.push(TranscriptEntry::System {
            text: format!("event {index}"),
        });
    }
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let track = state.scrollbar_hit.expect("scrollbar should be visible");

    handle_terminal_event(
        Some(Ok(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: track.x,
            row: track.y,
            modifiers: KeyModifiers::NONE,
        }))),
        &mut state,
    )
    .unwrap();
    assert_eq!(state.scroll.top, 0);
    assert!(state.scrollbar_dragging);

    handle_terminal_event(
        Some(Ok(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: track.x,
            row: track.bottom().saturating_sub(1),
            modifiers: KeyModifiers::NONE,
        }))),
        &mut state,
    )
    .unwrap();
    assert_eq!(state.scroll.top, state.scroll.max_top);
    assert!(state.scroll.follow_tail);
}

#[test]
fn long_user_prompt_uses_the_source_three_line_fold_and_clicks_open() {
    let backend = TestBackend::new(80, 18);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.transcript.push(TranscriptEntry::User {
        text: "one\ntwo\nthree\nfour".to_owned(),
        created_at: "8:19 AM".to_owned(),
    });

    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    let collapsed = terminal.backend().to_string();
    assert!(collapsed.contains("three …"));
    assert!(!collapsed.contains("four"));
    let target = state.user_hit_targets[0];

    handle_terminal_event(
        Some(Ok(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: target.area.x.saturating_add(3),
            row: target.area.y.saturating_add(1),
            modifiers: KeyModifiers::NONE,
        }))),
        &mut state,
    )
    .unwrap();
    terminal.draw(|frame| render(frame, &mut state)).unwrap();
    assert!(terminal.backend().to_string().contains("four"));
    assert!(state.expanded_user_entries.contains(&0));
}

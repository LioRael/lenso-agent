//! Terminal input reduction into the private TUI state machine.

use super::interaction::{append_interaction_other, handle_interaction_key};
use super::{
    Command, Event, Focus, InteractionHitAction, InteractionHitTarget, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind, ShortcutAction,
    SuggestionKind, SuggestionVisibility, TuiState, UiPhase, WheelDirection, io,
};

pub(in crate::tui::shell) fn handle_terminal_event(
    event: Option<Result<Event, io::Error>>,
    state: &mut TuiState,
) -> Result<bool, String> {
    let Some(event) = event else {
        return Ok(true);
    };
    let event = event.map_err(|error| format!("failed to read terminal input: {error}"))?;
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(handle_key(key, state)),
        Event::Paste(text)
            if state
                .interaction_draft
                .as_ref()
                .is_some_and(|draft| draft.editing_other) =>
        {
            append_interaction_other(state, &text);
            Ok(false)
        }
        Event::Paste(text) if !state.show_shortcuts => {
            state.append_input(&text);
            Ok(false)
        }
        Event::Mouse(mouse) => {
            handle_mouse_event(mouse, state);
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn handle_mouse_event(mouse: MouseEvent, state: &mut TuiState) {
    let position = ratatui::layout::Position::new(mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::ScrollUp => handle_mouse_scroll(position, state, true),
        MouseEventKind::ScrollDown => handle_mouse_scroll(position, state, false),
        MouseEventKind::Down(MouseButton::Left) => handle_mouse_click(mouse, position, state),
        MouseEventKind::Moved => handle_mouse_move(position, state),
        MouseEventKind::Drag(MouseButton::Left) if state.scrollbar_dragging => {
            if let Some(track) = state.scrollbar_hit {
                state.scroll.scroll_to_pointer(mouse.row, track);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => state.scrollbar_dragging = false,
        _ => {}
    }
}

fn handle_mouse_scroll(position: ratatui::layout::Position, state: &mut TuiState, up: bool) {
    if state
        .suggestion_hit_targets
        .iter()
        .any(|target| target.area.contains(position))
    {
        state.select_suggestion(up);
        return;
    }
    let direction = if up {
        WheelDirection::Up
    } else {
        WheelDirection::Down
    };
    let rows = state.wheel.rows(direction);
    if up {
        state.scroll.scroll_up(rows);
    } else {
        state.scroll.scroll_down(rows);
    }
}

pub(in crate::tui::shell) fn handle_mouse_click(
    mouse: MouseEvent,
    position: ratatui::layout::Position,
    state: &mut TuiState,
) {
    update_hovered_entry(position, state);
    if let Some(target) = state
        .interaction_hit_targets
        .iter()
        .copied()
        .find(|target| target.area.contains(position))
    {
        activate_interaction_hit(target, state);
    } else if let Some(target) = state
        .queue_hit_targets
        .iter()
        .copied()
        .find(|target| target.cancel.is_some_and(|area| area.contains(position)))
    {
        state.cancel_queued_input(target.index);
    } else if let Some(target) = state
        .queue_hit_targets
        .iter()
        .copied()
        .find(|target| target.edit.is_some_and(|area| area.contains(position)))
    {
        state.edit_queued_input(target.index);
    } else if let Some(target) = state
        .queue_hit_targets
        .iter()
        .copied()
        .find(|target| target.area.contains(position))
    {
        state.queue_hovered = Some(target.index);
    } else if let Some(target) = state
        .suggestion_hit_targets
        .iter()
        .copied()
        .find(|target| target.area.contains(position))
    {
        state.suggestion_selected = target.selection;
        state.accept_suggestion();
        state.focus = Focus::Prompt;
    } else if let Some(target) = state
        .shortcut_hit_targets
        .iter()
        .copied()
        .find(|target| target.area.contains(position))
    {
        handle_shortcut_action(target.action, state);
    } else if state.cancel_hit.is_some_and(|area| area.contains(position)) {
        cancel_active_turn(state);
    } else if state
        .composer_hit
        .is_some_and(|area| area.contains(position))
    {
        state.focus = Focus::Prompt;
    } else if state.follow_hit.is_some_and(|area| area.contains(position)) {
        state.scroll.goto_bottom();
    } else if let Some(track) = state.scrollbar_hit.filter(|area| area.contains(position)) {
        state.scrollbar_dragging = true;
        state.scroll.scroll_to_pointer(mouse.row, track);
        state.focus = Focus::Scrollback;
    } else if let Some(url) = state
        .link_hit_targets
        .iter()
        .find(|target| target.area.contains(position))
        .map(|target| target.url.clone())
    {
        if let Err(detail) = open_link(&url) {
            state.push_system(format!("Could not open link — {detail}"));
        }
    } else if !state.toggle_user_at(position) && !state.toggle_thinking_at(position) {
        let tool_target = state.tool_hit_targets.iter().any(|target| {
            mouse.column >= target.column_start
                && mouse.column <= target.column_end
                && mouse.row >= target.row_start
                && mouse.row <= target.row_end
        });
        if tool_target {
            state.toggle_tool_at(mouse.column, mouse.row);
        } else if let Some(target) = state
            .entry_hit_targets
            .iter()
            .find(|target| target.area.contains(position))
        {
            state.selected_entry = Some(target.entry_index);
            state.focus = Focus::Scrollback;
        }
    }
}

fn open_link(url: &str) -> Result<(), String> {
    if !safe_link_target(url) {
        return Err("unsupported or unsafe URL scheme".to_owned());
    }
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err("opening links is unsupported on this platform".to_owned());
    command
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("failed to launch system opener: {error}"))
}

pub(in crate::tui::shell) fn safe_link_target(url: &str) -> bool {
    if url.is_empty() || url.chars().any(char::is_control) {
        return false;
    }
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto" | "file"
    )
}

pub(in crate::tui::shell) fn handle_mouse_move(
    position: ratatui::layout::Position,
    state: &mut TuiState,
) {
    if let Some(target) = state
        .interaction_hit_targets
        .iter()
        .copied()
        .find(|target| target.area.contains(position))
    {
        focus_interaction_hit(target, state);
        return;
    }
    update_hovered_entry(position, state);
    state.queue_hovered = state
        .queue_hit_targets
        .iter()
        .find(|target| target.area.contains(position))
        .map(|target| target.index);
    if let Some(target) = state
        .suggestion_hit_targets
        .iter()
        .find(|target| target.area.contains(position))
    {
        state.suggestion_selected = target.selection;
    }
}

fn focus_interaction_hit(target: InteractionHitTarget, state: &mut TuiState) {
    let Some(draft) = state.interaction_draft.as_mut() else {
        return;
    };
    let cursor = match target.action {
        InteractionHitAction::Option(index) => index,
        InteractionHitAction::Other => state
            .pending_interaction
            .as_ref()
            .and_then(|interaction| interaction.questions.get(draft.question_index))
            .map_or(0, |question| question.options.len()),
    };
    draft.set_option_cursor(cursor);
    state.focus = Focus::Prompt;
}

fn activate_interaction_hit(target: InteractionHitTarget, state: &mut TuiState) {
    focus_interaction_hit(target, state);
    let multi_select = state
        .pending_interaction
        .as_ref()
        .zip(state.interaction_draft.as_ref())
        .and_then(|(interaction, draft)| interaction.questions.get(draft.question_index))
        .is_some_and(|question| question.multi_select);
    let code = match target.action {
        InteractionHitAction::Option(_) if multi_select => KeyCode::Char(' '),
        InteractionHitAction::Option(_) | InteractionHitAction::Other => KeyCode::Enter,
    };
    handle_interaction_key(KeyEvent::new(code, KeyModifiers::NONE), state);
}

fn update_hovered_entry(position: ratatui::layout::Position, state: &mut TuiState) {
    state.hovered_entry = state
        .entry_hit_targets
        .iter()
        .find(|target| target.area.contains(position))
        .map(|target| target.entry_index);
}

fn cancel_active_turn(state: &mut TuiState) {
    if state.active.take().is_some() {
        state.pending_interaction = None;
        state.interaction_draft = None;
        state.pending_answers = None;
        state.finish_active_thinking();
        state.push_system("Turn cancelled.");
        state.phase = UiPhase::Idle;
    }
}

fn handle_shortcut_action(action: ShortcutAction, state: &mut TuiState) {
    match action {
        ShortcutAction::Send if !state.input.trim().is_empty() => {
            if state.turn_is_running() {
                state.queue_input();
            } else {
                state.phase = UiPhase::SubmitRequested;
            }
        }
        ShortcutAction::Newline => state.append_input("\n"),
        ShortcutAction::FocusPrompt => state.focus = Focus::Prompt,
        ShortcutAction::FocusScrollback => state.focus = Focus::Scrollback,
        ShortcutAction::PageDown => state.scroll.scroll_down(state.scroll.page_rows()),
        ShortcutAction::ToggleSelectedTool => state.toggle_tool_details(),
        ShortcutAction::ShowShortcuts => state.show_shortcuts = true,
        ShortcutAction::Send => {}
    }
}

pub(in crate::tui::shell) fn handle_key(key: KeyEvent, state: &mut TuiState) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        state.active = None;
        return true;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('.') {
        state.show_shortcuts = !state.show_shortcuts;
        return false;
    }
    if state.show_shortcuts {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
            state.show_shortcuts = false;
        }
        return false;
    }
    if state.pending_interaction.is_some() {
        if state.focus == Focus::Scrollback {
            if key.code == KeyCode::Tab {
                state.focus = Focus::Prompt;
            } else {
                handle_scrollback_key(key, state);
            }
            return false;
        }
        handle_interaction_key(key, state);
        return false;
    }
    if state.pending_interaction.is_none() && state.suggestion_match().is_some() {
        match key.code {
            KeyCode::Esc => {
                state.suggestion_visibility = SuggestionVisibility::Dismissed;
                return false;
            }
            KeyCode::Up => {
                state.select_suggestion(true);
                return false;
            }
            KeyCode::Down => {
                state.select_suggestion(false);
                return false;
            }
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {}
            KeyCode::Enter if key.modifiers.is_empty() => {
                if state.accept_suggestion() == Some(SuggestionKind::Command) {
                    state.phase = UiPhase::SubmitRequested;
                }
                return false;
            }
            KeyCode::Tab => {
                state.accept_suggestion();
                return false;
            }
            _ => {}
        }
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::Up => state.select_adjacent_tool(true),
            KeyCode::Down => state.select_adjacent_tool(false),
            _ => {}
        }
        return false;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        handle_control_key(key.code, state);
        return false;
    }
    if let Some(quit) = handle_navigation_key(key.code, state) {
        return quit;
    }
    if key.code == KeyCode::Tab {
        state.focus = match state.focus {
            Focus::Prompt => Focus::Scrollback,
            Focus::Scrollback => Focus::Prompt,
        };
        return false;
    }
    if state.focus == Focus::Scrollback {
        return handle_scrollback_key(key, state);
    }
    if let Some(handled) = handle_editor_key(key, state) {
        return handled;
    }
    match key.code {
        KeyCode::BackTab => {
            if state.panel_open {
                state.selected_panel = (state.selected_panel + 1) % state.panel_count();
            } else {
                state.panel_open = true;
            }
            false
        }
        _ => false,
    }
}

fn handle_scrollback_key(key: KeyEvent, state: &mut TuiState) -> bool {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => state.select_adjacent_entry(false),
        KeyCode::Char('k') | KeyCode::Up => state.select_adjacent_entry(true),
        KeyCode::Char('g') | KeyCode::Home => state.scroll.goto_top(),
        KeyCode::Char('G') | KeyCode::End => state.scroll.goto_bottom(),
        KeyCode::Char('h') => state.set_tool_details(false),
        KeyCode::Char('l') => state.set_tool_details(true),
        KeyCode::Enter => state.toggle_selected_entry(),
        KeyCode::Char(' ') => state.focus = Focus::Prompt,
        KeyCode::Char(character) if state.active.is_none() => {
            state.focus = Focus::Prompt;
            state.append_input(&character.to_string());
        }
        _ => {}
    }
    false
}

pub(in crate::tui::shell) fn handle_control_key(code: KeyCode, state: &mut TuiState) {
    match code {
        KeyCode::Char('k') => state.scroll.scroll_up(1),
        KeyCode::Char('j') => state.scroll.scroll_down(1),
        KeyCode::Char('u') => state.scroll.scroll_up(state.scroll.half_page_rows()),
        KeyCode::Char('d') => state.scroll.scroll_down(state.scroll.half_page_rows()),
        KeyCode::Char('o') => state.toggle_tool_details(),
        KeyCode::Char('a') => state.move_line_edge(false),
        KeyCode::Char('e') => state.move_line_edge(true),
        KeyCode::Char('w') => state.delete_previous_word(),
        KeyCode::Char('p') if state.active.is_none() => state.previous_history(),
        KeyCode::Char('n') if state.active.is_none() => state.next_history(),
        _ => {}
    }
}

fn handle_navigation_key(code: KeyCode, state: &mut TuiState) -> Option<bool> {
    match code {
        KeyCode::PageUp => state.scroll.scroll_up(state.scroll.page_rows()),
        KeyCode::PageDown => state.scroll.scroll_down(state.scroll.page_rows()),
        KeyCode::Home
            if state.focus == Focus::Prompt
                && state.active.is_none()
                && !state.input.is_empty() =>
        {
            state.move_line_edge(false);
        }
        KeyCode::End
            if state.focus == Focus::Prompt
                && state.active.is_none()
                && !state.input.is_empty() =>
        {
            state.move_line_edge(true);
        }
        KeyCode::Home => state.scroll.goto_top(),
        KeyCode::End => state.scroll.goto_bottom(),
        KeyCode::Esc if state.active.is_some() => {
            cancel_active_turn(state);
        }
        KeyCode::Esc => return Some(true),
        _ => return None,
    }
    Some(false)
}

fn handle_editor_key(key: KeyEvent, state: &mut TuiState) -> Option<bool> {
    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            state.append_input("\n");
        }
        KeyCode::Enter if !state.input.trim().is_empty() => {
            if state.turn_is_running() {
                state.queue_input();
            } else {
                state.phase = UiPhase::SubmitRequested;
            }
        }
        KeyCode::Left => state.move_cursor(-1),
        KeyCode::Right => state.move_cursor(1),
        KeyCode::Up if state.input.contains('\n') => state.move_vertical(true),
        KeyCode::Down if state.input.contains('\n') => state.move_vertical(false),
        KeyCode::Up => state.previous_history(),
        KeyCode::Down => state.next_history(),
        KeyCode::Delete => state.delete_input(),
        KeyCode::Backspace => state.pop_input(),
        KeyCode::Char(character) => state.append_input(&character.to_string()),
        _ => return None,
    }
    Some(false)
}

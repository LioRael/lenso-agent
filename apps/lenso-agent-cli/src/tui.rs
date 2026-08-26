//! Interactive terminal surface for the composed Agent App.

mod markdown;

use std::{io, path::Path, time::Duration};

use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use lenso_agent_loop_module::RunScope;
use lenso_capability_agent::{
    Agent, RUN_TURN_OPERATION, RunTurnError, RunTurnRequest, RunTurnResponse, RunTurnResponseKind,
};
use lenso_capability_agent_tui_contribution::SnapshotResponsePanelsItem;
use lenso_kernel::{NativeStream, StreamEvent};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Clear, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Wrap,
    },
};

use crate::generation::{AgentApp, OnlineGenerationEvent, TurnGeneration};
use markdown::lines as markdown_lines;

const EVENT_TICK: Duration = Duration::from_millis(250);
const MAX_INPUT_CHARACTERS: usize = 262_144;
const PANEL_BREAKPOINT: u16 = 96;
const WHEEL_SCROLL_LINES: usize = 3;
const ACTIVE_TICK: Duration = Duration::from_millis(90);

struct Palette;

impl Palette {
    // Named ANSI colors inherit the terminal theme, preserving hierarchy on
    // both light and dark backgrounds without taking ownership of the canvas.
    const ACCENT: Color = Color::LightMagenta;
    const AGENT: Color = Color::LightCyan;
    const BORDER: Color = Color::DarkGray;
    const ERROR: Color = Color::LightRed;
    const MUTED: Color = Color::Gray;
    const QUIET: Color = Color::DarkGray;
    const CODE: Color = Color::LightYellow;
    const HEADING: Color = Color::LightBlue;
    const SURFACE: Color = Color::Rgb(45, 45, 48);
    const SURFACE_TEXT: Color = Color::White;
}

/// App-owned options that narrow one interactive TUI session.
#[derive(Clone, Debug, Default)]
pub struct TuiOptions {
    pub allowed_tools: Option<Vec<String>>,
    pub session_id: Option<String>,
}

#[derive(Debug)]
struct ActiveTurn {
    // Fields drop in declaration order. Cancel the stream before releasing the
    // App Generation lease that owns its runtime resources.
    stream: NativeStream<Agent>,
    _lease: TurnGeneration,
}

#[derive(Clone, Copy, Debug)]
enum Speaker {
    User,
    Agent,
    System,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiPhase {
    Idle,
    SubmitRequested,
    Active,
    Failed,
}

impl UiPhase {
    const fn activity(self, tick: u64) -> Option<(&'static str, Color)> {
        match self {
            Self::Idle => None,
            Self::SubmitRequested => Some(("◆ Starting turn", Palette::ACCENT)),
            Self::Active => Some((working_label(tick), Palette::AGENT)),
            Self::Failed => Some(("● Turn failed", Palette::ERROR)),
        }
    }
}

const fn working_label(tick: u64) -> &'static str {
    match tick % 4 {
        0 => "✦ Working",
        1 => "✧ Working",
        2 => "· Working",
        _ => "⋅ Working",
    }
}

#[derive(Debug)]
enum TranscriptEntry {
    Message { speaker: Speaker, text: String },
    Tool(ToolCard),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug)]
struct ToolCard {
    call_id: String,
    name: String,
    arguments_json: Option<String>,
    content: Option<String>,
    metadata_json: Option<String>,
    duration_ms: Option<u64>,
    error: Option<String>,
    status: ToolStatus,
    expanded: bool,
}

#[derive(Debug)]
struct ScrollState {
    top: usize,
    max_top: usize,
    viewport_rows: usize,
    follow_tail: bool,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            top: 0,
            max_top: 0,
            viewport_rows: 1,
            follow_tail: true,
        }
    }
}

impl ScrollState {
    fn update_metrics(&mut self, content_rows: usize, viewport_rows: usize) {
        self.viewport_rows = viewport_rows.max(1);
        self.max_top = content_rows.saturating_sub(self.viewport_rows);
        if self.follow_tail {
            self.top = self.max_top;
        } else {
            self.top = self.top.min(self.max_top);
            if self.top == self.max_top {
                self.follow_tail = true;
            }
        }
    }

    fn scroll_up(&mut self, rows: usize) {
        if self.max_top == 0 {
            return;
        }
        self.top = self.top.saturating_sub(rows.max(1));
        self.follow_tail = false;
    }

    fn scroll_down(&mut self, rows: usize) {
        self.top = self.top.saturating_add(rows.max(1)).min(self.max_top);
        self.follow_tail = self.top == self.max_top;
    }

    fn page_rows(&self) -> usize {
        self.viewport_rows.saturating_sub(1).max(1)
    }

    fn half_page_rows(&self) -> usize {
        self.viewport_rows.saturating_div(2).max(1)
    }

    fn goto_top(&mut self) {
        self.top = 0;
        self.follow_tail = self.max_top == 0;
    }

    fn goto_bottom(&mut self) {
        self.top = self.max_top;
        self.follow_tail = true;
    }

    fn rows_below(&self) -> usize {
        self.max_top.saturating_sub(self.top)
    }
}

#[derive(Debug)]
struct TuiState {
    input: String,
    input_characters: usize,
    input_cursor: usize,
    input_history: Vec<String>,
    history_cursor: Option<usize>,
    history_draft: String,
    transcript: Vec<TranscriptEntry>,
    selected_tool: Option<usize>,
    panels: Vec<SnapshotResponsePanelsItem>,
    selected_panel: usize,
    session_id: Option<String>,
    phase: UiPhase,
    active: Option<ActiveTurn>,
    tool_scope: String,
    scroll: ScrollState,
    workspace: String,
    show_shortcuts: bool,
    animation_tick: u64,
}

impl TuiState {
    fn new(options: &TuiOptions, panels: Vec<SnapshotResponsePanelsItem>) -> Self {
        Self {
            input: String::new(),
            input_characters: 0,
            input_cursor: 0,
            input_history: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            transcript: Vec::new(),
            selected_tool: None,
            panels,
            selected_panel: 0,
            session_id: options.session_id.clone(),
            phase: UiPhase::Idle,
            active: None,
            tool_scope: match &options.allowed_tools {
                None => "composed tools".to_owned(),
                Some(tools) if tools.is_empty() => "no tools".to_owned(),
                Some(tools) => format!("{} scoped tools", tools.len()),
            },
            scroll: ScrollState::default(),
            workspace: current_workspace_label(),
            show_shortcuts: false,
            animation_tick: 0,
        }
    }

    fn push_system(&mut self, text: impl Into<String>) {
        self.transcript.push(TranscriptEntry::Message {
            speaker: Speaker::System,
            text: text.into(),
        });
    }

    fn append_agent_text(&mut self, text: &str) {
        if let Some(last) = self.transcript.last_mut()
            && let TranscriptEntry::Message {
                speaker: Speaker::Agent,
                text: existing,
            } = last
        {
            existing.push_str(text);
            return;
        }
        self.transcript.push(TranscriptEntry::Message {
            speaker: Speaker::Agent,
            text: text.to_owned(),
        });
    }

    fn append_input(&mut self, text: &str) {
        let remaining = MAX_INPUT_CHARACTERS.saturating_sub(self.input_characters);
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let accepted: String = normalized.chars().take(remaining).collect();
        let accepted_characters = accepted.chars().count();
        let byte = char_to_byte(&self.input, self.input_cursor);
        self.input.insert_str(byte, &accepted);
        self.input_characters += accepted_characters;
        self.input_cursor += accepted_characters;
        self.leave_history();
    }

    fn pop_input(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let end = char_to_byte(&self.input, self.input_cursor);
        let start = char_to_byte(&self.input, self.input_cursor - 1);
        self.input.replace_range(start..end, "");
        self.input_cursor -= 1;
        self.input_characters -= 1;
        self.leave_history();
    }

    fn delete_input(&mut self) {
        if self.input_cursor >= self.input_characters {
            return;
        }
        let start = char_to_byte(&self.input, self.input_cursor);
        let end = char_to_byte(&self.input, self.input_cursor + 1);
        self.input.replace_range(start..end, "");
        self.input_characters -= 1;
        self.leave_history();
    }

    fn move_cursor(&mut self, delta: isize) {
        self.input_cursor = self
            .input_cursor
            .saturating_add_signed(delta)
            .min(self.input_characters);
    }

    fn move_line_edge(&mut self, end: bool) {
        let chars: Vec<char> = self.input.chars().collect();
        if end {
            self.input_cursor += chars[self.input_cursor..]
                .iter()
                .position(|character| *character == '\n')
                .unwrap_or(chars.len() - self.input_cursor);
        } else {
            self.input_cursor = chars[..self.input_cursor]
                .iter()
                .rposition(|character| *character == '\n')
                .map_or(0, |position| position + 1);
        }
    }

    fn move_vertical(&mut self, up: bool) {
        let chars: Vec<char> = self.input.chars().collect();
        let line_start = chars[..self.input_cursor]
            .iter()
            .rposition(|character| *character == '\n')
            .map_or(0, |position| position + 1);
        let column = self.input_cursor - line_start;
        if up {
            if line_start == 0 {
                return;
            }
            let previous_end = line_start - 1;
            let previous_start = chars[..previous_end]
                .iter()
                .rposition(|character| *character == '\n')
                .map_or(0, |position| position + 1);
            self.input_cursor = previous_start + column.min(previous_end - previous_start);
        } else {
            let Some(next_offset) = chars[self.input_cursor..]
                .iter()
                .position(|character| *character == '\n')
            else {
                return;
            };
            let next_start = self.input_cursor + next_offset + 1;
            let next_end = chars[next_start..]
                .iter()
                .position(|character| *character == '\n')
                .map_or(chars.len(), |offset| next_start + offset);
            self.input_cursor = next_start + column.min(next_end - next_start);
        }
    }

    fn delete_previous_word(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut start = self.input_cursor;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        if start == self.input_cursor {
            return;
        }
        let start_byte = char_to_byte(&self.input, start);
        let end_byte = char_to_byte(&self.input, self.input_cursor);
        self.input.replace_range(start_byte..end_byte, "");
        self.input_characters -= self.input_cursor - start;
        self.input_cursor = start;
        self.leave_history();
    }

    fn previous_history(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let next = match self.history_cursor {
            Some(index) => index.saturating_sub(1),
            None => {
                self.history_draft.clone_from(&self.input);
                self.input_history.len() - 1
            }
        };
        self.history_cursor = Some(next);
        self.set_input(self.input_history[next].clone());
    }

    fn next_history(&mut self) {
        let Some(index) = self.history_cursor else {
            return;
        };
        if index + 1 < self.input_history.len() {
            self.history_cursor = Some(index + 1);
            self.set_input(self.input_history[index + 1].clone());
        } else {
            self.history_cursor = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.set_input(draft);
        }
    }

    fn set_input(&mut self, input: String) {
        self.input_characters = input.chars().count();
        self.input_cursor = self.input_characters;
        self.input = input;
    }

    fn leave_history(&mut self) {
        self.history_cursor = None;
        self.history_draft.clear();
    }

    fn take_input(&mut self) -> String {
        self.input_characters = 0;
        self.input_cursor = 0;
        self.history_cursor = None;
        self.history_draft.clear();
        std::mem::take(&mut self.input)
    }

    fn start_tool(&mut self, message: RunTurnResponse) {
        let Some(call_id) = message.tool_call_id else {
            self.push_system("Ignored a Tool event without a call ID");
            return;
        };
        let Some(name) = message.tool_name else {
            self.push_system("Ignored a Tool event without a name");
            return;
        };
        self.transcript.push(TranscriptEntry::Tool(ToolCard {
            call_id,
            name,
            arguments_json: message.arguments_json.map(|value| value.to_string()),
            content: None,
            metadata_json: None,
            duration_ms: None,
            error: None,
            status: ToolStatus::Running,
            expanded: false,
        }));
        self.selected_tool = Some(self.transcript.len() - 1);
    }

    fn finish_tool(&mut self, message: RunTurnResponse, status: ToolStatus) {
        let Some(call_id) = message.tool_call_id else {
            self.push_system("Ignored a Tool result without a call ID");
            return;
        };
        let index = self.transcript.iter().rposition(
            |entry| matches!(entry, TranscriptEntry::Tool(card) if card.call_id == call_id),
        );
        let Some(index) = index else {
            self.push_system(format!(
                "Ignored a Tool result for unknown call `{call_id}`"
            ));
            return;
        };
        let TranscriptEntry::Tool(card) = &mut self.transcript[index] else {
            unreachable!("Tool lookup returned a message entry")
        };
        card.content = message.content;
        card.metadata_json = message.metadata_json.map(|value| value.to_string());
        card.duration_ms = message
            .duration_ms
            .and_then(|value| value.parse::<u64>().ok());
        card.error = message.error;
        card.status = status;
        self.selected_tool = Some(index);
    }

    fn toggle_tool_details(&mut self) {
        let Some(index) = self.selected_tool else {
            return;
        };
        if let Some(TranscriptEntry::Tool(card)) = self.transcript.get_mut(index) {
            card.expanded = !card.expanded;
        }
    }

    fn select_adjacent_tool(&mut self, previous: bool) {
        let tools = self
            .transcript
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| matches!(entry, TranscriptEntry::Tool(_)).then_some(index))
            .collect::<Vec<_>>();
        if tools.is_empty() {
            self.selected_tool = None;
            return;
        }
        let current = self
            .selected_tool
            .and_then(|selected| tools.iter().position(|index| *index == selected));
        let next = if previous {
            current
                .and_then(|position| position.checked_sub(1))
                .unwrap_or(tools.len() - 1)
        } else {
            current.map_or(0, |position| (position + 1) % tools.len())
        };
        self.selected_tool = Some(tools[next]);
    }
}

fn char_to_byte(text: &str, character: usize) -> usize {
    text.char_indices()
        .nth(character)
        .map_or(text.len(), |(byte, _)| byte)
}

fn current_workspace_label() -> String {
    std::env::current_dir()
        .ok()
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map_or_else(|| "workspace".to_owned(), ToOwned::to_owned)
}

/// Runs the TUI until the user exits, restoring terminal state on every return path.
pub async fn run(app: &AgentApp, options: TuiOptions) -> Result<(), String> {
    let panels = app.tui_panels().await?;
    let mut terminal = TerminalSession::start()?;
    let mut events = EventStream::new();
    let mut state = TuiState::new(&options, panels);

    let result = run_loop(
        app,
        &options,
        &mut terminal.terminal,
        &mut events,
        &mut state,
    )
    .await;
    terminal.restore()?;
    result
}

async fn run_loop(
    app: &AgentApp,
    options: &TuiOptions,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    events: &mut EventStream,
    state: &mut TuiState,
) -> Result<(), String> {
    loop {
        present_online_generation_events(app, state).await;
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
                    if state.active.is_none()
                        && !state.input.trim().is_empty()
                        && state.phase == UiPhase::SubmitRequested
                    {
                        submit(app, options, state).await?;
                    }
                }
                () = tokio::time::sleep(EVENT_TICK) => {
                    state.animation_tick = state.animation_tick.wrapping_add(1);
                }
            }
        }
    }
}

async fn present_online_generation_events(app: &AgentApp, state: &mut TuiState) {
    for event in app.take_online_generation_events() {
        match event {
            OnlineGenerationEvent::Switched {
                generation_spec_digest,
                previous_generation_spec_digest,
                routing_epoch,
                ..
            } => {
                match app.tui_panels().await {
                    Ok(panels) => {
                        state.panels = panels;
                        state.selected_panel = state
                            .selected_panel
                            .min(state.panels.len().saturating_sub(1));
                    }
                    Err(error) => state.transcript.push(TranscriptEntry::Message {
                        speaker: Speaker::Error,
                        text: format!(
                            "App Generation switched, but TUI contributions could not refresh: {error}"
                        ),
                    }),
                }
                state.push_system(format!(
                    "App Generation switched {} → {} at routing epoch {routing_epoch}",
                    short_digest(&previous_generation_spec_digest),
                    short_digest(&generation_spec_digest),
                ));
            }
            OnlineGenerationEvent::Rejected {
                active_set_digest,
                detail,
            } => state.transcript.push(TranscriptEntry::Message {
                speaker: Speaker::Error,
                text: format!(
                    "Plugin update was rejected{}; the current App Generation remains active: {detail}",
                    active_set_digest.as_deref().map_or_else(String::new, |digest| {
                        format!(" ({})", short_digest(digest))
                    })
                ),
            }),
        }
    }
}

fn short_digest(digest: &str) -> &str {
    digest.get(..digest.len().min(15)).unwrap_or(digest)
}

fn handle_terminal_event(
    event: Option<Result<Event, io::Error>>,
    state: &mut TuiState,
) -> Result<bool, String> {
    let Some(event) = event else {
        return Ok(true);
    };
    let event = event.map_err(|error| format!("failed to read terminal input: {error}"))?;
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(handle_key(key, state)),
        Event::Paste(text) if state.active.is_none() && !state.show_shortcuts => {
            state.append_input(&text);
            Ok(false)
        }
        Event::Mouse(mouse) => {
            match mouse.kind {
                MouseEventKind::ScrollUp => state.scroll.scroll_up(WHEEL_SCROLL_LINES),
                MouseEventKind::ScrollDown => state.scroll.scroll_down(WHEEL_SCROLL_LINES),
                _ => {}
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn handle_key(key: KeyEvent, state: &mut TuiState) -> bool {
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
    if let Some(handled) = handle_editor_key(key, state) {
        return handled;
    }
    match key.code {
        KeyCode::Tab if !state.panels.is_empty() => {
            state.selected_panel = (state.selected_panel + 1) % state.panels.len();
            false
        }
        KeyCode::BackTab if !state.panels.is_empty() => {
            state.selected_panel = state
                .selected_panel
                .checked_sub(1)
                .unwrap_or(state.panels.len() - 1);
            false
        }
        _ => false,
    }
}

fn handle_control_key(code: KeyCode, state: &mut TuiState) {
    match code {
        KeyCode::Char('k') => state.scroll.scroll_up(1),
        KeyCode::Char('j') => state.scroll.scroll_down(1),
        KeyCode::Char('u') => state.scroll.scroll_up(state.scroll.half_page_rows()),
        KeyCode::Char('d') => state.scroll.scroll_down(state.scroll.half_page_rows()),
        KeyCode::Char('o') => state.toggle_tool_details(),
        KeyCode::Char('a') if state.active.is_none() => state.move_line_edge(false),
        KeyCode::Char('e') if state.active.is_none() => state.move_line_edge(true),
        KeyCode::Char('w') if state.active.is_none() => state.delete_previous_word(),
        KeyCode::Char('p') if state.active.is_none() => state.previous_history(),
        KeyCode::Char('n') if state.active.is_none() => state.next_history(),
        _ => {}
    }
}

fn handle_navigation_key(code: KeyCode, state: &mut TuiState) -> Option<bool> {
    match code {
        KeyCode::PageUp => state.scroll.scroll_up(state.scroll.page_rows()),
        KeyCode::PageDown => state.scroll.scroll_down(state.scroll.page_rows()),
        KeyCode::Home if state.active.is_none() && !state.input.is_empty() => {
            state.move_line_edge(false);
        }
        KeyCode::End if state.active.is_none() && !state.input.is_empty() => {
            state.move_line_edge(true);
        }
        KeyCode::Home => state.scroll.goto_top(),
        KeyCode::End => state.scroll.goto_bottom(),
        KeyCode::Esc if state.active.take().is_some() => {
            state.push_system("Turn cancelled.");
            state.phase = UiPhase::Idle;
        }
        KeyCode::Esc => return Some(true),
        _ => return None,
    }
    Some(false)
}

fn handle_editor_key(key: KeyEvent, state: &mut TuiState) -> Option<bool> {
    if state.active.is_some() {
        return None;
    }
    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            state.append_input("\n");
        }
        KeyCode::Enter if !state.input.trim().is_empty() => {
            state.phase = UiPhase::SubmitRequested;
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

async fn submit(app: &AgentApp, options: &TuiOptions, state: &mut TuiState) -> Result<(), String> {
    let input = state.take_input();
    if input.chars().count() > MAX_INPUT_CHARACTERS {
        return Err(format!(
            "Agent input exceeds the {MAX_INPUT_CHARACTERS}-character limit"
        ));
    }
    state.transcript.push(TranscriptEntry::Message {
        speaker: Speaker::User,
        text: input.clone(),
    });
    if state.input_history.last() != Some(&input) {
        state.input_history.push(input.clone());
    }
    state.phase = UiPhase::Active;
    state.scroll.goto_bottom();

    let lease = app.lease_tui_turn().await?;
    let mut context = lease.invocation_context()?;
    if let Some(allowed_tools) = options.allowed_tools.clone() {
        context = RunScope::new(allowed_tools)?.attach(context)?;
    }
    let stream = lease
        .handle()
        .open_with_context(
            RUN_TURN_OPERATION,
            context,
            RunTurnRequest {
                input,
                session_id: state.session_id.clone(),
            },
        )
        .await
        .map_err(|error| format!("Agent stream failed to open: {error:?}"))?
        .map_err(|error| format!("Agent rejected the turn: {error:?}"))?;
    stream
        .close_send()
        .await
        .map_err(|error| format!("failed to half-close Agent input: {error:?}"))?;
    state.active = Some(ActiveTurn {
        stream,
        _lease: lease,
    });
    Ok(())
}

fn handle_stream_event(
    event: Result<StreamEvent<RunTurnResponse, RunTurnError>, lenso_kernel::RuntimeFailure>,
    state: &mut TuiState,
) {
    let event = match event {
        Ok(event) => event,
        Err(error) => {
            state.active = None;
            state.transcript.push(TranscriptEntry::Message {
                speaker: Speaker::Error,
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
                RunTurnResponseKind::TextDelta => state.append_agent_text(&message.text),
                RunTurnResponseKind::ToolStarted => state.start_tool(message),
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
            state.active = None;
            state.phase = UiPhase::Idle;
        }
        StreamEvent::Terminal(Err(error)) => {
            state.active = None;
            state.transcript.push(TranscriptEntry::Message {
                speaker: Speaker::Error,
                text: format!("Agent turn failed: {error:?}"),
            });
            state.phase = UiPhase::Failed;
        }
    }
}

fn runtime_failure_message(error: lenso_kernel::RuntimeFailure) -> String {
    match error {
        lenso_kernel::RuntimeFailure::ModuleFailure { detail } => {
            format!("Turn stopped — {detail}")
        }
        error => format!("Turn stopped — {error:?}"),
    }
}

fn render(frame: &mut Frame<'_>, state: &mut TuiState) {
    let area = content_area(frame.area());
    let compact = area.height <= 16;
    let input_width = area.width.saturating_sub(4).max(1);
    let input_rows = visual_input_rows(&state.input, usize::from(input_width));
    let composer_height = if compact {
        3
    } else {
        u16::try_from(input_rows.saturating_add(2))
            .unwrap_or(7)
            .clamp(4, 7)
    };
    let [header, body, activity, composer, shortcuts] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(1),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, header, state);

    if state.panels.is_empty() || body.width < PANEL_BREAKPOINT {
        render_transcript(frame, body, state);
    } else {
        let panel_width = body
            .width
            .saturating_mul(28)
            .saturating_div(100)
            .clamp(26, 36);
        let [chat, _, panel] = Layout::horizontal([
            Constraint::Min(48),
            Constraint::Length(2),
            Constraint::Length(panel_width),
        ])
        .areas(body);
        render_transcript(frame, chat, state);
        render_panel(frame, panel, state);
    }

    render_activity(frame, activity, state);
    render_composer(frame, composer, state);
    render_shortcuts(frame, shortcuts, state);
    if state.show_shortcuts {
        render_shortcuts_overlay(frame, area);
    }
}

fn content_area(area: Rect) -> Rect {
    let horizontal = if area.width >= 64 { 2 } else { 1 };
    let vertical = u16::from(area.height >= 18);
    Rect {
        x: area.x.saturating_add(horizontal),
        y: area.y.saturating_add(vertical),
        width: area.width.saturating_sub(horizontal.saturating_mul(2)),
        height: area.height.saturating_sub(vertical.saturating_mul(2)),
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let session = state.session_id.as_deref().unwrap_or("new session");
    let [workspace_area, session_area] =
        Layout::horizontal([Constraint::Min(18), Constraint::Length(30)]).areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "▣ ",
                Style::default()
                    .fg(Palette::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                state.workspace.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ])),
        workspace_area,
    );
    frame.render_widget(
        Paragraph::new(session)
            .alignment(ratatui::layout::Alignment::Right)
            .style(Style::default().fg(Palette::QUIET)),
        session_area,
    );
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
    let transcript_area = Block::default()
        .padding(Padding::new(1, 1, 1, 0))
        .inner(area);
    let mut lines = Vec::new();
    if state.transcript.is_empty() {
        lines.push(Line::from(Span::styled(
            "Lenso Agent",
            Style::default()
                .fg(Palette::ACCENT)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "What do you want to build?",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "Describe a task, ask about the codebase, or paste an error.",
            Style::default().fg(Palette::MUTED),
        )));
    }
    for (entry_index, entry) in state.transcript.iter().enumerate() {
        match entry {
            TranscriptEntry::Message { speaker, text } => match speaker {
                Speaker::User => {
                    lines.push(Line::default());
                    for (index, content) in text.lines().enumerate() {
                        let prefix = if index == 0 { "› " } else { "  " };
                        lines.push(surface_line(
                            format!("{prefix}{content}"),
                            usize::from(transcript_area.width),
                        ));
                    }
                }
                Speaker::Agent => lines.extend(markdown_lines(text)),
                Speaker::System => lines.push(Line::from(vec![
                    Span::styled("• ", Style::default().fg(Palette::MUTED)),
                    Span::styled(text.clone(), Style::default().fg(Palette::MUTED)),
                ])),
                Speaker::Error => lines.push(Line::from(vec![
                    Span::styled(
                        "● ",
                        Style::default()
                            .fg(Palette::ERROR)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(text.clone(), Style::default().fg(Palette::ERROR)),
                ])),
            },
            TranscriptEntry::Tool(card) => {
                render_tool_card(&mut lines, card, state.selected_tool == Some(entry_index));
            }
        }
        lines.push(Line::default());
    }
    let wrap_width = usize::from(transcript_area.width).max(1);
    let rendered_line_count = lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(wrap_width))
        .sum::<usize>();
    state
        .scroll
        .update_metrics(rendered_line_count, usize::from(transcript_area.height));
    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    let scroll = state.scroll.top.try_into().unwrap_or(u16::MAX);
    frame.render_widget(paragraph.scroll((scroll, 0)), transcript_area);
    if state.scroll.max_top > 0 {
        let mut scrollbar_state = ScrollbarState::new(rendered_line_count)
            .position(state.scroll.top)
            .viewport_content_length(usize::from(transcript_area.height));
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .track_style(Style::default().fg(Palette::QUIET))
                .thumb_symbol("┃")
                .thumb_style(Style::default().fg(Palette::MUTED)),
            transcript_area,
            &mut scrollbar_state,
        );
    }
}

fn render_tool_card(lines: &mut Vec<Line<'static>>, card: &ToolCard, selected: bool) {
    let (symbol, color, status) = match card.status {
        ToolStatus::Running => ("◆", Palette::AGENT, "running"),
        ToolStatus::Completed => ("✓", Color::LightGreen, "done"),
        ToolStatus::Failed => ("×", Palette::ERROR, "failed"),
    };
    let disclosure = if card.expanded { "▾" } else { "▸" };
    let mut header = vec![
        Span::styled(
            format!("{disclosure} {symbol} "),
            Style::default().fg(if selected { Palette::ACCENT } else { color }),
        ),
        Span::styled(
            card.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(summary) = tool_summary(card) {
        header.push(Span::styled(
            format!("  {summary}"),
            Style::default().fg(Palette::MUTED),
        ));
    }
    header.push(Span::styled(
        format!("  {status}"),
        Style::default().fg(color),
    ));
    if let Some(duration_ms) = card.duration_ms {
        header.push(Span::styled(
            format!("  {}", format_duration(duration_ms)),
            Style::default().fg(Palette::QUIET),
        ));
    }
    if selected {
        header.push(Span::styled(
            "  ctrl+o details",
            Style::default().fg(Palette::QUIET),
        ));
    }
    lines.push(Line::from(header));

    if !card.expanded {
        return;
    }
    if let Some(arguments) = card.arguments_json.as_deref() {
        push_tool_detail(lines, "arguments", arguments, true);
    }
    if let Some(content) = card.content.as_deref()
        && !content.is_empty()
    {
        push_tool_detail(lines, "output", content, false);
    }
    if let Some(metadata) = card.metadata_json.as_deref() {
        push_tool_detail(lines, "metadata", metadata, true);
    }
    if let Some(error) = card.error.as_deref() {
        push_tool_detail(lines, "error", error, false);
    }
}

fn push_tool_detail(
    lines: &mut Vec<Line<'static>>,
    label: &'static str,
    value: &str,
    pretty_json: bool,
) {
    lines.push(Line::from(vec![
        Span::styled("  │ ", Style::default().fg(Palette::BORDER)),
        Span::styled(label, Style::default().fg(Palette::QUIET)),
    ]));
    let value = if pretty_json {
        serde_json::from_str::<serde_json::Value>(value)
            .ok()
            .and_then(|value| serde_json::to_string_pretty(&value).ok())
            .unwrap_or_else(|| value.to_owned())
    } else {
        value.to_owned()
    };
    for line in bounded_preview(&value, 24, 4096).lines() {
        lines.push(Line::from(vec![
            Span::styled("  │   ", Style::default().fg(Palette::BORDER)),
            Span::styled(line.to_owned(), Style::default().fg(Palette::CODE)),
        ]));
    }
}

fn bounded_preview(value: &str, max_lines: usize, max_characters: usize) -> String {
    let mut preview = String::new();
    let mut characters = 0;
    let mut truncated = false;
    for (index, line) in value.lines().enumerate() {
        if index >= max_lines || characters >= max_characters {
            truncated = true;
            break;
        }
        if index > 0 {
            preview.push('\n');
            characters += 1;
        }
        let remaining = max_characters.saturating_sub(characters);
        let accepted = line.chars().take(remaining).collect::<String>();
        characters += accepted.chars().count();
        preview.push_str(&accepted);
        if accepted.chars().count() < line.chars().count() {
            truncated = true;
            break;
        }
    }
    if truncated {
        preview.push_str("\n… output truncated in TUI");
    }
    preview
}

fn tool_summary(card: &ToolCard) -> Option<String> {
    let metadata = card
        .metadata_json
        .as_deref()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok());
    if let Some(metadata) = metadata.as_ref() {
        if let (Some(operation), Some(path)) = (
            metadata
                .get("operation")
                .and_then(serde_json::Value::as_str),
            metadata.get("path").and_then(serde_json::Value::as_str),
        ) {
            let bytes = metadata
                .get("bytes_written")
                .and_then(serde_json::Value::as_u64)
                .map(|bytes| format!(" · {bytes} B"))
                .unwrap_or_default();
            return Some(format!("{operation} {path}{bytes}"));
        }
        if let Some(path) = metadata.get("path").and_then(serde_json::Value::as_str) {
            return Some(path.to_owned());
        }
        if let Some(program) = metadata.get("program").and_then(serde_json::Value::as_str) {
            let exit = metadata
                .get("exit_code")
                .and_then(serde_json::Value::as_str)
                .map(|code| format!(" · exit {code}"))
                .unwrap_or_default();
            return Some(format!("{program}{exit}"));
        }
    }
    card.arguments_json
        .as_deref()
        .and_then(|arguments| serde_json::from_str::<serde_json::Value>(arguments).ok())
        .and_then(|arguments| {
            arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn format_duration(duration_ms: u64) -> String {
    if duration_ms < 1_000 {
        format!("{duration_ms}ms")
    } else {
        format!(
            "{}.{:01}s",
            duration_ms / 1_000,
            (duration_ms % 1_000) / 100
        )
    }
}

fn surface_line(text: String, width: usize) -> Line<'static> {
    let content_width = Line::from(text.as_str()).width();
    let padding = " ".repeat(width.saturating_sub(content_width));
    Line::from(vec![
        Span::styled(
            text,
            Style::default()
                .fg(Palette::SURFACE_TEXT)
                .bg(Palette::SURFACE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            padding,
            Style::default()
                .fg(Palette::SURFACE_TEXT)
                .bg(Palette::SURFACE),
        ),
    ])
}

fn render_panel(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let panel = &state.panels[state.selected_panel];
    let title = if state.panels.len() > 1 {
        format!(
            " {} · {}/{} ",
            panel.title,
            state.selected_panel + 1,
            state.panels.len()
        )
    } else {
        format!(" {} ", panel.title)
    };
    frame.render_widget(
        Paragraph::new(panel.body.as_str())
            .style(Style::default().fg(Palette::MUTED))
            .block(
                Block::default()
                    .title(Span::styled(title, Style::default().fg(Palette::MUTED)))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Palette::BORDER))
                    .padding(Padding::horizontal(1)),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_activity(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let history = (!state.scroll.follow_tail).then(|| {
        format!(
            "↑ reading history · {} lines below · End follow",
            state.scroll.rows_below()
        )
    });
    let history_width = history.as_ref().map_or(0, |label| {
        u16::try_from(label.chars().count()).unwrap_or(u16::MAX)
    });
    let [phase_area, history_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(history_width.min(area.width)),
    ])
    .areas(area);

    if let Some((label, color)) = state.phase.activity(state.animation_tick) {
        let suffix = if state.phase == UiPhase::Active {
            "  Esc cancel"
        } else {
            ""
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(label, Style::default().fg(color)),
                Span::styled(suffix, Style::default().fg(Palette::QUIET)),
            ])),
            phase_area,
        );
    }
    if let Some(history) = history {
        frame.render_widget(
            Paragraph::new(history)
                .alignment(ratatui::layout::Alignment::Right)
                .style(Style::default().fg(Palette::MUTED)),
            history_area,
        );
    }
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let focused = state.active.is_none();
    let border = if focused {
        Palette::ACCENT
    } else {
        Palette::BORDER
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title_bottom(Span::styled(
            format!(" {} · normal ", state.tool_scope),
            Style::default().fg(Palette::QUIET),
        ))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let input = if state.input.is_empty() {
        vec![Line::from(vec![
            Span::styled("❯ ", Style::default().fg(border)),
            Span::styled("Message Lenso Agent…", Style::default().fg(Palette::QUIET)),
        ])]
    } else {
        state
            .input
            .split('\n')
            .enumerate()
            .map(|(index, line)| {
                Line::from(vec![
                    Span::styled(
                        if index == 0 { "❯ " } else { "  " },
                        Style::default().fg(border),
                    ),
                    Span::raw(line.to_owned()),
                ])
            })
            .collect::<Vec<_>>()
    };
    let cursor = composer_cursor(&state.input, state.input_cursor, usize::from(inner.width));
    let total_rows = visual_input_rows(&state.input, usize::from(inner.width));
    let hidden_rows = total_rows.saturating_sub(usize::from(inner.height));
    frame.render_widget(
        Paragraph::new(Text::from(input)).scroll((hidden_rows.try_into().unwrap_or(u16::MAX), 0)),
        inner,
    );

    if focused {
        let cursor_x = inner
            .x
            .saturating_add(u16::try_from(cursor.0).unwrap_or(u16::MAX))
            .min(inner.right().saturating_sub(1));
        let cursor_y = inner
            .y
            .saturating_add(u16::try_from(cursor.1.saturating_sub(hidden_rows)).unwrap_or(u16::MAX))
            .min(inner.bottom().saturating_sub(1));
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn visual_input_rows(input: &str, width: usize) -> usize {
    let width = width.max(1);
    input
        .split('\n')
        .map(|line| (2 + Line::from(line).width()).max(1).div_ceil(width))
        .sum::<usize>()
        .max(1)
}

fn composer_cursor(input: &str, cursor: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let before: String = input.chars().take(cursor).collect();
    let mut row = 0;
    let mut lines = before.split('\n').peekable();
    while let Some(line) = lines.next() {
        let position = 2 + Line::from(line).width();
        if lines.peek().is_some() {
            row += position.max(1).div_ceil(width);
        } else {
            row += position / width;
            return (position % width, row);
        }
    }
    (2, 0)
}

fn render_shortcuts(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let mut spans = Vec::new();
    if state.active.is_none() {
        spans.extend(shortcut("enter", "send"));
        if area.width >= 50 {
            spans.push(Span::raw("   "));
            spans.extend(shortcut("shift+enter", "newline"));
        }
        spans.push(Span::raw("   "));
    }
    spans.extend(shortcut(
        "esc",
        if state.active.is_some() {
            "cancel"
        } else {
            "quit"
        },
    ));
    if !state.panels.is_empty() && area.width >= PANEL_BREAKPOINT {
        spans.push(Span::raw("   "));
        spans.extend(shortcut("tab", "panels"));
    }
    if state.scroll.max_top > 0 && area.width >= 58 {
        spans.push(Span::raw("   "));
        if state.scroll.follow_tail {
            spans.extend(shortcut("pgup/pgdn", "scroll"));
        } else {
            spans.extend(shortcut("end", "follow"));
        }
    }
    if state.selected_tool.is_some() && area.width >= 68 {
        spans.push(Span::raw("   "));
        spans.extend(shortcut("ctrl+o", "details"));
    }
    if area.width >= 72 {
        spans.push(Span::raw("   "));
        spans.extend(shortcut("ctrl+.", "shortcuts"));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_shortcuts_overlay(frame: &mut Frame<'_>, area: Rect) {
    let overlay = centered_rect(area, 68.min(area.width.saturating_sub(2)), 16);
    frame.render_widget(Clear, overlay);
    let block = Block::default()
        .title(Span::styled(
            " Keyboard shortcuts ",
            Style::default()
                .fg(Palette::ACCENT)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Palette::BORDER))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);
    let rows = [
        ("Enter", "Send prompt"),
        ("Shift+Enter", "Insert newline"),
        ("← / →", "Move input cursor"),
        ("↑ / ↓", "Move by line or browse prompt history"),
        ("Ctrl+W", "Delete previous word"),
        ("Ctrl+O", "Expand or collapse the selected Tool card"),
        ("Alt+↑ / Alt+↓", "Select a previous or next Tool card"),
        ("PgUp / PgDn", "Scroll conversation"),
        ("End", "Return to the latest message"),
        ("Tab / Shift+Tab", "Switch composed panels"),
        ("Esc", "Cancel turn or close this panel"),
        ("Ctrl+C", "Quit immediately"),
    ];
    let lines = rows.into_iter().map(|(key, label)| {
        Line::from(vec![
            Span::styled(
                format!("{key:<18}"),
                Style::default()
                    .fg(Palette::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(label),
        ])
    });
    frame.render_widget(Paragraph::new(Text::from_iter(lines)), inner);
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn shortcut(key: &'static str, label: &'static str) -> [Span<'static>; 2] {
    [
        Span::styled(
            key,
            Style::default()
                .fg(Palette::MUTED)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {label}"), Style::default().fg(Palette::QUIET)),
    ]
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    restored: bool,
}

impl TerminalSession {
    fn start() -> Result<Self, String> {
        enable_raw_mode()
            .map_err(|error| format!("failed to enable terminal raw mode: {error}"))?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(format!("failed to enter alternate screen: {error}"));
        }
        if let Err(error) = execute!(stdout, EnableMouseCapture) {
            let _ = execute!(stdout, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(format!("failed to enable terminal mouse capture: {error}"));
        }
        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = disable_raw_mode();
                let mut stdout = io::stdout();
                let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
                return Err(format!("failed to initialize terminal: {error}"));
            }
        };
        Ok(Self {
            terminal,
            restored: false,
        })
    }

    fn restore(&mut self) -> Result<(), String> {
        if self.restored {
            return Ok(());
        }
        let raw_mode = disable_raw_mode();
        let mouse = execute!(self.terminal.backend_mut(), DisableMouseCapture);
        let alternate_screen = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let cursor = self.terminal.show_cursor();
        self.restored = true;

        raw_mode.map_err(|error| format!("failed to disable terminal raw mode: {error}"))?;
        mouse.map_err(|error| format!("failed to disable terminal mouse capture: {error}"))?;
        alternate_screen.map_err(|error| format!("failed to leave alternate screen: {error}"))?;
        cursor.map_err(|error| format!("failed to restore terminal cursor: {error}"))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if !self.restored {
            let _ = disable_raw_mode();
            let _ = execute!(self.terminal.backend_mut(), DisableMouseCapture);
            let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
            let _ = self.terminal.show_cursor();
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::MouseEvent;
    use ratatui::backend::TestBackend;

    use super::*;

    #[test]
    fn renders_composed_panel_and_input() {
        let backend = TestBackend::new(100, 24);
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
        assert!(content.contains("Lenso Agent"));
        assert!(content.contains("What do you want to build?"));
        assert!(content.contains("Help"));
        assert!(content.contains("hello"));
        assert!(content.contains("enter send"));
        assert!(!content.contains("Conversation"));
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
        assert!(content.contains("What do you want to build?"));
        assert!(content.contains("Message Lenso Agent…"));
        assert!(!content.contains("Esc quits"));
        assert!(!content.contains("tab panels"));
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
        assert!(content.contains("enter send"));
    }

    #[test]
    fn conversation_visually_separates_user_and_markdown_agent_content() {
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
        state.transcript = vec![
            TranscriptEntry::Message {
                speaker: Speaker::User,
                text: "Summarize it".to_owned(),
            },
            TranscriptEntry::Message {
                speaker: Speaker::Agent,
                text: "## Result\n- **Done**".to_owned(),
            },
        ];

        terminal.draw(|frame| render(frame, &mut state)).unwrap();
        let content = terminal.backend().to_string();
        assert!(content.contains("› Summarize it"));
        assert!(content.contains("Result"));
        assert!(content.contains("• Done"));
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
        assert!(collapsed.contains("edit"));
        assert!(collapsed.contains("edited src/lib.rs · 42 B"));
        assert!(collapsed.contains("done  12ms"));
        assert!(!collapsed.contains("old_text"));

        handle_control_key(KeyCode::Char('o'), &mut state);
        terminal.draw(|frame| render(frame, &mut state)).unwrap();
        let expanded = terminal.backend().to_string();
        assert!(expanded.contains("arguments"));
        assert!(expanded.contains("old_text"));
        assert!(expanded.contains("metadata"));
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
    fn markdown_distinguishes_headings_lists_code_and_emphasis() {
        let lines =
            markdown_lines("## Result\n- **done** with `cargo test`\n```rust\nfn main() {}\n```");
        let text = Text::from(lines);
        assert_eq!(text.lines[0].spans[0].content, "Result");
        assert_eq!(text.lines[1].spans[0].content, "• ");
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
        assert_eq!(text.lines[2].spans[0].content, "╭─ rust");
        assert_eq!(text.lines[3].spans[1].content, "fn main() {}");
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
    fn runtime_failure_stays_inline_and_keeps_the_tui_available() {
        let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
        handle_stream_event(
            Err(lenso_kernel::RuntimeFailure::ModuleFailure {
                detail: "fixture failure".to_owned(),
            }),
            &mut state,
        );
        assert_eq!(state.phase, UiPhase::Failed);
        assert!(state.active.is_none());
        assert!(matches!(
            state.transcript.last(),
            Some(TranscriptEntry::Message {
                speaker: Speaker::Error,
                text,
            }) if text.contains("fixture failure")
        ));
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
    fn rendered_history_exposes_scroll_position_and_follow_control() {
        let backend = TestBackend::new(80, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
        for index in 0..24 {
            state.transcript.push(TranscriptEntry::Message {
                speaker: Speaker::System,
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
        assert!(content.contains("reading history"));
        assert!(content.contains("End follow"));
        assert!(content.contains("end follow"));
    }
}

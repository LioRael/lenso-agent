//! Interactive terminal surface for the composed Agent App.

use std::{io, time::Duration};

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
    Agent, RUN_TURN_OPERATION, RunTurnError, RunTurnRequest, RunTurnResponse,
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
        Block, BorderType, Borders, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Wrap,
    },
};

use crate::generation::{AgentApp, TurnGeneration};

const EVENT_TICK: Duration = Duration::from_millis(250);
const MAX_INPUT_CHARACTERS: usize = 262_144;
const PANEL_BREAKPOINT: u16 = 96;
const WHEEL_SCROLL_LINES: usize = 3;

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
    const fn activity(self) -> Option<(&'static str, Color)> {
        match self {
            Self::Idle => None,
            Self::SubmitRequested => Some(("◆ Starting turn", Palette::ACCENT)),
            Self::Active => Some(("◆ Working", Palette::AGENT)),
            Self::Failed => Some(("● Turn failed", Palette::ERROR)),
        }
    }
}

#[derive(Debug)]
struct TranscriptEntry {
    speaker: Speaker,
    text: String,
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
    transcript: Vec<TranscriptEntry>,
    panels: Vec<SnapshotResponsePanelsItem>,
    selected_panel: usize,
    session_id: Option<String>,
    phase: UiPhase,
    active: Option<ActiveTurn>,
    tool_scope: String,
    scroll: ScrollState,
}

impl TuiState {
    fn new(options: &TuiOptions, panels: Vec<SnapshotResponsePanelsItem>) -> Self {
        Self {
            input: String::new(),
            input_characters: 0,
            transcript: Vec::new(),
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
        }
    }

    fn push_system(&mut self, text: impl Into<String>) {
        self.transcript.push(TranscriptEntry {
            speaker: Speaker::System,
            text: text.into(),
        });
    }

    fn append_agent_text(&mut self, text: &str) {
        if let Some(last) = self.transcript.last_mut()
            && matches!(last.speaker, Speaker::Agent)
        {
            last.text.push_str(text);
            return;
        }
        self.transcript.push(TranscriptEntry {
            speaker: Speaker::Agent,
            text: text.to_owned(),
        });
    }

    fn append_input(&mut self, text: &str) {
        let remaining = MAX_INPUT_CHARACTERS.saturating_sub(self.input_characters);
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let accepted: String = normalized.chars().take(remaining).collect();
        self.input_characters += accepted.chars().count();
        self.input.push_str(&accepted);
    }

    fn pop_input(&mut self) {
        if self.input.pop().is_some() {
            self.input_characters = self.input_characters.saturating_sub(1);
        }
    }

    fn take_input(&mut self) -> String {
        self.input_characters = 0;
        std::mem::take(&mut self.input)
    }
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
                () = tokio::time::sleep(EVENT_TICK) => {}
            }
        }
    }
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
        Event::Paste(text) if state.active.is_none() => {
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
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('k') => state.scroll.scroll_up(1),
            KeyCode::Char('j') => state.scroll.scroll_down(1),
            KeyCode::Char('u') => state.scroll.scroll_up(state.scroll.half_page_rows()),
            KeyCode::Char('d') => state.scroll.scroll_down(state.scroll.half_page_rows()),
            _ => return false,
        }
        return false;
    }
    match key.code {
        KeyCode::PageUp => {
            state.scroll.scroll_up(state.scroll.page_rows());
            false
        }
        KeyCode::PageDown => {
            state.scroll.scroll_down(state.scroll.page_rows());
            false
        }
        KeyCode::Home => {
            state.scroll.goto_top();
            false
        }
        KeyCode::End => {
            state.scroll.goto_bottom();
            false
        }
        KeyCode::Esc => {
            if state.active.take().is_some() {
                state.push_system("Turn cancelled.");
                state.phase = UiPhase::Idle;
                false
            } else {
                true
            }
        }
        KeyCode::Enter
            if state.active.is_none()
                && key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            state.append_input("\n");
            false
        }
        KeyCode::Enter if state.active.is_none() && !state.input.trim().is_empty() => {
            state.phase = UiPhase::SubmitRequested;
            false
        }
        KeyCode::Backspace if state.active.is_none() => {
            state.pop_input();
            false
        }
        KeyCode::Char(character) if state.active.is_none() => {
            state.append_input(&character.to_string());
            false
        }
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

async fn submit(app: &AgentApp, options: &TuiOptions, state: &mut TuiState) -> Result<(), String> {
    let input = state.take_input();
    if input.chars().count() > MAX_INPUT_CHARACTERS {
        return Err(format!(
            "Agent input exceeds the {MAX_INPUT_CHARACTERS}-character limit"
        ));
    }
    state.transcript.push(TranscriptEntry {
        speaker: Speaker::User,
        text: input.clone(),
    });
    state.transcript.push(TranscriptEntry {
        speaker: Speaker::Agent,
        text: String::new(),
    });
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
            state.transcript.push(TranscriptEntry {
                speaker: Speaker::Error,
                text: runtime_failure_message(error),
            });
            state.phase = UiPhase::Failed;
            return;
        }
    };
    match event {
        StreamEvent::Message(message) => {
            state.session_id = message.session_id.or_else(|| state.session_id.clone());
            state.append_agent_text(&message.text);
        }
        StreamEvent::PeerHalfClosed => {}
        StreamEvent::Terminal(Ok(())) => {
            state.active = None;
            state.phase = UiPhase::Idle;
        }
        StreamEvent::Terminal(Err(error)) => {
            state.active = None;
            state.transcript.push(TranscriptEntry {
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
    let input_rows = state.input.split('\n').count();
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
    let [identity, session_area] =
        Layout::horizontal([Constraint::Min(18), Constraint::Length(28)]).areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "LENSO",
                Style::default()
                    .fg(Palette::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  agent", Style::default().fg(Palette::MUTED)),
        ])),
        identity,
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
            "What do you want to work on?",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "Your Agent, tools, and UI are selected by this App Composition.",
            Style::default().fg(Palette::MUTED),
        )));
    }
    for entry in &state.transcript {
        let (prefix, color, modifier) = match entry.speaker {
            Speaker::User => ("❯ ", Palette::ACCENT, Modifier::BOLD),
            Speaker::Agent => ("◆ ", Palette::AGENT, Modifier::empty()),
            Speaker::System => ("• ", Palette::MUTED, Modifier::empty()),
            Speaker::Error => ("● ", Palette::ERROR, Modifier::BOLD),
        };
        let mut content = entry.text.lines();
        let first = content.next().unwrap_or_else(|| {
            if matches!(entry.speaker, Speaker::Agent) && state.phase == UiPhase::Active {
                "Working…"
            } else {
                ""
            }
        });
        lines.push(Line::from(vec![
            Span::styled(prefix, Style::default().fg(color).add_modifier(modifier)),
            Span::raw(first.to_owned()),
        ]));
        lines.extend(content.map(|line| Line::from(format!("  {line}"))));
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

    if let Some((label, color)) = state.phase.activity() {
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
            format!(" {} · ask ", state.tool_scope),
            Style::default().fg(Palette::QUIET),
        ))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let input = if state.input.is_empty() {
        vec![Line::from(vec![
            Span::styled("❯ ", Style::default().fg(border)),
            Span::styled("Ask Lenso Agent…", Style::default().fg(Palette::QUIET)),
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
    let hidden_rows = input.len().saturating_sub(usize::from(inner.height));
    frame.render_widget(
        Paragraph::new(Text::from(input)).scroll((hidden_rows.try_into().unwrap_or(u16::MAX), 0)),
        inner,
    );

    if focused {
        let last_line = state.input.rsplit('\n').next().unwrap_or_default();
        let cursor_x = inner
            .x
            .saturating_add(2)
            .saturating_add(u16::try_from(Line::from(last_line).width()).unwrap_or(u16::MAX))
            .min(inner.right().saturating_sub(1));
        let cursor_y = inner
            .y
            .saturating_add(
                u16::try_from(
                    state
                        .input
                        .split('\n')
                        .count()
                        .saturating_sub(1 + hidden_rows),
                )
                .unwrap_or(u16::MAX),
            )
            .min(inner.bottom().saturating_sub(1));
        frame.set_cursor_position((cursor_x, cursor_y));
    }
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
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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
        state.input = "hello".to_owned();
        terminal.draw(|frame| render(frame, &mut state)).unwrap();
        let content = terminal.backend().to_string();
        assert!(content.contains("LENSO  agent"));
        assert!(content.contains("What do you want to work on?"));
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
        assert!(content.contains("What do you want to work on?"));
        assert!(content.contains("Ask Lenso Agent…"));
        assert!(!content.contains("Esc quits"));
        assert!(!content.contains("tab panels"));
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
            Some(TranscriptEntry {
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
            state.transcript.push(TranscriptEntry {
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

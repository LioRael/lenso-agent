//! Interactive terminal surface for the composed Agent App.

use std::{io, time::Duration};

use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
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
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::generation::{AgentApp, TurnGeneration};

const EVENT_TICK: Duration = Duration::from_millis(250);
const MAX_INPUT_CHARACTERS: usize = 262_144;

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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiPhase {
    Idle,
    SubmitRequested,
    Active,
    Failed,
}

impl UiPhase {
    const fn status(self) -> &'static str {
        match self {
            Self::Idle => "idle · Esc quits · Tab changes panel",
            Self::SubmitRequested => "submitting",
            Self::Active => "thinking · Esc cancels",
            Self::Failed => "turn failed · Esc quits",
        }
    }
}

#[derive(Debug)]
struct TranscriptEntry {
    speaker: Speaker,
    text: String,
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
}

impl TuiState {
    fn new(options: &TuiOptions, panels: Vec<SnapshotResponsePanelsItem>) -> Self {
        Self {
            input: String::new(),
            input_characters: 0,
            transcript: vec![TranscriptEntry {
                speaker: Speaker::System,
                text: "Ready. Type a message and press Enter.".to_owned(),
            }],
            panels,
            selected_panel: 0,
            session_id: options.session_id.clone(),
            phase: UiPhase::Idle,
            active: None,
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
        let sanitized = text.replace(['\r', '\n'], " ");
        let accepted: String = sanitized.chars().take(remaining).collect();
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
                    handle_stream_event(stream_event, state)?;
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
        _ => Ok(false),
    }
}

fn handle_key(key: KeyEvent, state: &mut TuiState) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        state.active = None;
        return true;
    }
    match key.code {
        KeyCode::Esc => {
            if state.active.take().is_some() {
                state.push_system("Turn cancelled.");
                state.phase = UiPhase::Idle;
                false
            } else {
                true
            }
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
) -> Result<(), String> {
    match event.map_err(|error| format!("Agent stream failed: {error:?}"))? {
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
            state.push_system(format!("Agent turn failed: {error:?}"));
            state.phase = UiPhase::Failed;
        }
    }
    Ok(())
}

fn render(frame: &mut Frame<'_>, state: &TuiState) {
    let [header, body, input, status] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " Lenso Agent ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                state.session_id.as_deref().unwrap_or("new session"),
                Style::default().fg(Color::DarkGray),
            ),
        ]))
        .block(Block::default().borders(Borders::ALL)),
        header,
    );

    if state.panels.is_empty() || body.width < 80 {
        render_transcript(frame, body, state);
    } else {
        let [chat, panel] =
            Layout::horizontal([Constraint::Percentage(72), Constraint::Percentage(28)])
                .areas(body);
        render_transcript(frame, chat, state);
        render_panel(frame, panel, state);
    }

    frame.render_widget(
        Paragraph::new(state.input.as_str())
            .block(Block::default().title(" Message ").borders(Borders::ALL)),
        input,
    );
    if state.active.is_none() {
        let cursor_x = input
            .x
            .saturating_add(1)
            .saturating_add(u16::try_from(state.input.chars().count()).unwrap_or(u16::MAX))
            .min(input.right().saturating_sub(2));
        frame.set_cursor_position((cursor_x, input.y.saturating_add(1)));
    }
    frame.render_widget(
        Paragraph::new(state.phase.status()).style(Style::default().fg(Color::DarkGray)),
        status,
    );
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let mut lines = Vec::new();
    for entry in &state.transcript {
        let (label, color) = match entry.speaker {
            Speaker::User => ("You", Color::Cyan),
            Speaker::Agent => ("Agent", Color::Green),
            Speaker::System => ("System", Color::DarkGray),
        };
        lines.push(Line::from(Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )));
        lines.extend(entry.text.lines().map(|line| Line::from(line.to_owned())));
        lines.push(Line::default());
    }
    let visible_height = usize::from(area.height.saturating_sub(2));
    let scroll = lines
        .len()
        .saturating_sub(visible_height)
        .try_into()
        .unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(" Conversation ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
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
            .block(Block::default().title(title).borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
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
        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = disable_raw_mode();
                let mut stdout = io::stdout();
                let _ = execute!(stdout, LeaveAlternateScreen);
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
        let alternate_screen = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let cursor = self.terminal.show_cursor();
        self.restored = true;

        raw_mode.map_err(|error| format!("failed to disable terminal raw mode: {error}"))?;
        alternate_screen.map_err(|error| format!("failed to leave alternate screen: {error}"))?;
        cursor.map_err(|error| format!("failed to restore terminal cursor: {error}"))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if !self.restored {
            let _ = disable_raw_mode();
            let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
            let _ = self.terminal.show_cursor();
        }
    }
}

#[cfg(test)]
mod tests {
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
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let content = terminal.backend().to_string();
        assert!(content.contains("Lenso Agent"));
        assert!(content.contains("Conversation"));
        assert!(content.contains("Help"));
        assert!(content.contains("hello"));
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
}

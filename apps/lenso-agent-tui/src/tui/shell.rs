//! Composition root for terminal setup, App snapshots, and the volatile UI loop.

use std::{collections::VecDeque, io, path::Path, process::Command, time::Duration};

use super::{Palette, blocks, markdown};
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use lenso_agent_loop_plugin::RunScope;
use lenso_capability_agent::{
    Agent, RUN_TURN_OPERATION, RunTurnError, RunTurnRequest, RunTurnResponse, RunTurnResponseKind,
};
use lenso_capability_agent_context_source::{
    ContextRole, ReadResourceRequest, RenderPromptRequest,
};
use lenso_capability_agent_tui_contribution::SnapshotResponsePanelsItem;
use lenso_capability_agent_tui_suggestion::{
    Suggestion, SuggestionKind, validate_snapshot_suggestions,
};
use lenso_capability_agent_user_interaction::{
    InteractionAnswer, InteractionQuestion, PendingInteraction,
};
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
use time::OffsetDateTime;

use super::blocks::{
    ThinkingCard, ToolCard, ToolStatus, render_grouped_tool_block, render_thinking_block,
    render_tool_block, render_tool_group,
};
#[cfg(test)]
use super::markdown::lines as markdown_lines;
use super::markdown::lines_with_width as markdown_lines_with_width;
use lenso_agent_host::generation::{AgentApp, OnlineGenerationEvent, TurnGeneration};

const EVENT_TICK: Duration = Duration::from_millis(250);
const MAX_INPUT_CHARACTERS: usize = 262_144;
const PANEL_BREAKPOINT: u16 = 96;
const WHEEL_SCROLL_LINES: usize = 3;
const ACTIVE_TICK: Duration = Duration::from_millis(90);
const MAX_VISIBLE_SUGGESTIONS: usize = 6;
const MAX_VISIBLE_QUEUE_ROWS: usize = 3;
const MAX_VISIBLE_QUEUE_HEIGHT: u16 = 3;
const COLLAPSED_USER_ROWS: usize = 3;

/// App-owned options that narrow one interactive TUI session.
#[derive(Clone, Debug, Default)]
pub struct TuiOptions {
    pub allowed_tools: Option<Vec<String>>,
    pub profile: Option<String>,
    pub session_id: Option<String>,
}

/// Runs the TUI until the user exits, restoring terminal state on every return path.
pub async fn run(app: &AgentApp, options: TuiOptions) -> Result<(), String> {
    let panels = app.tui_panels().await?;
    let mut suggestions = app.tui_suggestions().await?;
    suggestions.extend(context_source_suggestions(app).await?);
    validate_snapshot_suggestions(&suggestions)?;
    let mut terminal = TerminalSession::start()?;
    let mut events = EventStream::new();
    let result = run_loop(
        app,
        &options,
        &mut terminal.terminal,
        &mut events,
        panels,
        suggestions,
    )
    .await;
    terminal.restore()?;
    result
}

mod context_suggestions;
use context_suggestions::context_source_suggestions;
mod state;
use state::run_loop;
mod terminal;
use terminal::TerminalSession;

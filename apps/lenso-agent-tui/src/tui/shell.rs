//! Composition root for terminal setup, App snapshots, and the volatile UI loop.

use std::{collections::VecDeque, io, path::Path, process::Command, rc::Rc, time::Duration};

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
use lenso_capability_agent_user_interaction::{
    InteractionAnswer, InteractionQuestion, PendingInteraction,
};
use lenso_capability_terminal_command::{
    CommandDefinition, CommandExecute, ExecuteError, ExecuteMessage, ExecuteOpen, OutputKind,
};
use lenso_capability_tui_panel::PanelItem;
use lenso_capability_tui_suggestion::{
    SuggestionItem as Suggestion, SuggestionKind, validate_snapshot_suggestions,
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
use lenso_agent_host::generation::{
    AgentApp, OnlineGenerationEvent, TerminalGeneration, TurnGeneration,
};
use lenso_terminal_cli_surface::{ParseOutcome, parse_line as parse_terminal_line};

const EVENT_TICK: Duration = Duration::from_millis(250);
const MAX_INPUT_CHARACTERS: usize = 262_144;
const PANEL_BREAKPOINT: u16 = 96;
const WHEEL_SCROLL_LINES: usize = 3;
const ACTIVE_TICK: Duration = Duration::from_millis(90);
const MAX_VISIBLE_SUGGESTIONS: usize = 6;
const MAX_VISIBLE_QUEUE_ROWS: usize = 3;
const MAX_VISIBLE_QUEUE_HEIGHT: u16 = 3;
const COLLAPSED_USER_ROWS: usize = 3;
const LOCAL_COMMAND_NAMESPACES: &[&str] = &[
    "clear",
    "compact",
    "fast",
    "help",
    "mcp-prompt",
    "mcp-resource",
    "mode",
    "model",
    "new",
    "permissions",
    "rename",
    "thinking",
];

/// App-owned options that narrow one interactive TUI session.
#[derive(Clone, Debug, Default)]
pub struct TuiOptions {
    pub allowed_tools: Option<Vec<String>>,
    pub profile: Option<String>,
    pub session_id: Option<String>,
}

/// Runs the TUI until the user exits, restoring terminal state on every return path.
pub async fn run(app: &AgentApp, options: TuiOptions) -> Result<(), String> {
    let snapshot = terminal_surface_snapshot(app).await?;
    let mut terminal = TerminalSession::start()?;
    let mut events = EventStream::new();
    let result = run_loop(app, &options, &mut terminal.terminal, &mut events, snapshot).await;
    terminal.restore()?;
    result
}

#[derive(Debug)]
struct TerminalSurfaceSnapshot {
    panels: Vec<PanelItem>,
    suggestions: Vec<Suggestion>,
    commands: Vec<CommandDefinition>,
    terminal: Rc<TerminalGeneration>,
}

async fn terminal_surface_snapshot(app: &AgentApp) -> Result<TerminalSurfaceSnapshot, String> {
    let terminal = Rc::new(app.lease_tui_terminal().await?);
    let panels = app.tui_panels().await?;
    let mut suggestions = builtin_command_suggestions();
    suggestions.extend(app.tui_suggestions().await?);
    suggestions.extend(context_source_suggestions(app).await?);
    let commands = terminal.catalog().await?.commands;
    validate_terminal_namespaces(&commands)?;
    suggestions.extend(commands.iter().map(terminal_command_suggestion));
    validate_snapshot_suggestions(&suggestions)?;
    Ok(TerminalSurfaceSnapshot {
        panels,
        suggestions,
        commands,
        terminal,
    })
}

fn validate_terminal_namespaces(commands: &[CommandDefinition]) -> Result<(), String> {
    if let Some(command) = commands.iter().find(|command| {
        command
            .path
            .first()
            .is_some_and(|root| LOCAL_COMMAND_NAMESPACES.contains(&root.as_str()))
    }) {
        return Err(format!(
            "terminal command namespace `{}` conflicts with a TUI-local command or suggestion",
            command.path[0]
        ));
    }
    Ok(())
}

fn terminal_command_suggestion(command: &CommandDefinition) -> Suggestion {
    let invocation = format!("/{}", command.path.join(" "));
    let insert_text = if command.parameters.is_empty() {
        invocation.clone()
    } else {
        format!("{invocation} ")
    };
    Suggestion {
        id: command.id.clone(),
        kind: SuggestionKind::Command,
        label: invocation,
        insert_text,
        description: command.summary.clone(),
    }
}

fn builtin_command_suggestions() -> Vec<Suggestion> {
    [
        ("help", "/help", "Show keyboard shortcuts"),
        ("clear", "/clear", "Clear the visible conversation"),
        ("new", "/new", "Start a new session"),
        ("compact", "/compact", "Compact the current session context"),
        ("model", "/model ", "List or select an admitted model"),
        (
            "permissions",
            "/permissions ",
            "Inspect or narrow Tool permissions",
        ),
        ("rename", "/rename ", "Rename the current session"),
    ]
    .into_iter()
    .map(|(id, insert_text, description)| Suggestion {
        id: format!("agent.command.{id}"),
        kind: SuggestionKind::Command,
        label: insert_text.trim_end().to_owned(),
        insert_text: insert_text.to_owned(),
        description: description.to_owned(),
    })
    .collect()
}

mod context_suggestions;
use context_suggestions::context_source_suggestions;
mod state;
use state::run_loop;
mod terminal;
use terminal::TerminalSession;
mod text;

#[cfg(test)]
mod terminal_command_tests {
    use super::*;
    use lenso_capability_terminal_command::OutputFormat;

    fn command(root: &str) -> CommandDefinition {
        CommandDefinition {
            id: format!("example.{root}"),
            path: vec![root.to_owned(), "show".to_owned()],
            summary: "Show an example".to_owned(),
            description: String::new(),
            parameters: Vec::new(),
            output_formats: vec![OutputFormat::Text],
        }
    }

    #[test]
    fn projects_catalog_commands_into_tui_suggestions() {
        let suggestion = terminal_command_suggestion(&command("project"));
        assert_eq!(suggestion.id, "example.project");
        assert_eq!(suggestion.label, "/project show");
        assert_eq!(suggestion.insert_text, "/project show");
    }

    #[test]
    fn rejects_tui_local_command_namespace_collisions() {
        assert!(validate_terminal_namespaces(&[command("clear")]).is_err());
        assert!(validate_terminal_namespaces(&[command("sessions")]).is_ok());
    }
}

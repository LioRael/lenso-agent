//! Volatile TUI state and event-loop orchestration.
//!
//! Durable Session facts stay with the Session Plugin; this state is discarded
//! when the terminal surface exits.

use super::{
    ACTIVE_TICK, Agent, AgentApp, Block, BorderType, Borders, COLLAPSED_USER_ROWS, Clear, Color,
    Command, CommandDefinition, CommandExecute, Constraint, ContextRole, CrosstermBackend,
    Duration, EVENT_TICK, Event, EventStream, ExecuteError, ExecuteMessage, ExecuteOpen, Frame,
    InteractionAnswer, InteractionQuestion, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, Layout,
    Line, MAX_INPUT_CHARACTERS, MAX_VISIBLE_QUEUE_HEIGHT, MAX_VISIBLE_QUEUE_ROWS,
    MAX_VISIBLE_SUGGESTIONS, Modifier, MouseButton, MouseEvent, MouseEventKind, NativeStream,
    OffsetDateTime, OnlineGenerationEvent, OutputKind, PANEL_BREAKPOINT, Padding, Palette,
    PanelItem, Paragraph, ParseOutcome, Path, PendingInteraction, RUN_TURN_OPERATION,
    ReadResourceRequest, Rect, RenderPromptRequest, RunScope, RunTurnError, RunTurnRequest,
    RunTurnResponse, RunTurnResponseKind, Scrollbar, ScrollbarOrientation, ScrollbarState, Span,
    StreamEvent, StreamExt, Style, Suggestion, SuggestionKind, Terminal, TerminalGeneration,
    TerminalSurfaceSnapshot, Text, ThinkingCard, ToolCard, ToolStatus, TuiOptions, TurnGeneration,
    VecDeque, WHEEL_SCROLL_LINES, Wrap, blocks, io, markdown, markdown_lines_with_width,
    parse_terminal_line, render_grouped_tool_block, render_thinking_block, render_tool_block,
    render_tool_group, terminal_surface_snapshot,
};
use std::{collections::BTreeSet, rc::Rc, time::Instant};

#[cfg(test)]
use super::markdown_lines;

mod composer;
mod transcript;

#[derive(Debug)]
struct ActiveTurn {
    // Fields drop in declaration order. Cancel the stream before releasing the
    // App Generation lease that owns its runtime resources.
    stream: NativeStream<Agent>,
    lease: Rc<TurnGeneration>,
    task_scope_id: u64,
    started_at: Instant,
}

#[derive(Debug)]
struct ActiveTerminalCommand {
    // Keep the immutable Generation lease alive until the stream is terminal.
    stream: NativeStream<CommandExecute>,
    _lease: Rc<TerminalGeneration>,
    emitted_output: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiPhase {
    Idle,
    SubmitRequested,
    Active,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    Prompt,
    Scrollback,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SessionMode {
    Normal,
    Plan,
    Auto,
    Custom(String),
}

impl SessionMode {
    fn from_profile(profile: Option<&str>) -> Self {
        match profile {
            None => Self::Normal,
            Some("plan") => Self::Plan,
            Some("code") => Self::Auto,
            Some(profile) => Self::Custom(profile.to_owned()),
        }
    }

    const fn label(&self) -> &str {
        match self {
            Self::Normal => "normal",
            Self::Plan => "plan",
            Self::Auto => "auto",
            Self::Custom(profile) => profile.as_str(),
        }
    }

    fn profile(&self) -> Option<String> {
        match self {
            Self::Normal => None,
            Self::Plan => Some("plan".to_owned()),
            Self::Auto => Some("code".to_owned()),
            Self::Custom(profile) => Some(profile.clone()),
        }
    }

    fn next(&self) -> Self {
        match self {
            Self::Normal | Self::Custom(_) => Self::Plan,
            Self::Plan => Self::Auto,
            Self::Auto => Self::Normal,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WheelDirection {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SuggestionVisibility {
    #[default]
    Auto,
    Dismissed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PollStatus {
    #[default]
    Ready,
    ErrorReported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskRouteScope {
    CurrentGeneration(u64),
    Turn(u64),
}

#[derive(Debug, Default)]
struct WheelState {
    last_at: Option<Instant>,
    direction: Option<WheelDirection>,
    burst: usize,
}

impl WheelState {
    fn rows(&mut self, direction: WheelDirection) -> usize {
        const STREAM_GAP: Duration = Duration::from_millis(80);
        let now = Instant::now();
        if self.direction == Some(direction)
            && self
                .last_at
                .is_some_and(|previous| now.duration_since(previous) <= STREAM_GAP)
        {
            self.burst = self.burst.saturating_add(1).min(12);
        } else {
            self.burst = 0;
        }
        self.last_at = Some(now);
        self.direction = Some(direction);
        WHEEL_SCROLL_LINES + self.burst.saturating_div(3)
    }
}

impl UiPhase {
    const fn activity(self, tick: u64) -> Option<(&'static str, Color)> {
        match self {
            Self::Idle => None,
            Self::SubmitRequested => Some(("◆ Starting turn…", Palette::SECONDARY_TEXT)),
            Self::Active => Some((working_label(tick), Palette::SECONDARY_TEXT)),
            Self::Failed => Some(("● Turn failed", Palette::ERROR)),
        }
    }
}

const fn working_label(tick: u64) -> &'static str {
    match tick % 4 {
        0 => "✦ Responding…",
        1 => "✧ Responding…",
        2 => "· Responding…",
        _ => "⋅ Responding…",
    }
}

#[derive(Debug)]
enum TranscriptEntry {
    User { text: String, created_at: String },
    Agent { text: String, created_at: String },
    Thinking(ThinkingCard),
    System { text: String },
    Error { text: String },
    Tool(ToolCard),
    TurnCompleted { elapsed: Duration },
}

#[derive(Clone, Copy, Debug)]
struct ToolHitTarget {
    column_start: u16,
    column_end: u16,
    row_start: u16,
    row_end: u16,
    selection: ToolSelection,
}

#[derive(Clone, Copy, Debug)]
struct ThinkingHitTarget {
    area: Rect,
    entry_index: usize,
}

#[derive(Clone, Copy, Debug)]
struct UserHitTarget {
    area: Rect,
    entry_index: usize,
}

#[derive(Clone, Copy, Debug)]
struct QueueHitTarget {
    area: Rect,
    index: usize,
    edit: Option<Rect>,
    cancel: Option<Rect>,
}

#[derive(Clone, Copy, Debug)]
struct SuggestionHitTarget {
    area: Rect,
    selection: usize,
}

#[derive(Clone, Copy, Debug)]
struct InteractionHitTarget {
    area: Rect,
    action: InteractionHitAction,
}

#[derive(Clone, Copy, Debug)]
enum InteractionHitAction {
    Option(usize),
    Other,
}

#[derive(Clone, Copy, Debug)]
struct ShortcutHitTarget {
    area: Rect,
    action: ShortcutAction,
}

#[derive(Clone, Copy, Debug)]
enum ShortcutAction {
    Send,
    Newline,
    FocusPrompt,
    FocusScrollback,
    PageDown,
    ToggleSelectedTool,
    ShowShortcuts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolSelection {
    Tool(usize),
    Group { start: usize, end: usize },
}

#[derive(Clone, Copy, Debug)]
struct RenderedToolRow {
    start_row: usize,
    end_row: usize,
    selection: ToolSelection,
}

#[derive(Clone, Copy, Debug)]
struct RenderedThinkingRow {
    start_row: usize,
    end_row: usize,
    entry_index: usize,
}

#[derive(Clone, Copy, Debug)]
struct RenderedUserRow {
    start_row: usize,
    end_row: usize,
    entry_index: usize,
    foldable: bool,
}

#[derive(Clone, Copy, Debug)]
struct RenderedEntryRow {
    start_row: usize,
    end_row: usize,
    entry_index: usize,
}

#[derive(Clone, Copy, Debug)]
struct EntryHitTarget {
    area: Rect,
    entry_index: usize,
    top_clipped: bool,
    bottom_clipped: bool,
}

#[derive(Clone, Debug)]
struct RenderedLinkRow {
    row: usize,
    column_start: usize,
    column_end: usize,
    url: String,
}

#[derive(Clone, Debug)]
struct LinkHitTarget {
    area: Rect,
    url: String,
}

struct TranscriptRender {
    lines: Vec<Line<'static>>,
    row_count: usize,
    entry_rows: Vec<RenderedEntryRow>,
    link_rows: Vec<RenderedLinkRow>,
    tool_rows: Vec<RenderedToolRow>,
    thinking_rows: Vec<RenderedThinkingRow>,
    user_rows: Vec<RenderedUserRow>,
    prompt_anchors: Vec<PromptAnchor>,
}

#[derive(Clone, Copy)]
struct UserEntryRender<'a> {
    text: &'a str,
    created_at: &'a str,
    width: usize,
    expanded: bool,
}

#[derive(Debug)]
struct PromptAnchor {
    start_row: usize,
    end_row: usize,
    text: String,
}

#[derive(Debug)]
struct ScrollState {
    top: usize,
    max_top: usize,
    viewport_rows: usize,
    follow_tail: bool,
    page_flip_pending: bool,
    page_flip_anchor: Option<usize>,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            top: 0,
            max_top: 0,
            viewport_rows: 1,
            follow_tail: true,
            page_flip_pending: false,
            page_flip_anchor: None,
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
        self.cancel_page_flip();
    }

    fn scroll_down(&mut self, rows: usize) {
        self.top = self.top.saturating_add(rows.max(1)).min(self.max_top);
        self.follow_tail = self.top == self.max_top;
        self.cancel_page_flip();
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
        self.cancel_page_flip();
    }

    fn goto_bottom(&mut self) {
        self.top = self.max_top;
        self.follow_tail = true;
        self.cancel_page_flip();
    }

    fn rows_below(&self) -> usize {
        self.max_top.saturating_sub(self.top)
    }

    fn begin_page_flip(&mut self) {
        self.page_flip_pending = true;
        self.page_flip_anchor = None;
    }

    fn apply_page_flip(&mut self, latest_prompt: Option<usize>, content_rows: usize) {
        if self.page_flip_pending {
            self.page_flip_pending = false;
            self.page_flip_anchor = latest_prompt;
            if let Some(anchor) = latest_prompt {
                self.top = anchor.min(self.max_top);
                self.follow_tail = false;
            }
        }
        if let Some(anchor) = self.page_flip_anchor
            && content_rows > anchor.saturating_add(self.viewport_rows)
        {
            self.top = self.max_top;
            self.follow_tail = true;
            self.page_flip_anchor = None;
        }
    }

    fn scroll_to_pointer(&mut self, pointer_row: u16, track: Rect) {
        if track.height <= 1 || self.max_top == 0 {
            self.goto_bottom();
            return;
        }
        let content_rows = self.max_top.saturating_add(self.viewport_rows).max(1);
        let thumb_rows = usize::from(track.height)
            .saturating_mul(self.viewport_rows)
            .saturating_div(content_rows)
            .clamp(1, usize::from(track.height));
        let travel = usize::from(track.height).saturating_sub(thumb_rows);
        if travel == 0 {
            self.goto_bottom();
            return;
        }
        let pointer = usize::from(
            pointer_row
                .saturating_sub(track.y)
                .min(track.height.saturating_sub(1)),
        );
        let relative = pointer
            .saturating_sub(thumb_rows.saturating_div(2))
            .min(travel);
        self.top = self.max_top.saturating_mul(relative).saturating_div(travel);
        self.follow_tail = self.top == self.max_top;
        self.cancel_page_flip();
    }

    fn cancel_page_flip(&mut self) {
        self.page_flip_pending = false;
        self.page_flip_anchor = None;
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
    queued_inputs: VecDeque<String>,
    expanded_user_entries: BTreeSet<usize>,
    selected_entry: Option<usize>,
    hovered_entry: Option<usize>,
    selected_block: Option<ToolSelection>,
    expanded_groups: BTreeSet<usize>,
    visible_tool_blocks: Vec<ToolSelection>,
    panels: Vec<PanelItem>,
    task_panel_body: String,
    suggestions: Vec<Suggestion>,
    command_catalog: Vec<CommandDefinition>,
    terminal_generation: Option<Rc<TerminalGeneration>>,
    suggestion_selected: usize,
    suggestion_scroll: usize,
    suggestion_visibility: SuggestionVisibility,
    selected_panel: usize,
    session_id: Option<String>,
    selected_model: Option<String>,
    selected_reasoning_effort: Option<String>,
    selected_service_tier: Option<String>,
    mode: SessionMode,
    pending_mode: Option<SessionMode>,
    allowed_tools: Option<Vec<String>>,
    phase: UiPhase,
    active: Option<ActiveTurn>,
    active_command: Option<ActiveTerminalCommand>,
    pending_interaction: Option<PendingInteraction>,
    interaction_draft: Option<InteractionDraft>,
    pending_answers: Option<Vec<InteractionAnswer>>,
    next_interaction_poll: Instant,
    interaction_poll_status: PollStatus,
    task_poll_status: PollStatus,
    task_generation_epoch: u64,
    task_projection_scope: Option<TaskRouteScope>,
    task_turn_sequence: u64,
    tool_scope: String,
    scroll: ScrollState,
    workspace: String,
    branch: Option<String>,
    focus: Focus,
    wheel: WheelState,
    scrollbar_hit: Option<Rect>,
    scrollbar_dragging: bool,
    follow_hit: Option<Rect>,
    cancel_hit: Option<Rect>,
    composer_hit: Option<Rect>,
    show_shortcuts: bool,
    panel_open: bool,
    tool_hit_targets: Vec<ToolHitTarget>,
    thinking_hit_targets: Vec<ThinkingHitTarget>,
    user_hit_targets: Vec<UserHitTarget>,
    entry_hit_targets: Vec<EntryHitTarget>,
    link_hit_targets: Vec<LinkHitTarget>,
    rendered_entry_rows: Vec<RenderedEntryRow>,
    queue_hit_targets: Vec<QueueHitTarget>,
    queue_hovered: Option<usize>,
    suggestion_hit_targets: Vec<SuggestionHitTarget>,
    interaction_hit_targets: Vec<InteractionHitTarget>,
    shortcut_hit_targets: Vec<ShortcutHitTarget>,
    animation_tick: u64,
}

impl TuiState {
    fn new(options: &TuiOptions, panels: Vec<PanelItem>) -> Self {
        Self {
            input: String::new(),
            input_characters: 0,
            input_cursor: 0,
            input_history: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            transcript: Vec::new(),
            queued_inputs: VecDeque::new(),
            expanded_user_entries: BTreeSet::new(),
            selected_entry: None,
            hovered_entry: None,
            selected_block: None,
            expanded_groups: BTreeSet::new(),
            visible_tool_blocks: Vec::new(),
            panels,
            task_panel_body: "Loading supervised tasks…".to_owned(),
            suggestions: Vec::new(),
            command_catalog: Vec::new(),
            terminal_generation: None,
            suggestion_selected: 0,
            suggestion_scroll: 0,
            suggestion_visibility: SuggestionVisibility::Auto,
            selected_panel: 0,
            session_id: options.session_id.clone(),
            selected_model: None,
            selected_reasoning_effort: None,
            selected_service_tier: None,
            mode: SessionMode::from_profile(options.profile.as_deref()),
            pending_mode: None,
            allowed_tools: options.allowed_tools.clone(),
            phase: UiPhase::Idle,
            active: None,
            active_command: None,
            pending_interaction: None,
            interaction_draft: None,
            pending_answers: None,
            next_interaction_poll: Instant::now(),
            interaction_poll_status: PollStatus::Ready,
            task_poll_status: PollStatus::Ready,
            task_generation_epoch: 0,
            task_projection_scope: None,
            task_turn_sequence: 0,
            tool_scope: match (&options.profile, &options.allowed_tools) {
                (Some(profile), None) => format!("{profile} profile · composed tools"),
                (Some(profile), Some(tools)) if tools.is_empty() => {
                    format!("{profile} profile · no tools")
                }
                (Some(profile), Some(tools)) => {
                    format!("{profile} profile · {} scoped tools", tools.len())
                }
                (None, None) => "composed tools".to_owned(),
                (None, Some(tools)) if tools.is_empty() => "no tools".to_owned(),
                (None, Some(tools)) => format!("{} scoped tools", tools.len()),
            },
            scroll: ScrollState::default(),
            workspace: current_workspace_label(),
            branch: current_branch_label(),
            focus: Focus::Prompt,
            wheel: WheelState::default(),
            scrollbar_hit: None,
            scrollbar_dragging: false,
            follow_hit: None,
            cancel_hit: None,
            composer_hit: None,
            show_shortcuts: false,
            panel_open: false,
            tool_hit_targets: Vec::new(),
            thinking_hit_targets: Vec::new(),
            user_hit_targets: Vec::new(),
            entry_hit_targets: Vec::new(),
            link_hit_targets: Vec::new(),
            rendered_entry_rows: Vec::new(),
            queue_hit_targets: Vec::new(),
            queue_hovered: None,
            suggestion_hit_targets: Vec::new(),
            interaction_hit_targets: Vec::new(),
            shortcut_hit_targets: Vec::new(),
            animation_tick: 0,
        }
    }

    fn panel_count(&self) -> usize {
        self.panels.len() + 1
    }

    fn panel_at(&self, index: usize) -> Option<(&str, &str)> {
        self.panels.get(index).map_or_else(
            || (index == self.panels.len()).then_some(("Tasks", self.task_panel_body.as_str())),
            |panel| Some((panel.title.as_str(), panel.body.as_str())),
        )
    }

    fn replace_plugin_panels(&mut self, panels: Vec<PanelItem>) {
        let tasks_selected = self.selected_panel == self.panels.len();
        self.panels = panels;
        self.selected_panel = if tasks_selected {
            self.panels.len()
        } else {
            self.selected_panel.min(self.panels.len())
        };
    }

    fn replace_terminal_surface(&mut self, snapshot: TerminalSurfaceSnapshot) {
        self.replace_plugin_panels(snapshot.panels);
        self.suggestions = snapshot.suggestions;
        self.command_catalog = snapshot.commands;
        self.terminal_generation = Some(snapshot.terminal);
        self.suggestion_selected = 0;
        self.suggestion_scroll = 0;
    }

    fn advance_task_generation_epoch(&mut self) {
        self.task_generation_epoch = self.task_generation_epoch.wrapping_add(1);
    }

    fn current_task_scope(&self) -> TaskRouteScope {
        self.active.as_ref().map_or(
            TaskRouteScope::CurrentGeneration(self.task_generation_epoch),
            |active| TaskRouteScope::Turn(active.task_scope_id),
        )
    }

    fn next_task_turn_scope_id(&mut self) -> u64 {
        self.task_turn_sequence = self.task_turn_sequence.wrapping_add(1);
        self.task_turn_sequence
    }

    fn reset_task_projection_if_stale(&mut self) -> bool {
        if self
            .task_projection_scope
            .is_some_and(|scope| scope != self.current_task_scope())
        {
            "Loading supervised tasks…".clone_into(&mut self.task_panel_body);
            self.task_poll_status = PollStatus::Ready;
            self.task_projection_scope = None;
            true
        } else {
            false
        }
    }

    fn push_system(&mut self, text: impl Into<String>) {
        self.transcript
            .push(TranscriptEntry::System { text: text.into() });
    }

    fn turn_is_running(&self) -> bool {
        self.active.is_some() || self.active_command.is_some() || self.phase == UiPhase::Active
    }

    fn request_next_mode(&mut self) {
        let current = self.pending_mode.as_ref().unwrap_or(&self.mode);
        self.pending_mode = Some(current.next());
    }
}

#[derive(Debug)]
struct SuggestionMatch {
    start: usize,
    indices: Vec<usize>,
}

fn char_to_byte(text: &str, character: usize) -> usize {
    text.char_indices()
        .nth(character)
        .map_or(text.len(), |(byte, _)| byte)
}

fn current_workspace_label() -> String {
    let cwd = std::env::current_dir().ok();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    match (cwd, home) {
        (Some(cwd), Some(home)) if cwd.starts_with(&home) => {
            cwd.strip_prefix(home).ok().map_or_else(
                || cwd.display().to_string(),
                |suffix| format!("~/{}", suffix.display()),
            )
        }
        (Some(cwd), _) => cwd.display().to_string(),
        (None, _) => Path::new("workspace").display().to_string(),
    }
}

fn current_branch_label() -> Option<String> {
    let output = Command::new("git")
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|branch| !branch.is_empty())
}

#[path = "online.rs"]
mod online;
use online::present_online_generation_events;
#[path = "input.rs"]
mod input;
#[path = "interaction.rs"]
mod interaction;
use input::handle_terminal_event;
#[cfg(test)]
use input::{
    handle_control_key, handle_key, handle_mouse_click, handle_mouse_move, safe_link_target,
};
use interaction::{InteractionDraft, sync_user_interaction};
#[cfg(test)]
use interaction::{finish_interaction_submission, handle_interaction_key};
#[path = "task_supervision.rs"]
mod task_supervision;
use task_supervision::TaskSnapshotPoll;
#[cfg(test)]
use task_supervision::{TASK_POLL_ERROR_INTERVAL, TASK_POLL_INTERVAL, apply_task_snapshot};
#[path = "turn.rs"]
mod turn;
#[cfg(test)]
use turn::{
    FastSelection, PermissionSelection, ThinkingSelection, fast_command, mode_command,
    model_command, permissions_command, rename_command, thinking_command,
};
use turn::{apply_pending_mode, handle_terminal_command_event, submit};
#[path = "turn_stream.rs"]
mod turn_stream;
use turn_stream::handle_stream_event;
#[path = "render.rs"]
mod render;
use render::{current_timestamp, render};
#[cfg(test)]
use render::{format_turn_duration, sticky_prompt, transcript_lines};
mod event_loop;
pub(super) use event_loop::run_loop;
#[cfg(test)]
mod tests;

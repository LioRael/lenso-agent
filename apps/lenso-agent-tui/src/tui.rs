//! Interactive terminal surface for the composed Agent App.

mod blocks;
mod markdown;

use std::{
    collections::{BTreeSet, VecDeque},
    io,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

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

use blocks::{
    ThinkingCard, ToolCard, ToolStatus, render_grouped_tool_block, render_thinking_block,
    render_tool_block, render_tool_group,
};
use lenso_agent_host::generation::{AgentApp, OnlineGenerationEvent, TurnGeneration};
use markdown::{lines as markdown_lines, lines_with_width as markdown_lines_with_width};

const EVENT_TICK: Duration = Duration::from_millis(250);
const MAX_INPUT_CHARACTERS: usize = 262_144;
const PANEL_BREAKPOINT: u16 = 96;
const WHEEL_SCROLL_LINES: usize = 3;
const ACTIVE_TICK: Duration = Duration::from_millis(90);
const MAX_VISIBLE_SUGGESTIONS: usize = 6;
const MAX_VISIBLE_QUEUE_ROWS: usize = 3;
const MAX_VISIBLE_QUEUE_HEIGHT: u16 = 3;
const COLLAPSED_USER_ROWS: usize = 3;

struct Palette;

impl Palette {
    // GrokNight's neutral canvas and semantic accents, adapted to Lenso's
    // terminal surface. RGB-capable terminals get the reference hierarchy;
    // Ratatui/crossterm handles lower-color terminal fallback.
    const BG_BASE: Color = Color::Rgb(20, 20, 20);
    const ACCENT: Color = Color::Rgb(187, 154, 247);
    const BORDER: Color = Color::Rgb(50, 50, 55);
    const BORDER_ACTIVE: Color = Color::Rgb(80, 80, 88);
    const SELECTION_BORDER: Color = Color::Rgb(60, 60, 65);
    const HOVER_BORDER: Color = Color::Rgb(30, 30, 34);
    const HOVER_SURFACE: Color = Color::Rgb(24, 24, 24);
    const ERROR: Color = Color::Rgb(247, 118, 142);
    const SUCCESS: Color = Color::Rgb(158, 206, 106);
    const MUTED: Color = Color::Rgb(108, 108, 108);
    const QUIET: Color = Color::Rgb(88, 88, 88);
    const CODE: Color = Color::Rgb(58, 149, 171);
    const COMMAND: Color = Color::Rgb(224, 175, 104);
    const PATH: Color = Color::Rgb(255, 158, 100);
    const HEADING_H1: Color = Color::Rgb(26, 188, 156);
    const HEADING_H2: Color = Color::Rgb(122, 162, 247);
    const HEADING_H3: Color = Color::Rgb(157, 124, 216);
    const HEADING_H4: Color = Color::Rgb(120, 120, 120);
    const HEADING_H5: Color = Color::Rgb(108, 108, 108);
    const HEADING_H6: Color = Color::Rgb(90, 90, 90);
    const LINK: Color = Color::Rgb(122, 166, 218);
    const SURFACE: Color = Color::Rgb(28, 28, 28);
    const VISUAL_SURFACE: Color = Color::Rgb(54, 54, 54);
    const USER_SURFACE: Color = Color::Rgb(36, 36, 36);
    const SURFACE_TEXT: Color = Color::Rgb(225, 225, 225);
    const SECONDARY_TEXT: Color = Color::Rgb(200, 200, 200);
    const USER_ACCENT: Color = Color::Rgb(200, 200, 200);
}

/// App-owned options that narrow one interactive TUI session.
#[derive(Clone, Debug, Default)]
pub struct TuiOptions {
    pub allowed_tools: Option<Vec<String>>,
    pub profile: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug)]
struct ActiveTurn {
    // Fields drop in declaration order. Cancel the stream before releasing the
    // App Generation lease that owns its runtime resources.
    stream: NativeStream<Agent>,
    lease: TurnGeneration,
    started_at: Instant,
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
enum InteractionPollStatus {
    #[default]
    Ready,
    ErrorReported,
}

#[derive(Debug)]
struct InteractionDraft {
    question_index: usize,
    option_cursors: Vec<usize>,
    selected: Vec<BTreeSet<String>>,
    other: Vec<Option<String>>,
    editing_other: bool,
    other_input: String,
}

impl InteractionDraft {
    fn new(interaction: &PendingInteraction) -> Self {
        let question_count = interaction.questions.len();
        Self {
            question_index: 0,
            option_cursors: vec![0; question_count],
            selected: vec![BTreeSet::new(); question_count],
            other: vec![None; question_count],
            editing_other: false,
            other_input: String::new(),
        }
    }

    fn option_cursor(&self) -> usize {
        self.option_cursors
            .get(self.question_index)
            .copied()
            .unwrap_or_default()
    }

    fn set_option_cursor(&mut self, cursor: usize) {
        if let Some(slot) = self.option_cursors.get_mut(self.question_index) {
            *slot = cursor;
        }
    }
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
    entry_index: usize,
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
    panels: Vec<SnapshotResponsePanelsItem>,
    suggestions: Vec<Suggestion>,
    suggestion_selected: usize,
    suggestion_scroll: usize,
    suggestion_visibility: SuggestionVisibility,
    selected_panel: usize,
    session_id: Option<String>,
    phase: UiPhase,
    active: Option<ActiveTurn>,
    pending_interaction: Option<PendingInteraction>,
    interaction_draft: Option<InteractionDraft>,
    pending_answers: Option<Vec<InteractionAnswer>>,
    next_interaction_poll: Instant,
    interaction_poll_status: InteractionPollStatus,
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
    fn new(options: &TuiOptions, panels: Vec<SnapshotResponsePanelsItem>) -> Self {
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
            suggestions: Vec::new(),
            suggestion_selected: 0,
            suggestion_scroll: 0,
            suggestion_visibility: SuggestionVisibility::Auto,
            selected_panel: 0,
            session_id: options.session_id.clone(),
            phase: UiPhase::Idle,
            active: None,
            pending_interaction: None,
            interaction_draft: None,
            pending_answers: None,
            next_interaction_poll: Instant::now(),
            interaction_poll_status: InteractionPollStatus::Ready,
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

    fn push_system(&mut self, text: impl Into<String>) {
        self.transcript
            .push(TranscriptEntry::System { text: text.into() });
    }

    fn turn_is_running(&self) -> bool {
        self.active.is_some() || self.phase == UiPhase::Active
    }

    fn append_agent_text(&mut self, text: &str) {
        self.finish_provisional_thinking();
        if let Some(last) = self.transcript.last_mut()
            && let TranscriptEntry::Agent { text: existing, .. } = last
        {
            existing.push_str(text);
            return;
        }
        self.transcript.push(TranscriptEntry::Agent {
            text: text.to_owned(),
            created_at: current_timestamp(),
        });
    }

    fn start_provisional_thinking(&mut self) {
        self.transcript
            .push(TranscriptEntry::Thinking(ThinkingCard::provisional()));
    }

    fn append_reasoning(&mut self, message: RunTurnResponse) {
        let Some(reasoning_id) = message.reasoning_id else {
            self.push_system("Ignored reasoning without an ID");
            return;
        };
        if let Some(TranscriptEntry::Thinking(card)) = self.transcript.last_mut()
            && card.is_running()
            && card
                .reasoning_id
                .as_deref()
                .is_none_or(|current| current == reasoning_id)
        {
            card.append(reasoning_id, &message.text);
            return;
        }
        let mut card = ThinkingCard::provisional();
        card.append(reasoning_id, &message.text);
        self.transcript.push(TranscriptEntry::Thinking(card));
    }

    fn complete_reasoning(&mut self, message: RunTurnResponse) {
        let Some(reasoning_id) = message.reasoning_id else {
            self.push_system("Ignored reasoning completion without an ID");
            return;
        };
        let duration_ms = message
            .duration_ms
            .and_then(|value| value.parse::<u64>().ok());
        if let Some(card) = self
            .transcript
            .iter_mut()
            .rev()
            .find_map(|entry| match entry {
                TranscriptEntry::Thinking(card)
                    if card.reasoning_id.as_deref() == Some(reasoning_id.as_str()) =>
                {
                    Some(card)
                }
                _ => None,
            })
        {
            card.finish(duration_ms);
        }
    }

    fn finish_provisional_thinking(&mut self) {
        let remove = matches!(
            self.transcript.last(),
            Some(TranscriptEntry::Thinking(card)) if card.is_running() && card.text.is_empty()
        );
        if remove {
            self.transcript.pop();
        }
    }

    fn finish_active_thinking(&mut self) {
        self.finish_provisional_thinking();
        if let Some(TranscriptEntry::Thinking(card)) = self.transcript.last_mut()
            && card.is_running()
        {
            card.finish(None);
        }
    }

    fn toggle_thinking_at(&mut self, position: ratatui::layout::Position) -> bool {
        let Some(target) = self
            .thinking_hit_targets
            .iter()
            .copied()
            .find(|target| target.area.contains(position))
        else {
            return false;
        };
        self.selected_entry = Some(target.entry_index);
        self.focus = Focus::Scrollback;
        if let Some(TranscriptEntry::Thinking(card)) = self.transcript.get_mut(target.entry_index) {
            card.expanded = !card.expanded;
        }
        true
    }

    fn toggle_user_at(&mut self, position: ratatui::layout::Position) -> bool {
        let Some(target) = self
            .user_hit_targets
            .iter()
            .copied()
            .find(|target| target.area.contains(position))
        else {
            return false;
        };
        self.selected_entry = Some(target.entry_index);
        self.focus = Focus::Scrollback;
        if !self.expanded_user_entries.remove(&target.entry_index) {
            self.expanded_user_entries.insert(target.entry_index);
        }
        true
    }

    fn queue_input(&mut self) {
        let input = self.take_input();
        if input.trim().is_empty() {
            return;
        }
        self.queued_inputs.push_back(input);
        self.queue_hovered = Some(self.queued_inputs.len().saturating_sub(1));
    }

    fn edit_queued_input(&mut self, index: usize) {
        let Some(input) = self.queued_inputs.remove(index) else {
            return;
        };
        self.set_input(input);
        self.focus = Focus::Prompt;
        self.queue_hovered = None;
    }

    fn cancel_queued_input(&mut self, index: usize) {
        self.queued_inputs.remove(index);
        self.queue_hovered = None;
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
        self.reset_suggestion_selection();
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
        self.reset_suggestion_selection();
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
        self.reset_suggestion_selection();
    }

    fn move_cursor(&mut self, delta: isize) {
        self.input_cursor = self
            .input_cursor
            .saturating_add_signed(delta)
            .min(self.input_characters);
        self.reset_suggestion_selection();
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
        self.reset_suggestion_selection();
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

    fn reset_suggestion_selection(&mut self) {
        self.suggestion_selected = 0;
        self.suggestion_scroll = 0;
        self.suggestion_visibility = SuggestionVisibility::Auto;
    }

    fn suggestion_match(&self) -> Option<SuggestionMatch> {
        if self.turn_is_running()
            || self.focus != Focus::Prompt
            || self.suggestion_visibility == SuggestionVisibility::Dismissed
        {
            return None;
        }
        let before = self
            .input
            .chars()
            .take(self.input_cursor)
            .collect::<String>();
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        let line = &before[line_start..];
        let (start, query, matches_kind) =
            if line.starts_with('/') && !line.contains(char::is_whitespace) {
                (
                    self.input_cursor - line.chars().count(),
                    line.to_ascii_lowercase(),
                    true,
                )
            } else {
                let token = line
                    .rsplit_once(char::is_whitespace)
                    .map_or(line, |(_, token)| token);
                if !token.starts_with('@') || token[1..].contains('@') {
                    return None;
                }
                (
                    self.input_cursor - token.chars().count(),
                    token[1..].to_ascii_lowercase(),
                    false,
                )
            };
        let mut indices = self
            .suggestions
            .iter()
            .enumerate()
            .filter(|(_, suggestion)| {
                if matches_kind {
                    matches!(
                        suggestion.kind,
                        SuggestionKind::Command
                            | SuggestionKind::Prompt
                            | SuggestionKind::Resource
                            | SuggestionKind::Skill
                    )
                } else {
                    suggestion.kind == SuggestionKind::File
                }
            })
            .filter(|(_, suggestion)| suggestion.label.to_ascii_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        indices.sort_by_key(|index| {
            let label = self.suggestions[*index].label.to_ascii_lowercase();
            (!label.starts_with(&query), label)
        });
        (!indices.is_empty()).then_some(SuggestionMatch { start, indices })
    }

    fn select_suggestion(&mut self, previous: bool) -> bool {
        let Some(matches) = self.suggestion_match() else {
            return false;
        };
        let len = matches.indices.len();
        self.suggestion_selected = if previous {
            self.suggestion_selected.checked_sub(1).unwrap_or(len - 1)
        } else {
            (self.suggestion_selected + 1) % len
        };
        if self.suggestion_selected < self.suggestion_scroll {
            self.suggestion_scroll = self.suggestion_selected;
        } else if self.suggestion_selected >= self.suggestion_scroll + MAX_VISIBLE_SUGGESTIONS {
            self.suggestion_scroll = self.suggestion_selected + 1 - MAX_VISIBLE_SUGGESTIONS;
        }
        true
    }

    fn accept_suggestion(&mut self) -> Option<SuggestionKind> {
        let matches = self.suggestion_match()?;
        let selected = self.suggestion_selected.min(matches.indices.len() - 1);
        let suggestion = &self.suggestions[matches.indices[selected]];
        let kind = suggestion.kind.clone();
        let mut replacement = suggestion.insert_text.clone();
        if matches!(
            suggestion.kind,
            SuggestionKind::File
                | SuggestionKind::Prompt
                | SuggestionKind::Resource
                | SuggestionKind::Skill
        ) {
            replacement.push(' ');
        }
        let start_byte = char_to_byte(&self.input, matches.start);
        let end_byte = char_to_byte(&self.input, self.input_cursor);
        self.input.replace_range(start_byte..end_byte, &replacement);
        self.input_characters = self.input.chars().count();
        self.input_cursor = matches.start + replacement.chars().count();
        self.leave_history();
        self.reset_suggestion_selection();
        Some(kind)
    }

    fn start_tool(&mut self, message: RunTurnResponse) {
        self.finish_provisional_thinking();
        let Some(call_id) = message.tool_call_id else {
            self.push_system("Ignored a Tool event without a call ID");
            return;
        };
        let Some(name) = message.tool_name else {
            self.push_system("Ignored a Tool event without a name");
            return;
        };
        self.transcript
            .push(TranscriptEntry::Tool(ToolCard::running(
                call_id,
                name,
                message.arguments_json.map(|value| value.to_string()),
            )));
        self.selected_block = Some(ToolSelection::Tool(self.transcript.len() - 1));
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
        self.selected_block = Some(ToolSelection::Tool(index));
    }

    fn append_tool_progress(&mut self, message: RunTurnResponse) {
        let Some(call_id) = message.tool_call_id else {
            self.push_system("Ignored Tool progress without a call ID");
            return;
        };
        let Some(content) = message.content else {
            return;
        };
        let index = self.transcript.iter().rposition(
            |entry| matches!(entry, TranscriptEntry::Tool(card) if card.call_id == call_id),
        );
        let Some(index) = index else {
            self.push_system(format!(
                "Ignored Tool progress for unknown call `{call_id}`"
            ));
            return;
        };
        let TranscriptEntry::Tool(card) = &mut self.transcript[index] else {
            unreachable!("Tool lookup returned a message entry")
        };
        card.append_progress(&content);
        self.selected_block = Some(ToolSelection::Tool(index));
    }

    fn toggle_tool_details(&mut self) {
        let Some(selection) = self.selected_block else {
            return;
        };
        match selection {
            ToolSelection::Tool(index) => {
                if let Some(TranscriptEntry::Tool(card)) = self.transcript.get_mut(index) {
                    card.expanded = !card.expanded;
                }
            }
            ToolSelection::Group { start, .. } => {
                if !self.expanded_groups.remove(&start) {
                    self.expanded_groups.insert(start);
                }
            }
        }
    }

    fn set_tool_details(&mut self, expanded: bool) {
        let Some(selection) = self.selected_block else {
            return;
        };
        match selection {
            ToolSelection::Tool(index) => {
                if let Some(TranscriptEntry::Tool(card)) = self.transcript.get_mut(index) {
                    card.expanded = expanded;
                }
            }
            ToolSelection::Group { start, .. } if expanded => {
                self.expanded_groups.insert(start);
            }
            ToolSelection::Group { start, .. } => {
                self.expanded_groups.remove(&start);
            }
        }
    }

    fn select_adjacent_tool(&mut self, previous: bool) {
        if self.visible_tool_blocks.is_empty() {
            self.selected_block = None;
            return;
        }
        let current = self.selected_block.and_then(|selected| {
            self.visible_tool_blocks
                .iter()
                .position(|item| *item == selected)
        });
        let next = if previous {
            current
                .and_then(|position| position.checked_sub(1))
                .unwrap_or(self.visible_tool_blocks.len() - 1)
        } else {
            current.map_or(0, |position| {
                (position + 1) % self.visible_tool_blocks.len()
            })
        };
        self.selected_block = Some(self.visible_tool_blocks[next]);
    }

    fn select_adjacent_entry(&mut self, previous: bool) {
        if self.rendered_entry_rows.is_empty() {
            self.selected_entry = None;
            return;
        }
        let current = self.selected_entry.and_then(|selected| {
            self.rendered_entry_rows
                .iter()
                .position(|row| row.entry_index == selected)
        });
        let next = if previous {
            current
                .and_then(|position| position.checked_sub(1))
                .unwrap_or(self.rendered_entry_rows.len() - 1)
        } else {
            current.map_or(0, |position| {
                (position + 1) % self.rendered_entry_rows.len()
            })
        };
        let row = self.rendered_entry_rows[next];
        self.selected_entry = Some(row.entry_index);
        if let Some(selection) = self.tool_selection_for_entry(row.entry_index) {
            self.selected_block = Some(selection);
        }
        if row.start_row < self.scroll.top {
            self.scroll.top = row.start_row;
            self.scroll.follow_tail = false;
        } else {
            let viewport_end = self.scroll.top.saturating_add(self.scroll.viewport_rows);
            if row.end_row >= viewport_end {
                self.scroll.top = row
                    .end_row
                    .saturating_add(1)
                    .saturating_sub(self.scroll.viewport_rows)
                    .min(self.scroll.max_top);
                self.scroll.follow_tail = self.scroll.top == self.scroll.max_top;
            }
        }
        self.scroll.cancel_page_flip();
    }

    fn toggle_selected_entry(&mut self) {
        let Some(index) = self.selected_entry else {
            return;
        };
        match self.transcript.get_mut(index) {
            Some(TranscriptEntry::User { .. }) => {
                if !self.expanded_user_entries.remove(&index) {
                    self.expanded_user_entries.insert(index);
                }
            }
            Some(TranscriptEntry::Thinking(card)) => card.expanded = !card.expanded,
            Some(TranscriptEntry::Tool(_)) => {
                self.selected_block = self
                    .tool_selection_for_entry(index)
                    .or(Some(ToolSelection::Tool(index)));
                self.toggle_tool_details();
            }
            _ => {}
        }
    }

    fn tool_selection_for_entry(&self, entry_index: usize) -> Option<ToolSelection> {
        self.visible_tool_blocks.iter().copied().find(|selection| {
            matches!(
                selection,
                ToolSelection::Tool(index) if *index == entry_index
            ) || matches!(
                selection,
                ToolSelection::Group { start, .. } if *start == entry_index
            )
        })
    }

    fn toggle_tool_at(&mut self, column: u16, row: u16) {
        let Some(target) = self
            .tool_hit_targets
            .iter()
            .find(|target| {
                column >= target.column_start
                    && column <= target.column_end
                    && row >= target.row_start
                    && row <= target.row_end
            })
            .copied()
        else {
            return;
        };
        self.selected_block = Some(target.selection);
        self.selected_entry = Some(match target.selection {
            ToolSelection::Tool(index) => index,
            ToolSelection::Group { start, .. } => start,
        });
        self.toggle_tool_details();
    }

    fn active_tool_activity(&self) -> Option<String> {
        self.transcript.iter().rev().find_map(|entry| match entry {
            TranscriptEntry::Tool(card) if card.status == ToolStatus::Running => {
                Some(card.activity())
            }
            _ => None,
        })
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

/// Runs the TUI until the user exits, restoring terminal state on every return path.
pub async fn run(app: &AgentApp, options: TuiOptions) -> Result<(), String> {
    let panels = app.tui_panels().await?;
    let mut suggestions = app.tui_suggestions().await?;
    suggestions.extend(context_source_suggestions(app).await?);
    validate_snapshot_suggestions(&suggestions)?;
    let mut terminal = TerminalSession::start()?;
    let mut events = EventStream::new();
    let mut state = TuiState::new(&options, panels);
    state.suggestions = suggestions;

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

async fn context_source_suggestions(app: &AgentApp) -> Result<Vec<Suggestion>, String> {
    let snapshot = app.tui_context_sources().await?;
    let mut suggestions = Vec::new();
    for (index, prompt) in snapshot.prompts.into_iter().enumerate() {
        let schema: serde_json::Value = serde_json::from_str(prompt.arguments_schema_json.as_str())
            .map_err(|error| format!("Context Prompt schema is invalid: {error}"))?;
        if schema["required"]
            .as_array()
            .is_some_and(|required| !required.is_empty())
            || !safe_context_token(&prompt.source)
            || !safe_context_token(&prompt.name)
        {
            continue;
        }
        suggestions.push(Suggestion {
            id: format!("mcp.prompt.{index}"),
            kind: SuggestionKind::Prompt,
            label: format!("/prompt:{}/{}", prompt.source, prompt.name),
            insert_text: format!("/mcp-prompt {}/{}", prompt.source, prompt.name),
            description: prompt.description,
        });
    }
    for (index, resource) in snapshot.resources.into_iter().enumerate() {
        if !safe_context_token(&resource.source) || resource.uri.contains(char::is_whitespace) {
            continue;
        }
        suggestions.push(Suggestion {
            id: format!("mcp.resource.{index}"),
            kind: SuggestionKind::Resource,
            label: format!("/resource:{}/{}", resource.source, resource.name),
            insert_text: format!("/mcp-resource {}={}", resource.source, resource.uri),
            description: resource.description,
        });
    }
    Ok(suggestions)
}

fn safe_context_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
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
        sync_user_interaction(state).await;
        if state.active.is_none() {
            if state.phase == UiPhase::SubmitRequested {
                submit(app, options, state).await?;
            } else if let Some(input) = state.queued_inputs.pop_front() {
                state.set_input(input);
                state.phase = UiPhase::SubmitRequested;
                submit(app, options, state).await?;
            }
        }
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
                }
                () = tokio::time::sleep(EVENT_TICK) => {
                    state.animation_tick = state.animation_tick.wrapping_add(1);
                }
            }
        }
    }
}

async fn sync_user_interaction(state: &mut TuiState) {
    if state.active.is_none() {
        state.pending_interaction = None;
        state.interaction_draft = None;
        state.pending_answers = None;
        return;
    }

    if let (Some(interaction), Some(answers)) = (
        state.pending_interaction.clone(),
        state.pending_answers.take(),
    ) {
        let result = {
            let active = state.active.as_ref().expect("active Turn checked");
            active
                .lease
                .answer_interaction(interaction.interaction_id.clone(), answers.clone())
                .await
        };
        finish_interaction_submission(state, result);
    }

    if state.pending_interaction.is_some() || Instant::now() < state.next_interaction_poll {
        return;
    }
    state.next_interaction_poll = Instant::now() + ACTIVE_TICK;
    let result = {
        let active = state.active.as_ref().expect("active Turn checked");
        active.lease.pending_interactions().await
    };
    match result {
        Ok(interactions) => {
            state.interaction_poll_status = InteractionPollStatus::Ready;
            if let Some(interaction) = interactions.into_iter().next() {
                state.interaction_draft = Some(InteractionDraft::new(&interaction));
                state.pending_interaction = Some(interaction);
                state.focus = Focus::Prompt;
            }
        }
        Err(error) => {
            state.next_interaction_poll = Instant::now() + Duration::from_secs(2);
            if state.interaction_poll_status == InteractionPollStatus::Ready {
                state.transcript.push(TranscriptEntry::Error {
                    text: format!("Could not read pending user questions: {error}"),
                });
                state.interaction_poll_status = InteractionPollStatus::ErrorReported;
            }
        }
    }
}

fn finish_interaction_submission(state: &mut TuiState, result: Result<(), String>) {
    match result {
        Ok(()) => {
            // This resolves the blocked ask_user Tool call. It is not a new
            // conversational prompt, so it must not become a User entry.
            state.pending_interaction = None;
            state.interaction_draft = None;
            state.next_interaction_poll = Instant::now() + ACTIVE_TICK;
        }
        Err(error) => state.transcript.push(TranscriptEntry::Error {
            text: format!("Answer was not accepted: {error}"),
        }),
    }
}

async fn present_online_generation_events(app: &AgentApp, state: &mut TuiState) {
    for event in app.take_online_generation_events() {
        match event {
            OnlineGenerationEvent::Switched { .. } => {
                match app.tui_panels().await {
                    Ok(panels) => {
                        state.panels = panels;
                        state.selected_panel = state
                            .selected_panel
                            .min(state.panels.len().saturating_sub(1));
                    }
                    Err(error) => state.transcript.push(TranscriptEntry::Error {
                        text: format!(
                            "Plugin changes were applied, but the interface could not refresh: {error}"
                        ),
                    }),
                }
                state.push_system("Plugin changes applied.".to_owned());
            }
            OnlineGenerationEvent::Rejected { detail, .. } => state.transcript.push(TranscriptEntry::Error {
                text: format!(
                    "Plugin changes were not loaded; the current plugins remain active: {detail}"
                ),
            }),
            OnlineGenerationEvent::RolledBack { detail, .. } => {
                match app.tui_panels().await {
                    Ok(panels) => {
                        state.panels = panels;
                        state.selected_panel = state
                            .selected_panel
                            .min(state.panels.len().saturating_sub(1));
                    }
                    Err(error) => state.transcript.push(TranscriptEntry::Error {
                        text: format!(
                            "The previous plugins were restored, but the interface could not refresh: {error}"
                        ),
                    }),
                }
                state.transcript.push(TranscriptEntry::Error {
                    text: format!(
                        "A plugin change failed and the previous plugins were restored: {detail}"
                    ),
                });
            }
            OnlineGenerationEvent::Failed { detail, .. } => state.transcript.push(TranscriptEntry::Error {
                text: format!(
                    "A plugin change failed and no working previous setup was available; new requests are paused: {detail}"
                ),
            }),
            OnlineGenerationEvent::WatchDegraded { detail } => {
                state.transcript.push(TranscriptEntry::Error {
                    text: format!(
                        "Plugin folder watching encountered a problem; periodic scanning remains active: {detail}"
                    ),
                });
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

fn handle_mouse_click(
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

fn safe_link_target(url: &str) -> bool {
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

fn handle_mouse_move(position: ratatui::layout::Position, state: &mut TuiState) {
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
        KeyCode::BackTab if !state.panels.is_empty() => {
            if state.panel_open {
                state.selected_panel = (state.selected_panel + 1) % state.panels.len();
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

fn handle_control_key(code: KeyCode, state: &mut TuiState) {
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

fn handle_interaction_key(key: KeyEvent, state: &mut TuiState) {
    let Some(interaction) = state.pending_interaction.clone() else {
        return;
    };
    let Some(question_index) = state
        .interaction_draft
        .as_ref()
        .map(|draft| draft.question_index)
    else {
        return;
    };
    let Some(question) = interaction.questions.get(question_index) else {
        return;
    };
    if state
        .interaction_draft
        .as_ref()
        .is_some_and(|draft| draft.editing_other)
    {
        handle_interaction_other_key(key, state, &interaction, question.multi_select);
        return;
    }
    let Some(draft) = state.interaction_draft.as_mut() else {
        return;
    };

    let item_count = question.options.len() + 1;
    match key.code {
        KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
            draft.set_option_cursor(
                draft
                    .option_cursor()
                    .checked_sub(1)
                    .unwrap_or(item_count - 1),
            );
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
            draft.set_option_cursor((draft.option_cursor() + 1) % item_count);
        }
        KeyCode::Left | KeyCode::Char('h' | '[') => {
            draft.question_index = draft
                .question_index
                .checked_sub(1)
                .unwrap_or_else(|| interaction.questions.len().saturating_sub(1));
        }
        KeyCode::Right | KeyCode::Char('l' | ']') => {
            draft.question_index = (draft.question_index + 1) % interaction.questions.len();
        }
        KeyCode::Char(' ') => toggle_focused_interaction_option(draft, question),
        KeyCode::Char('z') => {
            draft.set_option_cursor(question.options.len());
            draft.other_input = draft.other[draft.question_index]
                .clone()
                .unwrap_or_default();
            draft.editing_other = true;
        }
        KeyCode::Char(character) => {
            if select_interaction_shortcut(draft, question, character) {
                advance_interaction_question(state, &interaction);
            }
        }
        KeyCode::Enter => {
            if let Some(option) = question.options.get(draft.option_cursor()) {
                if question.multi_select {
                    if !draft.selected[draft.question_index].is_empty()
                        || draft.other[draft.question_index].is_some()
                    {
                        advance_interaction_question(state, &interaction);
                    }
                } else {
                    draft.selected[draft.question_index].clear();
                    draft.selected[draft.question_index].insert(option.option_id.clone());
                    draft.other[draft.question_index] = None;
                    advance_interaction_question(state, &interaction);
                }
            } else {
                draft.other_input = draft.other[draft.question_index]
                    .clone()
                    .unwrap_or_default();
                draft.editing_other = true;
            }
        }
        KeyCode::Esc => state.focus = Focus::Scrollback,
        _ => {}
    }
}

fn toggle_focused_interaction_option(draft: &mut InteractionDraft, question: &InteractionQuestion) {
    if let Some(option) = question.options.get(draft.option_cursor()) {
        let selected = &mut draft.selected[draft.question_index];
        if question.multi_select {
            if !selected.insert(option.option_id.clone()) {
                selected.remove(&option.option_id);
            }
        } else if selected.contains(&option.option_id) {
            selected.clear();
        } else {
            selected.clear();
            selected.insert(option.option_id.clone());
        }
    } else {
        draft.other_input = draft.other[draft.question_index]
            .clone()
            .unwrap_or_default();
        draft.editing_other = true;
    }
}

fn select_interaction_shortcut(
    draft: &mut InteractionDraft,
    question: &InteractionQuestion,
    character: char,
) -> bool {
    let Some(index) =
        interaction_option_index(character).filter(|index| *index < question.options.len())
    else {
        return false;
    };
    draft.set_option_cursor(index);
    let option = &question.options[index];
    if question.multi_select {
        let selected = &mut draft.selected[draft.question_index];
        if !selected.insert(option.option_id.clone()) {
            selected.remove(&option.option_id);
        }
        false
    } else {
        draft.selected[draft.question_index].clear();
        draft.selected[draft.question_index].insert(option.option_id.clone());
        draft.other[draft.question_index] = None;
        true
    }
}

fn handle_interaction_other_key(
    key: KeyEvent,
    state: &mut TuiState,
    interaction: &PendingInteraction,
    multi_select: bool,
) {
    let Some(draft) = state.interaction_draft.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Esc => {
            draft.editing_other = false;
            draft.other_input.clear();
        }
        KeyCode::Backspace => {
            draft.other_input.pop();
        }
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            append_interaction_other(state, "\n");
        }
        KeyCode::Enter if !draft.other_input.trim().is_empty() => {
            draft.other[draft.question_index] = Some(draft.other_input.trim().to_owned());
            draft.other_input.clear();
            draft.editing_other = false;
            if !multi_select {
                advance_interaction_question(state, interaction);
            }
        }
        KeyCode::Char(character) => append_interaction_other(state, &character.to_string()),
        _ => {}
    }
}

fn interaction_option_index(character: char) -> Option<usize> {
    match character {
        '1'..='9' => Some(usize::from(character as u8 - b'1')),
        'a'..='y' => Some(9 + usize::from(character as u8 - b'a')),
        _ => None,
    }
}

fn append_interaction_other(state: &mut TuiState, text: &str) {
    let Some(draft) = state.interaction_draft.as_mut() else {
        return;
    };
    let remaining = 4_096usize.saturating_sub(draft.other_input.chars().count());
    draft.other_input.extend(text.chars().take(remaining));
}

fn advance_interaction_question(state: &mut TuiState, interaction: &PendingInteraction) {
    let Some(draft) = state.interaction_draft.as_mut() else {
        return;
    };
    if draft.question_index + 1 < interaction.questions.len() {
        draft.question_index += 1;
        return;
    }
    state.pending_answers = Some(
        interaction
            .questions
            .iter()
            .enumerate()
            .map(|(index, question)| InteractionAnswer {
                question_id: question.question_id.clone(),
                selected_option_ids: draft.selected[index].iter().cloned().collect(),
                other: Some(draft.other[index].clone()),
            })
            .collect(),
    );
}

async fn submit(app: &AgentApp, options: &TuiOptions, state: &mut TuiState) -> Result<(), String> {
    let started_at = Instant::now();
    let input = state.take_input();
    if input.chars().count() > MAX_INPUT_CHARACTERS {
        return Err(format!(
            "Agent input exceeds the {MAX_INPUT_CHARACTERS}-character limit"
        ));
    }
    match input.trim() {
        "/help" => {
            state.show_shortcuts = true;
            state.phase = UiPhase::Idle;
            return Ok(());
        }
        "/clear" => {
            state.transcript.clear();
            state.scroll = ScrollState::default();
            state.phase = UiPhase::Idle;
            return Ok(());
        }
        "/new" => {
            state.transcript.clear();
            state.session_id = None;
            state.scroll = ScrollState::default();
            state.phase = UiPhase::Idle;
            return Ok(());
        }
        _ => {}
    }
    if let Some(title) = rename_command(&input)? {
        let session_id = state
            .session_id
            .clone()
            .ok_or_else(|| "Start the Session before renaming it".to_owned())?;
        let lease = app.lease_tui_turn().await?;
        let current = lease.read_session(session_id.clone(), 0, 1).await?;
        let renamed = lease
            .rename_session(
                session_id,
                title.to_owned(),
                current.title_revision.unwrap_or_else(|| "0".to_owned()),
            )
            .await
            .map_err(|error| format!("Session rename failed: {error:?}"))?;
        state.transcript.push(TranscriptEntry::System {
            text: format!("Session renamed to ‘{}’", renamed.title),
        });
        state.phase = UiPhase::Idle;
        return Ok(());
    }
    let model_input = compose_tui_context(app, &input).await?;
    state.transcript.push(TranscriptEntry::User {
        text: input.clone(),
        created_at: current_timestamp(),
    });
    state.start_provisional_thinking();
    if state.input_history.last() != Some(&input) {
        state.input_history.push(input.clone());
    }
    state.phase = UiPhase::Active;
    state.scroll.begin_page_flip();

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
                input: model_input,
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
        lease,
        started_at,
    });
    Ok(())
}

fn rename_command(input: &str) -> Result<Option<&str>, String> {
    let input = input.trim();
    if input == "/rename" {
        return Err("Usage: /rename <title>".to_owned());
    }
    let Some(title) = input.strip_prefix("/rename ") else {
        return Ok(None);
    };
    let title = title.trim();
    if title.is_empty() {
        Err("Usage: /rename <title>".to_owned())
    } else {
        Ok(Some(title))
    }
}

async fn compose_tui_context(app: &AgentApp, input: &str) -> Result<String, String> {
    if let Some(selection) = input.strip_prefix("/mcp-prompt ") {
        let (identity, task) = selection.split_once(char::is_whitespace).ok_or_else(|| {
            "an MCP Prompt selection must be followed by the user task".to_owned()
        })?;
        let (source, name) = identity
            .split_once('/')
            .ok_or_else(|| "invalid MCP Prompt source/name".to_owned())?;
        let rendered = app
            .render_tui_context_prompt(RenderPromptRequest {
                source: source.to_owned(),
                name: name.to_owned(),
                arguments_json: "{}"
                    .to_owned()
                    .try_into()
                    .expect("empty JSON object is valid"),
            })
            .await?;
        let messages = rendered
            .messages
            .into_iter()
            .map(|message| {
                let role = match message.role {
                    ContextRole::User => "user",
                    ContextRole::Assistant => "assistant",
                };
                format!("[{role}]\n{}", message.text)
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        return Ok(format!(
            "Selected Context Prompt: {source}/{name}\n{messages}\n\n---\n\nUser task:\n{}",
            task.trim()
        ));
    }
    if let Some(selection) = input.strip_prefix("/mcp-resource ") {
        let (identity, task) = selection.split_once(char::is_whitespace).ok_or_else(|| {
            "an MCP Resource selection must be followed by the user task".to_owned()
        })?;
        let (source, uri) = identity
            .split_once('=')
            .ok_or_else(|| "invalid MCP Resource source=URI".to_owned())?;
        let response = app
            .read_tui_context_resource(ReadResourceRequest {
                source: source.to_owned(),
                uri: uri.to_owned(),
            })
            .await?;
        let contents = response
            .contents
            .into_iter()
            .map(|content| {
                format!(
                    "URI: {}\nMIME: {}\n{}",
                    content.uri, content.mime_type, content.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        return Ok(format!(
            "Selected Context Resource: {source}/{uri}\n{contents}\n\n---\n\nUser task:\n{}",
            task.trim()
        ));
    }
    Ok(input.to_owned())
}

fn handle_stream_event(
    event: Result<StreamEvent<RunTurnResponse, RunTurnError>, lenso_kernel::RuntimeFailure>,
    state: &mut TuiState,
) {
    let event = match event {
        Ok(event) => event,
        Err(error) => {
            state.active = None;
            state.pending_interaction = None;
            state.interaction_draft = None;
            state.pending_answers = None;
            state.finish_active_thinking();
            state.transcript.push(TranscriptEntry::Error {
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
                RunTurnResponseKind::ReasoningDelta => state.append_reasoning(message),
                RunTurnResponseKind::ReasoningCompleted => state.complete_reasoning(message),
                RunTurnResponseKind::TextDelta => state.append_agent_text(&message.text),
                RunTurnResponseKind::ToolStarted => state.start_tool(message),
                RunTurnResponseKind::ToolProgress => state.append_tool_progress(message),
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
            state.finish_active_thinking();
            let elapsed = state
                .active
                .take()
                .map_or(Duration::ZERO, |active| active.started_at.elapsed());
            state
                .transcript
                .push(TranscriptEntry::TurnCompleted { elapsed });
            state.pending_interaction = None;
            state.interaction_draft = None;
            state.pending_answers = None;
            state.phase = UiPhase::Idle;
        }
        StreamEvent::Terminal(Err(error)) => {
            state.finish_active_thinking();
            state.active = None;
            state.pending_interaction = None;
            state.interaction_draft = None;
            state.pending_answers = None;
            state.transcript.push(TranscriptEntry::Error {
                text: format!("Agent turn failed: {error:?}"),
            });
            state.phase = UiPhase::Failed;
        }
    }
}

fn runtime_failure_message(error: lenso_kernel::RuntimeFailure) -> String {
    match error {
        lenso_kernel::RuntimeFailure::PluginFailure { detail } => {
            format!("Turn stopped — {detail}")
        }
        error => format!("Turn stopped — {error:?}"),
    }
}

fn render(frame: &mut Frame<'_>, state: &mut TuiState) {
    frame.render_widget(
        Block::default().style(
            Style::default()
                .fg(Palette::SURFACE_TEXT)
                .bg(Palette::BG_BASE),
        ),
        frame.area(),
    );
    let area = content_area(frame.area());
    let compact = area.height <= 16;
    let input_width = area.width.saturating_sub(4).max(1);
    let input_rows = visual_input_rows(&state.input, usize::from(input_width));
    let regular_composer_height = if compact {
        3
    } else {
        u16::try_from(input_rows.saturating_add(2))
            .unwrap_or(u16::MAX)
            .clamp(3, area.height.saturating_div(2).max(3))
    };
    let activity_height = u16::from(state.phase != UiPhase::Idle || !state.scroll.follow_tail);
    let queue_height = u16::try_from(state.queued_inputs.len().min(MAX_VISIBLE_QUEUE_ROWS))
        .unwrap_or(MAX_VISIBLE_QUEUE_HEIGHT);
    let interaction_open = state.pending_interaction.is_some();
    let composer_height = if interaction_open {
        interaction_card_height(state, area.height)
    } else {
        regular_composer_height
    };
    let suggestion_height = if interaction_open {
        0
    } else {
        state.suggestion_match().map_or(0, |matches| {
            u16::try_from(matches.indices.len().min(MAX_VISIBLE_SUGGESTIONS) + 2)
                .unwrap_or(8)
                .min(if compact { 5 } else { 8 })
        })
    };
    let prompt_gap_height = u16::from(!interaction_open && suggestion_height == 0);
    let [
        header,
        body,
        queue,
        activity,
        _prompt_gap,
        suggestions,
        composer,
        status,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(queue_height),
        Constraint::Length(activity_height),
        Constraint::Length(prompt_gap_height),
        Constraint::Length(suggestion_height),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, header, state);

    if !state.panel_open || state.panels.is_empty() || body.width < PANEL_BREAKPOINT {
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

    render_queue(frame, queue, state);
    render_activity(frame, activity, state);
    if interaction_open {
        render_interaction_card(frame, composer, state);
    } else {
        state.interaction_hit_targets.clear();
        render_suggestions(frame, suggestions, state);
        render_composer(frame, composer, state);
    }
    render_status_line(frame, status, state);
    if state.show_shortcuts {
        render_shortcuts_overlay(frame, area);
    }
}

fn interaction_card_height(state: &TuiState, screen_height: u16) -> u16 {
    let Some(question) = state
        .pending_interaction
        .as_ref()
        .zip(state.interaction_draft.as_ref())
        .and_then(|(interaction, draft)| interaction.questions.get(draft.question_index))
    else {
        return 0;
    };
    let option_rows = u16::try_from(question.options.len().saturating_add(1)).unwrap_or(u16::MAX);
    let body_cap = screen_height
        .saturating_mul(33)
        .saturating_div(100)
        .max(8)
        .min(screen_height.saturating_mul(80).saturating_div(100));
    option_rows
        .saturating_add(6)
        .max(8)
        .min(body_cap)
        .saturating_add(2)
}

fn render_interaction_card(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
    state.interaction_hit_targets.clear();
    let (Some(interaction), Some(draft)) = (
        state.pending_interaction.as_ref(),
        state.interaction_draft.as_ref(),
    ) else {
        return;
    };
    let Some(question) = interaction.questions.get(draft.question_index) else {
        return;
    };
    if area.width < 8 || area.height < 4 {
        return;
    }
    frame.render_widget(
        Block::default().style(Style::default().bg(Palette::SURFACE)),
        area,
    );
    for row in area.y..area.bottom() {
        frame.render_widget(
            Paragraph::new("┃").style(Style::default().fg(Palette::ACCENT).bg(Palette::SURFACE)),
            Rect::new(area.x, row, 1, 1),
        );
    }
    let content = Rect::new(
        area.x.saturating_add(3),
        area.y.saturating_add(1),
        area.width.saturating_sub(5),
        area.height.saturating_sub(3),
    );

    let preview = (!question.multi_select)
        .then(|| question.options.get(draft.option_cursor()))
        .flatten()
        .and_then(|option| option.preview.as_ref().and_then(Option::as_deref));
    let option_reserve = u16::try_from(question.options.len().saturating_add(1))
        .unwrap_or(u16::MAX)
        .min(content.height.saturating_sub(1))
        .max(1);
    let chrome_budget = content.height.saturating_sub(option_reserve).max(1);
    let prompt_height = u16::try_from(visual_input_rows(
        &question.prompt,
        usize::from(content.width.max(1)),
    ))
    .unwrap_or(u16::MAX)
    .clamp(1, chrome_budget);
    let preview_height = preview.map_or(0, |text| {
        u16::try_from(visual_input_rows(text, usize::from(content.width.max(1))))
            .unwrap_or(u16::MAX)
            .min(chrome_budget.saturating_sub(prompt_height))
    });
    let [prompt_area, preview_area, options_area] = Layout::vertical([
        Constraint::Length(prompt_height),
        Constraint::Length(preview_height),
        Constraint::Min(1),
    ])
    .areas(content);
    frame.render_widget(
        Paragraph::new(question.prompt.as_str())
            .style(
                Style::default()
                    .fg(Palette::SURFACE_TEXT)
                    .bg(Palette::SURFACE)
                    .add_modifier(Modifier::BOLD),
            )
            .wrap(Wrap { trim: false }),
        prompt_area,
    );
    if let Some(preview) = preview {
        render_interaction_preview(frame, preview_area, preview);
    }
    let interaction_hit_targets = render_interaction_choices(frame, options_area, question, draft);
    let footer = Rect::new(
        area.x.saturating_add(3),
        area.bottom().saturating_sub(1),
        area.width.saturating_sub(5),
        1,
    );
    render_interaction_help(frame, footer, interaction.questions.len(), question, draft);
    state.interaction_hit_targets = interaction_hit_targets;
}

fn render_interaction_choices(
    frame: &mut Frame<'_>,
    area: Rect,
    question: &InteractionQuestion,
    draft: &InteractionDraft,
) -> Vec<InteractionHitTarget> {
    let visible_option_rows = usize::from(area.height.saturating_sub(1));
    let focused_option = draft
        .option_cursor()
        .min(question.options.len().saturating_sub(1));
    let option_start = focused_option
        .saturating_add(1)
        .saturating_sub(visible_option_rows)
        .min(question.options.len().saturating_sub(visible_option_rows));
    let lines = question
        .options
        .iter()
        .enumerate()
        .skip(option_start)
        .take(visible_option_rows)
        .map(|(index, option)| interaction_option_line(question, draft, index, option))
        .collect::<Vec<_>>();
    let [options, other] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(u16::from(area.height > 0)),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(Palette::SURFACE)),
        options,
    );
    let mut hit_targets = Vec::with_capacity(question.options.len().saturating_add(1));
    let mut option_y = options.y;
    for (index, _) in question
        .options
        .iter()
        .enumerate()
        .skip(option_start)
        .take(visible_option_rows)
    {
        if option_y >= options.bottom() {
            break;
        }
        hit_targets.push(InteractionHitTarget {
            area: Rect::new(options.x, option_y, options.width, 1),
            action: InteractionHitAction::Option(index),
        });
        option_y = option_y.saturating_add(1);
    }
    let other_focused = draft.option_cursor() == question.options.len();
    let other_value = if draft.editing_other {
        format!("❯ {}", draft.other_input)
    } else {
        draft.other[draft.question_index]
            .as_deref()
            .map_or_else(|| "Type your answer here".to_owned(), ToOwned::to_owned)
    };
    let other_selected = draft.other[draft.question_index].is_some();
    let other_line = Line::from(vec![
        Span::styled(
            format!(
                "z {} ",
                if question.multi_select && other_selected {
                    "[x]"
                } else if !question.multi_select && other_selected {
                    "(●)"
                } else if question.multi_select {
                    "[ ]"
                } else {
                    "(○)"
                }
            ),
            Style::default().fg(if other_focused {
                Palette::ACCENT
            } else {
                Palette::MUTED
            }),
        ),
        Span::styled(other_value, Style::default().fg(Palette::SURFACE_TEXT)),
    ])
    .style(Style::default().bg(if other_focused {
        Palette::VISUAL_SURFACE
    } else {
        Palette::SURFACE
    }));
    frame.render_widget(
        Paragraph::new(other_line).style(Style::default().bg(Palette::SURFACE)),
        other,
    );
    if other.height > 0 {
        hit_targets.push(InteractionHitTarget {
            area: other,
            action: InteractionHitAction::Other,
        });
    }
    hit_targets
}

fn interaction_option_line(
    question: &InteractionQuestion,
    draft: &InteractionDraft,
    index: usize,
    option: &lenso_capability_agent_user_interaction::InteractionOption,
) -> Line<'static> {
    let focused = index == draft.option_cursor();
    let selected = draft.selected[draft.question_index].contains(&option.option_id);
    let marker = if question.multi_select {
        if selected { "[x]" } else { "[ ]" }
    } else if selected {
        "(●)"
    } else {
        "(○)"
    };
    Line::from(vec![
        Span::styled(
            format!("{} {marker} ", interaction_shortcut(index)),
            Style::default().fg(if focused {
                Palette::ACCENT
            } else {
                Palette::MUTED
            }),
        ),
        Span::styled(
            option.label.clone(),
            Style::default()
                .fg(Palette::SURFACE_TEXT)
                .add_modifier(if focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
        Span::styled(
            if option.description.is_empty() {
                String::new()
            } else {
                format!("  {}", option.description)
            },
            Style::default().fg(Palette::MUTED),
        ),
    ])
    .style(Style::default().bg(if focused {
        Palette::VISUAL_SURFACE
    } else {
        Palette::SURFACE
    }))
}

fn interaction_shortcut(index: usize) -> char {
    match index {
        0..=8 => char::from(b'1' + u8::try_from(index).unwrap_or_default()),
        9..=34 => char::from(b'a' + u8::try_from(index - 9).unwrap_or_default()),
        _ => ' ',
    }
}

fn render_interaction_help(
    frame: &mut Frame<'_>,
    area: Rect,
    question_count: usize,
    question: &InteractionQuestion,
    draft: &InteractionDraft,
) {
    let help = if draft.editing_other {
        "Shift+Enter newline"
    } else if question.multi_select {
        "↑/↓ navigate · Space toggle"
    } else {
        "↑/↓ navigate"
    };
    let counter = if question_count > 1 {
        format!(
            "[{}/{}] {help} · ←/→ question",
            draft.question_index + 1,
            question_count
        )
    } else {
        help.to_owned()
    };
    let action = if draft.editing_other {
        "Enter:save"
    } else if draft.question_index + 1 == question_count {
        "Enter:submit"
    } else {
        "Enter:select"
    };
    let action_width = u16::try_from(action.len())
        .unwrap_or(u16::MAX)
        .min(area.width);
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(action_width)]).areas(area);
    frame.render_widget(
        Paragraph::new(counter).style(Style::default().fg(Palette::MUTED).bg(Palette::SURFACE)),
        left,
    );
    frame.render_widget(
        Paragraph::new(action)
            .alignment(ratatui::layout::Alignment::Right)
            .style(Style::default().fg(Palette::ACCENT).bg(Palette::BG_BASE)),
        right,
    );
}

fn render_interaction_preview(frame: &mut Frame<'_>, area: Rect, preview: &str) {
    frame.render_widget(
        Paragraph::new(preview)
            .style(Style::default().fg(Palette::MUTED).bg(Palette::SURFACE))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_queue(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
    state.queue_hit_targets.clear();
    if area.height == 0 || state.queued_inputs.is_empty() {
        state.queue_hovered = None;
        return;
    }
    let left = ENTRY_ACCENT_WIDTH
        .saturating_add(ENTRY_PAD_LEFT)
        .saturating_sub(1);
    let inner = Rect {
        x: area
            .x
            .saturating_add(u16::try_from(left).unwrap_or(u16::MAX)),
        width: area
            .width
            .saturating_sub(u16::try_from(left).unwrap_or(u16::MAX)),
        ..area
    };
    let visible = state
        .queued_inputs
        .len()
        .saturating_sub(MAX_VISIBLE_QUEUE_ROWS);
    for (row, (index, input)) in state
        .queued_inputs
        .iter()
        .enumerate()
        .skip(visible)
        .enumerate()
    {
        let Ok(row) = u16::try_from(row) else {
            break;
        };
        if row >= inner.height {
            break;
        }
        let row_area = Rect {
            y: inner.y.saturating_add(row),
            height: 1,
            ..inner
        };
        let hovered = state.queue_hovered == Some(index);
        state
            .queue_hit_targets
            .push(render_queue_row(frame, row_area, index, input, hovered));
    }
}

fn render_queue_row(
    frame: &mut Frame<'_>,
    area: Rect,
    index: usize,
    input: &str,
    hovered: bool,
) -> QueueHitTarget {
    if hovered {
        frame.render_widget(
            Block::default().style(Style::default().bg(Palette::SURFACE)),
            area,
        );
    }
    let line_count = input.lines().count().max(1);
    let first_line = input
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let suffix = match line_count.saturating_sub(1) {
        0 => String::new(),
        1 => " (+1 line)".to_owned(),
        count => format!(" (+{count} lines)"),
    };
    let prefix = format!("#{} ", index + 1);
    let actions_width = if hovered { 14 } else { 0 };
    let available = usize::from(area.width)
        .saturating_sub(Line::from(prefix.as_str()).width())
        .saturating_sub(Line::from(suffix.as_str()).width())
        .saturating_sub(actions_width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prefix, Style::default().fg(Palette::MUTED)),
            Span::styled(
                truncate_text(first_line, available),
                Style::default().fg(Palette::USER_ACCENT),
            ),
            Span::styled(suffix, Style::default().fg(Palette::MUTED)),
        ])),
        area,
    );
    let (edit, cancel) = render_queue_actions(frame, area, hovered);
    QueueHitTarget {
        area,
        index,
        edit,
        cancel,
    }
}

fn render_queue_actions(
    frame: &mut Frame<'_>,
    area: Rect,
    hovered: bool,
) -> (Option<Rect>, Option<Rect>) {
    if !hovered || area.width < 14 {
        return (None, None);
    }
    let cancel = Rect {
        x: area.right().saturating_sub(8),
        width: 8,
        ..area
    };
    let edit = Rect {
        x: cancel.x.saturating_sub(6),
        width: 6,
        ..area
    };
    frame.render_widget(
        Paragraph::new("[edit]").style(Style::default().fg(Palette::MUTED)),
        edit,
    );
    frame.render_widget(
        Paragraph::new("[cancel]").style(Style::default().fg(Palette::MUTED)),
        cancel,
    );
    (Some(edit), Some(cancel))
}

fn render_suggestions(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
    state.suggestion_hit_targets.clear();
    if area.height == 0 {
        return;
    }
    let Some(matches) = state.suggestion_match() else {
        return;
    };
    let visible_rows = usize::from(area.height.saturating_sub(2));
    let selected = state.suggestion_selected.min(matches.indices.len() - 1);
    let scroll = state
        .suggestion_scroll
        .min(matches.indices.len().saturating_sub(visible_rows));
    let items_area = render_suggestion_chrome(frame, area, matches.indices.len());
    let label_budget = usize::from(items_area.width.saturating_sub(2))
        .saturating_mul(3)
        .saturating_div(5)
        .min(40);
    let label_width = matches
        .indices
        .iter()
        .map(|index| Line::from(state.suggestions[*index].label.as_str()).width())
        .max()
        .unwrap_or_default()
        .min(label_budget);
    for (offset, index) in matches
        .indices
        .iter()
        .skip(scroll)
        .take(visible_rows)
        .enumerate()
    {
        let suggestion = &state.suggestions[*index];
        let is_selected = scroll + offset == selected;
        let marker = if is_selected { "❯ " } else { "  " };
        let displayed_label = suggestion
            .label
            .chars()
            .take(label_width)
            .collect::<String>();
        let padding = label_width.saturating_sub(Line::from(displayed_label.as_str()).width());
        let mut spans = vec![Span::styled(
            format!("{marker}{displayed_label}{}", " ".repeat(padding)),
            Style::default()
                .fg(if is_selected {
                    Palette::SURFACE_TEXT
                } else {
                    Palette::MUTED
                })
                .add_modifier(if is_selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        )];
        if items_area.width >= 24 {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                suggestion.description.clone(),
                Style::default().fg(Palette::MUTED),
            ));
        }
        let row_area = Rect {
            y: items_area
                .y
                .saturating_add(u16::try_from(offset).unwrap_or(u16::MAX)),
            height: 1,
            ..items_area
        };
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(if is_selected {
                Palette::VISUAL_SURFACE
            } else {
                Palette::USER_SURFACE
            })),
            row_area,
        );
        state.suggestion_hit_targets.push(SuggestionHitTarget {
            area: row_area,
            selection: scroll + offset,
        });
    }
}

fn render_suggestion_chrome(frame: &mut Frame<'_>, area: Rect, item_count: usize) -> Rect {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(Palette::USER_SURFACE)),
        area,
    );
    let border = "─".repeat(usize::from(area.width));
    let border_style = Style::default()
        .fg(Palette::USER_SURFACE)
        .bg(Palette::BG_BASE);
    frame.render_widget(
        Paragraph::new(Span::styled(border.clone(), border_style)),
        Rect { height: 1, ..area },
    );
    frame.render_widget(
        Paragraph::new(Span::styled(border, border_style)),
        Rect {
            y: area.bottom().saturating_sub(1),
            height: 1,
            ..area
        },
    );
    let count = item_count.to_string();
    let count_width = u16::try_from(count.len()).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(Span::styled(
            count,
            Style::default().fg(Palette::MUTED).bg(Palette::BG_BASE),
        )),
        Rect {
            x: area.right().saturating_sub(count_width).saturating_sub(1),
            width: count_width,
            height: 1,
            ..area
        },
    );
    Rect {
        x: area.x.saturating_add(2),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

fn content_area(area: Rect) -> Rect {
    let horizontal = if area.width >= 40 { 2 } else { 1 };
    let vertical = u16::from(area.height >= 18);
    Rect {
        x: area.x.saturating_add(horizontal),
        y: area.y.saturating_add(vertical),
        width: area.width.saturating_sub(horizontal.saturating_mul(2)),
        height: area.height.saturating_sub(vertical.saturating_mul(2)),
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let right_label = state.tool_scope.clone();
    let right_width = u16::try_from(Line::from(right_label.as_str()).width())
        .unwrap_or(u16::MAX)
        .min(area.width.saturating_div(2));
    let [workspace_area, session_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);
    let mut workspace_spans = Vec::new();
    if let Some(branch) = state.branch.as_deref() {
        workspace_spans.push(Span::styled(
            format!("⎇ {branch} "),
            Style::default()
                .fg(Palette::MUTED)
                .add_modifier(Modifier::DIM),
        ));
    }
    if state.workspace.contains("/.worktrees/") {
        workspace_spans.push(Span::styled(
            "worktree ",
            Style::default().fg(Palette::USER_ACCENT),
        ));
    }
    workspace_spans.push(Span::styled(
        state.workspace.as_str(),
        Style::default().fg(Palette::QUIET),
    ));
    frame.render_widget(Paragraph::new(Line::from(workspace_spans)), workspace_area);
    frame.render_widget(
        Paragraph::new(right_label)
            .alignment(ratatui::layout::Alignment::Right)
            .style(Style::default().fg(Palette::MUTED)),
        session_area,
    );
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
    let transcript_area = Block::default()
        .padding(Padding::new(0, 0, 1, 0))
        .inner(area);
    let text_area = Rect {
        width: transcript_area.width.saturating_sub(1).max(1),
        ..transcript_area
    };
    let scrollbar_area = Rect {
        x: transcript_area.right().saturating_sub(1),
        width: 1,
        ..transcript_area
    };
    let TranscriptRender {
        lines,
        entry_rows,
        link_rows,
        tool_rows,
        thinking_rows,
        user_rows,
        prompt_anchors,
    } = transcript_lines(state, usize::from(text_area.width));
    let rendered_line_count = visual_rows(&lines, usize::from(text_area.width));
    state
        .scroll
        .update_metrics(rendered_line_count, usize::from(text_area.height));
    state.scroll.apply_page_flip(
        prompt_anchors.last().map(|anchor| anchor.start_row),
        rendered_line_count,
    );
    let sticky_prompt = sticky_prompt(&prompt_anchors, state.scroll.top);
    state.visible_tool_blocks = tool_rows.iter().map(|row| row.selection).collect();
    state.rendered_entry_rows.clone_from(&entry_rows);
    state.tool_hit_targets = visible_tool_targets(
        &tool_rows,
        text_area,
        &state.scroll,
        sticky_prompt.is_some(),
    );
    state.thinking_hit_targets = visible_thinking_targets(
        &thinking_rows,
        text_area,
        &state.scroll,
        sticky_prompt.is_some(),
    );
    state.user_hit_targets = visible_user_targets(
        &user_rows,
        text_area,
        &state.scroll,
        sticky_prompt.is_some(),
    );
    state.link_hit_targets = visible_link_targets(
        &link_rows,
        text_area,
        &state.scroll,
        sticky_prompt.is_some(),
    );
    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    let scroll = state.scroll.top.try_into().unwrap_or(u16::MAX);
    frame.render_widget(paragraph.scroll((scroll, 0)), text_area);
    if let Some(prompt) = sticky_prompt {
        let mut sticky = Vec::new();
        append_entry_lines(
            &mut sticky,
            vec![Line::from(vec![
                Span::styled("❯ ", Style::default().fg(Palette::USER_ACCENT)),
                Span::styled(
                    prompt.lines().next().unwrap_or_default().to_owned(),
                    Style::default()
                        .fg(Palette::SURFACE_TEXT)
                        .add_modifier(Modifier::BOLD),
                ),
            ])],
            usize::from(text_area.width),
            EntryChrome {
                background: Some(Palette::USER_SURFACE),
                ..EntryChrome::plain()
            },
        );
        frame.render_widget(
            Paragraph::new(sticky.into_iter().next().unwrap_or_default()),
            Rect {
                height: 1,
                ..text_area
            },
        );
    }
    update_visible_entry_targets(state, &entry_rows, text_area, sticky_prompt.is_some());
    render_hovered_entry(frame, state, text_area);
    render_selected_entry(frame, state, text_area);
    render_transcript_scrollbar(
        frame,
        state,
        scrollbar_area,
        rendered_line_count,
        transcript_area.height,
    );
}

fn update_visible_entry_targets(
    state: &mut TuiState,
    rows: &[RenderedEntryRow],
    text_area: Rect,
    sticky_prompt: bool,
) {
    state.entry_hit_targets = visible_entry_targets(rows, text_area, &state.scroll, sticky_prompt);
}

fn render_transcript_scrollbar(
    frame: &mut Frame<'_>,
    state: &mut TuiState,
    area: Rect,
    content_rows: usize,
    viewport_rows: u16,
) {
    if state.scroll.max_top == 0 {
        state.scrollbar_hit = None;
        state.scrollbar_dragging = false;
        return;
    }
    state.scrollbar_hit = Some(area);
    let mut scrollbar_state = ScrollbarState::new(content_rows)
        .position(state.scroll.top)
        .viewport_content_length(usize::from(viewport_rows));
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .track_style(Style::default().fg(Palette::QUIET))
            .thumb_symbol("┃")
            .thumb_style(Style::default().fg(Palette::MUTED)),
        area,
        &mut scrollbar_state,
    );
}

fn render_selected_entry(frame: &mut Frame<'_>, state: &mut TuiState, text_area: Rect) {
    let target = state.selected_entry.and_then(|selected| {
        state
            .entry_hit_targets
            .iter()
            .find(|target| target.entry_index == selected)
            .copied()
    });
    if let Some(target) = target {
        render_entry_frame(frame, target, text_area, Palette::SELECTION_BORDER);
    }
}

fn render_hovered_entry(frame: &mut Frame<'_>, state: &TuiState, text_area: Rect) {
    let Some(entry_index) = state
        .hovered_entry
        .filter(|hovered| state.selected_entry != Some(*hovered))
    else {
        return;
    };
    let Some(target) = state
        .entry_hit_targets
        .iter()
        .find(|target| target.entry_index == entry_index)
        .copied()
    else {
        return;
    };

    if entry_has_collapsed_header(state, entry_index) {
        render_collapsed_header_hover(frame, target);
    }
    render_entry_frame(frame, target, text_area, Palette::HOVER_BORDER);
}

fn entry_has_collapsed_header(state: &TuiState, entry_index: usize) -> bool {
    match state.transcript.get(entry_index) {
        Some(TranscriptEntry::Thinking(card)) => !card.expanded,
        Some(TranscriptEntry::Tool(card)) => tool_group_at(&state.transcript, entry_index)
            .map_or(!card.expanded, |(_, _)| {
                !state.expanded_groups.contains(&entry_index)
            }),
        _ => false,
    }
}

fn render_collapsed_header_hover(frame: &mut Frame<'_>, target: EntryHitTarget) {
    if target.area.width <= 2 || target.area.height == 0 {
        return;
    }
    let left = target
        .area
        .x
        .saturating_add(u16::try_from(ENTRY_ACCENT_WIDTH).unwrap_or(u16::MAX));
    let right = target
        .area
        .right()
        .saturating_sub(u16::try_from(ENTRY_ACCENT_WIDTH).unwrap_or(u16::MAX));
    let buffer = frame.buffer_mut();
    for y in target.area.y..target.area.bottom() {
        for x in left..right {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_bg(Palette::HOVER_SURFACE);
            }
        }
    }
    let indicator_x = target
        .area
        .x
        .saturating_add(u16::try_from(ENTRY_ACCENT_WIDTH + ENTRY_PAD_LEFT).unwrap_or(u16::MAX));
    if let Some(cell) = buffer.cell_mut((indicator_x, target.area.y)) {
        cell.set_char('›');
    }
}

fn transcript_lines(state: &TuiState, width: usize) -> TranscriptRender {
    let mut rendered = TranscriptRender {
        lines: Vec::new(),
        entry_rows: Vec::new(),
        link_rows: Vec::new(),
        tool_rows: Vec::new(),
        thinking_rows: Vec::new(),
        user_rows: Vec::new(),
        prompt_anchors: Vec::new(),
    };
    let mut entry_index = 0;
    while entry_index < state.transcript.len() {
        entry_index = render_transcript_entry(state, entry_index, width, &mut rendered);
        rendered.lines.push(Line::default());
    }
    rendered
}

fn render_transcript_entry(
    state: &TuiState,
    entry_index: usize,
    width: usize,
    rendered: &mut TranscriptRender,
) -> usize {
    let start_row = visual_rows(&rendered.lines, width);
    match &state.transcript[entry_index] {
        TranscriptEntry::User { text, created_at } => render_user_entry(
            &mut rendered.lines,
            &mut rendered.user_rows,
            &mut rendered.prompt_anchors,
            UserEntryRender {
                text,
                created_at,
                width,
                entry_index,
                expanded: state.expanded_user_entries.contains(&entry_index),
            },
        ),
        TranscriptEntry::Agent { text, created_at } => {
            let first_line = rendered.lines.len();
            append_timestamped_entry_lines(
                &mut rendered.lines,
                markdown_lines_with_width(text, entry_content_width(width))
                    .into_iter()
                    .map(|line| line.style(Style::default().fg(Palette::SECONDARY_TEXT)))
                    .collect(),
                width,
                EntryChrome::plain(),
                created_at,
            );
            collect_markdown_link_rows(
                &rendered.lines[first_line..],
                start_row,
                &markdown::links(text),
                &mut rendered.link_rows,
            );
        }
        TranscriptEntry::Thinking(card) => {
            let mut content = Vec::new();
            render_thinking_block(&mut content, card, state.animation_tick);
            append_entry_lines(
                &mut rendered.lines,
                content,
                width,
                EntryChrome {
                    accent: (card.is_running() && !card.text.is_empty()).then_some(Palette::ACCENT),
                    ..EntryChrome::plain()
                },
            );
            rendered.thinking_rows.push(RenderedThinkingRow {
                start_row,
                end_row: visual_rows(&rendered.lines, width).saturating_sub(1),
                entry_index,
            });
        }
        TranscriptEntry::System { text } => append_entry_lines(
            &mut rendered.lines,
            vec![Line::from(Span::styled(
                text.to_owned(),
                Style::default().fg(Palette::MUTED),
            ))],
            width,
            EntryChrome::plain(),
        ),
        TranscriptEntry::Error { text } => append_entry_lines(
            &mut rendered.lines,
            vec![Line::from(vec![
                Span::styled("× ", Style::default().fg(Palette::ERROR)),
                Span::styled(text.to_owned(), Style::default().fg(Palette::ERROR)),
            ])],
            width,
            EntryChrome::plain(),
        ),
        TranscriptEntry::Tool(card) => {
            let next = render_tool_entry(
                state,
                entry_index,
                card,
                &mut rendered.lines,
                &mut rendered.tool_rows,
                width,
            )
            .unwrap_or(entry_index + 1);
            record_entry_row(
                &mut rendered.entry_rows,
                &rendered.lines,
                width,
                start_row,
                entry_index,
            );
            return next;
        }
        TranscriptEntry::TurnCompleted { elapsed } => {
            append_turn_completed_entry(&mut rendered.lines, width, *elapsed);
        }
    }
    record_entry_row(
        &mut rendered.entry_rows,
        &rendered.lines,
        width,
        start_row,
        entry_index,
    );
    entry_index + 1
}

fn append_turn_completed_entry(lines: &mut Vec<Line<'static>>, width: usize, elapsed: Duration) {
    append_entry_lines(
        lines,
        vec![Line::from(Span::styled(
            format!("Worked for {}", format_turn_duration(elapsed)),
            Style::default().fg(Palette::MUTED),
        ))],
        width,
        EntryChrome::plain(),
    );
}

fn record_entry_row(
    rows: &mut Vec<RenderedEntryRow>,
    lines: &[Line<'_>],
    width: usize,
    start_row: usize,
    entry_index: usize,
) {
    rows.push(RenderedEntryRow {
        start_row,
        end_row: visual_rows(lines, width).saturating_sub(1),
        entry_index,
    });
}

fn collect_markdown_link_rows(
    lines: &[Line<'_>],
    start_row: usize,
    source_links: &[markdown::LinkTarget],
    output: &mut Vec<RenderedLinkRow>,
) {
    let mut link_index = 0;
    let mut remaining = source_links
        .first()
        .map_or(0, |link| Line::from(link.label.as_str()).width());
    for (line_offset, line) in lines.iter().enumerate() {
        let mut column = 0;
        for span in &line.spans {
            let span_width = Line::from(span.content.as_ref()).width();
            let is_link = span.style.fg == Some(Palette::LINK)
                && span.style.add_modifier.contains(Modifier::UNDERLINED);
            if is_link {
                while remaining == 0 && link_index + 1 < source_links.len() {
                    link_index += 1;
                    remaining = Line::from(source_links[link_index].label.as_str()).width();
                }
                if let Some(link) = source_links.get(link_index) {
                    let painted = span_width.min(remaining);
                    if painted > 0 {
                        output.push(RenderedLinkRow {
                            row: start_row.saturating_add(line_offset),
                            column_start: column,
                            column_end: column.saturating_add(painted),
                            url: link.url.clone(),
                        });
                        remaining = remaining.saturating_sub(painted);
                    }
                }
            }
            column = column.saturating_add(span_width);
        }
    }
}

fn visible_link_targets(
    rows: &[RenderedLinkRow],
    transcript_area: Rect,
    scroll: &ScrollState,
    sticky_prompt: bool,
) -> Vec<LinkHitTarget> {
    let viewport_end = scroll
        .top
        .saturating_add(usize::from(transcript_area.height));
    let first_visible = scroll
        .top
        .saturating_add(usize::from(u8::from(sticky_prompt)));
    rows.iter()
        .filter(|link| link.row >= first_visible && link.row < viewport_end)
        .filter_map(|link| {
            let start = link.column_start.min(usize::from(transcript_area.width));
            let end = link.column_end.min(usize::from(transcript_area.width));
            (start < end).then_some(LinkHitTarget {
                area: Rect {
                    x: transcript_area.x.saturating_add(u16::try_from(start).ok()?),
                    y: transcript_area
                        .y
                        .saturating_add(u16::try_from(link.row.saturating_sub(scroll.top)).ok()?),
                    width: u16::try_from(end.saturating_sub(start)).ok()?,
                    height: 1,
                },
                url: link.url.clone(),
            })
        })
        .collect()
}

fn render_user_entry(
    lines: &mut Vec<Line<'static>>,
    user_rows: &mut Vec<RenderedUserRow>,
    prompt_anchors: &mut Vec<PromptAnchor>,
    entry: UserEntryRender<'_>,
) {
    let UserEntryRender {
        text,
        created_at,
        width,
        entry_index,
        expanded,
    } = entry;
    let start_row = visual_rows(lines, width);
    let content = text
        .lines()
        .enumerate()
        .map(|(index, content)| {
            Line::from(vec![
                Span::styled(
                    if index == 0 { "❯ " } else { "  " },
                    Style::default().fg(Palette::USER_ACCENT),
                ),
                Span::styled(
                    content.to_owned(),
                    Style::default()
                        .fg(Palette::SURFACE_TEXT)
                        .add_modifier(Modifier::BOLD),
                ),
            ])
        })
        .collect::<Vec<_>>();
    let foldable = user_prompt_is_foldable(text);
    let content = if foldable && !expanded {
        collapse_user_content(content, width, created_at)
    } else {
        content
    };
    append_timestamped_entry_lines(
        lines,
        content,
        width,
        EntryChrome {
            background: Some(Palette::USER_SURFACE),
            vertical_padding: true,
            ..EntryChrome::plain()
        },
        created_at,
    );
    prompt_anchors.push(PromptAnchor {
        start_row,
        end_row: visual_rows(lines, width).saturating_sub(1),
        text: text.to_owned(),
    });
    user_rows.push(RenderedUserRow {
        start_row,
        end_row: visual_rows(lines, width).saturating_sub(1),
        entry_index,
        foldable,
    });
}

fn render_tool_entry(
    state: &TuiState,
    entry_index: usize,
    card: &ToolCard,
    lines: &mut Vec<Line<'static>>,
    tool_rows: &mut Vec<RenderedToolRow>,
    width: usize,
) -> Option<usize> {
    let Some((kind, group_end)) = tool_group_at(&state.transcript, entry_index) else {
        let selection = ToolSelection::Tool(entry_index);
        push_tool_row(
            lines,
            tool_rows,
            card,
            selection,
            state.selected_block,
            width,
        );
        return None;
    };
    let selection = ToolSelection::Group {
        start: entry_index,
        end: group_end,
    };
    let expanded = state.expanded_groups.contains(&entry_index);
    let cards = state.transcript[entry_index..group_end]
        .iter()
        .filter_map(|entry| match entry {
            TranscriptEntry::Tool(card) => Some(card),
            _ => None,
        })
        .collect::<Vec<_>>();
    let start_row = visual_rows(lines, width);
    let mut content = Vec::new();
    render_tool_group(
        &mut content,
        kind,
        &cards,
        expanded,
        selection_is(state.selected_block, selection),
    );
    append_entry_lines(lines, content, width, EntryChrome::plain());
    tool_rows.push(RenderedToolRow {
        start_row,
        end_row: visual_rows(lines, width).saturating_sub(1),
        selection,
    });
    if expanded {
        for (offset, card) in cards.into_iter().enumerate() {
            push_nested_tool_row(
                lines,
                tool_rows,
                card,
                ToolSelection::Tool(entry_index + offset),
                state.selected_block,
                width,
            );
        }
    }
    Some(group_end)
}

fn visible_entry_targets(
    rows: &[RenderedEntryRow],
    transcript_area: Rect,
    scroll: &ScrollState,
    sticky_prompt: bool,
) -> Vec<EntryHitTarget> {
    rows.iter()
        .filter_map(|row| {
            let viewport_end = scroll
                .top
                .saturating_add(usize::from(transcript_area.height));
            if row.end_row < scroll.top || row.start_row >= viewport_end {
                return None;
            }
            let sticky_rows = usize::from(u8::from(sticky_prompt));
            let visible_start = row.start_row.saturating_sub(scroll.top).max(sticky_rows);
            let visible_end = row
                .end_row
                .saturating_sub(scroll.top)
                .min(usize::from(transcript_area.height.saturating_sub(1)));
            (visible_start <= visible_end).then_some(EntryHitTarget {
                area: Rect {
                    x: transcript_area.x,
                    y: transcript_area
                        .y
                        .saturating_add(visible_start.try_into().ok()?),
                    width: transcript_area.width,
                    height: visible_end
                        .saturating_sub(visible_start)
                        .saturating_add(1)
                        .try_into()
                        .ok()?,
                },
                entry_index: row.entry_index,
                top_clipped: row.start_row < scroll.top.saturating_add(sticky_rows),
                bottom_clipped: row.end_row >= viewport_end,
            })
        })
        .collect()
}

// Mirrors Grok Build's SelectionBox: side rails live in the entry's reserved
// edge columns, corners occupy the separator rows, and clipped edges become
// dashed to communicate that the selection continues beyond the viewport.
fn render_entry_frame(frame: &mut Frame<'_>, target: EntryHitTarget, viewport: Rect, color: Color) {
    if target.area.width == 0 || target.area.height == 0 {
        return;
    }
    let style = Style::default().fg(color);
    let left = target.area.x;
    let right = target.area.right().saturating_sub(1);
    let top = target.area.y;
    let bottom = target.area.bottom().saturating_sub(1);
    let buffer = frame.buffer_mut();
    for y in top..=bottom {
        let clipped = (y == top && target.top_clipped) || (y == bottom && target.bottom_clipped);
        let symbol = if clipped { '┆' } else { '│' };
        if let Some(cell) = buffer.cell_mut((left, y)) {
            cell.set_char(symbol)
                .set_style(style)
                .set_bg(Palette::BG_BASE);
        }
        if let Some(cell) = buffer.cell_mut((right, y)) {
            cell.set_char(symbol)
                .set_style(style)
                .set_bg(Palette::BG_BASE);
        }
    }
    if !target.top_clipped && top > 0 {
        if let Some(cell) = buffer.cell_mut((left, top - 1)) {
            cell.set_char('┌').set_style(style).set_bg(Palette::BG_BASE);
        }
        if let Some(cell) = buffer.cell_mut((right, top - 1)) {
            cell.set_char('┐').set_style(style).set_bg(Palette::BG_BASE);
        }
    }
    if !target.bottom_clipped && bottom.saturating_add(1) < viewport.bottom() {
        if let Some(cell) = buffer.cell_mut((left, bottom + 1)) {
            cell.set_char('└').set_style(style).set_bg(Palette::BG_BASE);
        }
        if let Some(cell) = buffer.cell_mut((right, bottom + 1)) {
            cell.set_char('┘').set_style(style).set_bg(Palette::BG_BASE);
        }
    }
}

fn visible_thinking_targets(
    rows: &[RenderedThinkingRow],
    transcript_area: Rect,
    scroll: &ScrollState,
    sticky_prompt: bool,
) -> Vec<ThinkingHitTarget> {
    rows.iter()
        .filter_map(|row| {
            let viewport_end = scroll
                .top
                .saturating_add(usize::from(transcript_area.height));
            if row.end_row < scroll.top || row.start_row >= viewport_end {
                return None;
            }
            let visible_start = row
                .start_row
                .saturating_sub(scroll.top)
                .max(usize::from(u8::from(sticky_prompt)));
            let visible_end = row
                .end_row
                .saturating_sub(scroll.top)
                .min(usize::from(transcript_area.height.saturating_sub(1)));
            (visible_start <= visible_end).then_some(ThinkingHitTarget {
                area: Rect {
                    x: transcript_area.x,
                    y: transcript_area
                        .y
                        .saturating_add(visible_start.try_into().ok()?),
                    width: transcript_area.width,
                    height: visible_end
                        .saturating_sub(visible_start)
                        .saturating_add(1)
                        .try_into()
                        .ok()?,
                },
                entry_index: row.entry_index,
            })
        })
        .collect()
}

fn visible_user_targets(
    rows: &[RenderedUserRow],
    transcript_area: Rect,
    scroll: &ScrollState,
    sticky_prompt: bool,
) -> Vec<UserHitTarget> {
    rows.iter()
        .filter(|row| row.foldable)
        .filter_map(|row| {
            let viewport_end = scroll
                .top
                .saturating_add(usize::from(transcript_area.height));
            if row.end_row < scroll.top || row.start_row >= viewport_end {
                return None;
            }
            let visible_start = row
                .start_row
                .saturating_sub(scroll.top)
                .max(usize::from(u8::from(sticky_prompt)));
            let visible_end = row
                .end_row
                .saturating_sub(scroll.top)
                .min(usize::from(transcript_area.height.saturating_sub(1)));
            (visible_start <= visible_end).then_some(UserHitTarget {
                area: Rect {
                    x: transcript_area.x,
                    y: transcript_area
                        .y
                        .saturating_add(visible_start.try_into().ok()?),
                    width: transcript_area.width,
                    height: visible_end
                        .saturating_sub(visible_start)
                        .saturating_add(1)
                        .try_into()
                        .ok()?,
                },
                entry_index: row.entry_index,
            })
        })
        .collect()
}

fn push_tool_row(
    lines: &mut Vec<Line<'static>>,
    tool_rows: &mut Vec<RenderedToolRow>,
    card: &ToolCard,
    selection: ToolSelection,
    selected: Option<ToolSelection>,
    width: usize,
) {
    let start_row = visual_rows(lines, width);
    let mut content = Vec::new();
    render_tool_block(&mut content, card, selection_is(selected, selection));
    append_entry_lines(
        lines,
        content,
        width,
        EntryChrome {
            accent: card.expanded.then_some(match card.status {
                ToolStatus::Running => Palette::ACCENT,
                ToolStatus::Completed => Palette::SUCCESS,
                ToolStatus::Failed => Palette::ERROR,
            }),
            ..EntryChrome::plain()
        },
    );
    tool_rows.push(RenderedToolRow {
        start_row,
        end_row: visual_rows(lines, width).saturating_sub(1),
        selection,
    });
}

fn push_nested_tool_row(
    lines: &mut Vec<Line<'static>>,
    tool_rows: &mut Vec<RenderedToolRow>,
    card: &ToolCard,
    selection: ToolSelection,
    selected: Option<ToolSelection>,
    width: usize,
) {
    let start_row = visual_rows(lines, width);
    let mut content = Vec::new();
    render_grouped_tool_block(&mut content, card, selection_is(selected, selection));
    append_entry_lines(lines, content, width, EntryChrome::plain());
    tool_rows.push(RenderedToolRow {
        start_row,
        end_row: visual_rows(lines, width).saturating_sub(1),
        selection,
    });
}

fn tool_group_at(
    transcript: &[TranscriptEntry],
    start: usize,
) -> Option<(blocks::ToolGroupKind, usize)> {
    let TranscriptEntry::Tool(first) = transcript.get(start)? else {
        return None;
    };
    let kind = first.group_kind()?;
    let mut end = start + 1;
    while let Some(TranscriptEntry::Tool(card)) = transcript.get(end) {
        if card.group_kind() != Some(kind) {
            break;
        }
        end += 1;
    }
    (end.saturating_sub(start) >= 2).then_some((kind, end))
}

fn selection_is(selected: Option<ToolSelection>, candidate: ToolSelection) -> bool {
    match (selected, candidate) {
        (
            Some(ToolSelection::Group {
                start: selected, ..
            }),
            ToolSelection::Group {
                start: candidate, ..
            },
        ) => selected == candidate,
        (Some(selected), candidate) => selected == candidate,
        (None, _) => false,
    }
}

fn sticky_prompt(anchors: &[PromptAnchor], scroll_top: usize) -> Option<&str> {
    anchors
        .iter()
        .rev()
        .find(|anchor| anchor.start_row <= scroll_top && anchor.end_row < scroll_top)
        .map(|anchor| anchor.text.as_str())
}

fn visible_tool_targets(
    tool_rows: &[RenderedToolRow],
    transcript_area: Rect,
    scroll: &ScrollState,
    sticky_prompt: bool,
) -> Vec<ToolHitTarget> {
    tool_rows
        .iter()
        .filter_map(|row| {
            let viewport_end = scroll
                .top
                .saturating_add(usize::from(transcript_area.height));
            if row.end_row < scroll.top || row.start_row >= viewport_end {
                return None;
            }
            let visible_start = row
                .start_row
                .saturating_sub(scroll.top)
                .max(usize::from(u8::from(sticky_prompt)));
            let visible_end = row
                .end_row
                .saturating_sub(scroll.top)
                .min(usize::from(transcript_area.height.saturating_sub(1)));
            if visible_start > visible_end {
                return None;
            }
            Some(ToolHitTarget {
                column_start: transcript_area.x,
                column_end: transcript_area.right().saturating_sub(1),
                row_start: transcript_area
                    .y
                    .saturating_add(visible_start.try_into().ok()?),
                row_end: transcript_area
                    .y
                    .saturating_add(visible_end.try_into().ok()?),
                selection: row.selection,
            })
        })
        .collect()
}

#[derive(Clone, Copy)]
struct EntryChrome {
    accent: Option<Color>,
    background: Option<Color>,
    vertical_padding: bool,
}

impl EntryChrome {
    const fn plain() -> Self {
        Self {
            accent: None,
            background: None,
            vertical_padding: false,
        }
    }
}

// Geometry follows xai-org/grok-build's HorizontalLayout and EntryRenderer at
// commit 77cd7eb675ba911c225c3aaeeece3a20cbccc426 (Apache-2.0): one reserved
// accent column, two columns of left padding, and two columns of right padding.
// Both interaction rails sit immediately outside the item surface. Padding
// remains part of that surface, so changing its edge does not move the content.
const ENTRY_ACCENT_WIDTH: usize = 1;
const ENTRY_PAD_LEFT: usize = 2;
const ENTRY_PAD_RIGHT: usize = 2;

fn entry_content_width(width: usize) -> usize {
    width
        .saturating_sub(ENTRY_ACCENT_WIDTH + ENTRY_PAD_LEFT + ENTRY_PAD_RIGHT)
        .max(1)
}

fn append_entry_lines(
    output: &mut Vec<Line<'static>>,
    content: Vec<Line<'static>>,
    width: usize,
    chrome: EntryChrome,
) {
    let content_width = entry_content_width(width);
    if chrome.vertical_padding {
        output.push(entry_row(Line::default(), content_width, chrome));
    }
    for line in content {
        for wrapped in wrap_entry_line(line, content_width) {
            output.push(entry_row(wrapped, content_width, chrome));
        }
    }
    if chrome.vertical_padding {
        output.push(entry_row(Line::default(), content_width, chrome));
    }
}

fn append_timestamped_entry_lines(
    output: &mut Vec<Line<'static>>,
    content: Vec<Line<'static>>,
    width: usize,
    chrome: EntryChrome,
    timestamp: &str,
) {
    let content_width = entry_content_width(width);
    let timestamp = format!("  {timestamp}");
    let timestamp_width = Line::from(timestamp.as_str()).width();
    let text_width = content_width.saturating_sub(timestamp_width).max(1);
    let mut wrapped = content
        .into_iter()
        .flat_map(|line| wrap_entry_line(line, text_width))
        .collect::<Vec<_>>();
    if let Some(first) = wrapped.first_mut()
        && content_width > timestamp_width
    {
        let spacer = content_width
            .saturating_sub(timestamp_width)
            .saturating_sub(first.width());
        first.spans.push(Span::raw(" ".repeat(spacer)));
        first
            .spans
            .push(Span::styled(timestamp, Style::default().fg(Palette::MUTED)));
    }
    if chrome.vertical_padding {
        output.push(entry_row(Line::default(), content_width, chrome));
    }
    for line in wrapped {
        output.push(entry_row(line, content_width, chrome));
    }
    if chrome.vertical_padding {
        output.push(entry_row(Line::default(), content_width, chrome));
    }
}

fn current_timestamp() -> String {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let hour = now.hour();
    let (hour, period) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!("{hour}:{:02} {period}", now.minute())
}

fn entry_row(mut line: Line<'static>, content_width: usize, chrome: EntryChrome) -> Line<'static> {
    let background = line
        .style
        .bg
        .or(chrome.background)
        .map(|color| Style::default().bg(color));
    if let Some(background) = background {
        for span in &mut line.spans {
            span.style = span.style.patch(background);
        }
    }
    let padding = content_width.saturating_sub(line.width());
    let pad_style = background.unwrap_or_default();
    let mut spans = Vec::with_capacity(line.spans.len() + 6);
    spans.push(Span::styled(
        if chrome.accent.is_some() { "┃" } else { " " },
        Style::default().fg(chrome.accent.unwrap_or(Palette::QUIET)),
    ));
    spans.push(Span::styled(" ".repeat(ENTRY_PAD_LEFT), pad_style));
    spans.extend(line.spans);
    spans.push(Span::styled(" ".repeat(padding), pad_style));
    spans.push(Span::styled(
        " ".repeat(ENTRY_PAD_RIGHT.saturating_sub(ENTRY_ACCENT_WIDTH)),
        pad_style,
    ));
    spans.push(Span::raw(" ".repeat(ENTRY_ACCENT_WIDTH)));
    Line::from(spans)
}

fn format_turn_duration(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs_f64();
    if seconds < 60.0 {
        return format!("{seconds:.1}s");
    }
    let total_seconds = elapsed.as_secs();
    format!("{}m{}s", total_seconds / 60, total_seconds % 60)
}

fn wrap_entry_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut row_width: usize = 0;
    for span in line.spans {
        let mut segment = String::new();
        for character in span.content.chars() {
            let character_width = Line::from(character.to_string()).width();
            if row_width > 0 && row_width.saturating_add(character_width) > width {
                if !segment.is_empty() {
                    row.push(Span::styled(std::mem::take(&mut segment), span.style));
                }
                rows.push(Line::from(std::mem::take(&mut row)));
                row_width = 0;
            }
            segment.push(character);
            row_width = row_width.saturating_add(character_width);
        }
        if !segment.is_empty() {
            row.push(Span::styled(segment, span.style));
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(Line::from(row));
    }
    rows
}

fn user_prompt_is_foldable(text: &str) -> bool {
    const MIN_CONTENT_WIDTH: usize = 60;
    let mut visual_lines = 0;
    for line in text.lines() {
        visual_lines += Line::from(line).width().max(1).div_ceil(MIN_CONTENT_WIDTH);
        if visual_lines > COLLAPSED_USER_ROWS {
            return true;
        }
    }
    false
}

fn collapse_user_content(
    content: Vec<Line<'static>>,
    width: usize,
    timestamp: &str,
) -> Vec<Line<'static>> {
    let timestamp_width = Line::from(format!("  {timestamp}")).width();
    let text_width = entry_content_width(width)
        .saturating_sub(timestamp_width)
        .max(1);
    let mut wrapped = content
        .into_iter()
        .flat_map(|line| wrap_entry_line(line, text_width))
        .collect::<Vec<_>>();
    if wrapped.len() <= COLLAPSED_USER_ROWS {
        return wrapped;
    }
    wrapped.truncate(COLLAPSED_USER_ROWS);
    if let Some(last) = wrapped.last_mut() {
        let style = last
            .spans
            .last()
            .map_or_else(Style::default, |span| span.style);
        *last = truncate_line(last.clone(), text_width.saturating_sub(2));
        last.spans.push(Span::styled(" …", style));
    }
    wrapped
}

fn truncate_line(line: Line<'static>, max_width: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let mut used: usize = 0;
    'outer: for span in line.spans {
        let mut text = String::new();
        for character in span.content.chars() {
            let width = Line::from(character.to_string()).width();
            if used.saturating_add(width) > max_width {
                if !text.is_empty() {
                    spans.push(Span::styled(text, span.style));
                }
                break 'outer;
            }
            text.push(character);
            used = used.saturating_add(width);
        }
        if !text.is_empty() {
            spans.push(Span::styled(text, span.style));
        }
    }
    Line::from(spans)
}

fn truncate_text(text: &str, max_width: usize) -> String {
    if Line::from(text).width() <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut used: usize = 0;
    for character in text.chars() {
        let width = Line::from(character.to_string()).width();
        if used.saturating_add(width).saturating_add(1) > max_width {
            break;
        }
        output.push(character);
        used = used.saturating_add(width);
    }
    output.push('…');
    output
}

fn visual_rows(lines: &[Line<'_>], width: usize) -> usize {
    let width = width.max(1);
    lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum()
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
                    .borders(Borders::LEFT)
                    .border_style(Style::default().fg(Palette::BORDER))
                    .padding(Padding::horizontal(1)),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_activity(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
    state.cancel_hit = None;
    let area = Block::default()
        .padding(Padding::new(2, 2, 0, 0))
        .inner(area);
    let history = (!state.scroll.follow_tail && state.scroll.rows_below() > 0).then(|| {
        format!(
            "▼ {} lines below · End to follow",
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

    if let Some((fallback, color)) = state.phase.activity(state.animation_tick) {
        let activity = state.active_tool_activity();
        let label = activity.as_deref().unwrap_or(fallback);
        frame.render_widget(
            Paragraph::new(Span::styled(label, Style::default().fg(color))),
            phase_area,
        );
        if state.phase == UiPhase::Active && phase_area.width >= 6 {
            let stop_area = Rect {
                x: phase_area.right().saturating_sub(6),
                y: phase_area.y,
                width: 6,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new("[stop]")
                    .alignment(ratatui::layout::Alignment::Right)
                    .style(Style::default().fg(Palette::MUTED)),
                stop_area,
            );
            state.cancel_hit = Some(stop_area);
        }
    }
    if let Some(history) = history {
        state.follow_hit = Some(history_area);
        frame.render_widget(
            Paragraph::new(history)
                .alignment(ratatui::layout::Alignment::Right)
                .style(Style::default().fg(Palette::MUTED)),
            history_area,
        );
    } else {
        state.follow_hit = None;
    }
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
    state.composer_hit = Some(area);
    let focused = state.focus == Focus::Prompt;
    let border = if focused {
        Palette::BORDER_ACTIVE
    } else {
        Palette::BORDER
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(Palette::BG_BASE))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let input = if state.input.is_empty() {
        let mut spans = vec![Span::styled(
            "❯ ",
            Style::default().fg(Palette::USER_ACCENT),
        )];
        if !focused {
            spans.push(Span::styled(
                "Build anything",
                Style::default().fg(Palette::MUTED),
            ));
        }
        vec![Line::from(spans)]
    } else {
        state
            .input
            .split('\n')
            .enumerate()
            .map(|(index, line)| {
                Line::from(vec![
                    Span::styled(
                        if index == 0 { "❯ " } else { "  " },
                        Style::default().fg(Palette::USER_ACCENT),
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
        Paragraph::new(Text::from(input))
            .style(Style::default().bg(Palette::BG_BASE))
            .scroll((hidden_rows.try_into().unwrap_or(u16::MAX), 0)),
        inner,
    );

    render_composer_caption(frame, area, state);

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

fn render_composer_caption(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    if area.width <= 24 {
        return;
    }
    let caption_style = Style::default().bg(Palette::BG_BASE);
    let mut spans = vec![
        Span::styled(" lenso-agent", caption_style.fg(Palette::MUTED)),
        Span::styled(
            format!(" · {}", state.tool_scope),
            caption_style.fg(Palette::QUIET),
        ),
    ];
    if state.input.contains('\n') {
        spans.push(Span::styled(
            " · multiline",
            caption_style.fg(Palette::QUIET),
        ));
    }
    if state.pending_interaction.is_some() {
        spans.push(Span::styled(
            " · answer required",
            caption_style.fg(Palette::COMMAND),
        ));
    }
    spans.push(Span::styled(" ", caption_style));
    let info = Line::from(spans);
    let width = u16::try_from(info.width())
        .unwrap_or(u16::MAX)
        .min(area.width.saturating_sub(2));
    frame.render_widget(
        Paragraph::new(info),
        Rect {
            x: area.right().saturating_sub(width).saturating_sub(1),
            y: area.bottom().saturating_sub(1),
            width,
            height: 1,
        },
    );
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

fn render_status_line(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
    state.shortcut_hit_targets.clear();
    let mut hints: Vec<(&str, &str, ShortcutAction)> = Vec::new();
    match state.focus {
        Focus::Prompt => {
            if !state.input.trim().is_empty() {
                hints.push(("enter", "send", ShortcutAction::Send));
                if area.width >= 64 {
                    hints.push(("shift+enter", "newline", ShortcutAction::Newline));
                }
            }
            if area.width >= 104 {
                hints.push(("pgdn", "scroll", ShortcutAction::PageDown));
            }
            if state.input.trim().is_empty() || area.width >= 64 {
                hints.push(("tab", "scrollback", ShortcutAction::FocusScrollback));
            }
        }
        Focus::Scrollback => {
            hints.push(("j/k", "scroll", ShortcutAction::PageDown));
            if area.width >= 67 {
                hints.push(("h/l", "fold", ShortcutAction::ToggleSelectedTool));
            }
            if area.width >= 82 {
                hints.push(("tab", "prompt", ShortcutAction::FocusPrompt));
            }
        }
    }
    hints.push(("Ctrl+.", "shortcuts", ShortcutAction::ShowShortcuts));
    let mut spans = Vec::new();
    let mut used = 0_u16;
    for (key, label, action) in hints {
        let hint_width = u16::try_from(key.len() + label.len() + 1).unwrap_or(u16::MAX);
        let separator_width = u16::from(!spans.is_empty()) * 5;
        if used
            .saturating_add(separator_width)
            .saturating_add(hint_width)
            > area.width
        {
            break;
        }
        if separator_width > 0 {
            spans.push(Span::styled("  │  ", Style::default().fg(Palette::QUIET)));
            used = used.saturating_add(separator_width);
        }
        state.shortcut_hit_targets.push(ShortcutHitTarget {
            area: Rect {
                x: area.x.saturating_add(used),
                y: area.y,
                width: hint_width,
                height: 1,
            },
            action,
        });
        spans.push(Span::styled(
            key.to_owned(),
            Style::default()
                .fg(Palette::MUTED)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(":{label}"),
            Style::default().fg(Palette::QUIET),
        ));
        used = used.saturating_add(hint_width);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_shortcuts_overlay(frame: &mut Frame<'_>, area: Rect) {
    let overlay = centered_rect(area, 68.min(area.width.saturating_sub(2)), 19);
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
        ("Tab", "Switch between prompt and scrollback focus"),
        ("j / k, g / G", "Scroll by line or jump to top/bottom"),
        ("h / l", "Collapse or expand the selected Tool block"),
        ("PgUp / PgDn", "Scroll conversation"),
        ("End", "Return to the latest message"),
        ("Shift+Tab", "Open or cycle composed context panels"),
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
    use lenso_capability_agent_user_interaction::InteractionOption;
    use ratatui::backend::TestBackend;

    use super::*;

    #[test]
    fn rename_command_requires_and_extracts_a_title() {
        assert_eq!(
            rename_command("/rename Project Atlas").unwrap(),
            Some("Project Atlas")
        );
        assert!(rename_command("/rename").is_err());
        assert!(rename_command("/rename   ").is_err());
        assert_eq!(rename_command("/renamed normally").unwrap(), None);
    }

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
        assert!(terminal.backend().to_string().contains("Esc quits"));
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
            ratatui::layout::Position::new(
                tool_target.area.x.saturating_add(3),
                tool_target.area.y,
            ),
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
                progress_channel: Some(
                    lenso_capability_agent::RunTurnResponseProgressChannel::Stderr,
                ),
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
            text:
                "Read the [official docs](https://example.com/docs).\nOr visit https://x.ai/build."
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
    fn runtime_failure_stays_inline_and_keeps_the_tui_available() {
        let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
        handle_stream_event(
            Err(lenso_kernel::RuntimeFailure::PluginFailure {
                detail: "fixture failure".to_owned(),
            }),
            &mut state,
        );
        assert_eq!(state.phase, UiPhase::Failed);
        assert!(state.active.is_none());
        assert!(matches!(
            state.transcript.last(),
            Some(TranscriptEntry::Error { text }) if text.contains("fixture failure")
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
}

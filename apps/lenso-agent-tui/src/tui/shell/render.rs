//! Layout composition over semantic state; rendering owns no Agent authority.

use super::{
    Block, BorderType, Borders, COLLAPSED_USER_ROWS, Clear, Color, Constraint, Duration,
    EntryHitTarget, Focus, Frame, InteractionDraft, InteractionHitAction, InteractionHitTarget,
    InteractionQuestion, Layout, Line, LinkHitTarget, MAX_VISIBLE_QUEUE_HEIGHT,
    MAX_VISIBLE_QUEUE_ROWS, MAX_VISIBLE_SUGGESTIONS, Modifier, OffsetDateTime, PANEL_BREAKPOINT,
    Padding, Palette, Paragraph, PromptAnchor, QueueHitTarget, Rect, RenderedEntryRow,
    RenderedLinkRow, RenderedThinkingRow, RenderedToolRow, RenderedUserRow, ScrollState, Scrollbar,
    ScrollbarOrientation, ScrollbarState, ShortcutAction, ShortcutHitTarget, Span, Style,
    SuggestionHitTarget, Text, ThinkingHitTarget, ToolCard, ToolHitTarget, ToolSelection,
    ToolStatus, TranscriptEntry, TranscriptRender, TuiState, UiPhase, UserEntryRender,
    UserHitTarget, Wrap, blocks, markdown, markdown_lines_with_width, render_grouped_tool_block,
    render_thinking_block, render_tool_block, render_tool_group,
};

const ENTRY_ACCENT_WIDTH: usize = 1;
const ENTRY_PAD_LEFT: usize = 2;

pub(super) fn render(frame: &mut Frame<'_>, state: &mut TuiState) {
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

#[path = "render/interaction.rs"]
mod interaction;
use interaction::{interaction_card_height, render_interaction_card};
#[path = "render/overlays.rs"]
mod overlays;
use overlays::{content_area, render_queue, render_suggestions};
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

#[path = "render/transcript.rs"]
mod transcript;
pub(super) use transcript::current_timestamp;
use transcript::render_transcript;
#[cfg(test)]
pub(super) use transcript::{format_turn_duration, sticky_prompt, transcript_lines};
#[path = "render/chrome.rs"]
mod chrome;
use chrome::{
    render_activity, render_composer, render_panel, render_shortcuts_overlay, render_status_line,
    visual_input_rows,
};

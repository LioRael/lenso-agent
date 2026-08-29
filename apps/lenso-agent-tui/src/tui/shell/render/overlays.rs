use super::{
    Block, Clear, ENTRY_ACCENT_WIDTH, ENTRY_PAD_LEFT, Frame, Line, MAX_VISIBLE_QUEUE_ROWS,
    Modifier, Palette, Paragraph, QueueHitTarget, Rect, Span, Style, SuggestionHitTarget, TuiState,
};

pub(super) fn render_queue(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
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

pub(super) fn render_suggestions(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
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

pub(super) fn content_area(area: Rect) -> Rect {
    let horizontal = if area.width >= 40 { 2 } else { 1 };
    let vertical = u16::from(area.height >= 18);
    Rect {
        x: area.x.saturating_add(horizontal),
        y: area.y.saturating_add(vertical),
        width: area.width.saturating_sub(horizontal.saturating_mul(2)),
        height: area.height.saturating_sub(vertical.saturating_mul(2)),
    }
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

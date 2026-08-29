use super::{
    Block, BorderType, Borders, Clear, Constraint, Focus, Frame, Layout, Line, Modifier, Padding,
    Palette, Paragraph, Rect, ShortcutAction, ShortcutHitTarget, Span, Style, Text, TuiState,
    UiPhase, Wrap,
};

pub(super) fn render_panel(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
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

pub(super) fn render_activity(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
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

pub(super) fn render_composer(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
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

pub(super) fn visual_input_rows(input: &str, width: usize) -> usize {
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

pub(super) fn render_status_line(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
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

pub(super) fn render_shortcuts_overlay(frame: &mut Frame<'_>, area: Rect) {
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

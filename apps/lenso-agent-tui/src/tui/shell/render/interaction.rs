use super::{
    Block, Constraint, Frame, InteractionDraft, InteractionHitAction, InteractionHitTarget,
    InteractionQuestion, Layout, Line, Modifier, Palette, Paragraph, Rect, Span, Style, TuiState,
    Wrap, visual_input_rows,
};

pub(super) fn interaction_card_height(state: &TuiState, screen_height: u16) -> u16 {
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

pub(super) fn render_interaction_card(frame: &mut Frame<'_>, area: Rect, state: &mut TuiState) {
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

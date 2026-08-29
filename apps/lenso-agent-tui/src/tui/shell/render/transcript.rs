use super::{
    Block, COLLAPSED_USER_ROWS, Color, Duration, ENTRY_ACCENT_WIDTH, ENTRY_PAD_LEFT,
    EntryHitTarget, Frame, Line, LinkHitTarget, Modifier, OffsetDateTime, Padding, Palette,
    Paragraph, PromptAnchor, Rect, RenderedEntryRow, RenderedLinkRow, RenderedThinkingRow,
    RenderedToolRow, RenderedUserRow, ScrollState, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Span, Style, Text, ThinkingHitTarget, ToolCard, ToolHitTarget, ToolSelection, ToolStatus,
    TranscriptEntry, TranscriptRender, TuiState, UserEntryRender, UserHitTarget, Wrap, blocks,
    markdown, markdown_lines_with_width, render_grouped_tool_block, render_thinking_block,
    render_tool_block, render_tool_group,
};

pub(in crate::tui::shell) fn render_transcript(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &mut TuiState,
) {
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

pub(in crate::tui::shell) fn transcript_lines(state: &TuiState, width: usize) -> TranscriptRender {
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

pub(in crate::tui::shell) fn sticky_prompt(
    anchors: &[PromptAnchor],
    scroll_top: usize,
) -> Option<&str> {
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

#[path = "transcript/format.rs"]
mod format;
use format::{
    EntryChrome, append_entry_lines, append_timestamped_entry_lines, collapse_user_content,
    entry_content_width, user_prompt_is_foldable, visual_rows,
};
pub(in crate::tui::shell) use format::{current_timestamp, format_turn_duration};

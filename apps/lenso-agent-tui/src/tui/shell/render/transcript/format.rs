use super::{
    COLLAPSED_USER_ROWS, Color, Duration, ENTRY_ACCENT_WIDTH, ENTRY_PAD_LEFT, Line, OffsetDateTime,
    Palette, Span, Style,
};

#[derive(Clone, Copy)]
pub(super) struct EntryChrome {
    pub(super) accent: Option<Color>,
    pub(super) background: Option<Color>,
    pub(super) vertical_padding: bool,
}

impl EntryChrome {
    pub(super) const fn plain() -> Self {
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
const ENTRY_PAD_RIGHT: usize = 2;

pub(super) fn entry_content_width(width: usize) -> usize {
    width
        .saturating_sub(ENTRY_ACCENT_WIDTH + ENTRY_PAD_LEFT + ENTRY_PAD_RIGHT)
        .max(1)
}

pub(super) fn append_entry_lines(
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

pub(super) fn append_timestamped_entry_lines(
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

pub(in crate::tui::shell) fn current_timestamp() -> String {
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

pub(in crate::tui::shell) fn format_turn_duration(elapsed: Duration) -> String {
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

pub(super) fn user_prompt_is_foldable(text: &str) -> bool {
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

pub(super) fn collapse_user_content(
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

pub(super) fn visual_rows(lines: &[Line<'_>], width: usize) -> usize {
    let width = width.max(1);
    lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum()
}

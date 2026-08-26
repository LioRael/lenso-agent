use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::Palette;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LinkTarget {
    pub label: String,
    pub url: String,
}

pub(super) fn lines(markdown: &str) -> Vec<Line<'static>> {
    lines_with_width(markdown, usize::MAX)
}

pub(super) fn lines_with_width(markdown: &str, max_width: usize) -> Vec<Line<'static>> {
    let source = markdown.lines().collect::<Vec<_>>();
    let mut rendered = Vec::new();
    let mut index = 0;
    let mut code_fence = false;
    while index < source.len() {
        let raw = source[index];
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            if !code_fence
                && rendered
                    .last()
                    .is_some_and(|line: &Line<'_>| line.width() > 0)
            {
                rendered.push(Line::default());
            }
            code_fence = !code_fence;
            index += 1;
            continue;
        }
        if code_fence {
            rendered.push(code_line(raw));
            index += 1;
            continue;
        }
        if index + 1 < source.len() && is_table_row(raw) && is_table_separator(source[index + 1]) {
            let consumed = render_table(&source[index..], &mut rendered, max_width);
            index += consumed;
            continue;
        }
        rendered.push(markdown_line(raw));
        index += 1;
    }
    rendered
}

pub(super) fn links(markdown: &str) -> Vec<LinkTarget> {
    let mut links = Vec::new();
    let mut remaining = markdown;
    while let Some((_, label, url, rest)) = next_link(remaining) {
        links.push(LinkTarget {
            label: label.to_owned(),
            url: url.to_owned(),
        });
        remaining = rest;
    }
    links
}

fn markdown_line(raw: &str) -> Line<'static> {
    let trimmed = raw.trim_start();
    if let Some(heading) = trimmed.strip_prefix("###### ") {
        heading_line(heading, Palette::HEADING_H6, false)
    } else if let Some(heading) = trimmed.strip_prefix("##### ") {
        heading_line(heading, Palette::HEADING_H5, true)
    } else if let Some(heading) = trimmed.strip_prefix("#### ") {
        heading_line(heading, Palette::HEADING_H4, true)
    } else if let Some(heading) = trimmed.strip_prefix("### ") {
        heading_line(heading, Palette::HEADING_H3, true)
    } else if let Some(heading) = trimmed.strip_prefix("## ") {
        heading_line(heading, Palette::HEADING_H2, true)
    } else if let Some(heading) = trimmed.strip_prefix("# ") {
        heading_line(heading, Palette::HEADING_H1, true)
    } else if let Some((indent, item)) = unordered_item(raw) {
        list_line(indent, "• ", item)
    } else if let Some((indent, number, item)) = ordered_item(raw) {
        list_line(indent, &format!("{number}. "), item)
    } else if trimmed.starts_with('>') {
        quote_line(trimmed)
    } else if matches!(trimmed, "---" | "***" | "___") {
        Line::from(Span::styled("───", Style::default().fg(Palette::MUTED)))
    } else {
        Line::from(inline(raw))
    }
}

fn code_line(raw: &str) -> Line<'static> {
    Line::from(Span::styled(
        raw.to_owned(),
        Style::default().fg(Palette::CODE).bg(Palette::SURFACE),
    ))
    .style(Style::default().bg(Palette::SURFACE))
}

fn heading_line(text: &str, color: ratatui::style::Color, bold: bool) -> Line<'static> {
    let style = if bold {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(color)
    };
    Line::from(Span::styled(text.to_owned(), style))
}

fn unordered_item(raw: &str) -> Option<(usize, &str)> {
    let indent = raw.len().saturating_sub(raw.trim_start().len());
    let item = raw.trim_start();
    item.strip_prefix("- ")
        .or_else(|| item.strip_prefix("* "))
        .map(|item| (indent, item))
}

fn ordered_item(raw: &str) -> Option<(usize, &str, &str)> {
    let indent = raw.len().saturating_sub(raw.trim_start().len());
    let (number, item) = raw.trim_start().split_once(". ")?;
    number
        .chars()
        .all(|character| character.is_ascii_digit())
        .then_some((indent, number, item))
}

fn list_line(indent: usize, marker: &str, item: &str) -> Line<'static> {
    let mut spans = vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(marker.to_owned(), Style::default().fg(Palette::MUTED)),
    ];
    if let Some(task) = item
        .strip_prefix("[x] ")
        .or_else(|| item.strip_prefix("[X] "))
    {
        spans.push(Span::styled("[x] ", Style::default().fg(Palette::SUCCESS)));
        spans.extend(inline(task));
    } else if let Some(task) = item.strip_prefix("[ ] ") {
        spans.push(Span::styled(
            "[ ] ",
            Style::default().fg(Palette::SECONDARY_TEXT),
        ));
        spans.extend(inline(task));
    } else {
        spans.extend(inline(item));
    }
    Line::from(spans)
}

fn quote_line(mut text: &str) -> Line<'static> {
    let mut depth = 0;
    while let Some(rest) = text.strip_prefix('>') {
        depth += 1;
        text = rest.strip_prefix(' ').unwrap_or(rest);
    }
    let mut spans = vec![Span::styled(
        "│ ".repeat(depth),
        Style::default().fg(Palette::MUTED),
    )];
    spans.extend(inline(text));
    Line::from(spans)
}

fn is_table_row(raw: &str) -> bool {
    raw.trim().starts_with('|') && raw.trim().ends_with('|') && parse_cells(raw).len() > 1
}

fn is_table_separator(raw: &str) -> bool {
    let cells = parse_cells(raw);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let cell = cell.trim_matches(':').trim();
            cell.len() >= 3 && cell.chars().all(|character| character == '-')
        })
}

fn parse_cells(raw: &str) -> Vec<&str> {
    raw.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

fn render_table(source: &[&str], rendered: &mut Vec<Line<'static>>, max_width: usize) -> usize {
    let header = parse_cells(source[0]);
    let mut rows = vec![header];
    let mut consumed = 2;
    while consumed < source.len() && is_table_row(source[consumed]) {
        rows.push(parse_cells(source[consumed]));
        consumed += 1;
    }
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| Line::from(*cell).width())
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    constrain_table_widths(&mut widths, max_width);
    rendered.push(table_rule('┌', '┬', '┐', &widths));
    for (index, row) in rows.iter().enumerate() {
        rendered.push(table_row(row, &widths, index == 0));
        if index == 0 {
            rendered.push(table_rule('├', '┼', '┤', &widths));
        }
    }
    rendered.push(table_rule('└', '┴', '┘', &widths));
    consumed
}

fn constrain_table_widths(widths: &mut [usize], max_width: usize) {
    let table_width = |widths: &[usize]| {
        1_usize
            .saturating_add(widths.iter().sum::<usize>())
            .saturating_add(widths.len().saturating_mul(3))
    };
    while table_width(widths) > max_width {
        let Some((index, _)) = widths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > 1)
            .max_by_key(|(index, width)| (**width, usize::MAX.saturating_sub(*index)))
        else {
            break;
        };
        widths[index] = widths[index].saturating_sub(1);
    }
}

fn table_rule(left: char, middle: char, right: char, widths: &[usize]) -> Line<'static> {
    let mut text = String::from(left);
    for (index, width) in widths.iter().enumerate() {
        text.push_str(&"─".repeat(width.saturating_add(2)));
        text.push(if index + 1 == widths.len() {
            right
        } else {
            middle
        });
    }
    Line::from(Span::styled(text, Style::default().fg(Palette::HEADING_H2)))
}

fn table_row(cells: &[&str], widths: &[usize], header: bool) -> Line<'static> {
    let mut spans = vec![Span::styled("│ ", Style::default().fg(Palette::HEADING_H2))];
    for (index, width) in widths.iter().enumerate() {
        let cell = cells.get(index).copied().unwrap_or_default();
        let mut style = Style::default().fg(Palette::SECONDARY_TEXT);
        if header {
            style = style.add_modifier(Modifier::BOLD);
        }
        let fitted = fit_cell(cell, *width);
        spans.extend(
            inline(&fitted)
                .into_iter()
                .map(|span| span.patch_style(style)),
        );
        spans.push(Span::raw(
            " ".repeat(width.saturating_sub(Line::from(fitted.as_str()).width())),
        ));
        spans.push(Span::styled(
            if index + 1 == widths.len() {
                " │"
            } else {
                " │ "
            },
            Style::default().fg(Palette::HEADING_H2),
        ));
    }
    Line::from(spans)
}

fn fit_cell(text: &str, width: usize) -> String {
    if Line::from(text).width() <= width {
        return text.to_owned();
    }
    if width <= 1 {
        return "…".to_owned();
    }
    let mut fitted = String::new();
    let mut used: usize = 0;
    for character in text.chars() {
        let character_width = Line::from(character.to_string()).width();
        if used.saturating_add(character_width).saturating_add(1) > width {
            break;
        }
        fitted.push(character);
        used = used.saturating_add(character_width);
    }
    fitted.push('…');
    fitted
}

fn inline(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let link = next_link(remaining);
        let marker = next_inline_marker(remaining);
        if let Some((index, display, _, rest)) = link
            && marker
                .as_ref()
                .is_none_or(|(marker_index, _, _)| index <= *marker_index)
        {
            if index > 0 {
                spans.push(Span::raw(remaining[..index].to_owned()));
            }
            spans.push(Span::styled(
                display.to_owned(),
                Style::default()
                    .fg(Palette::LINK)
                    .add_modifier(Modifier::UNDERLINED),
            ));
            remaining = rest;
            continue;
        }
        let Some((index, marker, style)) = marker else {
            spans.push(Span::raw(remaining.to_owned()));
            break;
        };
        if index > 0 {
            spans.push(Span::raw(remaining[..index].to_owned()));
        }
        let after = &remaining[index + marker.len()..];
        if let Some(end) = after.find(marker) {
            spans.push(Span::styled(after[..end].to_owned(), style));
            remaining = &after[end + marker.len()..];
        } else {
            spans.push(Span::raw(remaining[index..].to_owned()));
            break;
        }
    }
    spans
}

fn markdown_link(text: &str) -> Option<(usize, &str, &str, &str)> {
    let start = text.find('[')?;
    let display_end = text[start + 1..].find("](")? + start + 1;
    let url = &text[display_end + 2..];
    let url_end = url.find(')')?;
    Some((
        start,
        &text[start + 1..display_end],
        &url[..url_end],
        &url[url_end + 1..],
    ))
}

fn bare_url(text: &str) -> Option<(usize, &str, &str, &str)> {
    let start = ["https://", "http://", "mailto:"]
        .into_iter()
        .filter_map(|prefix| text.find(prefix))
        .min()?;
    let candidate = text[start..]
        .split_once(char::is_whitespace)
        .map_or(&text[start..], |(url, _)| url);
    let url = candidate.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
    (!url.is_empty()).then_some((start, url, url, &text[start + url.len()..]))
}

fn next_link(text: &str) -> Option<(usize, &str, &str, &str)> {
    match (markdown_link(text), bare_url(text)) {
        (Some(markdown), Some(bare)) if bare.0 < markdown.0 => Some(bare),
        (Some(markdown), _) => Some(markdown),
        (None, bare) => bare,
    }
}

fn next_inline_marker(text: &str) -> Option<(usize, &'static str, Style)> {
    [
        (
            "`",
            Style::default()
                .fg(Palette::CODE)
                .add_modifier(Modifier::BOLD),
        ),
        ("**", Style::default().add_modifier(Modifier::BOLD)),
        ("~~", Style::default().add_modifier(Modifier::CROSSED_OUT)),
        ("*", Style::default().add_modifier(Modifier::ITALIC)),
        ("_", Style::default().add_modifier(Modifier::ITALIC)),
    ]
    .into_iter()
    .filter_map(|(marker, style)| text.find(marker).map(|index| (index, marker, style)))
    .min_by_key(|(index, marker, _)| (*index, usize::MAX - marker.len()))
}

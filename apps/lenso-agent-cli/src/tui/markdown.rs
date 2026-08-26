use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::Palette;

pub(super) fn lines(markdown: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut code_fence = false;
    for raw in markdown.lines() {
        let trimmed = raw.trim_start();
        if let Some(language) = trimmed.strip_prefix("```") {
            code_fence = !code_fence;
            lines.push(code_fence_line(language, code_fence));
        } else if code_fence {
            lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Palette::QUIET)),
                Span::styled(raw.to_owned(), Style::default().fg(Palette::CODE)),
            ]));
        } else if let Some(heading) = trimmed.strip_prefix("### ") {
            lines.push(heading_line(heading, false));
        } else if let Some(heading) = trimmed
            .strip_prefix("## ")
            .or_else(|| trimmed.strip_prefix("# "))
        {
            lines.push(heading_line(heading, true));
        } else if let Some(item) = unordered_item(trimmed) {
            let mut spans = vec![Span::styled("• ", Style::default().fg(Palette::ACCENT))];
            spans.extend(inline(item));
            lines.push(Line::from(spans));
        } else if let Some((number, item)) = ordered_item(trimmed) {
            let mut spans = vec![Span::styled(
                format!("{number}. "),
                Style::default().fg(Palette::ACCENT),
            )];
            spans.extend(inline(item));
            lines.push(Line::from(spans));
        } else if let Some(quote) = trimmed.strip_prefix("> ") {
            let mut spans = vec![Span::styled("│ ", Style::default().fg(Palette::BORDER))];
            spans.extend(inline(quote));
            lines.push(Line::from(spans).style(Style::default().fg(Palette::MUTED)));
        } else if matches!(trimmed, "---" | "***" | "___") {
            lines.push(Line::from(Span::styled(
                "────────",
                Style::default().fg(Palette::QUIET),
            )));
        } else {
            lines.push(Line::from(inline(raw)));
        }
    }
    lines
}

fn code_fence_line(language: &str, opening: bool) -> Line<'static> {
    let label = if opening {
        if language.is_empty() {
            "╭─ code".to_owned()
        } else {
            format!("╭─ {language}")
        }
    } else {
        "╰─".to_owned()
    };
    Line::from(Span::styled(label, Style::default().fg(Palette::QUIET)))
}

fn heading_line(text: &str, prominent: bool) -> Line<'static> {
    let mut style = Style::default().fg(Palette::HEADING);
    if prominent {
        style = style.add_modifier(Modifier::BOLD);
    }
    Line::from(Span::styled(text.to_owned(), style))
}

fn unordered_item(text: &str) -> Option<&str> {
    text.strip_prefix("- ").or_else(|| text.strip_prefix("* "))
}

fn ordered_item(text: &str) -> Option<(&str, &str)> {
    let (number, item) = text.split_once(". ")?;
    number
        .chars()
        .all(|character| character.is_ascii_digit())
        .then_some((number, item))
}

fn inline(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let code = remaining.find('`').map(|index| (index, '`'));
        let bold = remaining.find("**").map(|index| (index, '*'));
        let Some((index, marker)) = [code, bold].into_iter().flatten().min_by_key(|item| item.0)
        else {
            spans.push(Span::raw(remaining.to_owned()));
            break;
        };
        if index > 0 {
            spans.push(Span::raw(remaining[..index].to_owned()));
        }
        let marker_width = usize::from(marker == '*') + 1;
        let after = &remaining[index + marker_width..];
        let closing = if marker == '`' { "`" } else { "**" };
        if let Some(end) = after.find(closing) {
            let style = if marker == '`' {
                Style::default().fg(Palette::CODE)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            spans.push(Span::styled(after[..end].to_owned(), style));
            remaining = &after[end + marker_width..];
        } else {
            spans.push(Span::raw(remaining[index..].to_owned()));
            break;
        }
    }
    spans
}

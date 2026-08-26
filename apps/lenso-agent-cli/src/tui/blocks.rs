//! Semantic transcript blocks for the terminal shell.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde_json::Value;

use super::Palette;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolKind {
    Read,
    Search,
    List,
    Execute,
    Edit,
    Create,
    Skill,
    Other,
}

impl ToolKind {
    fn from_name(name: &str) -> Self {
        match name {
            "read" | "read_text" | "read_resource" => Self::Read,
            "search" | "memory_search" | "web_search" => Self::Search,
            "list" | "list_resources" | "list_dir" => Self::List,
            "run_process" | "execute" | "shell" | "bash" => Self::Execute,
            "edit" | "apply_patch" => Self::Edit,
            "create_file" | "write" => Self::Create,
            "skill" | "skill_resource" => Self::Skill,
            _ => Self::Other,
        }
    }

    const fn verb(self, finished: bool) -> &'static str {
        match (self, finished) {
            (Self::Read, false) => "Reading",
            (Self::Read, true) => "Read",
            (Self::Search, false) => "Searching",
            (Self::Search, true) => "Searched",
            (Self::List, false) => "Listing",
            (Self::List, true) => "Listed",
            (Self::Execute, false) => "Running",
            (Self::Execute, true) => "Ran",
            (Self::Edit, false) => "Editing",
            (Self::Edit, true) => "Edited",
            (Self::Create, false) => "Creating",
            (Self::Create, true) => "Created",
            (Self::Skill, false) => "Loading skill",
            (Self::Skill, true) => "Loaded skill",
            (Self::Other, false) => "Calling",
            (Self::Other, true) => "Called",
        }
    }

    const fn color(self) -> Color {
        match self {
            Self::Read | Self::List | Self::Skill => Color::LightBlue,
            Self::Search => Color::LightMagenta,
            Self::Execute => Color::LightCyan,
            Self::Edit | Self::Create => Color::LightGreen,
            Self::Other => Color::Gray,
        }
    }
}

#[derive(Debug)]
pub(super) struct ToolCard {
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) arguments_json: Option<String>,
    pub(super) content: Option<String>,
    pub(super) metadata_json: Option<String>,
    pub(super) duration_ms: Option<u64>,
    pub(super) error: Option<String>,
    pub(super) status: ToolStatus,
    pub(super) expanded: bool,
    kind: ToolKind,
}

impl ToolCard {
    pub(super) fn running(call_id: String, name: String, arguments_json: Option<String>) -> Self {
        let kind = ToolKind::from_name(&name);
        Self {
            call_id,
            name,
            arguments_json,
            content: None,
            metadata_json: None,
            duration_ms: None,
            error: None,
            status: ToolStatus::Running,
            expanded: matches!(kind, ToolKind::Execute | ToolKind::Edit | ToolKind::Create),
            kind,
        }
    }

    pub(super) fn activity(&self) -> String {
        format!("{} {}", self.kind.verb(false), self.subject())
    }

    fn subject(&self) -> String {
        let arguments = self.arguments();
        match self.kind {
            ToolKind::Read | ToolKind::List | ToolKind::Edit | ToolKind::Create => arguments
                .as_ref()
                .and_then(|value| string_field(value, &["path", "file_path"]))
                .unwrap_or_else(|| self.name.clone()),
            ToolKind::Search => arguments
                .as_ref()
                .and_then(|value| string_field(value, &["query", "pattern"]))
                .map_or_else(
                    || self.name.clone(),
                    |value| format!("\u{201c}{value}\u{201d}"),
                ),
            ToolKind::Execute => command(arguments.as_ref()).unwrap_or_else(|| self.name.clone()),
            ToolKind::Skill => arguments
                .as_ref()
                .and_then(|value| string_field(value, &["name", "skill", "path"]))
                .unwrap_or_else(|| self.name.clone()),
            ToolKind::Other => self.name.replace('_', " "),
        }
    }

    fn arguments(&self) -> Option<Value> {
        self.arguments_json
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
    }
}

pub(super) fn render_tool_block(lines: &mut Vec<Line<'static>>, card: &ToolCard, selected: bool) {
    let failed = card.status == ToolStatus::Failed;
    let running = card.status == ToolStatus::Running;
    let color = if failed {
        Palette::ERROR
    } else {
        card.kind.color()
    };
    let bullet = if running {
        "●"
    } else if failed {
        "×"
    } else {
        "◆"
    };
    let disclosure = if card.expanded { "▾" } else { "▸" };
    let mut header = vec![
        Span::styled(
            format!("{disclosure} {bullet} "),
            Style::default().fg(if selected { Palette::ACCENT } else { color }),
        ),
        Span::styled(
            format!("{} ", card.kind.verb(!running)),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(card.subject(), Style::default().fg(path_color(card.kind))),
    ];
    if let Some(summary) = result_summary(card) {
        header.push(Span::styled(
            format!("  {summary}"),
            Style::default().fg(Palette::MUTED),
        ));
    }
    if let Some(duration_ms) = card.duration_ms {
        header.push(Span::styled(
            format!("  {}", format_duration(duration_ms)),
            Style::default().fg(Palette::QUIET),
        ));
    }
    lines.push(Line::from(header));

    match card.kind {
        ToolKind::Execute if card.expanded => render_process(lines, card),
        ToolKind::Edit | ToolKind::Create if card.expanded => render_edit(lines, card),
        ToolKind::Read if card.expanded => render_read(lines, card),
        ToolKind::Search | ToolKind::List if card.expanded => render_results(lines, card),
        _ if card.expanded => render_generic(lines, card),
        _ => {}
    }
}

fn render_process(lines: &mut Vec<Line<'static>>, card: &ToolCard) {
    if let Some(command) = command(card.arguments().as_ref()) {
        detail_line(lines, "$ ", &command, Palette::CODE);
    }
    if let Some(content) = card.content.as_deref().filter(|value| !value.is_empty()) {
        for line in bounded_preview(content, 12, 4096).lines() {
            detail_line(lines, "  ", line, Palette::MUTED);
        }
    }
    render_error(lines, card);
}

fn render_edit(lines: &mut Vec<Line<'static>>, card: &ToolCard) {
    let arguments = card.arguments();
    let old = arguments
        .as_ref()
        .and_then(|value| string_field(value, &["old_text"]));
    let new = arguments
        .as_ref()
        .and_then(|value| string_field(value, &["new_text", "content"]));
    if let Some(old) = old {
        for line in bounded_preview(&old, 10, 2048).lines() {
            lines.push(Line::from(vec![
                Span::styled("  - ", Style::default().fg(Color::LightRed)),
                Span::styled(line.to_owned(), Style::default().fg(Color::LightRed)),
            ]));
        }
    }
    if let Some(new) = new {
        for line in bounded_preview(&new, 10, 2048).lines() {
            lines.push(Line::from(vec![
                Span::styled("  + ", Style::default().fg(Color::LightGreen)),
                Span::styled(line.to_owned(), Style::default().fg(Color::LightGreen)),
            ]));
        }
    }
    render_error(lines, card);
}

fn render_read(lines: &mut Vec<Line<'static>>, card: &ToolCard) {
    if let Some(content) = card.content.as_deref().filter(|value| !value.is_empty()) {
        for line in bounded_preview(content, 16, 4096).lines() {
            detail_line(lines, "  ", line, Palette::CODE);
        }
    }
    render_error(lines, card);
}

fn render_results(lines: &mut Vec<Line<'static>>, card: &ToolCard) {
    if let Some(content) = card.content.as_deref().filter(|value| !value.is_empty()) {
        let formatted = serde_json::from_str::<Value>(content)
            .ok()
            .and_then(|value| serde_json::to_string_pretty(&value).ok())
            .unwrap_or_else(|| content.to_owned());
        for line in bounded_preview(&formatted, 18, 4096).lines() {
            detail_line(lines, "  ", line, Palette::MUTED);
        }
    }
    render_error(lines, card);
}

fn render_generic(lines: &mut Vec<Line<'static>>, card: &ToolCard) {
    if let Some(arguments) = card.arguments_json.as_deref() {
        let formatted = pretty_json(arguments);
        for line in bounded_preview(&formatted, 8, 2048).lines() {
            detail_line(lines, "  ", line, Palette::CODE);
        }
    }
    if let Some(content) = card.content.as_deref().filter(|value| !value.is_empty()) {
        for line in bounded_preview(content, 12, 4096).lines() {
            detail_line(lines, "  ", line, Palette::MUTED);
        }
    }
    render_error(lines, card);
}

fn render_error(lines: &mut Vec<Line<'static>>, card: &ToolCard) {
    if let Some(error) = card.error.as_deref() {
        detail_line(lines, "× ", error, Palette::ERROR);
    }
}

fn detail_line(lines: &mut Vec<Line<'static>>, marker: &str, value: &str, color: Color) {
    lines.push(Line::from(vec![
        Span::styled("  │ ", Style::default().fg(Palette::BORDER)),
        Span::styled(marker.to_owned(), Style::default().fg(color)),
        Span::styled(value.to_owned(), Style::default().fg(color)),
    ]));
}

fn result_summary(card: &ToolCard) -> Option<String> {
    let metadata = card
        .metadata_json
        .as_deref()
        .and_then(|value| serde_json::from_str::<Value>(value).ok());
    match card.kind {
        ToolKind::Search => metadata.as_ref().and_then(|value| {
            number_field(value, &["matches", "match_count"]).map(|count| {
                if count == 1 {
                    "1 match".to_owned()
                } else {
                    format!("{count} matches")
                }
            })
        }),
        ToolKind::List => metadata.as_ref().and_then(|value| {
            number_field(value, &["entries", "count"]).map(|count| {
                if count == 1 {
                    "1 entry".to_owned()
                } else {
                    format!("{count} entries")
                }
            })
        }),
        ToolKind::Execute => metadata.as_ref().and_then(|value| {
            value
                .get("exit_code")
                .and_then(Value::as_str)
                .map(|code| format!("exit {code}"))
        }),
        ToolKind::Edit | ToolKind::Create => metadata.as_ref().and_then(|value| {
            value
                .get("bytes_written")
                .and_then(Value::as_u64)
                .map(|bytes| format!("{bytes} B"))
        }),
        _ => None,
    }
}

fn command(arguments: Option<&Value>) -> Option<String> {
    let arguments = arguments?;
    let program = string_field(arguments, &["program", "command"])?;
    let args = arguments
        .get("arguments")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    Some(if args.is_empty() {
        program
    } else {
        format!("{program} {args}")
    })
}

fn string_field(value: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn number_field(value: &Value, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_u64))
}

fn path_color(kind: ToolKind) -> Color {
    if matches!(
        kind,
        ToolKind::Read | ToolKind::List | ToolKind::Edit | ToolKind::Create
    ) {
        Color::LightBlue
    } else {
        Palette::MUTED
    }
}

fn pretty_json(value: &str) -> String {
    serde_json::from_str::<Value>(value)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| value.to_owned())
}

fn bounded_preview(value: &str, max_lines: usize, max_characters: usize) -> String {
    let mut preview = String::new();
    let mut characters = 0;
    let mut truncated = false;
    for (index, line) in value.lines().enumerate() {
        if index >= max_lines || characters >= max_characters {
            truncated = true;
            break;
        }
        if index > 0 {
            preview.push('\n');
            characters += 1;
        }
        let remaining = max_characters.saturating_sub(characters);
        let accepted = line.chars().take(remaining).collect::<String>();
        characters += accepted.chars().count();
        preview.push_str(&accepted);
        if accepted.chars().count() < line.chars().count() {
            truncated = true;
            break;
        }
    }
    if truncated {
        preview.push_str("\n\u{2026} truncated");
    }
    preview
}

fn format_duration(duration_ms: u64) -> String {
    if duration_ms < 1_000 {
        format!("{duration_ms}ms")
    } else {
        format!(
            "{}.{:01}s",
            duration_ms / 1_000,
            (duration_ms % 1_000) / 100
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_block_uses_semantic_title_and_diff_lines() {
        let mut card = ToolCard::running(
            "call-1".to_owned(),
            "edit".to_owned(),
            Some(r#"{"path":"src/lib.rs","old_text":"old","new_text":"new"}"#.to_owned()),
        );
        card.status = ToolStatus::Completed;
        let mut lines = Vec::new();
        render_tool_block(&mut lines, &card, false);
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("Edited src/lib.rs"));
        assert!(text.contains("- old"));
        assert!(text.contains("+ new"));
    }

    #[test]
    fn process_subject_is_a_shell_command() {
        let card = ToolCard::running(
            "call-1".to_owned(),
            "run_process".to_owned(),
            Some(r#"{"program":"cargo","arguments":["test","-q"]}"#.to_owned()),
        );
        assert_eq!(card.activity(), "Running cargo test -q");
    }
}

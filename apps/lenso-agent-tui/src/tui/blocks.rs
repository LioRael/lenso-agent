//! Semantic transcript blocks for the terminal shell.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde_json::Value;
use std::time::Instant;

use super::Palette;
use super::markdown_lines;

const THINKING_TAIL_LINES: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ThinkingStatus {
    Running,
    Completed,
}

#[derive(Debug)]
pub(super) struct ThinkingCard {
    pub(super) reasoning_id: Option<String>,
    pub(super) text: String,
    pub(super) status: ThinkingStatus,
    pub(super) expanded: bool,
    started_at: Instant,
    duration_ms: Option<u64>,
}

impl ThinkingCard {
    pub(super) fn provisional() -> Self {
        Self {
            reasoning_id: None,
            text: String::new(),
            status: ThinkingStatus::Running,
            expanded: false,
            started_at: Instant::now(),
            duration_ms: None,
        }
    }

    pub(super) fn append(&mut self, reasoning_id: String, text: &str) {
        self.reasoning_id = Some(reasoning_id);
        self.text.push_str(text);
    }

    pub(super) fn finish(&mut self, duration_ms: Option<u64>) {
        self.status = ThinkingStatus::Completed;
        self.expanded = false;
        self.duration_ms = duration_ms.or_else(|| {
            Some(u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX))
        });
    }

    pub(super) fn is_running(&self) -> bool {
        self.status == ThinkingStatus::Running
    }

    fn elapsed_ms(&self) -> u64 {
        self.duration_ms.unwrap_or_else(|| {
            u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
        })
    }
}

pub(super) fn render_thinking_block(
    lines: &mut Vec<Line<'static>>,
    card: &ThinkingCard,
    tick: u64,
) {
    let running = card.status == ThinkingStatus::Running;
    let bullet = "◆";
    let bullet_color = if running {
        if tick.is_multiple_of(2) {
            Palette::ACCENT
        } else {
            Palette::MUTED
        }
    } else {
        Palette::QUIET
    };
    let mut header = vec![
        Span::styled(format!("{bullet} "), Style::default().fg(bullet_color)),
        Span::styled(
            if running { "Thinking…" } else { "Thought" },
            Style::default()
                .fg(Palette::MUTED)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if !running {
        header.push(Span::styled(
            format!(" for {}", format_thinking_duration(card.elapsed_ms())),
            Style::default().fg(Palette::QUIET),
        ));
    }
    lines.push(Line::from(header));

    if card.text.is_empty() || (!running && !card.expanded) {
        return;
    }
    lines.push(Line::default());
    let mut body = markdown_lines(&card.text);
    if running && body.len() > THINKING_TAIL_LINES {
        body = body.split_off(body.len() - THINKING_TAIL_LINES);
        lines.push(Line::from(Span::styled(
            "  …",
            Style::default().fg(Palette::QUIET),
        )));
    }
    for mut line in body {
        for span in &mut line.spans {
            span.style = span.style.patch(Style::default().fg(Palette::MUTED));
        }
        lines.push(line);
    }
}

fn format_thinking_duration(duration_ms: u64) -> String {
    if duration_ms < 60_000 {
        let seconds = duration_ms / 1_000;
        let tenths = duration_ms % 1_000 / 100;
        format!("{seconds}.{tenths}s")
    } else {
        let minutes = duration_ms / 60_000;
        let remaining = (duration_ms % 60_000).saturating_add(500) / 1_000;
        format!("{minutes}m{remaining}s")
    }
}

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
    Checkpoint,
    Skill,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToolGroupKind {
    Read,
    Search,
    List,
    Execute,
    Edit,
}

impl ToolGroupKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Read | Self::Edit => "files",
            Self::Search => "searches",
            Self::List => "directories",
            Self::Execute => "commands",
        }
    }

    const fn verb(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::Search => "Searched",
            Self::List => "Listed",
            Self::Execute => "Ran",
            Self::Edit => "Edited",
        }
    }
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
            "checkpoint_create" | "checkpoint_review" | "checkpoint_accept"
            | "checkpoint_restore" => Self::Checkpoint,
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
            (Self::Checkpoint, false) => "Reviewing",
            (Self::Checkpoint, true) => "Reviewed",
            (Self::Skill, false) => "Loading skill",
            (Self::Skill, true) => "Loaded skill",
            (Self::Other, false) => "Calling",
            (Self::Other, true) => "Called",
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
            expanded: false,
            kind,
        }
    }

    pub(super) fn activity(&self) -> String {
        format!("{} {}", self.verb(false), self.subject())
    }

    pub(super) fn append_progress(&mut self, chunk: &str) {
        const MAX_PROGRESS_CHARACTERS: usize = 1_048_576;
        let content = self.content.get_or_insert_with(String::new);
        let current = content.chars().count();
        if current >= MAX_PROGRESS_CHARACTERS {
            return;
        }
        content.extend(chunk.chars().take(MAX_PROGRESS_CHARACTERS - current));
    }

    pub(super) fn group_kind(&self) -> Option<ToolGroupKind> {
        if self.status != ToolStatus::Completed || self.error.is_some() {
            return None;
        }
        match self.kind {
            ToolKind::Read | ToolKind::Skill => Some(ToolGroupKind::Read),
            ToolKind::Search => Some(ToolGroupKind::Search),
            ToolKind::List => Some(ToolGroupKind::List),
            ToolKind::Execute => Some(ToolGroupKind::Execute),
            ToolKind::Edit | ToolKind::Create | ToolKind::Checkpoint => Some(ToolGroupKind::Edit),
            ToolKind::Other => None,
        }
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
            ToolKind::Checkpoint => arguments
                .as_ref()
                .and_then(|value| string_field(value, &["checkpoint_id"]))
                .map_or_else(
                    || self.name.replace('_', " "),
                    |value| format!("checkpoint {}", value.chars().take(8).collect::<String>()),
                ),
            ToolKind::Other => self.name.replace('_', " "),
        }
    }

    fn verb(&self, finished: bool) -> &'static str {
        match (self.name.as_str(), finished) {
            ("checkpoint_create", false) => "Creating",
            ("checkpoint_create", true) => "Created",
            ("checkpoint_accept", false) => "Accepting",
            ("checkpoint_accept", true) => "Accepted",
            ("checkpoint_restore", false) => "Restoring",
            ("checkpoint_restore", true) => "Restored",
            _ => self.kind.verb(finished),
        }
    }

    fn arguments(&self) -> Option<Value> {
        self.arguments_json
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
    }
}

pub(super) fn render_tool_group(
    lines: &mut Vec<Line<'static>>,
    kind: ToolGroupKind,
    cards: &[&ToolCard],
    expanded: bool,
    selected: bool,
) {
    let disclosure = if expanded { "▾" } else { "▸" };
    let count = cards.len();
    let duration = cards
        .iter()
        .filter_map(|card| card.duration_ms)
        .reduce(u64::saturating_add);
    let mut header = vec![
        Span::styled(
            format!("{disclosure} ● "),
            Style::default().fg(if selected {
                Palette::ACCENT
            } else {
                Palette::SUCCESS
            }),
        ),
        Span::styled(
            format!("{} ", kind.verb()),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{count} {}", kind.label()),
            Style::default().fg(Palette::MUTED),
        ),
    ];
    if let Some(duration) = duration {
        header.push(Span::styled(
            format!("  {}", format_duration(duration)),
            Style::default().fg(Palette::QUIET),
        ));
    }
    lines.push(Line::from(header));
}

pub(super) fn render_tool_block(lines: &mut Vec<Line<'static>>, card: &ToolCard, selected: bool) {
    let failed = card.status == ToolStatus::Failed;
    let running = card.status == ToolStatus::Running;
    let accent = if failed {
        Palette::ERROR
    } else if running {
        Palette::ACCENT
    } else {
        Palette::SUCCESS
    };
    let bullet = if failed { "×" } else { "◆" };
    let disclosure = if card.expanded { "▾" } else { "▸" };
    let mut header = vec![
        Span::styled(
            format!("{bullet} "),
            Style::default().fg(if selected { Palette::ACCENT } else { accent }),
        ),
        Span::styled(
            format!("{} ", card.verb(!running)),
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
    if card.expanded || card.content.is_some() || card.arguments_json.is_some() {
        header.push(Span::styled(
            format!("  {disclosure}"),
            Style::default().fg(Palette::QUIET),
        ));
    }
    lines.push(Line::from(header));

    match card.kind {
        ToolKind::Execute if card.expanded => render_process(lines, card),
        ToolKind::Edit | ToolKind::Create if card.expanded => render_edit(lines, card),
        ToolKind::Checkpoint if card.expanded => render_checkpoint(lines, card),
        ToolKind::Read if card.expanded => render_read(lines, card),
        ToolKind::Search | ToolKind::List if card.expanded => render_results(lines, card),
        _ if card.expanded => render_generic(lines, card),
        _ => {}
    }
}

pub(super) fn render_grouped_tool_block(
    lines: &mut Vec<Line<'static>>,
    card: &ToolCard,
    selected: bool,
) {
    let mut nested = Vec::new();
    render_tool_block(&mut nested, card, selected);
    for mut line in nested {
        line.spans.insert(
            0,
            Span::styled("  │ ", Style::default().fg(Palette::BORDER)),
        );
        lines.push(line);
    }
}

fn render_process(lines: &mut Vec<Line<'static>>, card: &ToolCard) {
    if let Some(command) = command(card.arguments().as_ref()) {
        detail_line(lines, "$ ", &command, Palette::COMMAND);
    }
    if let Some(content) = card.content.as_deref().filter(|value| !value.is_empty()) {
        for line in bounded_tail(content, 12, 4096).lines() {
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
    let start_line = card
        .metadata_json
        .as_deref()
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .and_then(|value| value.get("start_line").and_then(Value::as_u64))
        .and_then(|line| usize::try_from(line).ok())
        .unwrap_or(1);
    let old_lines = old.as_deref().map_or(0, |text| text.lines().count());
    let new_lines = new.as_deref().map_or(0, |text| text.lines().count());
    lines.push(Line::from(Span::styled(
        format!("  @@ -{start_line},{old_lines} +{start_line},{new_lines} @@"),
        Style::default().fg(Palette::QUIET),
    )));
    if let Some(old) = old {
        render_diff_side(lines, &old, start_line, true);
    }
    if let Some(new) = new {
        render_diff_side(lines, &new, start_line, false);
    }
    render_error(lines, card);
}

fn render_checkpoint(lines: &mut Vec<Line<'static>>, card: &ToolCard) {
    if let Some(content) = card.content.as_deref().filter(|value| !value.is_empty()) {
        for line in bounded_preview(content, 24, 8192).lines() {
            let color = if line.starts_with('+') && !line.starts_with("+++") {
                Color::LightGreen
            } else if line.starts_with('-') && !line.starts_with("---") {
                Color::LightRed
            } else {
                Palette::MUTED
            };
            detail_line(lines, "  ", line, color);
        }
    }
    render_error(lines, card);
}

fn render_diff_side(lines: &mut Vec<Line<'static>>, text: &str, start_line: usize, removed: bool) {
    let (marker, color) = if removed {
        ("-", Color::LightRed)
    } else {
        ("+", Color::LightGreen)
    };
    for (offset, content) in bounded_preview(text, 14, 4096).lines().enumerate() {
        let line = start_line.saturating_add(offset);
        let number = if removed {
            format!("{line:>4}     ")
        } else {
            format!("     {line:>4}")
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {number} {marker} "), Style::default().fg(color)),
            Span::styled(content.to_owned(), Style::default().fg(color)),
        ]));
    }
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
    lines.push(
        Line::from(vec![
            Span::styled("    ", Style::default().bg(Palette::SURFACE)),
            Span::styled(
                marker.to_owned(),
                Style::default().fg(color).bg(Palette::SURFACE),
            ),
            Span::styled(
                value.to_owned(),
                Style::default().fg(color).bg(Palette::SURFACE),
            ),
            Span::styled("  ", Style::default().bg(Palette::SURFACE)),
        ])
        .style(Style::default().bg(Palette::SURFACE)),
    );
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
        ToolKind::Checkpoint => metadata.as_ref().and_then(|value| {
            let changes = value.get("changes").and_then(Value::as_u64);
            let conflicts = value.get("conflicts").and_then(Value::as_u64);
            match (changes, conflicts) {
                (Some(changes), Some(0)) => Some(format!("{changes} changes")),
                (Some(changes), Some(conflicts)) => {
                    Some(format!("{changes} changes, {conflicts} conflicts"))
                }
                _ => value
                    .get("files")
                    .and_then(Value::as_u64)
                    .map(|files| format!("{files} files")),
            }
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
        ToolKind::Read | ToolKind::List | ToolKind::Edit | ToolKind::Create | ToolKind::Checkpoint
    ) {
        Palette::PATH
    } else if kind == ToolKind::Execute {
        Palette::COMMAND
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

fn bounded_tail(value: &str, max_lines: usize, max_characters: usize) -> String {
    let all_lines = value.lines().collect::<Vec<_>>();
    let omitted = all_lines.len().saturating_sub(max_lines);
    let tail = all_lines
        .into_iter()
        .skip(omitted)
        .collect::<Vec<_>>()
        .join("\n");
    let clipped = tail.chars().count().saturating_sub(max_characters);
    let mut preview = if clipped == 0 {
        tail
    } else {
        tail.chars()
            .rev()
            .take(max_characters)
            .collect::<String>()
            .chars()
            .rev()
            .collect()
    };
    if clipped > 0 {
        preview.insert_str(0, &format!("\u{2026} {clipped} earlier characters\n"));
    }
    if omitted > 0 {
        preview.insert_str(0, &format!("\u{2026} {omitted} earlier lines\n"));
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
        card.expanded = true;
        card.metadata_json = Some(r#"{"start_line":12}"#.to_owned());
        let mut lines = Vec::new();
        render_tool_block(&mut lines, &card, false);
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("Edited src/lib.rs"));
        assert!(text.contains("@@ -12,1 +12,1 @@"));
        assert!(text.contains("12"));
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

    #[test]
    fn checkpoint_review_renders_a_semantic_diff_card() {
        let mut card = ToolCard::running(
            "call-1".to_owned(),
            "checkpoint_review".to_owned(),
            Some(r#"{"checkpoint_id":"12345678-1234-1234-1234-123456789abc"}"#.to_owned()),
        );
        card.status = ToolStatus::Completed;
        card.expanded = true;
        card.content =
            Some("--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n".to_owned());
        card.metadata_json = Some(r#"{"changes":1,"conflicts":0}"#.to_owned());
        let mut lines = Vec::new();
        render_tool_block(&mut lines, &card, false);
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("Reviewed checkpoint 12345678"));
        assert!(text.contains("1 changes"));
        assert!(text.contains("-old"));
        assert!(text.contains("+new"));
    }

    #[test]
    fn checkpoint_actions_use_their_exact_verbs() {
        for (name, running, completed) in [
            ("checkpoint_create", "Creating", "Created"),
            ("checkpoint_accept", "Accepting", "Accepted"),
            ("checkpoint_restore", "Restoring", "Restored"),
        ] {
            let mut card = ToolCard::running(
                "call-1".to_owned(),
                name.to_owned(),
                Some(r#"{"checkpoint_id":"12345678-1234-1234-1234-123456789abc"}"#.to_owned()),
            );
            assert!(card.activity().starts_with(running));
            card.status = ToolStatus::Completed;
            let mut lines = Vec::new();
            render_tool_block(&mut lines, &card, false);
            let text = lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(text.contains(&format!("{completed} checkpoint")));
        }
    }

    #[test]
    fn process_output_keeps_the_recent_tail() {
        let mut card = ToolCard::running(
            "call-1".to_owned(),
            "run_process".to_owned(),
            Some(r#"{"program":"cargo","arguments":["test"]}"#.to_owned()),
        );
        card.status = ToolStatus::Completed;
        card.expanded = true;
        card.content = Some(
            (1..=20)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let mut lines = Vec::new();
        render_tool_block(&mut lines, &card, false);
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("8 earlier lines"));
        assert!(!text.contains("line 8"));
        assert!(text.contains("line 20"));
    }
}

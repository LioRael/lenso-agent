//! Display-width-aware text helpers shared by terminal projections and overlays.

use ratatui::text::Line;

pub(super) fn truncate_text(text: &str, max_width: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_preserves_display_width_for_wide_text() {
        assert_eq!(truncate_text("short", 5), "short");
        assert_eq!(truncate_text("abc界def", 6), "abc界…");
        assert_eq!(truncate_text("anything", 0), "");
    }
}

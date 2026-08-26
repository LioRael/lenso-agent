//! Semantic composer suggestions contributed to the Agent TUI Shell.

#[allow(dead_code)]
mod contract;

include!("generated.rs");

/// Maximum number of semantic suggestions one provider may return.
pub const MAX_SUGGESTIONS_PER_SNAPSHOT: usize = 2_048;

/// Enforces provider-local identity and size rules on native typed providers.
pub fn validate_snapshot_suggestions(suggestions: &[Suggestion]) -> Result<(), String> {
    if suggestions.len() > MAX_SUGGESTIONS_PER_SNAPSHOT {
        return Err(format!(
            "snapshot exceeds the {MAX_SUGGESTIONS_PER_SNAPSHOT}-suggestion limit"
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for suggestion in suggestions {
        let valid_id = !suggestion.id.is_empty()
            && suggestion.id.chars().count() <= 128
            && suggestion.id.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || (index > 0 && matches!(byte, b'.' | b'_' | b'/' | b'-'))
            });
        if !valid_id
            || suggestion.label.is_empty()
            || suggestion.label.chars().count() > 256
            || suggestion.insert_text.is_empty()
            || suggestion.insert_text.chars().count() > 1_024
            || suggestion.description.chars().count() > 512
            || !ids.insert(suggestion.id.as_str())
        {
            return Err(format!(
                "invalid or duplicate TUI suggestion `{}`",
                suggestion.id
            ));
        }
    }
    Ok(())
}

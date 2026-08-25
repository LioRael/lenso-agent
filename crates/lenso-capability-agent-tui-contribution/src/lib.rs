//! Semantic read-only panels contributed to the Agent TUI Shell.

include!("generated.rs");

/// Maximum number of semantic panels one provider may return per snapshot.
pub const MAX_PANELS_PER_SNAPSHOT: usize = 16;

/// Enforces the portable response Schema on native typed providers.
pub fn validate_snapshot_panels(panels: &[SnapshotResponsePanelsItem]) -> Result<(), String> {
    if panels.len() > MAX_PANELS_PER_SNAPSHOT {
        return Err(format!(
            "snapshot exceeds the {MAX_PANELS_PER_SNAPSHOT}-panel limit"
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for panel in panels {
        let valid_id = !panel.id.is_empty()
            && panel.id.chars().count() <= 128
            && panel.id.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || (index > 0 && matches!(byte, b'.' | b'_' | b'/' | b'-'))
            });
        if !valid_id
            || panel.title.is_empty()
            || panel.title.chars().count() > 80
            || panel.body.chars().count() > 65_536
            || !ids.insert(panel.id.as_str())
        {
            return Err(format!("invalid or duplicate TUI panel `{}`", panel.id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_validation_enforces_the_portable_schema() {
        let panel = SnapshotResponsePanelsItem {
            id: "agent.help".to_owned(),
            title: "Help".to_owned(),
            body: String::new(),
        };
        assert!(validate_snapshot_panels(std::slice::from_ref(&panel)).is_ok());
        assert!(validate_snapshot_panels(&[panel.clone(), panel]).is_err());
    }
}

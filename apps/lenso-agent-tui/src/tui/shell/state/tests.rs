use crossterm::event::MouseEvent;
use lenso_capability_agent_user_interaction::InteractionOption;
use ratatui::backend::TestBackend;

// Behavioral regression tests for the private TUI state machine.

use super::*;

#[test]
fn rename_command_requires_and_extracts_a_title() {
    assert_eq!(
        rename_command("/rename Project Atlas").unwrap(),
        Some("Project Atlas")
    );
    assert!(rename_command("/rename").is_err());
    assert!(rename_command("/rename   ").is_err());
    assert_eq!(rename_command("/renamed normally").unwrap(), None);
}

mod composer;
mod interaction;
mod stream;
mod transcript;

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

#[test]
fn model_command_requires_one_exact_model_id() {
    assert_eq!(
        model_command("/model fixture/alternate-v1").unwrap(),
        Some("fixture/alternate-v1")
    );
    assert_eq!(model_command("/model   ").unwrap(), None);
    assert!(model_command("/model two models").is_err());
    assert_eq!(model_command("/models").unwrap(), None);
}

#[test]
fn permissions_command_only_narrows_to_explicit_tool_names() {
    assert_eq!(
        permissions_command("/permissions composed").unwrap(),
        Some(PermissionSelection::Composed)
    );
    assert_eq!(
        permissions_command("/permissions none").unwrap(),
        Some(PermissionSelection::Restricted(Vec::new()))
    );
    assert_eq!(
        permissions_command("/permissions allow read_text, list_files").unwrap(),
        Some(PermissionSelection::Restricted(vec![
            "read_text".to_owned(),
            "list_files".to_owned()
        ]))
    );
    assert!(permissions_command("/permissions allow ").is_err());
    assert!(permissions_command("/permissions all").is_err());
}

#[test]
fn mode_command_and_cycle_follow_normal_plan_auto_order() {
    assert_eq!(
        mode_command("/mode normal").unwrap(),
        Some(SessionMode::Normal)
    );
    assert_eq!(mode_command("/mode plan").unwrap(), Some(SessionMode::Plan));
    assert_eq!(mode_command("/mode auto").unwrap(), Some(SessionMode::Auto));
    assert!(mode_command("/mode unsafe").is_err());
    assert_eq!(SessionMode::Normal.next(), SessionMode::Plan);
    assert_eq!(SessionMode::Plan.next(), SessionMode::Auto);
    assert_eq!(SessionMode::Auto.next(), SessionMode::Normal);
}

#[test]
fn inference_commands_are_explicit_and_bounded() {
    assert_eq!(fast_command("/fast on").unwrap(), Some(FastSelection::On));
    assert_eq!(fast_command("/fast off").unwrap(), Some(FastSelection::Off));
    assert!(fast_command("/fast maybe").is_err());
    assert_eq!(
        thinking_command("/thinking default").unwrap(),
        Some(ThinkingSelection::Default)
    );
    assert_eq!(
        thinking_command("/thinking ultra").unwrap(),
        Some(ThinkingSelection::Effort("ultra"))
    );
    assert!(thinking_command("/thinking infinite").is_err());
}

mod composer;
mod interaction;
mod stream;
mod task_supervision;
mod transcript;

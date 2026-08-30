//! Generic TUI surface consumer Plugin identity.

use lenso::{ManyPort, Port, plugin};
use lenso_capability_terminal_command as command_capability;
use lenso_capability_tui_panel as panel_capability;
use lenso_capability_tui_suggestion as suggestion_capability;

#[plugin(consumer)]
#[derive(Clone, Debug)]
struct TerminalTuiSurface {
    commands: Port<command_capability::CommandClient>,
    panels: ManyPort<panel_capability::PanelClient>,
    suggestions: ManyPort<suggestion_capability::SuggestionClient>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_declares_the_generic_terminal_ports() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(descriptor["provided_capabilities"], serde_json::json!([]));
        assert_eq!(
            descriptor["required_capabilities"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }
}
